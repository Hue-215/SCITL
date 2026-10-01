//! 外部ツールサーバー(MCP)クライアント。サーバーへの接続・ツール一覧の取得と、応答生成
//! 1ターンの中でのツール呼び出しを担う。
//!
//! 接続は1ターンの間だけ張り、ターンが終われば切断する([`McpSessions`])。設定画面からの
//! ツール一覧取得は1回ごとに開いて閉じる。取得したツール一覧はアプリ起動中だけ
//! [`ToolCatalog`]に持ち、config.tomlには書かない(サーバー側の更新に追従できないため)。
//!
//! `rmcp`のOAuth/認可系(`auth`)featureは有効にしない。`.well-known`ディスカバリ等で、
//! ユーザーが登録していない先へ通信しうるため。

mod http;
mod stdio;

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock};
use rmcp::service::RunningService;
use rmcp::RoleClient;
use secrecy::SecretString;
use serde_json::{json, Map, Value};

use crate::config::{McpEndpoint, McpServerConfig, SecretRef};
use crate::error::CoreError;
use crate::net::ExternalUrl;
use crate::secrets;
use crate::text;

/// 接続・ツール一覧取得・ツール呼び出しそれぞれに設ける固定タイムアウト。応答しない
/// サーバーで設定画面やターンが固まらないようにする。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const LIST_TOOLS_TIMEOUT: Duration = Duration::from_secs(30);
/// 1回のツール呼び出しの上限。ターン全体で使える時間の合計は設定から決まるが
/// (`orchestration::ToolLimits`)、ターン側は呼び出しの区切りで判定するだけで、実行中の
/// 呼び出しは打ち切らない。1回の呼び出しが長居しないよう、ここで上限を掛ける。
const CALL_TOOL_TIMEOUT: Duration = Duration::from_secs(60);
/// 切断の上限。ここに上限が無いと、graceful shutdownに応じないサーバーが1台あるだけで、
/// 応答を保存し終えたあとのターンが切断待ちのまま返らなくなる。
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// クライアント側のMCPセッション。サーバーからの要求(サンプリング等)で通信や処理が増えない
/// よう、意図的にハンドラを持たない(`()`)。
type ClientService = RunningService<RoleClient, ()>;

/// サーバーから受け取ったツール1件。どの値も受け取ったまま持つ。画面へ出す形は
/// `settings::view`が作り、これ自体はWebViewへ渡さない。
#[derive(Debug, Clone)]
pub struct McpToolInfo {
    /// 有効化の照合とサーバー呼び出しに使う識別子。書き換えると、サーバー上の別の
    /// ツールを呼びうる。
    pub name: String,
    pub description: Option<String>,
    /// サーバーが宣言した引数スキーマ(JSON Schema)。モデルへツールを公開するときに渡す。
    /// 中身は検証しない(信頼境界はユーザーが登録したこと自体に置く)。設定画面へは渡さない。
    pub input_schema: Value,
}

/// 取得済みツール一覧のメモリキャッシュ。設定画面の表示と、ターン開始時のツール公開の
/// 両方がここを読む。
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

/// 応答生成1ターンの間だけ生きるセッション置き場。サーバーごとに最初に必要になった時点で
/// 接続し、ターンの終わりに[`Self::close`]でまとめて切断する。呼び出し側は成功・
/// 失敗どちらの経路でも必ず`close`を通ること。
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
            .map_err(|e| {
                CoreError::Mcp(format!(
                    "failed to list tools: {}",
                    describe_server_error(&e)
                ))
            })?;
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
        .map_err(|e| {
            CoreError::Mcp(format!(
                "failed to call tool: {}",
                describe_server_error(&e)
            ))
        })?;

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
    /// 切断にも上限を設ける(応じないサーバーがあっても待ち続けない)。閉じきれなかった
    /// 接続は`RunningService`のDropが後始末する(stdioは子プロセスのkillまで含む)。
    pub async fn close(self) {
        for (_, mut service) in self.by_server {
            let _ = service.close_with_timeout(CLOSE_TIMEOUT).await;
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
        name: tool.name.to_string(),
        description: tool.description.map(|d| d.to_string()),
        input_schema: Value::Object(Map::clone(&tool.input_schema)),
    }
}

/// ツール呼び出しの結果を、モデルへ返す・実行記録として保存するためのJSONに変換する。
/// 構造化された結果があればそれを、無ければテキストブロックを連結して返す。
/// 画像等の非テキストブロックは扱わず、件数だけを残す。
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
                json!("omitted because the result is too long"),
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
        value.insert("omitted_non_text_blocks".to_string(), json!(omitted));
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

/// ツール結果のテキストの上限。モデルの入力にも実行記録にも載るため、際限なく膨らまないように。
const MAX_RESULT_CHARS: usize = 20_000;

fn truncate_result_text(result: &str) -> String {
    match text::truncate_chars(result, MAX_RESULT_CHARS) {
        (head, true) => {
            format!("{head}\n…(the rest of the result was omitted because it is too long)")
        }
        (head, false) => head,
    }
}

/// streamable_http方式のURLを検証する(サーバー登録時に呼ぶ)。検証本体は[`ExternalUrl::parse`]。
pub fn validate_streamable_http_url(url: &str) -> Result<(), CoreError> {
    ExternalUrl::parse(url).map(drop).map_err(CoreError::Mcp)
}

/// リクエストの構造やMCPプロトコル自体が管理するヘッダー名。ユーザーが登録した
/// カスタムヘッダーで上書きされてはならない(`authorization`はMCPサーバーの認証に
/// 使う主用途のため許可する)。
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

/// `refs`が指す秘密情報をまとめて解決する。`secrets::load`は資格情報ストアを呼ぶ同期I/Oの
/// ため、[`crate::blocking::run`]を通す。
async fn resolve_secrets(refs: &[SecretRef]) -> Result<Vec<(String, SecretString)>, CoreError> {
    let refs = refs.to_vec();
    crate::blocking::run(move || {
        refs.into_iter()
            .map(|r| secrets::load(&r.key_ref).map(|secret| (r.name, secret)))
            .collect()
    })
    .await
}

/// サーバーとのやり取りの失敗を、エラー文言に載せる形にする。rmcpのエラー表示には
/// サーバーが書いた`message`と任意のJSON(`data`)がそのまま入り、設定画面・ツール実行記録・
/// モデルへ返す結果のすべてに載るため、画面に出す診断文字列として整える。
const MAX_SERVER_ERROR_CHARS: usize = 512;

fn describe_server_error(e: &impl std::fmt::Display) -> String {
    text::display_label(&e.to_string(), MAX_SERVER_ERROR_CHARS)
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
