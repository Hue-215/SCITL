//! Anthropic形式(Messages API)のアダプタ。方言の吸収はこのファイル内に閉じる。

use std::collections::HashMap;
use std::time::Duration;

use reqwest::StatusCode;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{json, Value};

use crate::config::{ApiFormat, ReasoningEffort};
use crate::error::CoreError;
use crate::llm::{
    AdapterIdentity, ChatMessage, DetectedCapabilities, ErrorDetail, FinishReason, LlmAdapter,
    LlmError, PromptText, Readiness, Replay, RequestPreview, ResponseEvent, ToolArguments,
    ToolOffer,
};
use crate::net::ExternalUrl;

use super::{built, received, KeyHeader, RequestPart};

/// Anthropic形式のアダプタ。
pub struct AnthropicAdapter {
    client: reqwest::Client,
    base_url: ExternalUrl,
    api_key: SecretString,
    model: String,
}

impl AnthropicAdapter {
    /// `base_url`は`/v1`を含まない(`https://api.anthropic.com`)。`request_timeout`は
    /// 設定の応答タイムアウト(`config::GeneralConfig::response_timeout`)。
    pub fn new(
        base_url: impl Into<String>,
        api_key: SecretString,
        model: impl Into<String>,
        request_timeout: Duration,
    ) -> Result<Self, CoreError> {
        let base_url = super::parse_base_url(&base_url.into())?;
        let client = crate::net::hardened_client(&base_url, Some(request_timeout))?;
        Ok(Self {
            client,
            base_url,
            api_key,
            model: model.into(),
        })
    }
}

const MESSAGES: &str = "v1/messages";
const MODELS: &str = "v1/models";

/// `anthropic-version`ヘッダーの値。
const API_VERSION: &str = "2023-06-01";

/// 1回の応答で生成してよいトークン数の上限。Anthropic形式では必須。ストリーミングしない
/// 呼び出しで応答の待ち時間が長くなりすぎない値にする。
const MAX_TOKENS: u32 = 16_000;

/// 鍵と版のヘッダーを付けて送る。
async fn send(
    request: reqwest::RequestBuilder,
    api_key: &SecretString,
) -> Result<reqwest::Response, LlmError> {
    super::send_with_key(
        request.header("anthropic-version", API_VERSION),
        api_key,
        KeyHeader::Named("x-api-key"),
    )
    .await
}

// ---- モデル一覧と能力 ----

