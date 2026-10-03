//! 外部ツールサーバー(MCP)クライアント。サーバーへの接続・ツール一覧の取得と、応答生成
//! 1ターンの中でのツール呼び出しを担う。
//!
//! 接続は1ターンの間だけ張り、ターンが終われば切断する([`McpSessions`])。ターンの始めの
//! ツール一覧取得は、サーバーごとに並行して接続する。設定画面からのツール一覧取得は1回ごとに
//! 開いて閉じる。取得したツール一覧はアプリ起動中だけ[`ToolCatalog`]に持ち、config.tomlには
//! 書かない(サーバー側の更新に追従できないため)。
//!
//! 接続方式はstreamable_httpだけ。サーバーを子プロセスとして起動する方式(stdio)は持たない
//! (`docs/spec/tools.md`「外部(MCP)ツールの公開」)。
//!
//! `rmcp`のOAuth/認可系(`auth`)featureは有効にしない。`.well-known`ディスカバリ等で、
//! ユーザーが登録していない先へ通信しうるため。

mod http;

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ClientCapabilities, ClientConfig,
    ContentBlock, Implementation,
};
use rmcp::service::{ClientInitializeError, RunningService, ServiceError};
use rmcp::transport::DynamicTransportError;
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
/// ターンの始めのツール一覧取得に、続けてこの回数だけ失敗したサーバーは、アプリ起動中は
/// ターンで試さない([`ToolCatalog::gave_up`])。応答しないサーバー1台のために、毎ターン
/// 接続と一覧取得のタイムアウトを待たないため。設定画面でサーバーを有効にし直すと数え直す。
/// 今は固定値で、数値の設定から変えられるようにする余地を残してここに置く。
pub const MAX_CONSECUTIVE_FAILURES: u32 = 3;
/// 切断の上限。ここに上限が無いと、graceful shutdownに応じないサーバーが1台あるだけで、
/// 応答を保存し終えたあとのターンが切断待ちのまま返らなくなる。
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// クライアント側のMCPセッション。ハンドラには名乗り(`client_config`)だけを渡す。サーバーからの
/// 要求(サンプリング等)は処理せずrmcpの既定の応答を返し、通信や処理を増やさない。
type ClientService = RunningService<RoleClient, ClientConfig>;

/// 接続のときに外部ツールサーバーへ名乗る情報(`initialize`の`clientInfo`)。能力は何も宣言しない。
fn client_config() -> ClientConfig {
    ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("scitl", env!("CARGO_PKG_VERSION")).with_title(crate::PRODUCT_NAME),
    )
}

/// サーバーから受け取ったツール1件。どの値も受け取ったまま持つ。画面へ出す形は
/// `settings::view`が作り、これ自体はWebViewへ渡さない。
#[derive(Debug, Clone)]
pub struct McpToolInfo {
    /// 有効化の照合とサーバー呼び出しに使う識別子。書き換えると、サーバー上の別の
    /// ツールを呼びうる。
    pub name: String,
    pub description: Option<String>,
    /// サーバーが宣言した引数スキーマ(JSON Schema)。モデルへツールを公開するときに渡す。
    /// 中身は検証しない(信頼境界はユーザーが登録したこと自体に置く)。公開できる形かは
    /// `tools::external`が見る。設定画面へは渡さない。
    pub input_schema: Value,
}

/// 取得済みツール一覧のメモリキャッシュ。設定画面の表示と、ターン開始時のツール公開の
/// 両方がここを読む。ターンでの一覧取得に続けて失敗した回数も、アプリ起動中だけここに持つ。
#[derive(Debug, Default)]
pub struct ToolCatalog {
    by_server: Mutex<HashMap<String, Vec<McpToolInfo>>>,
    failures: Mutex<HashMap<String, u32>>,
}

