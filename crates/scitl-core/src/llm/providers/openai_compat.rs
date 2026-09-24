use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use crate::db::error::CoreError;
use crate::llm::{
    ChatMessage, FinishReason, LlmAdapter, PromptText, Readiness, ResponseEvent, ToolArguments,
    ToolCallRequest, ToolSchema,
};

/// HTTPエラー時にエラー文へ載せるプロバイダ応答本文の上限。
const MAX_ERROR_BODY_CHARS: usize = 512;
/// 応答本文に送信した鍵が現れたときの置き換え先。
const REDACTED: &str = "[redacted]";

/// LLMプロバイダ第一弾: OpenAI互換チャットコンプリーションAPI
/// (docs/spec/rebuild/architecture.md 2節)。方言吸収はこのファイル内に閉じ込め、
/// `orchestration::turn`は本アダプタの存在を知らない。
pub struct OpenAiCompatAdapter {
    client: reqwest::Client,
    base_url: String,
    api_key: SecretString,
    model: String,
}

impl OpenAiCompatAdapter {
    /// `api_key`は呼び出し元(`secrets.rs`経由)から`SecretString`のまま受け取る。
    /// このアダプタ自身はkeyringに触れない(architecture.md 6節)。`SecretString`を
    /// 引数の型にすることで、呼び出し元が平文`String`を経由する経路を作れないようにする。
    ///
    /// `request_timeout`は設定画面(Issue #22)の「応答タイムアウト」を解決した値
    /// (`config::GeneralConfig::response_timeout`)。
    pub fn new(
        base_url: impl Into<String>,
        api_key: SecretString,
        model: impl Into<String>,
        request_timeout: Duration,
    ) -> Result<Self, CoreError> {
        let base_url = base_url.into();
        validate_base_url(&base_url)?;

        // ハードニング済みクライアントの組み立ては`net::hardened_client`に集約する
        // (MCP streamable_httpと共有)。
        let client = crate::net::hardened_client(&base_url, Some(request_timeout))?;
        Ok(Self {
            client,
            base_url,
            api_key,
            model: model.into(),
        })
    }
}

/// `http://`宛に`bearer_auth`で鍵を平文で送る範囲を絞るための検証。httpを許す範囲は
/// `net::classify_host`が決める(ループバック、またはプライベートIPリテラルのLAN上の
/// 推論サーバー。architecture.md 5節)。LAN宛の場合はAPIキーが平文で流れることを
/// 設定画面のヒントで明示している(principles.md 4節)。検証本体は
/// [`crate::net::validate_external_url`]に集約する(MCP streamable_httpのURL検証と
/// 共有)。
pub fn validate_base_url(base_url: &str) -> Result<(), CoreError> {
    let url = reqwest::Url::parse(base_url)
        .map_err(|e| CoreError::ProviderConfig(format!("base_url is not a valid URL: {e}")))?;
    crate::net::validate_external_url(&url).map_err(CoreError::ProviderConfig)
}

/// `base_url`と`chat/completions`を安全に連結する。文字列の`format!`連結は末尾スラッシュの
/// 有無で壊れやすく(`//chat/completions`等)、`validate_base_url`が防ぐ意図(パスがクエリの
/// 置き場所にならないこと)とも噛み合わないため`Url::join`を使う。
fn completions_endpoint(base_url: &str) -> Result<reqwest::Url, CoreError> {
    let mut url = reqwest::Url::parse(base_url)
        .map_err(|e| CoreError::ProviderConfig(format!("base_url is not a valid URL: {e}")))?;
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    url.join("chat/completions")
        .map_err(|e| CoreError::ProviderConfig(format!("failed to build endpoint: {e}")))
}

/// reqwestのエラーDisplayは要求URLを含む。base_urlにクエリ形式で鍵を置く構成の
/// プロバイダでは鍵がエラー文に混入するため、URLを剥がしてから文字列化する。
fn provider_error(e: reqwest::Error) -> CoreError {
    CoreError::Llm(e.without_url().to_string())
}