/// `GET /v1/models`で、提供されるモデルのIDを取得する。名前順に並べ、重複と空の名前を除く。
pub async fn list_models(base_url: &str, api_key: &SecretString) -> Result<Vec<String>, CoreError> {
    let base_url = super::parse_base_url(base_url)?;
    let client = crate::net::hardened_client(&base_url, Some(super::METADATA_TIMEOUT))?;
    let mut names = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let mut url = super::endpoint(&base_url, MODELS)?;
        url.query_pairs_mut().append_pair("limit", "1000");
        if let Some(after) = &after {
            url.query_pairs_mut().append_pair("after_id", after);
        }
        let page: ModelPage =
            super::read_success_json(send(client.get(url), api_key).await?, api_key).await?;
        names.extend(
            page.data
                .into_iter()
                .map(|m| m.id)
                .filter(|id| !id.trim().is_empty()),
        );
        match (page.has_more, page.last_id) {
            (true, Some(last)) if after.as_ref() != Some(&last) => after = Some(last),
            _ => break,
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

/// `models`の能力を`GET /v1/models/{id}`で問い合わせる。知らないモデル(404)は結果に含めない。
pub async fn detect(
    base_url: &str,
    api_key: &SecretString,
    models: &[String],
) -> Result<HashMap<String, DetectedCapabilities>, CoreError> {
    let base_url = super::parse_base_url(base_url)?;
    let client = crate::net::hardened_client(&base_url, Some(super::METADATA_TIMEOUT))?;
    let mut found = HashMap::new();
    for model in models {
        let mut url = super::endpoint(&base_url, MODELS)?;
        url.path_segments_mut()
            .map_err(|()| CoreError::ProviderConfig("failed to build endpoint".to_string()))?
            .push(model);
        let response = send(client.get(url), api_key).await?;
        if response.status() == StatusCode::NOT_FOUND {
            continue;
        }
        let info: ModelInfo = super::read_success_json(response, api_key).await?;
        found.insert(model.clone(), info.detected());
    }
    Ok(found)
}

#[derive(Deserialize)]
struct ModelPage {
    data: Vec<ListedModel>,
    #[serde(default)]
    has_more: bool,
    last_id: Option<String>,
}

#[derive(Deserialize)]
struct ListedModel {
    id: String,
}

/// `GET /v1/models/{id}`のうち、能力に使う項目。無い項目は「分からない」として扱う。
#[derive(Deserialize)]
struct ModelInfo {
    max_input_tokens: Option<u32>,
    #[serde(default)]
    capabilities: Value,
}

impl ModelInfo {
    fn detected(&self) -> DetectedCapabilities {
        let supported = |path: &str| self.capabilities.pointer(path).and_then(Value::as_bool);
        DetectedCapabilities {
            image: supported("/image_input/supported"),
            // ツールはMessages APIのすべてのモデルが受け付ける。
            tools: Some(true),
            // 思考の強さはadaptiveの思考とeffortで渡すので、adaptiveに対応するモデルだけを
            // 思考ありとする。
            thinking: supported("/thinking/types/adaptive/supported"),
            context_length: self.max_input_tokens,
        }
    }
}

// ---- リクエスト ----

#[derive(Serialize)]
struct RequestBody<'a> {
    model: &'a str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    system: Vec<Value>,
    messages: Vec<RequestMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config: Option<Value>,
    /// 伸びていく会話の末尾に、キャッシュの目印を自動で置かせる。
    cache_control: Value,
}

#[derive(Serialize)]
struct RequestMessage {
    role: &'static str,
    content: Vec<RequestPart>,
}

/// 思考をどう指定するか。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Thinking {
    /// 指定しない(思考に対応しないモデル)。
    Unspecified,
    Disabled,
    Adaptive(&'static str),
}

impl Thinking {
    fn from_effort(effort: Option<ReasoningEffort>) -> Self {
        match effort {
            None => Self::Unspecified,
            Some(ReasoningEffort::Off) => Self::Disabled,
            Some(ReasoningEffort::Low) => Self::Adaptive("low"),
            Some(ReasoningEffort::Medium) => Self::Adaptive("medium"),
            Some(ReasoningEffort::High) => Self::Adaptive("high"),
        }
    }
}

fn ephemeral() -> Value {
    json!({ "type": "ephemeral" })
}

fn request_body<'a>(
    model: &'a str,
    messages: &[ChatMessage],
    tools: ToolOffer<'_>,
    thinking: Thinking,
) -> RequestBody<'a> {
    let (system, messages) = to_request_messages(messages);
    let (thinking, output_config) = match thinking {
        Thinking::Unspecified => (None, None),
        Thinking::Disabled => (Some(json!({ "type": "disabled" })), None),
        // 既定では思考の中身が空で返る。画面に出すため要約を返させる。
        Thinking::Adaptive(effort) => (
            Some(json!({ "type": "adaptive", "display": "summarized" })),
            Some(json!({ "effort": effort })),
        ),
    };
    RequestBody {
        model,
        max_tokens: MAX_TOKENS,
        system,
        messages,
        tools: tools
            .schemas
            .iter()
            .map(|t| {
                json!({
                    "name": t.name(),
                    "description": t.description(),
                    "input_schema": t.parameters(),
                })
            })
            .collect(),
        // 呼べない呼び出しでも定義は外さない。ツールの往復の途中で返す思考ブロックは、
        // それより前(ツールの定義を含む)が変わると受け付けられない。
        tool_choice: (!tools.callable && !tools.schemas.is_empty())
            .then(|| json!({ "type": "none" })),
        thinking,
        output_config,
        cache_control: ephemeral(),
    }
}

