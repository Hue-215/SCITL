use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use crate::db::error::CoreError;
use crate::llm::{
    ChatMessage, FinishReason, LlmAdapter, Readiness, ResponseEvent, ToolCallRequest, ToolSchema,
};

// reqwestの既定はタイムアウト無制限。応答しないエンドポイント1つでターンが
// 永久に固まるのを避ける(生成が長い非ストリーミング応答も想定し余裕を持たせる)。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// HTTPエラー時にエラー文へ載せるプロバイダ応答本文の上限。
const MAX_ERROR_BODY_CHARS: usize = 512;

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
    /// `request_timeout`は設定画面(Issue #22)の「応答タイムアウト」。未設定(`None`)なら
    /// [`REQUEST_TIMEOUT`]を既定値として使う。
    pub fn new(
        base_url: impl Into<String>,
        api_key: SecretString,
        model: impl Into<String>,
        request_timeout: Option<Duration>,
    ) -> Result<Self, CoreError> {
        let base_url = base_url.into();
        validate_base_url(&base_url)?;

        // ハードニング済みクライアントの組み立ては`net::hardened_client`に集約する
        // (MCP streamable_httpと共有。Opusレビュー指摘)。
        let client = crate::net::hardened_client(&base_url, request_timeout.unwrap_or(REQUEST_TIMEOUT))?;
        Ok(Self {
            client,
            base_url,
            api_key,
            model: model.into(),
        })
    }
}

/// 非ループバックの`http://`宛に`bearer_auth`で鍵を送らないための検証。
/// ローカル推論サーバー向けにhttpを許す必要はあるが、その用途はループバックに限られる
/// (principles.md 4節、architecture.md 5節)。検証本体は[`crate::net::validate_external_url`]
/// に集約する(MCP streamable_httpのURL検証と共有。Opusレビュー指摘)。
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

/// プロバイダ制御下の応答本文をそのままエラーに載せると、表示側でのサニタイズが前提に
/// なる。本文はプロバイダ側の失敗理由を知るために残すが、長さを制限し制御文字を潰し、
/// 送信した鍵そのものが含まれていれば伏せ字にしてから載せる(principles.md 4節)。
/// ゲートウェイがリクエストヘッダをエコーバックする構成だと`Authorization`ヘッダの
/// 値がそのまま本文に現れうるため、512文字というサイズ制限だけでは防げない
/// (Opusレビュー指摘)。
/// `turn_error::classify`が`"http {status}: ..."`の数値部分を再パースして
/// auth/rate_limit等を分類する(Opusレビュー指摘)。`StatusCode`のDisplayは
/// `"401 Unauthorized"`のように理由句を含み再パースできないため、必ず`as_u16()`で
/// 数値のみを埋め込む。HTTPリクエストから切り離してテストできるよう関数として独立させる。
fn http_error(status: reqwest::StatusCode, body: &str, api_key: &str) -> CoreError {
    CoreError::Llm(format!(
        "http {}: {}",
        status.as_u16(),
        sanitize_error_body(body, api_key)
    ))
}

fn sanitize_error_body(body: &str, api_key: &str) -> String {
    let redacted = if api_key.is_empty() {
        body.to_string()
    } else {
        body.replace(api_key, "[redacted]")
    };
    let mut sanitized: String = redacted
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_ERROR_BODY_CHARS)
        .collect();
    if redacted.chars().nth(MAX_ERROR_BODY_CHARS).is_some() {
        sanitized.push('…');
    }
    sanitized
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

fn to_request_message(message: &ChatMessage) -> RequestMessage {
    match message {
        ChatMessage::System(content) => RequestMessage::System {
            content: content.clone(),
        },
        ChatMessage::User(content) => RequestMessage::User {
            content: content.clone(),
        },
        ChatMessage::Assistant { content, tool_calls } => RequestMessage::Assistant {
            content: content.clone(),
            tool_calls: tool_calls.iter().map(to_request_tool_call).collect(),
        },
        ChatMessage::Tool { tool_call_id, content } => RequestMessage::Tool {
            tool_call_id: tool_call_id.clone(),
            content: content.clone(),
        },
    }
}