/// プロバイダ制御下の応答本文は、エラー発言の詳細としてDBに残り画面にも出る
/// (Issue #159。`data-model.md` messages「error_detail」)。画面に出す診断文字列として
/// 1行に整えて長さを制限し(architecture.md 10節)、送信した鍵そのものが含まれていれば
/// 伏せ字にしてから載せる(principles.md 4節)。ゲートウェイがリクエストヘッダをエコーバックする
/// 構成だと`Authorization`ヘッダの値がそのまま本文に現れうるため、サイズ制限だけでは
/// 防げない。鍵をURLに置く構成は`validate_base_url`がクエリ・userinfoを拒否して塞いで
/// いるため、伏せ字の対象は鍵1つで足りる。HTTPリクエストから切り離してテストできるよう
/// 関数として独立させる。
fn http_error(status: reqwest::StatusCode, body: &str, api_key: &str) -> CoreError {
    CoreError::LlmHttp {
        status: status.as_u16(),
        body: sanitize_error_body(body, api_key),
    }
}

fn sanitize_error_body(body: &str, api_key: &str) -> String {
    if api_key.is_empty() {
        return crate::text::display_label(body, MAX_ERROR_BODY_CHARS);
    }
    // 伏せ字は整える前と後の両方で掛ける。後で掛けるのは、鍵の途中に見えない文字を挟んだ形が
    // 除いた時点で鍵として現れるため。その照合は整えた鍵で行う(鍵の前後に空白が付いたまま
    // 保存されていても、ヘッダー値としては空白を落とした形で送られ、そのまま返ってくる)。
    // 切り詰めは伏せ字の後に行い、境界で鍵の一部が残らないようにする。
    let visible = crate::text::visible_line(&body.replace(api_key, REDACTED));
    let visible_key = crate::text::visible_line(api_key);
    let redacted = if visible_key.is_empty() {
        visible
    } else {
        visible.replace(&visible_key, REDACTED)
    };
    crate::text::ellipsize(&redacted, MAX_ERROR_BODY_CHARS)
}

#[derive(Serialize)]
struct RequestBody<'a> {
    model: &'a str,
    messages: Vec<RequestMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<RequestTool>,
    stream: bool,
}

/// `ChatMessage`(core側の型)をOpenAI互換の発言列に変換する。役割ごとに必要な
/// フィールドだけを持たせるのは`ChatMessage`と同じ理由(architecture.md 3節)。
#[derive(Serialize)]
#[serde(tag = "role", rename_all = "snake_case")]
enum RequestMessage {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        content: Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<RequestToolCall>,
    },
    Tool {
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_call_id: Option<String>,
        content: String,
    },
}

#[derive(Serialize)]
struct RequestToolCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(rename = "type")]
    kind: &'static str,
    function: RequestToolCallFunction,
}

#[derive(Serialize)]
struct RequestToolCallFunction {
    name: String,
    arguments: String,
}

/// 会話がアシスタント発言から始まるときに、その前へ補うユーザー発言の本文。
const PLACEHOLDER_USER_TEXT: &str = "(The earlier part of this conversation is not available.)";

/// 発言列を、userから始まりuserとassistantが交互に並ぶ形に整えて変換する
/// (architecture.md 3節)。発言の削除やエラー発言の除外で、履歴はアシスタント発言から
/// 始まったり同じ役割が続いたりする。チャットテンプレートが交互の並びを要求するサーバー
/// (llama.cppのGemma・Mistral系等)は、そのままではリクエストを拒否する。
///
/// - 同じ役割が続いたら1つにまとめる。ユーザー発言は組み立てた囲みごと連結するので、
///   発言ごとの送信日時は残る
/// - 最初の発言がアシスタント発言なら、その前にユーザー発言を補う。補う発言に日時は
///   付けない(`PromptText::user_message`の`sent_at`)
///
/// ツールの往復(`tool_calls`を持つassistantに続くtool)には手を加えない。
fn to_request_messages(messages: &[ChatMessage]) -> Vec<RequestMessage> {
    let mut out: Vec<RequestMessage> = Vec::with_capacity(messages.len() + 1);
    for message in messages {
        match (out.last_mut(), to_request_message(message)) {
            (Some(RequestMessage::User { content: prev }), RequestMessage::User { content }) => {
                append_paragraph(prev, &content);
            }
            (
                Some(RequestMessage::Assistant {
                    content: prev,
                    tool_calls: prev_calls,
                }),
                RequestMessage::Assistant {
                    content,
                    tool_calls,
                },
            ) => {
                if let Some(content) = content {
                    match prev {
                        Some(prev) => append_paragraph(prev, &content),
                        None => *prev = Some(content),
                    }
                }
                prev_calls.extend(tool_calls);
            }
            (last, message) => {
                if matches!(message, RequestMessage::Assistant { .. })
                    && matches!(last, None | Some(RequestMessage::System { .. }))
                {
                    out.push(to_request_message(&ChatMessage::User(
                        PromptText::user_message(PLACEHOLDER_USER_TEXT, None),
                    )));
                }
                out.push(message);
            }
        }
    }
    out
}