/// 発言列を、`system`と、userから始まりuserとassistantが交互に並ぶ`messages`に変換する。
///
/// - システムプロンプトは`system`に置き、末尾にキャッシュの目印を付ける(ツールの定義と
///   システムプロンプトが、会話がどう変わってもキャッシュから読まれる)
/// - ツール結果は、次のユーザー発言(`tool_result`のブロック)として送る。結果の画像は
///   `tool_result`の中に置く
/// - 同じ役割が続いたら1つにまとめる。ツール結果のあとに続くユーザー発言(ツールの上限の
///   一節等)は、`tool_result`のブロックの後ろに並ぶ
/// - 最初の発言がアシスタント発言なら、その前にユーザー発言を補う
/// - [`Replay`]を持つアシスタント発言は、本文と呼び出しから組み立てずに、受け取ったブロックを返す
fn to_request_messages(messages: &[ChatMessage]) -> (Vec<Value>, Vec<RequestMessage>) {
    let mut system_text = String::new();
    let mut out: Vec<RequestMessage> = Vec::with_capacity(messages.len() + 1);
    for message in messages {
        let (role, content) = match message {
            ChatMessage::System(text) => {
                if !system_text.is_empty() {
                    system_text.push_str("\n\n");
                }
                system_text.push_str(text);
                continue;
            }
            ChatMessage::User { text, images } => {
                let mut content = text_block(text.as_str());
                content.extend(images.iter().map(image_block));
                ("user", built(content))
            }
            ChatMessage::Assistant {
                content,
                tool_calls,
                replay,
            } => {
                let blocks = match replay.elements() {
                    [] => {
                        let mut blocks = content.as_deref().map(text_block).unwrap_or_default();
                        blocks.extend(tool_calls.iter().map(|call| {
                            json!({
                                "type": "tool_use",
                                "id": call.id.clone().unwrap_or_default(),
                                "name": call.name,
                                "input": super::object_arguments(&call.arguments),
                            })
                        }));
                        built(blocks)
                    }
                    _ => received(replay),
                };
                ("assistant", blocks)
            }
            ChatMessage::Tool {
                tool_call_id,
                content,
                images,
            } => {
                let mut result = text_block(content.as_str());
                result.extend(images.iter().map(image_block));
                (
                    "user",
                    built([json!({
                        "type": "tool_result",
                        "tool_use_id": tool_call_id.clone().unwrap_or_default(),
                        "content": result,
                    })]),
                )
            }
        };
        if content.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.role == role => last.content.extend(content),
            last => {
                if role == "assistant" && last.is_none() {
                    out.push(RequestMessage {
                        role: "user",
                        content: built(text_block(
                            PromptText::user_message(super::PLACEHOLDER_USER_TEXT, None).as_str(),
                        )),
                    });
                }
                out.push(RequestMessage { role, content });
            }
        }
    }
    let system = if system_text.is_empty() {
        Vec::new()
    } else {
        vec![json!({ "type": "text", "text": system_text, "cache_control": ephemeral() })]
    };
    (system, out)
}

/// 空のtextブロックは受け付けられないので、空なら何も返さない。
fn text_block(text: &str) -> Vec<Value> {
    if text.is_empty() {
        Vec::new()
    } else {
        vec![json!({ "type": "text", "text": text })]
    }
}

fn image_block(image: &crate::llm::InlineImage) -> Value {
    let (media_type, data) = image.media_type_and_data();
    json!({
        "type": "image",
        "source": { "type": "base64", "media_type": media_type, "data": data },
    })
}

/// プレビューの本文で縮める値。画像の実体と、送り返す思考の署名。ブロックは発言の`content`と、
/// ツール結果の`content`に並ぶ。
const NESTED: &[&str] = &["content"];
const ABBREVIATED: &[(&str, &[&str])] = &[
    ("image", &["source", "data"]),
    ("thinking", &["signature"]),
    ("redacted_thinking", &["data"]),
];

// ---- 応答 ----

#[derive(Deserialize)]
struct MessageResponse {
    /// 受け取ったまま送り返すため、生のJSONで持つ([`Replay`])。
    #[serde(default)]
    content: Vec<Box<RawValue>>,
    stop_reason: Option<String>,
    stop_details: Option<Value>,
}

/// 非成功の状態コードとともに返った本文を種類付きにする。本文でしか分からない種類だけを
/// ここで判定し、残りは状態コードによる共通の分類に任せる。
fn http_error(status: StatusCode, body: &str, api_key: &str, thinking: Thinking) -> LlmError {
    let detail = || ErrorDetail::http(status, body, api_key);
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.pointer("/error/message")?.as_str().map(str::to_lowercase))
        .unwrap_or_default();
    if status == StatusCode::BAD_REQUEST {
        if message.contains("prompt is too long") || message.contains("context window") {
            return LlmError::ContextExceeded(detail());
        }
        // adaptiveの思考を持たないモデル(思考の指定は予算でしか受け付けない)。
        if matches!(thinking, Thinking::Adaptive(_))
            && message.contains("adaptive thinking is not supported")
        {
            return LlmError::ReasoningEffortRejected(detail());
        }
    }
    LlmError::from_status(status, body, api_key)
}

/// 思考を切れないモデルが、切る指定を拒んだか。
fn rejects_disabled_thinking(error: &LlmError) -> bool {
    matches!(error, LlmError::Http(detail) if detail.as_str().contains("thinking.type.disabled"))
}

