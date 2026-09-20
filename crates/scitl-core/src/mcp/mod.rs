//! 外部ツールサーバー(MCP)クライアント。Issue #28のスコープは「接続してツール一覧を
//! 取得する」ところまでで、実際のターン中のツール呼び出し(`call_tool`)への組み込みは
//! 別Issue(`orchestration::turn`はまだMCPを一切知らない)。
//!
//! 接続の都度サーバーへ繋ぎ、取得し終えたら切断するステートレスな設計とする。常駐接続や
//! コネクションプールは持たない(旧実装と同じく、ツール一覧はconfig.tomlにも永続化せず
//! 都度取得する。legacy/frontend.md 4節)。
//!
//! `rmcp`(公式Rust SDK)を使う。有効化するfeatureは`client`・`transport-child-process`・
//! `transport-streamable-http-client-reqwest`のみで、OAuth/認可系(`auth`)は有効化しない
//! (`.well-known`ディスカバリ等でユーザーが登録していない先への通信が発生し得るため。
//! principles.md 1節、Opusレビュー指摘)。

mod http;
mod stdio;

use std::time::Duration;

use secrecy::SecretString;
use serde::Serialize;

use crate::config::{McpEndpoint, McpServerConfig, SecretRef};
use crate::db::error::CoreError;
use crate::secrets;

/// 接続・ツール一覧取得それぞれに設ける固定タイムアウト。応答しないサーバーで
/// 設定画面が固まらないようにする(architecture.md 5節と同じ考え方)。
const LIST_TOOLS_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize)]
pub struct McpToolInfo {
    pub name: String,
    pub description: Option<String>,
}

/// streamable_http方式のURLを検証する(実際に接続する前、サーバー登録時のIPC層から呼ぶ)。
/// 検証本体は[`crate::net::validate_external_url`]に集約する(LLMプロバイダーのbase_url
/// 検証と共有。Opusレビュー指摘)。
pub fn validate_streamable_http_url(url: &str) -> Result<(), CoreError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| CoreError::Mcp(format!("url is not a valid URL: {e}")))?;
    crate::net::validate_external_url(&parsed).map_err(CoreError::Mcp)
}

/// リクエストの構造やMCPプロトコル自体が管理するヘッダー名。ユーザーが登録した
/// カスタムヘッダーで上書きされてはならない(`authorization`はMCPサーバーの認証に
/// 使う主用途のため許可する。Opusレビュー指摘)。
const RESERVED_HEADER_NAMES: &[&str] = &[
    "host",
    "content-length",
    "content-type",
    "transfer-encoding",
    "connection",
    "upgrade",
    "te",
    "trailer",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "accept",
    "mcp-session-id",
    "last-event-id",
];

/// ヘッダー名を検証する(サーバー登録時、実際に接続する前のIPC層から呼ぶ)。
/// CRLF・制御文字・空白等のトークン外文字は`HeaderName`のパース自体が拒否する
/// (ヘッダーインジェクション対策)。加えて予約名を拒否する。
pub fn validate_header_name(name: &str) -> Result<(), CoreError> {
    reqwest::header::HeaderName::from_bytes(name.as_bytes())
        .map_err(|e| CoreError::Mcp(format!("invalid header name '{name}': {e}")))?;
    if RESERVED_HEADER_NAMES
        .iter()
        .any(|r| name.eq_ignore_ascii_case(r))
    {
        return Err(CoreError::Mcp(format!("header name '{name}' is reserved")));
    }
    Ok(())
}

/// ヘッダー値を検証する。CRLF等のトークン外文字は`HeaderValue`のパース自体が拒否する。
pub fn validate_header_value(value: &str) -> Result<(), CoreError> {
    reqwest::header::HeaderValue::from_str(value)
        .map(|_| ())
        .map_err(|e| CoreError::Mcp(format!("invalid header value: {e}")))
}

/// `refs`が指す秘密情報をまとめて解決する。`secrets::load`(keyring呼び出し)は同期I/Oで
/// あり、architecture.md 4節の規律(同期処理は`spawn_blocking`から呼ぶ)に従って
/// 非同期タスク上で直接呼ばない(Opusレビュー指摘)。
async fn resolve_secrets(refs: &[SecretRef]) -> Result<Vec<(String, SecretString)>, CoreError> {
    let refs = refs.to_vec();
    tokio::task::spawn_blocking(move || {
        refs.into_iter()
            .map(|r| secrets::load(&r.key_ref).map(|secret| (r.name, secret)))
            .collect::<Result<Vec<_>, _>>()
    })
    .await
    .map_err(|e| CoreError::Mcp(format!("secret resolution task panicked: {e}")))?
}

/// サーバーへ接続し、ツール一覧を取得する。成功・失敗どちらの場合も接続は残さない。
pub async fn list_tools(server: &McpServerConfig) -> Result<Vec<McpToolInfo>, CoreError> {
    let result = match &server.endpoint {
        McpEndpoint::Stdio {
            command,
            args,
            env_refs,
        } => tokio::time::timeout(LIST_TOOLS_TIMEOUT, stdio::list_tools(command, args, env_refs)).await,
        McpEndpoint::StreamableHttp { url, header_refs } => {
            tokio::time::timeout(LIST_TOOLS_TIMEOUT, http::list_tools(url, header_refs)).await
        }
    };
    result.map_err(|_| CoreError::Mcp("timed out connecting to MCP server".to_string()))?
}

/// サーバーが書いた文字列(ツール名・説明・stderr)をUIへ渡す前の無害化。
/// 制御文字を除去し、長さの上限を設ける(principles.md 4節「防御は多層にする」;
/// WebViewはモデル/外部サーバー出力を描画する境界であるため、テキストとしてのみ
/// 扱われることを前提にしても、表示に使う文字種と長さは呼び出し側で絞る)。
const MAX_TEXT_CHARS: usize = 2000;

fn sanitize_tool_text(input: &str) -> String {
    let cleaned: String = input
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    if cleaned.chars().count() > MAX_TEXT_CHARS {
        cleaned.chars().take(MAX_TEXT_CHARS).collect()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_header_name_rejects_reserved_names() {
        assert!(validate_header_name("Authorization").is_ok());
        assert!(validate_header_name("X-Api-Key").is_ok());
        assert!(validate_header_name("Host").is_err());
        assert!(validate_header_name("Content-Length").is_err());
        assert!(validate_header_name("Mcp-Session-Id").is_err());
    }

    #[test]
    fn validate_header_name_rejects_injection_attempts() {
        assert!(validate_header_name("X-Bad\r\nEvil: 1").is_err());
        assert!(validate_header_name("").is_err());
        assert!(validate_header_name("has space").is_err());
    }

    #[test]
    fn validate_header_value_rejects_crlf() {
        assert!(validate_header_value("normal-value").is_ok());
        assert!(validate_header_value("bad\r\nX-Injected: 1").is_err());
    }
}