impl ToolCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, server_id: &str) -> Option<Vec<McpToolInfo>> {
        self.lock().get(server_id).cloned()
    }

    /// 取得できた一覧を載せる。続けて失敗した回数は数え直す。
    pub fn store(&self, server_id: &str, tools: Vec<McpToolInfo>) {
        self.lock().insert(server_id.to_string(), tools);
        self.failures().remove(server_id);
    }

    /// サーバーの削除・接続先の変更でキャッシュを捨てる。
    pub fn forget(&self, server_id: &str) {
        self.lock().remove(server_id);
        self.failures().remove(server_id);
    }

    /// ターンでの一覧取得に失敗したことを数える。試すのをやめる回数に達したら`true`。
    pub fn record_failure(&self, server_id: &str) -> bool {
        let mut failures = self.failures();
        let count = failures.entry(server_id.to_string()).or_default();
        *count = count.saturating_add(1);
        *count >= MAX_CONSECUTIVE_FAILURES
    }

    /// 続けて失敗したので、ターンでは試さないサーバーか([`MAX_CONSECUTIVE_FAILURES`])。
    pub fn gave_up(&self, server_id: &str) -> bool {
        self.failures()
            .get(server_id)
            .is_some_and(|count| *count >= MAX_CONSECUTIVE_FAILURES)
    }

    /// 失敗の回数を数え直し、次のターンでまた試す(設定画面でサーバーを有効にし直したとき)。
    pub fn retry(&self, server_id: &str) {
        self.failures().remove(server_id);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<McpToolInfo>>> {
        self.by_server.lock().expect("tool catalog mutex poisoned")
    }

    fn failures(&self) -> std::sync::MutexGuard<'_, HashMap<String, u32>> {
        self.failures.lock().expect("tool catalog mutex poisoned")
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
        list_tools_of(service).await
    }

    /// 複数のサーバーのツール一覧を、サーバーごとに並行して接続して取得する。待つのは
    /// 一番遅い1台の分になる。結果は渡した順に返す。繋がった接続は、一覧の取得に失敗しても
    /// このターンの間は残す(呼び出しに使い回し、[`Self::close`]で閉じる)。
    ///
    /// まだ接続していないサーバーだけを渡すこと(ターンの始めに呼ぶ)。返る前にこのfutureを
    /// 捨てると(停止の指示)、接続の途中のものも打ち切って捨てる。
    pub async fn list_tools_all(
        &mut self,
        servers: &[&McpServerConfig],
    ) -> Vec<Result<Vec<McpToolInfo>, CoreError>> {
        let mut tasks = tokio::task::JoinSet::new();
        for (index, server) in servers.iter().enumerate() {
            let server = (*server).clone();
            tasks.spawn(async move {
                let (service, listed) = match connect(&server).await {
                    Ok(service) => {
                        let listed = list_tools_of(&service).await;
                        (Some(service), listed)
                    }
                    Err(e) => (None, Err(e)),
                };
                (index, server.id, service, listed)
            });
        }
        let mut results: Vec<Option<Result<Vec<McpToolInfo>, CoreError>>> =
            servers.iter().map(|_| None).collect();
        while let Some(joined) = tasks.join_next().await {
            // 中断(パニック)したタスクは、どのサーバーの分か分からないので下で失敗にする。
            let Ok((index, server_id, service, listed)) = joined else {
                continue;
            };
            if let Some(service) = service {
                self.by_server.insert(server_id, service);
            }
            results[index] = Some(listed);
        }
        results
            .into_iter()
            .map(|listed| {
                listed.unwrap_or_else(|| {
                    Err(CoreError::Mcp(
                        "listing tools stopped unexpectedly".to_string(),
                    ))
                })
            })
            .collect()
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
    /// 接続は`RunningService`のDropが後始末する。
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

/// 接続済みのサーバーからツール一覧を取得する。
async fn list_tools_of(service: &ClientService) -> Result<Vec<McpToolInfo>, CoreError> {
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

/// サーバーへ接続する。接続自体にもタイムアウトを設ける(応答しないサーバーで
/// ターンが止まらないようにする)。
async fn connect(server: &McpServerConfig) -> Result<ClientService, CoreError> {
    let McpEndpoint::StreamableHttp { url, header_refs } = &server.endpoint;
    tokio::time::timeout(CONNECT_TIMEOUT, http::connect(url, header_refs))
        .await
        .map_err(|_| CoreError::Mcp("timed out connecting to MCP server".to_string()))?
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

/// ヘッダー値を検証する。載せられる値の規則は[`crate::net::secret_header_value`]が決める。
pub fn validate_header_value(value: &str) -> Result<(), CoreError> {
    crate::net::secret_header_value(value)
        .map(drop)
        .ok_or_else(|| CoreError::Mcp(HEADER_VALUE_REFUSED.to_string()))
}

/// 値は秘密情報なので、エラー文に含めない。
pub(super) const HEADER_VALUE_REFUSED: &str =
    "the header value must contain only ASCII characters without line breaks";

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

fn describe_server_error(e: &impl ServerError) -> String {
    let message = match e.transport_error() {
        Some(transport) => transport.error.to_string(),
        None => e.to_string(),
    };
    text::display_label(&message, MAX_SERVER_ERROR_CHARS)
}

/// rmcpのエラーのうち、通信路の失敗を包むもの。包みの表示には通信路の型名(約150文字)と
/// rmcpの内部の段階名が入り、原因の部分を上限の外へ押し出すので、中身の`error`だけを使う。
/// 段階は呼び出し側が「failed to connect」等で付ける。包みは`source()`でたどれない
/// (rmcpが`#[source]`を付けていない)ので、列挙子を直接見る。
trait ServerError: std::fmt::Display {
    fn transport_error(&self) -> Option<&DynamicTransportError>;
}

impl ServerError for ServiceError {
    fn transport_error(&self) -> Option<&DynamicTransportError> {
        match self {
            Self::TransportSend(error) => Some(error),
            _ => None,
        }
    }
}

impl ServerError for ClientInitializeError {
    fn transport_error(&self) -> Option<&DynamicTransportError> {
        match self {
            Self::TransportError { error, .. } => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::ProtocolVersion;

    /// 名乗りはSCITLの名前と版だけで、能力は宣言せず、プロトコルの版はrmcpの既定のまま。
    /// rmcpを上げて既定が変わったときに気付けるよう、送る中身を固定する。
    #[test]
    fn client_config_names_scitl_and_declares_no_capabilities() {
        let config = client_config();
        assert_eq!(config.client_info.name, "scitl");
        assert_eq!(config.client_info.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(
            config.client_info.title.as_deref(),
            Some(crate::PRODUCT_NAME)
        );
        assert_eq!(config.capabilities, ClientCapabilities::default());
        assert_eq!(config.protocol_version, ProtocolVersion::default());
        assert!(config.meta.is_none());
    }

    #[test]
    fn server_errors_leave_out_the_transport_type_name() {
        let transport = || {
            DynamicTransportError::from_parts(
                "rmcp::transport::worker::WorkerTransport<…>",
                std::any::TypeId::of::<()>(),
                "unexpected server response: HTTP 401 Unauthorized".into(),
            )
        };
        let expected = "unexpected server response: HTTP 401 Unauthorized";

        assert_eq!(
            describe_server_error(&ServiceError::TransportSend(transport())),
            expected
        );
        assert_eq!(
            describe_server_error(&ClientInitializeError::TransportError {
                error: transport(),
                context: "send initialize request".into(),
            }),
            expected
        );
        // 通信路を包まない失敗は、表示をそのまま使う。
        assert_eq!(
            describe_server_error(&ServiceError::TransportClosed),
            "Transport closed"
        );
    }

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
    fn validate_header_value_rejects_crlf_and_non_ascii() {
        assert!(validate_header_value("normal-value").is_ok());
        assert!(validate_header_value("bad\r\nX-Injected: 1").is_err());
        assert!(validate_header_value("Bearer\u{3000}token").is_err());
    }

    #[test]
    fn the_catalog_gives_up_on_a_server_after_consecutive_failures_until_it_is_retried() {
        let catalog = ToolCatalog::new();
        for _ in 1..MAX_CONSECUTIVE_FAILURES {
            assert!(!catalog.record_failure("s"));
        }
        assert!(!catalog.gave_up("s"));
        assert!(catalog.record_failure("s"));
        assert!(catalog.gave_up("s"));
        assert!(!catalog.gave_up("other"));

        catalog.retry("s");
        assert!(!catalog.gave_up("s"));

        // 間に成功を挟めば、続けての失敗ではない。
        for _ in 1..MAX_CONSECUTIVE_FAILURES {
            catalog.record_failure("s");
        }
        catalog.store("s", Vec::new());
        assert!(!catalog.record_failure("s"));
    }

    /// `initialize`と`tools/list`に、`delay`だけ待ってから答える最小のサーバー。
    async fn slow_server(delay: Duration) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buf = [0u8; 4096];
                    // ヘッダーと、Content-Length分の本文を読む。
                    let body = loop {
                        let n = socket.read(&mut buf).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        request.extend_from_slice(&buf[..n]);
                        let text = String::from_utf8_lossy(&request).to_string();
                        let Some(end) = text.find("\r\n\r\n") else {
                            continue;
                        };
                        let length = text[..end]
                            .lines()
                            .find_map(|l| {
                                let (name, value) = l.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())?
                            })
                            .unwrap_or(0);
                        if text.len() >= end + 4 + length {
                            break text[end + 4..end + 4 + length].to_string();
                        }
                    };
                    let message: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                    let reply = match message["method"].as_str() {
                        Some("initialize") => json!({
                            "jsonrpc": "2.0", "id": message["id"],
                            "result": {
                                "protocolVersion": message["params"]["protocolVersion"],
                                "capabilities": { "tools": {} },
                                "serverInfo": { "name": "slow", "version": "1" },
                            },
                        }),
                        Some("tools/list") => json!({
                            "jsonrpc": "2.0", "id": message["id"],
                            "result": { "tools": [
                                { "name": "echo", "inputSchema": { "type": "object" } },
                            ] },
                        }),
                        _ => Value::Null,
                    };
                    let response = if reply.is_null() {
                        "HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                            .to_string()
                    } else {
                        tokio::time::sleep(delay).await;
                        let reply = reply.to_string();
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                            reply.len()
                        )
                    };
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        format!("http://{addr}/mcp")
    }

    fn server_at(id: &str, url: String) -> McpServerConfig {
        McpServerConfig {
            id: id.to_string(),
            name: id.to_string(),
            enabled: true,
            endpoint: McpEndpoint::StreamableHttp {
                url,
                header_refs: Vec::new(),
            },
            enabled_tools: Default::default(),
        }
    }

    /// サーバーごとに並行して接続するので、待つのは一番遅い1台の分で済む。結果は渡した順。
    #[tokio::test]
    async fn listing_tools_of_several_servers_waits_only_for_the_slowest() {
        let delay = Duration::from_millis(500);
        let mut servers = Vec::new();
        for id in ["a", "b", "c"] {
            servers.push(server_at(id, slow_server(delay).await));
        }
        // bindしたままlistenしないソケット。接続はすぐ拒否される。
        let refusing = tokio::net::TcpSocket::new_v4().unwrap();
        refusing.bind(([127, 0, 0, 1], 0).into()).unwrap();
        servers.insert(
            1,
            server_at(
                "down",
                format!("http://{}/mcp", refusing.local_addr().unwrap()),
            ),
        );

        let mut sessions = McpSessions::new();
        let started = std::time::Instant::now();
        let listed = sessions
            .list_tools_all(&servers.iter().collect::<Vec<_>>())
            .await;
        let elapsed = started.elapsed();
        sessions.close().await;

        // 1台あたり、接続(initialize)と一覧で2回待つ。逐次なら3台で6回分になる。
        assert!(elapsed < delay * 4, "took {elapsed:?}");
        let names: Vec<_> = listed
            .iter()
            .map(|r| r.as_ref().map(|tools| tools[0].name.clone()).ok())
            .collect();
        assert_eq!(
            names,
            [
                Some("echo".to_string()),
                None,
                Some("echo".to_string()),
                Some("echo".to_string())
            ]
        );
    }
}
