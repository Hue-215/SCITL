//! 外部ツールサーバー(MCP)クライアント。サーバーの登録・接続・ツール一覧の取得
//! (Issue #28)と、応答生成1ターンの中でのツール呼び出し(Issue #44)を担う。
//!
//! 接続は会話1ターンの間だけ張り、ターンが終われば切断する([`McpSessions`]。常駐接続や
//! コネクションプールは持たない。legacy/backend.md 9節「方針として重要」)。設定画面からの
//! ツール一覧取得は1回の取得で開いて閉じる。
//!
//! 取得したツール一覧はアプリ起動中だけ[`ToolCatalog`]に保持し、config.tomlには書かない
//! (Issue #104。ツール名・説明はユーザーの設定ではなくサーバー側の持ち物で、永続化した
//! 写しはサーバー側の更新を検知できない。legacy/backend.md 9節「ツール一覧の事前取得と
//! キャッシュ」)。
//!
//! `rmcp`(公式Rust SDK)を使う。有効化するfeatureは`client`・`transport-child-process`・
//! `transport-streamable-http-client-reqwest`のみで、OAuth/認可系(`auth`)は有効化しない
//! (`.well-known`ディスカバリ等でユーザーが登録していない先への通信が発生し得るため。
//! principles.md 1節、Opusレビュー指摘)。

mod http;
mod stdio;

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock};
use rmcp::service::RunningService;
use rmcp::RoleClient;
use secrecy::SecretString;
use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::config::{McpEndpoint, McpServerConfig, SecretRef};
use crate::db::error::CoreError;
use crate::secrets;

/// 接続・ツール一覧取得・ツール呼び出しそれぞれに設ける固定タイムアウト。応答しない
/// サーバーで設定画面やターンが固まらないようにする(architecture.md 5節と同じ考え方)。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const LIST_TOOLS_TIMEOUT: Duration = Duration::from_secs(30);
/// 1回のツール呼び出しの上限。ターン全体の上限を設定可能にするのは Issue #71。
const CALL_TOOL_TIMEOUT: Duration = Duration::from_secs(60);

/// クライアント側のMCPセッション。ハンドラを持たない(`()`)ため、サーバーからの
/// サンプリング要求等には応答しない(公開範囲を広げないための既定。principles.md 4節)。
type ClientService = RunningService<RoleClient, ()>;

#[derive(Debug, Clone, Serialize)]
pub struct McpToolInfo {
    pub name: String,
    pub description: Option<String>,
    /// サーバーが宣言した引数スキーマ(JSON Schema)。モデルへツールを公開するときに
    /// そのまま渡す(信頼境界は登録したこと自体に置く。principles.md 4節)。
    /// 設定画面へは渡さない(表示に使わないものをWebViewへ出さない)。
    pub input_schema: Value,
}

/// 取得済みツール一覧のメモリキャッシュ(Issue #104)。アプリ起動中のみ有効で、
/// config.tomlには書かない。設定画面の表示と、ターン開始時のツール公開の両方が
/// ここを読む(同じ一覧の出どころを2つ持たない。principles.md 5節)。
#[derive(Debug, Default)]
pub struct ToolCatalog {
    by_server: Mutex<HashMap<String, Vec<McpToolInfo>>>,
}

impl ToolCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, server_id: &str) -> Option<Vec<McpToolInfo>> {
        self.lock().get(server_id).cloned()
    }

    pub fn store(&self, server_id: &str, tools: Vec<McpToolInfo>) {
        self.lock().insert(server_id.to_string(), tools);
    }

    /// サーバーの削除・接続先の変更でキャッシュを捨てる。
    pub fn forget(&self, server_id: &str) {
        self.lock().remove(server_id);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<McpToolInfo>>> {
        self.by_server.lock().expect("tool catalog mutex poisoned")
    }
}

/// 応答生成1ターンの間だけ生きるセッション置き場(legacy/backend.md 9節)。
/// サーバーごとに最初に必要になった時点で接続し、ターンの終わりに[`Self::close`]で
/// まとめて切断する。呼び出し側は成功・失敗どちらの経路でも必ず`close`を通ること。
#[derive(Default)]
pub struct McpSessions {
    by_server: HashMap<String, ClientService>,
}