fn append_paragraph(prev: &mut String, next: &str) {
    prev.push_str("\n\n");
    prev.push_str(next);
}

fn to_request_message(message: &ChatMessage) -> RequestMessage {
    match message {
        ChatMessage::System(content) => RequestMessage::System {
            content: content.clone(),
        },
        ChatMessage::User(content) => RequestMessage::User {
            content: content.as_str().to_string(),
        },
        ChatMessage::Assistant {
            content,
            tool_calls,
        } => RequestMessage::Assistant {
            content: content.clone(),
            tool_calls: tool_calls.iter().map(to_request_tool_call).collect(),
        },
        ChatMessage::Tool {
            tool_call_id,
            content,
        } => RequestMessage::Tool {
            tool_call_id: tool_call_id.clone(),
            content: content.as_str().to_string(),
        },
    }
}

fn to_request_tool_call(call: &ToolCallRequest) -> RequestToolCall {
    RequestToolCall {
        id: call.id.clone(),
        kind: "function",
        function: RequestToolCallFunction {
            name: call.name.clone(),
            arguments: call.arguments.to_wire_string(),
        },
    }
}

#[derive(Serialize)]
struct RequestTool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: RequestFunction,
}

#[derive(Serialize)]
struct RequestFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Deserialize)]
struct CompletionResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ResponseToolCall>,
    /// 思考(reasoning)専用の本文(Issue #42)。標準のOpenAI Chat Completions APIには
    /// 無いフィールドだが、`reasoning_content`はOpenAI互換を名乗るプロバイダ・ゲートウェイ
    /// (DeepSeek、vLLMのreasoning parser経由の出力等)で広く使われている拡張のため対応する。
    /// フィールド自体が無いプロバイダでは`None`のまま(`#[serde(default)]`)。
    #[serde(default)]
    reasoning_content: Option<String>,
}

#[derive(Deserialize)]
struct ResponseToolCall {
    id: Option<String>,
    function: ResponseFunctionCall,
}

#[derive(Deserialize)]
struct ResponseFunctionCall {
    name: String,
    arguments: String,
}

#[async_trait::async_trait]
impl LlmAdapter for OpenAiCompatAdapter {
    fn readiness(&self) -> Readiness {
        // `providers::build_active_adapter`はモデル未選択でもエラーにせず空文字のまま
        // `OpenAiCompatAdapter`を作る(全プロバイダー削除同様、チャット送信時に初めて
        // 表面化させる設計)。APIキーの空はここでは判定しない(`Readiness`のドキュメント
        // 参照: ローカルプロバイダーの「認証不要で意図的に空」と区別できないため)。
        if self.model.is_empty() {
            Readiness::NoModel
        } else {
            Readiness::Ready
        }
    }

