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
mod net;
mod stdio;

use std::time::Duration;

use serde::Serialize;

use crate::config::{McpEndpoint, McpServerConfig};
use crate::db::error::CoreError;

/// 接続・ツール一覧取得それぞれに設ける固定タイムアウト。応答しないサーバーで
/// 設定画面が固まらないようにする(architecture.md 5節と同じ考え方)。
const LIST_TOOLS_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize)]
pub struct McpToolInfo {
    pub name: String,
    pub description: Option<String>,
}

/// streamable_http方式のURLを検証する(実際に接続する前、サーバー登録時のIPC層から呼ぶ)。
pub fn validate_streamable_http_url(url: &str) -> Result<(), CoreError> {
    net::validate(url)
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