impl McpSessions {
    pub fn new() -> Self {
        Self::default()
    }

    /// ツール一覧を取得する。このターンで既に接続済みならその接続を使い回す。
    pub async fn list_tools(
        &mut self,
        server: &McpServerConfig,
    ) -> Result<Vec<McpToolInfo>, CoreError> {
        let service = self.session(server).await?;
        let result = tokio::time::timeout(LIST_TOOLS_TIMEOUT, service.list_tools(None))
            .await
            .map_err(|_| CoreError::Mcp("timed out listing tools".to_string()))?
            .map_err(|e| CoreError::Mcp(format!("failed to list tools: {e}")))?;
        Ok(result.tools.into_iter().map(to_tool_info).collect())
    }

    /// ツールを1件呼び出し、結果をモデルへ渡せるJSONに変換して返す。
    /// サーバーが「エラー」として返した結果も`Ok`で返す(通信・接続の失敗と、
    /// ツール自体の失敗を呼び出し側が区別できるようにする)。
    pub async fn call_tool(
        &mut self,
        server: &McpServerConfig,
        tool_name: &str,
        arguments: &Value,
    ) -> Result<Value, CoreError> {
        let arguments = match arguments {
            Value::Object(map) => Some(map.clone()),
            Value::Null => None,
            other => {
                return Err(CoreError::Mcp(format!(
                    "tool arguments must be a JSON object, got: {other}"
                )))
            }
        };

        let service = self.session(server).await?;
        let response = tokio::time::timeout(
            CALL_TOOL_TIMEOUT,
            service.call_tool_once(call_params(tool_name, arguments)),
        )
        .await
        .map_err(|_| CoreError::Mcp("timed out calling tool".to_string()))?
        .map_err(|e| CoreError::Mcp(format!("failed to call tool: {e}")))?;

        match response {
            CallToolResponse::Complete(result) => Ok(to_result_value(result)),
            // サーバー主導の追加入力要求(MRTR)・非同期タスク化には対応しない。
            // どちらも利用者へ問い返す導線が要るため、ここでは結果として拒否を返し、
            // モデルには「このツールはこの経路では完了できない」とだけ伝える。
            CallToolResponse::InputRequired(_) => Err(CoreError::Mcp(
                "tool requires interactive input, which is not supported".to_string(),
            )),
            CallToolResponse::Task(_) => Err(CoreError::Mcp(
                "tool returned an asynchronous task, which is not supported".to_string(),
            )),
            // `CallToolResponse`は`#[non_exhaustive]`。将来プロトコルに追加される
            // 完了以外の応答も、対応する導線が無い以上ここでは扱えない。
            _ => Err(CoreError::Mcp(
                "tool returned an unsupported response".to_string(),
            )),
        }
    }

    async fn session(&mut self, server: &McpServerConfig) -> Result<&ClientService, CoreError> {
        if !self.by_server.contains_key(&server.id) {
            let service = connect(server).await?;
            self.by_server.insert(server.id.clone(), service);
        }
        Ok(self
            .by_server
            .get(&server.id)
            .expect("session inserted above"))
    }

    /// 開いたセッションをすべて閉じる。ターンの終わりに必ず呼ぶ。
    pub async fn close(self) {
        for (_, service) in self.by_server {
            let _ = service.cancel().await;
        }
    }
}

/// 呼び出しパラメータの組み立て。`CallToolRequestParams`は`#[non_exhaustive]`のため
/// コンストラクタ経由で作り、こちらで使う項目だけを設定する。
fn call_params(tool_name: &str, arguments: Option<Map<String, Value>>) -> CallToolRequestParams {
    let mut params = CallToolRequestParams::new(tool_name.to_string());
    params.arguments = arguments;
    params
}

/// サーバーへ接続する。接続自体にもタイムアウトを設ける(応答しないサーバーで
/// ターンが止まらないようにする)。
async fn connect(server: &McpServerConfig) -> Result<ClientService, CoreError> {
    let connecting = match &server.endpoint {
        McpEndpoint::Stdio {
            command,
            args,
            env_refs,
        } => tokio::time::timeout(CONNECT_TIMEOUT, stdio::connect(command, args, env_refs)).await,
        McpEndpoint::StreamableHttp { url, header_refs } => {
            tokio::time::timeout(CONNECT_TIMEOUT, http::connect(url, header_refs)).await
        }
    };
    connecting.map_err(|_| CoreError::Mcp("timed out connecting to MCP server".to_string()))?
}