    async fn send(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSchema],
    ) -> Result<Vec<ResponseEvent>, CoreError> {
        let body = RequestBody {
            model: &self.model,
            messages: to_request_messages(messages),
            tools: tools
                .iter()
                .map(|t| RequestTool {
                    kind: "function",
                    function: RequestFunction {
                        name: t.name().to_string(),
                        description: t.description().to_string(),
                        parameters: t.parameters().clone(),
                    },
                })
                .collect(),
            // 非ストリーミングでも戻り値はイベント列に組み立て直す(`ResponseEvent`参照)。
            stream: false,
        };

        let endpoint = completions_endpoint(&self.base_url)?;
        let mut request = self.client.post(endpoint);
        // 認証不要のローカル推論サーバー向けに、鍵が空なら`Authorization`ヘッダーごと付けない
        // (`Bearer `だけを送ると、空の鍵を不正な鍵として弾くサーバーがある)。
        if !self.api_key.expose_secret().is_empty() {
            request = request.bearer_auth(self.api_key.expose_secret());
        }
        let response = request.json(&body).send().await.map_err(provider_error)?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(http_error(status, &text, self.api_key.expose_secret()));
        }

        let parsed: CompletionResponse = response.json().await.map_err(provider_error)?;

        let choice = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| CoreError::Llm("empty choices".to_string()))?;

        let mut events = Vec::new();
        // 思考は生成順として本文・ツール呼び出しより先に置く(`principles.md` 3節
        // 「応答はイベントの並びとして受け取る」)。非ストリーミングAPIのため実際の生成順は
        // 観測できないが、モデルが思考してから本文/ツール呼び出しを出す一般的な順序に合わせる。
        if let Some(reasoning) = choice.message.reasoning_content {
            if !reasoning.is_empty() {
                events.push(ResponseEvent::ReasoningDelta { text: reasoning });
            }
        }
        if let Some(text) = choice.message.content {
            if !text.is_empty() {
                events.push(ResponseEvent::TextDelta { text });
            }
        }
        for call in choice.message.tool_calls {
            events.push(ResponseEvent::ToolCall {
                id: call.id,
                name: call.function.name,
                arguments: ToolArguments::parse(call.function.arguments),
            });
        }

        let finish_reason = match choice.finish_reason.as_deref() {
            Some("tool_calls") => FinishReason::ToolCall,
            Some("length") => FinishReason::Length,
            Some("stop") | None => FinishReason::Stop,
            Some(_) => FinishReason::Stop,
        };
        events.push(ResponseEvent::Done { finish_reason });

        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::*;

    const TEST_TIMEOUT: Duration = Duration::from_secs(30);

    const MINIMAL_COMPLETION: &str =
        r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}]}"#;

    /// 1回だけ接続を受け、`body`をコンプリーション応答として返す。受け取ったリクエストの
    /// ヘッダー部を返す。
    fn spawn_capturing(body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut received = Vec::new();
            let mut buf = [0u8; 4096];
            while !received.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = stream.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                received.extend_from_slice(&buf[..n]);
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let text = String::from_utf8_lossy(&received).to_string();
            text.split("\r\n\r\n")
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase()
        });
        (format!("http://{addr}/v1"), handle)
    }

    async fn send_with_key(api_key: &str) -> String {
        let (base_url, handle) = spawn_capturing(MINIMAL_COMPLETION);
        let adapter =
            OpenAiCompatAdapter::new(base_url, SecretString::from(api_key), "model", TEST_TIMEOUT)
                .unwrap();
        adapter.send(&[], &[]).await.unwrap();
        handle.join().unwrap()
    }

    #[tokio::test]
    async fn empty_api_key_sends_no_authorization_header() {
        let headers = send_with_key("").await;
        assert!(!headers.contains("authorization:"));
    }

    #[tokio::test]
    async fn api_key_is_sent_as_bearer() {
        let headers = send_with_key("sk-test").await;
        assert!(headers.contains("authorization: bearer sk-test"));
    }

    #[tokio::test]
    async fn malformed_tool_arguments_are_passed_up_instead_of_failing_the_send() {
        let (base_url, handle) = spawn_capturing(
            r#"{"choices":[{"message":{"tool_calls":[{"id":"call_1","type":"function","function":{"name":"update_task","arguments":"{\"title\": "}}]},"finish_reason":"tool_calls"}]}"#,
        );
        let adapter =
            OpenAiCompatAdapter::new(base_url, SecretString::from(""), "model", TEST_TIMEOUT)
                .unwrap();
        let events = adapter.send(&[], &[]).await.unwrap();
        handle.join().unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            ResponseEvent::ToolCall { name, arguments: ToolArguments::Malformed { raw, .. }, .. }
                if name == "update_task" && raw == "{\"title\": "
        )));
    }

    #[test]
    fn accepts_https_base_url() {
        assert!(validate_base_url("https://api.openai.com/v1").is_ok());
    }

    #[test]
    fn accepts_http_loopback_base_url() {
        assert!(validate_base_url("http://127.0.0.1:8080/v1").is_ok());
        assert!(validate_base_url("http://localhost:8080/v1").is_ok());
        assert!(validate_base_url("http://[::1]:8080/v1").is_ok());
    }

    #[test]
    fn accepts_http_private_ip_literal_base_url() {
        // 境界値の網羅はnet.rs側で行い、ここではclassify_hostがLLMプロバイダー
        // 側にも効いていることだけを確認する。
        assert!(validate_base_url("http://192.168.1.107:11434/v1").is_ok());
    }

    #[test]
    fn rejects_http_hostname_base_url() {
        // ホスト名(localhost以外)は名前解決しないため常に拒否する。
        // プライベートIPかどうかではなく「ホスト名だから」拒否される点に注意
        // (`http://192.168.1.1/v1`はホスト名でなくIPリテラルなので許可される)。
        let err = validate_base_url("http://example.com/v1").unwrap_err();
        assert!(matches!(err, CoreError::ProviderConfig(_)));
    }

    #[test]
    fn rejects_unsupported_scheme() {
        let err = validate_base_url("ftp://example.com/v1").unwrap_err();
        assert!(matches!(err, CoreError::ProviderConfig(_)));
    }

    #[test]
    fn rejects_base_url_with_query_fragment_or_userinfo() {
        assert!(validate_base_url("https://api.example.com/v1?key=secret").is_err());
        assert!(validate_base_url("https://api.example.com/v1#frag").is_err());
        assert!(validate_base_url("https://user:pass@api.example.com/v1").is_err());
    }

    #[test]
    fn completions_endpoint_joins_regardless_of_trailing_slash() {
        assert_eq!(
            completions_endpoint("https://api.openai.com/v1")
                .unwrap()
                .as_str(),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            completions_endpoint("https://api.openai.com/v1/")
                .unwrap()
                .as_str(),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn http_error_carries_status_and_sanitized_body() {
        let err = http_error(
            reqwest::StatusCode::UNAUTHORIZED,
            "bad token sk-secret\n",
            "sk-secret",
        );
        let CoreError::LlmHttp { status, body } = err else {
            panic!("expected CoreError::LlmHttp");
        };
        assert_eq!(status, 401);
        assert_eq!(body, "bad token [redacted]");
    }

    #[test]
    fn sanitize_error_body_strips_control_chars_and_truncates() {
        let body = format!("line1\nline2\x07{}", "x".repeat(600));
        let sanitized = sanitize_error_body(&body, "unused-key");
        assert!(!sanitized.contains('\n'));
        assert!(!sanitized.contains('\x07'));
        assert!(sanitized.ends_with('…'));
        assert!(sanitized.chars().count() <= MAX_ERROR_BODY_CHARS + 1);
    }

    #[test]
    fn sanitize_error_body_removes_bidi_and_zero_width_chars() {
        let body = "a\u{202E}b\u{2066}c\u{200B}d\u{FEFF}e";
        assert_eq!(sanitize_error_body(body, ""), "abcde");
    }

    #[test]
    fn sanitize_error_body_redacts_key_saved_with_surrounding_spaces() {
        let body = "token sk-supersecret1234 rejected";
        let sanitized = sanitize_error_body(body, " sk-supersecret1234\t");
        assert!(!sanitized.contains("sk-supersecret1234"));
        assert!(sanitized.contains("[redacted]"));
    }

    #[test]
    fn sanitize_error_body_redacts_key_split_by_invisible_chars() {
        let body = "token sk-super\u{200B}secret1234 rejected";
        let sanitized = sanitize_error_body(body, "sk-supersecret1234");
        assert!(!sanitized.contains("sk-supersecret1234"));
        assert!(sanitized.contains("[redacted]"));
    }

    #[test]
    fn sanitize_error_body_redacts_leaked_api_key() {
        let body = "upstream rejected token sk-supersecret1234 for this request";
        let sanitized = sanitize_error_body(body, "sk-supersecret1234");
        assert!(!sanitized.contains("sk-supersecret1234"));
        assert!(sanitized.contains("[redacted]"));
    }

    #[test]
    fn serializes_system_user_and_assistant_text_as_openai_expects() {
        let system = serde_json::to_value(to_request_message(&ChatMessage::System(
            "be helpful".to_string(),
        )))
        .unwrap();
        assert_eq!(
            system,
            serde_json::json!({"role": "system", "content": "be helpful"})
        );

        let user = serde_json::to_value(to_request_message(&ChatMessage::User(
            PromptText::user_message("hi", Some("2026-09-22T04:12:00Z")),
        )))
        .unwrap();
        assert_eq!(
            user,
            serde_json::json!({
                "role": "user",
                "content": "<scitl:user-message sent_at=\"2026-09-22T04:12:00Z\">\nhi\n</scitl:user-message>",
            })
        );

        let assistant = serde_json::to_value(to_request_message(&ChatMessage::Assistant {
            content: Some("done".to_string()),
            tool_calls: Vec::new(),
        }))
        .unwrap();
        assert_eq!(
            assistant,
            serde_json::json!({"role": "assistant", "content": "done"})
        );
    }

    fn user(text: &str, sent_at: &str) -> ChatMessage {
        ChatMessage::User(PromptText::user_message(text, Some(sent_at)))
    }

    fn assistant(text: &str) -> ChatMessage {
        ChatMessage::Assistant {
            content: Some(text.to_string()),
            tool_calls: Vec::new(),
        }
    }

    fn tool_call(id: &str) -> ToolCallRequest {
        ToolCallRequest {
            id: Some(id.to_string()),
            name: "get_current_task_detail".to_string(),
            arguments: serde_json::json!({}).into(),
        }
    }

    fn request_json(messages: &[ChatMessage]) -> Vec<serde_json::Value> {
        to_request_messages(messages)
            .iter()
            .map(|m| serde_json::to_value(m).unwrap())
            .collect()
    }

    fn roles(sent: &[serde_json::Value]) -> Vec<&str> {
        sent.iter().map(|m| m["role"].as_str().unwrap()).collect()
    }

    #[test]
    fn fills_in_a_user_message_when_the_conversation_starts_with_the_assistant() {
        let sent = request_json(&[
            ChatMessage::System("s".to_string()),
            assistant("a"),
            user("u", "2026-09-22T04:12:00Z"),
        ]);

        assert_eq!(roles(&sent), vec!["system", "user", "assistant", "user"]);
        // 補った発言も同じ囲みで送り、日時の属性だけを省く。
        assert_eq!(
            sent[1]["content"],
            PromptText::user_message(PLACEHOLDER_USER_TEXT, None).as_str()
        );
    }

    #[test]
    fn merges_consecutive_messages_of_the_same_role() {
        let sent = request_json(&[
            ChatMessage::System("s".to_string()),
            user("u1", "2026-09-22T04:12:00Z"),
            user("u2", "2026-09-22T05:00:00Z"),
            assistant("a1"),
            assistant("a2"),
        ]);

        assert_eq!(roles(&sent), vec!["system", "user", "assistant"]);
        // 発言ごとの囲みと送信日時はそのまま残る。
        assert_eq!(
            sent[1]["content"],
            format!(
                "{}\n\n{}",
                PromptText::user_message("u1", Some("2026-09-22T04:12:00Z")).as_str(),
                PromptText::user_message("u2", Some("2026-09-22T05:00:00Z")).as_str(),
            )
        );
        assert_eq!(sent[2]["content"], "a1\n\na2");
    }

    #[test]
    fn merges_assistant_text_into_a_following_tool_call() {
        let sent = request_json(&[
            ChatMessage::System("s".to_string()),
            user("u", "2026-09-22T04:12:00Z"),
            assistant("a"),
            ChatMessage::Assistant {
                content: None,
                tool_calls: vec![tool_call("call_1")],
            },
            ChatMessage::Tool {
                tool_call_id: Some("call_1".to_string()),
                content: PromptText::json(&serde_json::json!({})),
            },
        ]);

        assert_eq!(roles(&sent), vec!["system", "user", "assistant", "tool"]);
        assert_eq!(sent[2]["content"], "a");
        assert_eq!(sent[2]["tool_calls"][0]["id"], "call_1");
    }

    #[test]
    fn leaves_tool_round_trips_as_they_are() {
        let round_trip = |id: &str| {
            [
                ChatMessage::Assistant {
                    content: None,
                    tool_calls: vec![tool_call(id)],
                },
                ChatMessage::Tool {
                    tool_call_id: Some(id.to_string()),
                    content: PromptText::json(&serde_json::json!({})),
                },
            ]
        };
        let mut messages = vec![
            ChatMessage::System("s".to_string()),
            user("u", "2026-09-22T04:12:00Z"),
        ];
        messages.extend(round_trip("call_1"));
        messages.extend(round_trip("call_2"));

        assert_eq!(
            roles(&request_json(&messages)),
            vec!["system", "user", "assistant", "tool", "assistant", "tool"]
        );
    }

    #[test]
    fn serializes_assistant_tool_calls_with_json_encoded_arguments() {
        let assistant = serde_json::to_value(to_request_message(&ChatMessage::Assistant {
            content: None,
            tool_calls: vec![ToolCallRequest {
                id: Some("call_1".to_string()),
                name: "add_steps".to_string(),
                arguments: serde_json::json!({ "descriptions": ["買い出し"] }).into(),
            }],
        }))
        .unwrap();

        assert_eq!(
            assistant,
            serde_json::json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "add_steps",
                        "arguments": "{\"descriptions\":[\"買い出し\"]}"
                    }
                }]
            })
        );
    }

    #[test]
    fn echoes_malformed_tool_arguments_back_verbatim() {
        let assistant = serde_json::to_value(to_request_message(&ChatMessage::Assistant {
            content: None,
            tool_calls: vec![ToolCallRequest {
                id: Some("call_1".to_string()),
                name: "update_task".to_string(),
                arguments: ToolArguments::parse("{\"title\": ".to_string()),
            }],
        }))
        .unwrap();

        assert_eq!(
            assistant["tool_calls"][0]["function"]["arguments"],
            "{\"title\": "
        );
    }

    #[test]
    fn deserializes_reasoning_content_extension_when_present() {
        let parsed: ResponseMessage = serde_json::from_value(serde_json::json!({
            "content": "答え",
            "reasoning_content": "考え中…"
        }))
        .unwrap();
        assert_eq!(parsed.content.as_deref(), Some("答え"));
        assert_eq!(parsed.reasoning_content.as_deref(), Some("考え中…"));
    }

    #[test]
    fn reasoning_content_defaults_to_none_when_absent() {
        let parsed: ResponseMessage = serde_json::from_value(serde_json::json!({
            "content": "答え"
        }))
        .unwrap();
        assert_eq!(parsed.reasoning_content, None);
    }

    #[test]
    fn serializes_tool_response_and_omits_missing_tool_call_id() {
        let with_id = serde_json::to_value(to_request_message(&ChatMessage::Tool {
            tool_call_id: Some("call_1".to_string()),
            content: PromptText::json(&serde_json::json!({})),
        }))
        .unwrap();
        assert_eq!(
            with_id,
            serde_json::json!({"role": "tool", "tool_call_id": "call_1", "content": "{}"})
        );

        // 呼び出しIDを払い出さないプロバイダー向け: 捏造せずフィールドごと省略する
        // (architecture.md 3節)。
        let without_id = serde_json::to_value(to_request_message(&ChatMessage::Tool {
            tool_call_id: None,
            content: PromptText::json(&serde_json::json!({})),
        }))
        .unwrap();
        assert_eq!(
            without_id,
            serde_json::json!({"role": "tool", "content": "{}"})
        );
    }
}