impl AnthropicAdapter {
    async fn post(
        &self,
        messages: &[ChatMessage],
        tools: ToolOffer<'_>,
        thinking: Thinking,
    ) -> Result<MessageResponse, LlmError> {
        let body = request_body(&self.model, messages, tools, thinking);
        let endpoint = super::endpoint(&self.base_url, MESSAGES).map_err(|_| {
            LlmError::InvalidRequest(ErrorDetail::internal("failed to build endpoint"))
        })?;
        let response = send(self.client.post(endpoint).json(&body), &self.api_key).await?;
        let key = self.api_key.expose_secret();
        let response = super::reject_failure(response, |status, body| {
            http_error(status, body, key, thinking)
        })
        .await?;
        super::read_json(response, &self.api_key).await
    }
}

#[async_trait::async_trait]
impl LlmAdapter for AnthropicAdapter {
    fn readiness(&self) -> Readiness {
        if self.model.is_empty() {
            Readiness::NoModel
        } else {
            Readiness::Ready
        }
    }

    fn identity(&self) -> Option<AdapterIdentity> {
        Some(AdapterIdentity {
            api_format: ApiFormat::Anthropic,
            model: self.model.clone(),
            server: super::server(&self.base_url),
        })
    }

    /// 別のモデルが出したブロックも送り返す。読めないモデルのブロックはAPIが黙って捨て、元の
    /// モデルに戻ったときにまた読まれる。
    fn accepts_replay(&self, origin: &AdapterIdentity) -> bool {
        origin.api_format == ApiFormat::Anthropic
    }

    fn request_preview(
        &self,
        messages: &[ChatMessage],
        tools: ToolOffer<'_>,
        reasoning_effort: Option<ReasoningEffort>,
    ) -> Option<RequestPreview> {
        let body = request_body(
            &self.model,
            messages,
            tools,
            Thinking::from_effort(reasoning_effort),
        );
        let mut body = serde_json::to_value(body).expect("request body serializes to JSON");
        super::abbreviate(&mut body["messages"], NESTED, ABBREVIATED);
        Some(RequestPreview { body })
    }