/// サーバーへ接続し、ツール一覧を取得して切断する(設定画面からの1回限りの取得)。
/// 成功・失敗どちらの場合も接続は残さない。
pub async fn list_tools(server: &McpServerConfig) -> Result<Vec<McpToolInfo>, CoreError> {
    let mut sessions = McpSessions::new();
    let result = sessions.list_tools(server).await;
    sessions.close().await;
    result
}

fn to_tool_info(tool: rmcp::model::Tool) -> McpToolInfo {
    McpToolInfo {
        name: sanitize_tool_text(&tool.name),
        description: tool.description.as_deref().map(sanitize_tool_text),
        input_schema: Value::Object(Map::clone(&tool.input_schema)),
    }
}

/// ツール呼び出しの結果を、モデルへ返す・実行記録として保存するためのJSONに変換する。
/// 構造化された結果があればそれを、無ければテキストブロックを連結して返す。
/// 画像等の非テキストブロックはこの経路では扱わない(添付として扱う仕組みはIssue #21)。
fn to_result_value(result: CallToolResult) -> Value {
    let mut value = Map::new();
    if let Some(structured) = result.structured_content {
        // 構造化された結果も同じ上限で抑える。丸めた形を作らず、丸ごと落として
        // その旨だけを残す(欠けた構造をモデルに読ませるより、無いと伝える方が安全)。
        if structured.to_string().chars().count() <= MAX_RESULT_CHARS {
            value.insert("structured_content".to_string(), structured);
        } else {
            value.insert(
                "omitted_structured_content".to_string(),
                json!("結果が長いため省略しました"),
            );
        }
    }

    let mut text = String::new();
    let mut omitted = 0usize;
    for block in &result.content {
        match block {
            ContentBlock::Text(t) => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&t.text);
            }
            _ => omitted += 1,
        }
    }
    if !text.is_empty() {
        value.insert("text".to_string(), json!(truncate_result_text(&text)));
    }
    if omitted > 0 {
        value.insert(
            "omitted_non_text_blocks".to_string(),
            json!(omitted),
        );
    }
    if result.is_error.unwrap_or(false) {
        // 実行記録の表示(`ExternalToolLine`/「思考・ツール」折りたたみ)は`result.error`の
        // 有無でエラーを判定する。サーバーが返したエラーもその形に合わせる。
        value.insert(
            "error".to_string(),
            json!(value
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("tool reported an error")),
        );
    }
    Value::Object(value)
}

/// ツール結果のテキストは、モデルの入力にも実行記録にも載る。上限を設けて
/// 際限なく膨らまないようにする(履歴トリミングの方式はIssue #7の範囲)。
const MAX_RESULT_CHARS: usize = 20_000;

fn truncate_result_text(text: &str) -> String {
    if text.chars().count() <= MAX_RESULT_CHARS {
        return text.to_string();
    }
    let mut out: String = text.chars().take(MAX_RESULT_CHARS).collect();
    out.push_str("
…(結果が長いため以降を省略しました)");
    out
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
    fn tool_result_carries_text_and_structure_and_marks_errors() {
        let mut result = CallToolResult::success(vec![ContentBlock::text("done")]);
        result.structured_content = Some(json!({ "count": 2 }));
        let value = to_result_value(result.clone());
        assert_eq!(value["text"], json!("done"));
        assert_eq!(value["structured_content"], json!({ "count": 2 }));
        assert!(value.get("error").is_none());

        result.is_error = Some(true);
        let value = to_result_value(result);
        // 表示側(`ExternalToolLine`)はresult.errorの有無でエラーを判定する。
        assert_eq!(value["error"], json!("done"));
    }

    #[test]
    fn tool_result_reports_blocks_it_cannot_carry() {
        let result = CallToolResult::success(vec![ContentBlock::image(
            "base64data".to_string(),
            "image/png".to_string(),
        )]);
        let value = to_result_value(result);
        assert_eq!(value["omitted_non_text_blocks"], json!(1));
        assert!(value.get("text").is_none());
    }

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