fn to_request_tool_call(call: &ToolCallRequest) -> RequestToolCall {
    RequestToolCall {
        id: call.id.clone(),
        kind: "function",
        function: RequestToolCallFunction {
            name: call.name.clone(),
            // モデルへ返す際は受け取った引数をそのまま再直列化する
            // (往復であり、こちらで内容を作り変えない)。
            arguments: call.arguments.to_string(),
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
    /// フィールド自体が無いプロバイダでは`None`のまま(`#[serde(default)]`)。新しい通信先を
    /// 追加するものではなく既存エンドポイントの応答を追加で読むだけなので、Opusを呼ぶ条件
    /// 「外部通信」には該当しないと判断した(PR本文に記載)。
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
        // `main.rs::build_adapter_for`はモデル未選択でもエラーにせず空文字のまま
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
            messages: messages.iter().map(to_request_message).collect(),
            tools: tools
                .iter()
                .map(|t| RequestTool {
                    kind: "function",
                    function: RequestFunction {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        parameters: t.parameters.clone(),
                    },
                })
                .collect(),
            // 非ストリーミングでも戻り値はイベント列に組み立て直す(本モジュール冒頭コメント参照)。
            stream: false,
        };

        let endpoint = completions_endpoint(&self.base_url)?;
        let response = self
            .client
            .post(endpoint)
            .bearer_auth(self.api_key.expose_secret())
            .json(&body)
            .send()
            .await
            .map_err(provider_error)?;

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
            // 引数の型が期待と違う場合は変換を試みず、エラーとして返す(principles.md 3節)。
            // 空オブジェクトへのフォールバックは、引数を伴うツールを引数無しで
            // 発火させてしまうため避ける。
            let arguments: serde_json::Value = serde_json::from_str(&call.function.arguments)
                .map_err(|e| {
                    CoreError::Llm(format!("invalid tool call arguments from provider: {e}"))
                })?;
            events.push(ResponseEvent::ToolCall {
                id: call.id,
                name: call.function.name,
                arguments,
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
    use super::*;

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
    fn rejects_http_non_loopback_base_url() {
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
            completions_endpoint("https://api.openai.com/v1").unwrap().as_str(),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            completions_endpoint("https://api.openai.com/v1/").unwrap().as_str(),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn http_error_embeds_numeric_status_code_only() {
        let err = http_error(reqwest::StatusCode::UNAUTHORIZED, "invalid key", "");
        let CoreError::Llm(message) = err else {
            panic!("expected CoreError::Llm");
        };
        // `turn_error::classify`が期待する形式(`orchestration/turn_error.rs`の
        // `parse_http_status`参照)。理由句(" Unauthorized"等)を含めてしまうと
        // 再パースに失敗し、全HTTPエラーがUnexpectedに落ちる(Opusレビューで検出)。
        assert_eq!(message, "http 401: invalid key");
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
        assert_eq!(system, serde_json::json!({"role": "system", "content": "be helpful"}));

        let user =
            serde_json::to_value(to_request_message(&ChatMessage::User("hi".to_string())))
                .unwrap();
        assert_eq!(user, serde_json::json!({"role": "user", "content": "hi"}));

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

    #[test]
    fn serializes_assistant_tool_calls_with_json_encoded_arguments() {
        let assistant = serde_json::to_value(to_request_message(&ChatMessage::Assistant {
            content: None,
            tool_calls: vec![ToolCallRequest {
                id: Some("call_1".to_string()),
                name: "add_steps".to_string(),
                arguments: serde_json::json!({ "descriptions": ["買い出し"] }),
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
            content: "{}".to_string(),
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
            content: "{}".to_string(),
        }))
        .unwrap();
        assert_eq!(without_id, serde_json::json!({"role": "tool", "content": "{}"}));
    }
}