    async fn send(
        &self,
        messages: &[ChatMessage],
        tools: ToolOffer<'_>,
        reasoning_effort: Option<ReasoningEffort>,
        on_event: &mut (dyn FnMut(ResponseEvent) + Send),
    ) -> Result<Replay, CoreError> {
        let thinking = Thinking::from_effort(reasoning_effort);
        let response = match self.post(messages, tools, thinking).await {
            // 思考を切れないモデルでは、いちばん弱い思考で呼び直す。まだイベントを渡して
            // いないので、呼び直しても画面に二重に出ない。
            Err(e) if thinking == Thinking::Disabled && rejects_disabled_thinking(&e) => {
                self.post(messages, tools, Thinking::Adaptive("low"))
                    .await?
            }
            result => result?,
        };

        // 断った応答は、途中まで書いた本文も渡さない(イベントを渡す前に判定する)。
        if response.stop_reason.as_deref() == Some("refusal") {
            let details = response
                .stop_details
                .map(|d| d.to_string())
                .unwrap_or_default();
            let key = self.api_key.expose_secret();
            return Err(LlmError::Refused(ErrorDetail::http(StatusCode::OK, &details, key)).into());
        }

        let blocks = super::read_elements(&response.content, &self.api_key)?;
        let mut replay = false;
        for block in &blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("thinking") => {
                    replay = true;
                    if let Some(text) = block.get("thinking").and_then(Value::as_str) {
                        if !text.is_empty() {
                            on_event(ResponseEvent::ReasoningDelta {
                                text: text.to_string(),
                            });
                        }
                    }
                }
                Some("redacted_thinking") => replay = true,
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        if !text.is_empty() {
                            on_event(ResponseEvent::TextDelta {
                                text: text.to_string(),
                            });
                        }
                    }
                }
                Some("tool_use") => on_event(ResponseEvent::ToolCall {
                    id: block.get("id").and_then(Value::as_str).map(str::to_string),
                    name: block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    arguments: ToolArguments::from(
                        block.get("input").cloned().unwrap_or(json!({})),
                    ),
                }),
                _ => {}
            }
        }

        let finish_reason = match response.stop_reason.as_deref() {
            Some("tool_use") => FinishReason::ToolCall,
            Some("max_tokens" | "model_context_window_exceeded") => FinishReason::Length,
            _ => FinishReason::Stop,
        };
        on_event(ResponseEvent::Done { finish_reason });

        // 思考ブロックは、ツールの往復の次の呼び出しで受け取ったまま返す必要がある。並びも
        // 変えられないので、応答のブロックをすべてそのまま返す。
        Ok(if replay {
            Replay::new(response.content)
        } else {
            Replay::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_server::spawn_server;
    use super::*;
    use crate::llm::{InlineImage, ToolCallRequest, ToolSchema};

    const TEST_TIMEOUT: Duration = Duration::from_secs(30);

    fn adapter(base_url: &str, key: &str) -> AnthropicAdapter {
        AnthropicAdapter::new(
            base_url,
            SecretString::from(key),
            "claude-test",
            TEST_TIMEOUT,
        )
        .unwrap()
    }

    fn user(text: &str) -> ChatMessage {
        ChatMessage::user(PromptText::user_message(text, None))
    }

    fn schema() -> ToolSchema {
        ToolSchema::internal("add_steps", "add steps", json!({"type": "object"}))
    }

    fn body(messages: &[ChatMessage], tools: ToolOffer<'_>, thinking: Thinking) -> Value {
        serde_json::to_value(request_body("claude-test", messages, tools, thinking)).unwrap()
    }

    #[test]
    fn puts_the_system_prompt_apart_and_marks_both_cache_points() {
        let body = body(
            &[ChatMessage::System("system".to_string()), user("hi")],
            ToolOffer::NONE,
            Thinking::Unspecified,
        );
        assert_eq!(
            body["system"],
            json!([{"type": "text", "text": "system", "cache_control": {"type": "ephemeral"}}])
        );
        assert_eq!(body["cache_control"], json!({"type": "ephemeral"}));
        assert_eq!(body["max_tokens"], MAX_TOKENS);
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(body["messages"][0]["role"], "user");
        assert!(body.get("thinking").is_none());
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn maps_the_reasoning_effort_to_adaptive_thinking() {
        let adaptive = body(
            &[user("hi")],
            ToolOffer::NONE,
            Thinking::from_effort(Some(ReasoningEffort::Low)),
        );
        assert_eq!(
            adaptive["thinking"],
            json!({"type": "adaptive", "display": "summarized"})
        );
        assert_eq!(adaptive["output_config"], json!({"effort": "low"}));

        let off = body(
            &[user("hi")],
            ToolOffer::NONE,
            Thinking::from_effort(Some(ReasoningEffort::Off)),
        );
        assert_eq!(off["thinking"], json!({"type": "disabled"}));
        assert!(off.get("output_config").is_none());
    }

    #[test]
    fn keeps_the_tools_and_forbids_calls_when_they_cannot_be_called() {
        let schemas = [schema()];
        let forbidden = body(
            &[user("hi")],
            ToolOffer {
                schemas: &schemas,
                callable: false,
            },
            Thinking::Unspecified,
        );
        assert_eq!(forbidden["tools"][0]["name"], "add_steps");
        assert_eq!(
            forbidden["tools"][0]["input_schema"],
            json!({"type": "object"})
        );
        assert_eq!(forbidden["tool_choice"], json!({"type": "none"}));

        let callable = body(
            &[user("hi")],
            ToolOffer {
                schemas: &schemas,
                callable: true,
            },
            Thinking::Unspecified,
        );
        assert!(callable.get("tool_choice").is_none());
    }

    /// ツールの往復は`tool_use`と、続くユーザー発言の`tool_result`になる。結果の後ろに続く
    /// ユーザー発言は、同じユーザー発言の`tool_result`の後ろに並ぶ。
    #[test]
    fn sends_the_tool_round_trip_as_tool_use_and_tool_result() {
        let image = InlineImage::from_bytes(b"\x89PNG\r\n\x1a\n0000").unwrap();
        let messages = [
            user("add a step"),
            ChatMessage::Assistant {
                content: Some("adding".to_string()),
                tool_calls: vec![ToolCallRequest {
                    id: Some("toolu_1".to_string()),
                    name: "add_steps".to_string(),
                    arguments: json!({"descriptions": ["draft"]}).into(),
                }],
                replay: Replay::default(),
            },
            ChatMessage::Tool {
                tool_call_id: Some("toolu_1".to_string()),
                content: PromptText::json(&json!({"ok": true})),
                images: vec![image],
            },
            ChatMessage::user(PromptText::note("limit reached")),
        ];
        let body = body(&messages, ToolOffer::NONE, Thinking::Unspecified);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(
            messages[1]["content"],
            json!([
                {"type": "text", "text": "adding"},
                {"type": "tool_use", "id": "toolu_1", "name": "add_steps", "input": {"descriptions": ["draft"]}},
            ])
        );
        let result = &messages[2]["content"];
        assert_eq!(result[0]["type"], "tool_result");
        assert_eq!(result[0]["tool_use_id"], "toolu_1");
        assert_eq!(
            result[0]["content"][0],
            json!({"type": "text", "text": "{\"ok\":true}"})
        );
        assert_eq!(result[0]["content"][1]["source"]["media_type"], "image/png");
        assert_eq!(result[1]["type"], "text");
        assert!(result[1]["text"]
            .as_str()
            .unwrap()
            .contains("limit reached"));
    }

    /// 受け取ったブロックは、キーの順も変えずにそのまま返す。
    #[test]
    fn echoes_the_replayed_blocks_instead_of_rebuilding_the_assistant_message() {
        let thinking = r#"{"type":"thinking","thinking":"","signature":"sig"}"#;
        let tool_use = r#"{"type":"tool_use","id":"toolu_1","name":"add_steps","input":{}}"#;
        let messages = [
            user("hi"),
            ChatMessage::Assistant {
                content: None,
                tool_calls: Vec::new(),
                replay: Replay::from_json(&format!("[{thinking},{tool_use}]")),
            },
        ];
        let request = request_body(
            "claude-test",
            &messages,
            ToolOffer::NONE,
            Thinking::Unspecified,
        );
        let text = serde_json::to_string(&request).unwrap();
        assert!(text.contains(&format!(r#""content":[{thinking},{tool_use}]"#)));
    }

    /// 受け取ったブロックは、要素の中の空白も含めてそのまま返す。続くアシスタント発言と同じ
    /// 役割でまとめても、組み立てたブロックの前にそのまま並ぶ。
    #[test]
    fn keeps_received_blocks_verbatim_when_merged_with_built_ones() {
        let thinking = r#"{ "type" : "thinking", "thinking": "", "signature": "sig" }"#;
        let messages = [
            user("hi"),
            ChatMessage::Assistant {
                content: None,
                tool_calls: Vec::new(),
                replay: Replay::from_json(&format!("[{thinking}]")),
            },
            ChatMessage::Assistant {
                content: Some("more".to_string()),
                tool_calls: Vec::new(),
                replay: Replay::default(),
            },
        ];
        let request = request_body(
            "claude-test",
            &messages,
            ToolOffer::NONE,
            Thinking::Unspecified,
        );
        let text = serde_json::to_string(&request).unwrap();
        assert!(text.contains(&format!(
            r#""content":[{thinking},{{"text":"more","type":"text"}}]"#
        )));
    }

    #[test]
    fn prepends_a_user_message_when_the_conversation_starts_with_the_assistant() {
        let messages = [
            ChatMessage::Assistant {
                content: Some("opening".to_string()),
                tool_calls: Vec::new(),
                replay: Replay::default(),
            },
            user("hi"),
        ];
        let body = body(&messages, ToolOffer::NONE, Thinking::Unspecified);
        let roles: Vec<_> = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["user", "assistant", "user"]);
    }

    /// プレビューでは、送り返す思考の署名も縮める(読めないので)。思考の要約は縮めない。
    #[test]
    fn preview_shortens_the_signature_of_replayed_thinking() {
        let messages = [
            user("hi"),
            ChatMessage::Assistant {
                content: None,
                tool_calls: Vec::new(),
                replay: Replay::from_json(
                    r#"[{"type":"thinking","thinking":"plan","signature":"0123456789"}]"#,
                ),
            },
            user("next"),
        ];
        let preview = adapter("http://127.0.0.1:9", "")
            .request_preview(&messages, ToolOffer::NONE, None)
            .unwrap();
        let block = &preview.body["messages"][1]["content"][0];
        assert_eq!(block["signature"], "… (10 bytes)");
        assert_eq!(block["thinking"], "plan");
    }

    /// ツール結果の画像も縮める。ツールの引数の中は、同じ形の値でも縮めない(モデルに渡る値を
    /// 隠さない)。
    #[test]
    fn preview_shortens_the_result_image_but_not_the_arguments() {
        let image = InlineImage::from_bytes(b"\x89PNG\r\n\x1a\n0000").unwrap();
        let messages = [
            user("hi"),
            ChatMessage::Assistant {
                content: None,
                tool_calls: vec![ToolCallRequest {
                    id: Some("toolu_1".to_string()),
                    name: "look".to_string(),
                    arguments: json!({"type": "image", "source": {"data": "kept"}}).into(),
                }],
                replay: Replay::default(),
            },
            ChatMessage::Tool {
                tool_call_id: Some("toolu_1".to_string()),
                content: PromptText::untrusted("{}"),
                images: vec![image],
            },
        ];
        let preview = adapter("http://127.0.0.1:9", "")
            .request_preview(&messages, ToolOffer::NONE, None)
            .unwrap()
            .body;
        let call = &preview["messages"][1]["content"][0];
        assert_eq!(call["input"]["source"]["data"], "kept");
        let result = &preview["messages"][2]["content"][0]["content"][1];
        assert!(result["source"]["data"]
            .as_str()
            .unwrap()
            .starts_with("… ("));
    }

    /// 同じ方言なら、別のモデルの思考も送り返す(読めないブロックはAPIが捨てる)。
    #[test]
    fn accepts_replays_from_any_model_of_the_same_format() {
        let adapter = adapter("http://127.0.0.1:9", "");
        let origin = |api_format, model: &str| AdapterIdentity {
            api_format,
            model: model.to_string(),
            server: "http://127.0.0.1:9".to_string(),
        };
        assert!(adapter.accepts_replay(&origin(ApiFormat::Anthropic, "claude-other")));
        assert!(!adapter.accepts_replay(&origin(ApiFormat::Gemini, "claude-test")));
    }

    #[test]
    fn preview_shortens_only_the_image_data() {
        let image = InlineImage::from_bytes(b"\x89PNG\r\n\x1a\n0000").unwrap();
        let messages = [ChatMessage::User {
            text: PromptText::user_message("look", None),
            images: vec![image],
        }];
        let preview = adapter("http://127.0.0.1:1", "")
            .request_preview(&messages, ToolOffer::NONE, None)
            .unwrap()
            .body;
        let image = &preview["messages"][0]["content"][1];
        assert_eq!(image["source"]["media_type"], "image/png");
        assert!(image["source"]["data"].as_str().unwrap().starts_with("… ("));
        assert!(preview["messages"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("look"));
    }

    const THINKING_AND_TOOL_USE: &str = r#"{"content":[
        {"type":"thinking","thinking":"plan","signature":"sig"},
        {"type":"text","text":"adding"},
        {"type":"tool_use","id":"toolu_1","name":"add_steps","input":{"descriptions":["draft"]}}
    ],"stop_reason":"tool_use"}"#;

    #[tokio::test]
    async fn sends_the_key_as_x_api_key_and_returns_the_thinking_to_replay() {
        let (base_url, handle) = spawn_server(vec![(200, THINKING_AND_TOOL_USE)]);
        let mut events = Vec::new();
        let replay = adapter(&base_url, "sk-test")
            .send(
                &[user("hi")],
                ToolOffer::NONE,
                Some(ReasoningEffort::High),
                &mut |e| events.push(e),
            )
            .await
            .unwrap();
        let received = handle.join().unwrap();

        let headers = &received[0].headers;
        assert!(headers.starts_with("post /v1/messages "));
        assert!(headers.contains("x-api-key: sk-test"));
        assert!(headers.contains("anthropic-version: 2023-06-01"));
        assert!(!headers.contains("authorization"));

        assert_eq!(
            events,
            vec![
                ResponseEvent::ReasoningDelta {
                    text: "plan".to_string()
                },
                ResponseEvent::TextDelta {
                    text: "adding".to_string()
                },
                ResponseEvent::ToolCall {
                    id: Some("toolu_1".to_string()),
                    name: "add_steps".to_string(),
                    arguments: json!({"descriptions": ["draft"]}).into(),
                },
                ResponseEvent::Done {
                    finish_reason: FinishReason::ToolCall
                },
            ]
        );
        let expected: MessageResponse = serde_json::from_str(THINKING_AND_TOOL_USE).unwrap();
        let expected: Vec<&str> = expected.content.iter().map(|b| b.get()).collect();
        let replayed: Vec<&str> = replay.elements().iter().map(|b| b.get()).collect();
        assert_eq!(replayed, expected);
    }

    #[tokio::test]
    async fn returns_nothing_to_replay_without_thinking_and_reports_truncation() {
        let (base_url, handle) = spawn_server(vec![(
            200,
            r#"{"content":[{"type":"text","text":"cut"}],"stop_reason":"max_tokens"}"#,
        )]);
        let mut events = Vec::new();
        let replay = adapter(&base_url, "")
            .send(&[user("hi")], ToolOffer::NONE, None, &mut |e| {
                events.push(e)
            })
            .await
            .unwrap();
        let received = handle.join().unwrap();

        assert!(!received[0].headers.contains("x-api-key"));
        assert_eq!(replay, Replay::default());
        assert_eq!(
            events.last(),
            Some(&ResponseEvent::Done {
                finish_reason: FinishReason::Length
            })
        );
    }

    #[tokio::test]
    async fn a_refusal_is_an_error_without_passing_the_partial_reply() {
        let (base_url, handle) = spawn_server(vec![(
            200,
            r#"{"content":[{"type":"text","text":"partial"}],"stop_reason":"refusal","stop_details":{"type":"refusal","category":"cyber"}}"#,
        )]);
        let mut events = Vec::new();
        let result = adapter(&base_url, "")
            .send(&[user("hi")], ToolOffer::NONE, None, &mut |e| {
                events.push(e)
            })
            .await;
        handle.join().unwrap();

        assert!(
            matches!(&result, Err(CoreError::Llm(LlmError::Refused(detail))) if detail.as_str().contains("cyber")),
            "{result:?}"
        );
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn retries_with_low_effort_when_the_model_cannot_disable_thinking() {
        let (base_url, handle) = spawn_server(vec![
            (
                400,
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"\"thinking.type.disabled\" is not supported for this model."}}"#,
            ),
            (
                200,
                r#"{"content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn"}"#,
            ),
        ]);
        let mut events = Vec::new();
        adapter(&base_url, "")
            .send(
                &[user("hi")],
                ToolOffer::NONE,
                Some(ReasoningEffort::Off),
                &mut |e| events.push(e),
            )
            .await
            .unwrap();
        let received = handle.join().unwrap();

        assert_eq!(received[0].body["thinking"], json!({"type": "disabled"}));
        assert_eq!(received[1].body["thinking"]["type"], "adaptive");
        assert_eq!(received[1].body["output_config"], json!({"effort": "low"}));
        assert_eq!(events.len(), 2, "本文とDoneが1回ずつ");
    }

    #[test]
    fn classifies_errors_found_only_in_the_body() {
        let error = |message: &str| {
            format!(
                r#"{{"type":"error","error":{{"type":"invalid_request_error","message":"{message}"}}}}"#
            )
        };
        let adaptive = Thinking::Adaptive("high");
        assert!(matches!(
            http_error(
                StatusCode::BAD_REQUEST,
                &error("prompt is too long: 250000 tokens > 200000 maximum"),
                "",
                adaptive
            ),
            LlmError::ContextExceeded(_)
        ));
        assert!(matches!(
            http_error(
                StatusCode::BAD_REQUEST,
                &error("adaptive thinking is not supported on this model"),
                "",
                adaptive
            ),
            LlmError::ReasoningEffortRejected(_)
        ));
        assert!(matches!(
            http_error(
                StatusCode::BAD_REQUEST,
                &error("adaptive thinking is not supported on this model"),
                "",
                Thinking::Unspecified
            ),
            LlmError::Http(_)
        ));
        assert!(matches!(
            http_error(
                StatusCode::TOO_MANY_REQUESTS,
                &error("rate limited"),
                "",
                adaptive
            ),
            LlmError::RateLimit(_)
        ));
    }

    #[test]
    fn reads_the_capabilities_from_the_model_info() {
        let info: ModelInfo = serde_json::from_value(json!({
            "id": "claude-test",
            "max_input_tokens": 1_000_000,
            "capabilities": {
                "image_input": {"supported": true},
                "thinking": {"supported": true, "types": {"adaptive": {"supported": false}}},
            },
        }))
        .unwrap();
        assert_eq!(
            info.detected(),
            DetectedCapabilities {
                image: Some(true),
                tools: Some(true),
                thinking: Some(false),
                context_length: Some(1_000_000),
            }
        );
    }

    #[tokio::test]
    async fn lists_the_models_across_pages() {
        let (base_url, handle) = spawn_server(vec![
            (
                200,
                r#"{"data":[{"id":"claude-b"}],"has_more":true,"last_id":"claude-b"}"#,
            ),
            (
                200,
                r#"{"data":[{"id":"claude-a"}],"has_more":false,"last_id":"claude-a"}"#,
            ),
        ]);
        let names = list_models(&base_url, &SecretString::from("sk-test".to_string()))
            .await
            .unwrap();
        let received = handle.join().unwrap();

        assert_eq!(names, ["claude-a", "claude-b"]);
        assert!(received[1].headers.contains("after_id=claude-b"));
        assert!(received[0].headers.contains("x-api-key: sk-test"));
    }
}
