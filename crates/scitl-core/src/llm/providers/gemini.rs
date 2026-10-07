//! Gemini形式(Interactions API)のアダプタ。方言の吸収はこのファイル内に閉じる。

use std::collections::HashMap;
use std::time::Duration;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{json, Value};

use crate::config::{ApiFormat, ReasoningEffort};
use crate::error::CoreError;
use crate::llm::{
    AdapterIdentity, ChatMessage, DetectedCapabilities, ErrorDetail, FinishReason, InlineImage,
    LlmAdapter, LlmError, PromptText, Readiness, Replay, RequestPreview, ResponseEvent,
    SentSecrets, SessionId, ToolArguments, ToolOffer,
};
use crate::net::ExternalUrl;

use super::{received, Credentials, KeyHeader, RequestPart};

/// SCITL自身が付けるヘッダー(鍵)。カスタムヘッダーには使わせない
/// ([`super::validate_header_name`])。
pub(super) const OWN_HEADERS: &[&str] = &["x-goog-api-key"];

/// Gemini形式のアダプタ。
pub struct GeminiAdapter {
    client: reqwest::Client,
    base_url: ExternalUrl,
    credentials: Credentials,
    model: String,
}

impl GeminiAdapter {
    /// `base_url`は`/v1beta`を含まない(`https://generativelanguage.googleapis.com`)。
    /// `request_timeout`は設定の応答タイムアウト(`config::GeneralConfig::response_timeout`)。
    pub fn new(
        base_url: impl Into<String>,
        credentials: Credentials,
        model: impl Into<String>,
        request_timeout: Duration,
    ) -> Result<Self, CoreError> {
        let base_url = super::parse_base_url(&base_url.into())?;
        let client = crate::net::hardened_client(
            &base_url,
            crate::net::RequestTimeout::Total(request_timeout),
        )?;
        Ok(Self {
            client,
            base_url,
            credentials,
            model: model.into(),
        })
    }
}

const INTERACTIONS: &str = "v1beta/interactions";
const MODELS: &str = "v1beta/models";

/// 鍵とカスタムヘッダーを付けて送る。`session`は[`super::send_with_key`]と同じ。
async fn send(
    request: reqwest::RequestBuilder,
    credentials: &Credentials,
    session: Option<&SessionId>,
) -> Result<reqwest::Response, LlmError> {
    super::send_with_key(
        request,
        credentials,
        KeyHeader::Named("x-goog-api-key"),
        session,
    )
    .await
}

// ---- モデル一覧と能力 ----

/// `GET /v1beta/models`で、会話の生成に使えるモデルの名前(`models/`を除いたもの)を取得する。
/// 名前順に並べ、重複と空の名前を除く。
pub async fn list_models(
    base_url: &str,
    credentials: &Credentials,
) -> Result<Vec<String>, CoreError> {
    let base_url = super::parse_base_url(base_url)?;
    let client = crate::net::hardened_client(
        &base_url,
        crate::net::RequestTimeout::Total(super::METADATA_TIMEOUT),
    )?;
    let mut names = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let mut url = super::endpoint(&base_url, MODELS)?;
        url.query_pairs_mut().append_pair("pageSize", "1000");
        if let Some(token) = &page_token {
            url.query_pairs_mut().append_pair("pageToken", token);
        }
        let response = send(client.get(url), credentials, None).await?;
        let page: ModelPage =
            super::read_success_json_with(response, credentials.secrets(), metadata_error).await?;
        names.extend(
            page.models
                .into_iter()
                // 埋め込み・画像生成等の、会話を生成しないモデルは除く。
                .filter(|m| {
                    m.supported_generation_methods
                        .iter()
                        .any(|g| g == "generateContent")
                })
                .map(|m| model_id(&m.name).to_string())
                .filter(|id| !id.trim().is_empty()),
        );
        match page.next_page_token.filter(|t| !t.is_empty()) {
            Some(token) if page_token.as_ref() != Some(&token) => page_token = Some(token),
            _ => break,
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

/// `models/gemini-…`の`models/`を除く。
fn model_id(name: &str) -> &str {
    name.strip_prefix("models/").unwrap_or(name)
}

/// `models`の能力を`GET /v1beta/models/{id}`で問い合わせる。知らないモデル(404)は結果に含めない。
pub async fn detect(
    base_url: &str,
    credentials: &Credentials,
    models: &[String],
) -> Result<HashMap<String, DetectedCapabilities>, CoreError> {
    let base_url = super::parse_base_url(base_url)?;
    let client = crate::net::hardened_client(
        &base_url,
        crate::net::RequestTimeout::Total(super::METADATA_TIMEOUT),
    )?;
    let mut found = HashMap::new();
    for model in models {
        let mut url = super::endpoint(&base_url, MODELS)?;
        url.path_segments_mut()
            .map_err(|()| CoreError::ProviderConfig("failed to build endpoint".to_string()))?
            .push(model);
        let response = send(client.get(url), credentials, None).await?;
        if response.status() == StatusCode::NOT_FOUND {
            continue;
        }
        let info: ListedModel =
            super::read_success_json_with(response, credentials.secrets(), metadata_error).await?;
        found.insert(model.clone(), info.detected());
    }
    Ok(found)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelPage {
    #[serde(default)]
    models: Vec<ListedModel>,
    next_page_token: Option<String>,
}

/// モデルの情報のうち、一覧と能力に使う項目。無い項目は「分からない」として扱う。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListedModel {
    name: String,
    #[serde(default)]
    supported_generation_methods: Vec<String>,
    input_token_limit: Option<u32>,
    thinking: Option<bool>,
}

impl ListedModel {
    fn detected(&self) -> DetectedCapabilities {
        DetectedCapabilities {
            // モデルの情報に画像入力の可否は無い。
            image: None,
            tools: Some(true),
            thinking: self.thinking,
            context_length: self.input_token_limit,
        }
    }
}

// ---- リクエスト ----

#[derive(Serialize)]
struct RequestBody<'a> {
    model: &'a str,
    /// やり取りをGoogle側に保存させない。会話はこのアプリが持ち、毎回すべて送る。
    store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<String>,
    input: Vec<RequestPart>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<Value>,
}

/// 思考の強さ(`thinking_level`)。思考を切る指定は無いので、「オフ」はいちばん弱い
/// `minimal`にする。
fn thinking_level(effort: Option<ReasoningEffort>) -> Option<&'static str> {
    effort.map(|effort| match effort {
        ReasoningEffort::Off => "minimal",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
    })
}

fn request_body<'a>(
    model: &'a str,
    messages: &[ChatMessage],
    tools: ToolOffer<'_>,
    thinking_level: Option<&'static str>,
) -> RequestBody<'a> {
    let (system_instruction, input) = to_input(messages);
    let mut config = serde_json::Map::new();
    if let Some(level) = thinking_level {
        config.insert("thinking_level".into(), json!(level));
        // 要約を返させないと、思考の中身が画面に出ない。
        config.insert("thinking_summaries".into(), json!("auto"));
    }
    // 呼べない呼び出しでも定義は外さない(ターン内の往復を追記だけに保つ)。
    if !tools.callable && !tools.schemas.is_empty() {
        config.insert("tool_choice".into(), json!("none"));
    }
    RequestBody {
        model,
        store: false,
        system_instruction,
        input,
        tools: tools
            .schemas
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": t.name(),
                    "description": t.description(),
                    "parameters": t.parameters(),
                })
            })
            .collect(),
        generation_config: (!config.is_empty()).then_some(Value::Object(config)),
    }
}

/// 発言列を、`system_instruction`と入力のステップの並びに変換する。
///
/// - ユーザー発言は`user_input`、アシスタント発言は`model_output`と`function_call`、
///   ツール結果は`function_result`にする。結果の画像は`function_result`の中に置く
/// - 最初のステップがユーザー発言でなければ、その前にユーザー発言を補う
/// - [`Replay`]を持つアシスタント発言は、本文と呼び出しから組み立てずに、受け取ったステップを返す
fn to_input(messages: &[ChatMessage]) -> (Option<String>, Vec<RequestPart>) {
    let mut system = String::new();
    let mut steps: Vec<RequestPart> = Vec::with_capacity(messages.len() + 1);
    for message in messages {
        match message {
            ChatMessage::System(text) => {
                if !system.is_empty() {
                    system.push_str("\n\n");
                }
                system.push_str(text);
            }
            ChatMessage::User { text, images } => {
                steps.push(RequestPart::Built(json!({
                    "type": "user_input",
                    "content": content(text.as_str(), images),
                })));
            }
            ChatMessage::Assistant {
                content: text,
                tool_calls,
                replay,
            } => {
                if steps.is_empty() {
                    steps.push(RequestPart::Built(json!({
                        "type": "user_input",
                        "content": content(
                            PromptText::user_message(super::PLACEHOLDER_USER_TEXT, None).as_str(),
                            &[],
                        ),
                    })));
                }
                match replay.elements() {
                    [] => {
                        if let Some(text) = text.as_deref().filter(|t| !t.is_empty()) {
                            steps.push(RequestPart::Built(json!({
                                "type": "model_output",
                                "content": [{ "type": "text", "text": text }],
                            })));
                        }
                        steps.extend(tool_calls.iter().map(|call| {
                            RequestPart::Built(json!({
                                "type": "function_call",
                                "id": call.id.clone().unwrap_or_default(),
                                "name": call.name,
                                "arguments": super::object_arguments(&call.arguments),
                            }))
                        }));
                    }
                    _ => steps.extend(received(replay)),
                }
            }
            ChatMessage::Tool {
                tool_call_id,
                content: result,
                images,
            } => {
                let call_id = tool_call_id.clone().unwrap_or_default();
                let mut step = json!({
                    "type": "function_result",
                    "call_id": call_id,
                    "result": content(result.as_str(), images),
                });
                if let Some(name) = called_name(&steps, &call_id) {
                    step["name"] = json!(name);
                }
                steps.push(RequestPart::Built(step));
            }
        }
    }
    ((!system.is_empty()).then_some(system), steps)
}

/// `call_id`の呼び出しの名前を、それまでのステップから引く。受け取ったまま返すステップは
/// その場で読む。
fn called_name(steps: &[RequestPart], call_id: &str) -> Option<String> {
    steps.iter().rev().find_map(|step| {
        let received;
        let step = match step {
            RequestPart::Built(value) => value,
            RequestPart::Received(raw) => {
                received = serde_json::from_str::<Value>(raw.get()).ok()?;
                &received
            }
        };
        (step.get("type").and_then(Value::as_str) == Some("function_call")
            && step.get("id").and_then(Value::as_str) == Some(call_id))
        .then(|| step.get("name")?.as_str().map(str::to_string))
        .flatten()
    })
}

/// 本文を先に、画像をその後に並べる。
fn content(text: &str, images: &[InlineImage]) -> Vec<Value> {
    let mut parts = Vec::with_capacity(1 + images.len());
    if !text.is_empty() {
        parts.push(json!({ "type": "text", "text": text }));
    }
    parts.extend(images.iter().map(|image| {
        let (mime_type, data) = image.media_type_and_data();
        json!({ "type": "image", "data": data, "mime_type": mime_type })
    }));
    parts
}

/// プレビューの本文で縮める値。画像の実体と、送り返す思考の署名。画像はステップの`content`と、
/// ツール結果の`result`に並ぶ。
const NESTED: &[&str] = &["content", "result"];
const ABBREVIATED: &[(&str, &[&str])] = &[("image", &["data"]), ("thought", &["signature"])];

// ---- 応答 ----

#[derive(Deserialize)]
struct InteractionResponse {
    status: String,
    /// 受け取ったまま送り返すため、生のJSONで持つ([`Replay`])。
    #[serde(default)]
    steps: Vec<Box<RawValue>>,
    #[serde(default)]
    errors: Vec<Value>,
}

/// 方針・安全上の判定で出力を止めたことを表すエラーコード(公式ドキュメントの
/// Generation blocked codes)。
const BLOCKED_CODES: &[&str] = &[
    "safety",
    "recitation",
    "language",
    "prohibited_content",
    "spii",
    "blocklist",
    "image_safety",
    "image_prohibited_content",
    "image_recitation",
    "image_other",
    "content_blocked",
];

fn error_code(error: &Value) -> Option<&str> {
    error.get("code").and_then(Value::as_str)
}

/// APIキーが無効なことを表す`ErrorInfo`の`reason`。キーの検証はAPIの手前の共通の入口で行われ、
/// 状態コードは401ではなく400で返る(`{"error":{"status":"INVALID_ARGUMENT","details":[{"reason":
/// "API_KEY_INVALID",…}]}}`)。文面は変わりうるので見ない。
const INVALID_KEY_REASON: &str = "API_KEY_INVALID";

/// 本文の`error.details`に、キーが無効なことを表す項目があるか。
fn rejects_key(body: &str) -> bool {
    serde_json::from_str::<Value>(body)
        .ok()
        .as_ref()
        .and_then(|v| v.pointer("/error/details"))
        .and_then(Value::as_array)
        .is_some_and(|details| {
            details
                .iter()
                .any(|d| d.get("reason").and_then(Value::as_str) == Some(INVALID_KEY_REASON))
        })
}

/// 一覧・能力の問い合わせの失敗の分類。キーが無効なことは本文でしか分からない。
fn metadata_error(status: StatusCode, body: &str, secrets: &SentSecrets) -> LlmError {
    if rejects_key(body) {
        return LlmError::Auth(ErrorDetail::http(status, body, secrets));
    }
    LlmError::from_status(status, body, secrets)
}

/// 非成功の状態コードとともに返った本文を種類付きにする。本文でしか分からない種類だけを
/// ここで判定し、残りは状態コードによる共通の分類に任せる。
fn http_error(
    status: StatusCode,
    body: &str,
    secrets: &SentSecrets,
    thinking_sent: bool,
) -> LlmError {
    let detail = || ErrorDetail::http(status, body, secrets);
    let parsed = serde_json::from_str::<Value>(body).ok();
    let error = parsed.as_ref().and_then(|v| v.get("error"));
    let code = error.and_then(error_code).unwrap_or_default();
    let message = error
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .map(str::to_lowercase)
        .unwrap_or_default();
    if BLOCKED_CODES.contains(&code) {
        return LlmError::Refused(detail());
    }
    if rejects_key(body) {
        return LlmError::Auth(detail());
    }
    if status == StatusCode::BAD_REQUEST {
        // 入力が長すぎる専用のコードは無く、文面でしか分からない。
        if message.contains("token") && (message.contains("exceed") || message.contains("limit")) {
            return LlmError::ContextExceeded(detail());
        }
        if thinking_sent && message.contains("thinking_level") {
            return LlmError::ReasoningEffortValueRejected(detail());
        }
    }
    LlmError::from_status(status, body, secrets)
}

impl GeminiAdapter {
    async fn post(
        &self,
        session: Option<&SessionId>,
        messages: &[ChatMessage],
        tools: ToolOffer<'_>,
        thinking_level: Option<&'static str>,
    ) -> Result<InteractionResponse, LlmError> {
        let body = request_body(&self.model, messages, tools, thinking_level);
        let endpoint = super::endpoint(&self.base_url, INTERACTIONS).map_err(|_| {
            LlmError::InvalidRequest(ErrorDetail::internal("failed to build endpoint"))
        })?;
        let request = self.client.post(endpoint).json(&body);
        let response = send(request, &self.credentials, session).await?;
        let secrets = self.credentials.secrets();
        let response = super::reject_failure(response, |status, body| {
            http_error(status, body, secrets, thinking_level.is_some())
        })
        .await?;
        super::read_json(response, secrets).await
    }
}

#[async_trait::async_trait]
impl LlmAdapter for GeminiAdapter {
    fn readiness(&self) -> Readiness {
        if self.model.is_empty() {
            Readiness::NoModel
        } else {
            Readiness::Ready
        }
    }

    fn identity(&self) -> Option<AdapterIdentity> {
        Some(AdapterIdentity {
            api_format: ApiFormat::Gemini,
            model: self.model.clone(),
            server: super::server(&self.base_url),
        })
    }

    /// 別のモデルが出した`thought`も送り返す。
    fn accepts_replay(&self, origin: &AdapterIdentity) -> bool {
        origin.api_format == ApiFormat::Gemini
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
            thinking_level(reasoning_effort),
        );
        let mut body = serde_json::to_value(body).expect("request body serializes to JSON");
        super::abbreviate(&mut body["input"], NESTED, ABBREVIATED);
        Some(RequestPreview { body })
    }

    async fn send(
        &self,
        session: Option<&SessionId>,
        messages: &[ChatMessage],
        tools: ToolOffer<'_>,
        reasoning_effort: Option<ReasoningEffort>,
        on_event: &mut (dyn FnMut(ResponseEvent) + Send),
    ) -> Result<Replay, CoreError> {
        let level = thinking_level(reasoning_effort);
        let response = match self.post(session, messages, tools, level).await {
            // `minimal`を持たないモデルでは、1つ上の強さで呼び直す。まだイベントを渡して
            // いないので、呼び直しても画面に二重に出ない。
            Err(LlmError::ReasoningEffortValueRejected(_)) if level == Some("minimal") => {
                self.post(session, messages, tools, Some("low")).await?
            }
            result => result?,
        };

        let secrets = self.credentials.secrets();
        let finish_reason = match response.status.as_str() {
            "completed" => FinishReason::Stop,
            "requires_action" => FinishReason::ToolCall,
            "incomplete" => FinishReason::Length,
            // 断った応答は、途中まで書いた本文も渡さない(イベントを渡す前に判定する)。
            "failed" => {
                let errors = Value::Array(response.errors.clone()).to_string();
                let detail = ErrorDetail::http(StatusCode::OK, &errors, secrets);
                let blocked = response
                    .errors
                    .iter()
                    .filter_map(error_code)
                    .any(|code| BLOCKED_CODES.contains(&code));
                return Err(if blocked {
                    LlmError::Refused(detail)
                } else {
                    LlmError::Http(detail)
                }
                .into());
            }
            other => {
                let detail =
                    ErrorDetail::http(StatusCode::OK, &format!("status: {other}"), secrets);
                return Err(LlmError::InvalidResponse(detail).into());
            }
        };

        let steps = super::read_elements(&response.steps, secrets)?;
        let mut replayed = Vec::new();
        let mut thought = false;
        for (raw, step) in response.steps.iter().zip(&steps) {
            match step.get("type").and_then(Value::as_str) {
                Some("thought") => {
                    thought = true;
                    let summary: String = step
                        .get("summary")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|part| part.get("text").and_then(Value::as_str))
                        .collect();
                    if !summary.is_empty() {
                        on_event(ResponseEvent::ReasoningDelta { text: summary });
                    }
                }
                Some("model_output") => {
                    let text: String = step
                        .get("content")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                        .filter_map(|part| part.get("text").and_then(Value::as_str))
                        .collect();
                    if !text.is_empty() {
                        on_event(ResponseEvent::TextDelta { text });
                    }
                }
                Some("function_call") => on_event(ResponseEvent::ToolCall {
                    id: step.get("id").and_then(Value::as_str).map(str::to_string),
                    name: step
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    arguments: ToolArguments::from(
                        step.get("arguments").cloned().unwrap_or(json!({})),
                    ),
                }),
                _ => continue,
            }
            replayed.push(raw.clone());
        }
        on_event(ResponseEvent::Done { finish_reason });

        // 思考のステップは、次の呼び出しで受け取ったまま返す必要がある。並びも変えないよう、
        // 上で読んだ出力のステップ(思考・本文・呼び出し)を並びごと返す。
        Ok(if thought {
            Replay::new(replayed)
        } else {
            Replay::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_server::spawn_server;
    use super::*;
    use crate::llm::{ToolCallRequest, ToolSchema};
    use secrecy::SecretString;

    const TEST_TIMEOUT: Duration = Duration::from_secs(30);

    fn adapter(base_url: &str, key: &str) -> GeminiAdapter {
        GeminiAdapter::new(
            base_url,
            Credentials::key_only(SecretString::from(key)),
            "gemini-test",
            TEST_TIMEOUT,
        )
        .unwrap()
    }

    fn user(text: &str) -> ChatMessage {
        ChatMessage::user(PromptText::user_message(text, None))
    }

    fn body(messages: &[ChatMessage], tools: ToolOffer<'_>, level: Option<&'static str>) -> Value {
        serde_json::to_value(request_body("gemini-test", messages, tools, level)).unwrap()
    }

    #[test]
    fn never_stores_the_interaction_and_puts_the_system_prompt_apart() {
        let body = body(
            &[ChatMessage::System("system".to_string()), user("hi")],
            ToolOffer::NONE,
            None,
        );
        assert_eq!(body["store"], false);
        assert_eq!(body["system_instruction"], "system");
        assert_eq!(body["input"].as_array().unwrap().len(), 1);
        assert_eq!(body["input"][0]["type"], "user_input");
        assert!(body.get("generation_config").is_none());
        assert!(body.get("previous_interaction_id").is_none());
    }

    #[test]
    fn maps_the_reasoning_effort_to_the_thinking_level() {
        assert_eq!(thinking_level(Some(ReasoningEffort::Off)), Some("minimal"));
        assert_eq!(thinking_level(Some(ReasoningEffort::High)), Some("high"));
        assert_eq!(thinking_level(None), None);
        let body = body(&[user("hi")], ToolOffer::NONE, Some("medium"));
        assert_eq!(
            body["generation_config"],
            json!({"thinking_level": "medium", "thinking_summaries": "auto"})
        );
    }

    #[test]
    fn keeps_the_tools_and_forbids_calls_when_they_cannot_be_called() {
        let schemas = [ToolSchema::internal(
            "add_steps",
            "add steps",
            json!({"type": "object"}),
        )];
        let forbidden = body(
            &[user("hi")],
            ToolOffer {
                schemas: &schemas,
                callable: false,
            },
            None,
        );
        assert_eq!(
            forbidden["tools"][0],
            json!({"type": "function", "name": "add_steps", "description": "add steps", "parameters": {"type": "object"}})
        );
        assert_eq!(forbidden["generation_config"]["tool_choice"], "none");
        let callable = body(
            &[user("hi")],
            ToolOffer {
                schemas: &schemas,
                callable: true,
            },
            None,
        );
        assert!(callable.get("generation_config").is_none());
    }

    #[test]
    fn sends_the_tool_round_trip_as_steps() {
        let image = InlineImage::from_bytes(b"\x89PNG\r\n\x1a\n0000").unwrap();
        let messages = [
            user("add a step"),
            ChatMessage::Assistant {
                content: Some("adding".to_string()),
                tool_calls: vec![ToolCallRequest {
                    id: Some("call_1".to_string()),
                    name: "add_steps".to_string(),
                    arguments: json!({"descriptions": ["draft"]}).into(),
                }],
                replay: Replay::default(),
            },
            ChatMessage::Tool {
                tool_call_id: Some("call_1".to_string()),
                content: PromptText::json(&json!({"ok": true})),
                images: vec![image],
            },
        ];
        let body = body(&messages, ToolOffer::NONE, None);
        let input = body["input"].as_array().unwrap();
        let types: Vec<_> = input.iter().map(|s| s["type"].as_str().unwrap()).collect();
        assert_eq!(
            types,
            [
                "user_input",
                "model_output",
                "function_call",
                "function_result"
            ]
        );
        assert_eq!(
            input[2],
            json!({"type": "function_call", "id": "call_1", "name": "add_steps", "arguments": {"descriptions": ["draft"]}})
        );
        assert_eq!(input[3]["call_id"], "call_1");
        assert_eq!(input[3]["name"], "add_steps");
        assert_eq!(
            input[3]["result"][0],
            json!({"type": "text", "text": "{\"ok\":true}"})
        );
        assert_eq!(input[3]["result"][1]["mime_type"], "image/png");
    }

    /// 受け取ったステップは、キーの順も変えずにそのまま返す。
    #[test]
    fn echoes_the_replayed_steps_instead_of_rebuilding_the_assistant_message() {
        let thought = r#"{"type":"thought","signature":"sig"}"#;
        let call = r#"{"type":"function_call","id":"call_1","name":"add_steps","arguments":{}}"#;
        let messages = [
            user("hi"),
            ChatMessage::Assistant {
                content: None,
                tool_calls: Vec::new(),
                replay: Replay::from_json(&format!("[{thought},{call}]")),
            },
        ];
        let request = request_body("gemini-test", &messages, ToolOffer::NONE, None);
        let text = serde_json::to_string(&request).unwrap();
        assert!(text.contains(&format!(r#""type":"user_input"}},{thought},{call}]"#)));
    }

    /// 受け取ったまま返した呼び出しにも、続く結果に呼び出しの名前を添える。
    #[test]
    fn names_the_result_of_a_replayed_call() {
        let messages = [
            user("hi"),
            ChatMessage::Assistant {
                content: None,
                tool_calls: Vec::new(),
                replay: Replay::from_json(
                    r#"[{"type":"thought","signature":"sig"},{"type":"function_call","id":"call_1","name":"add_steps","arguments":{}}]"#,
                ),
            },
            ChatMessage::Tool {
                tool_call_id: Some("call_1".to_string()),
                content: PromptText::untrusted("{}"),
                images: Vec::new(),
            },
        ];
        let body = body(&messages, ToolOffer::NONE, None);
        assert_eq!(body["input"][3]["type"], "function_result");
        assert_eq!(body["input"][3]["name"], "add_steps");
    }

    #[test]
    fn prepends_a_user_input_when_the_conversation_starts_with_the_assistant() {
        let messages = [ChatMessage::Assistant {
            content: Some("opening".to_string()),
            tool_calls: Vec::new(),
            replay: Replay::default(),
        }];
        let body = body(&messages, ToolOffer::NONE, None);
        assert_eq!(body["input"][0]["type"], "user_input");
        assert_eq!(body["input"][1]["type"], "model_output");
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
        let image = &preview["input"][0]["content"][1];
        assert_eq!(image["mime_type"], "image/png");
        assert!(image["data"].as_str().unwrap().starts_with("… ("));
    }

    /// プレビューでは、送り返す思考の署名とツール結果の画像も縮める。ツールの引数の中は、
    /// 同じ形の値でも縮めない(モデルに渡る値を隠さない)。
    #[test]
    fn preview_shortens_the_signature_and_the_result_image_but_not_the_arguments() {
        let image = InlineImage::from_bytes(b"\x89PNG\r\n\x1a\n0000").unwrap();
        let messages = [
            user("hi"),
            ChatMessage::Assistant {
                content: None,
                tool_calls: Vec::new(),
                replay: Replay::from_json(
                    r#"[{"type":"thought","signature":"0123456789"},{"type":"function_call","id":"call_1","name":"look","arguments":{"type":"image","data":"kept"}}]"#,
                ),
            },
            ChatMessage::Tool {
                tool_call_id: Some("call_1".to_string()),
                content: PromptText::untrusted("{}"),
                images: vec![image],
            },
        ];
        let preview = adapter("http://127.0.0.1:1", "")
            .request_preview(&messages, ToolOffer::NONE, None)
            .unwrap()
            .body;
        assert_eq!(preview["input"][1]["signature"], "… (10 bytes)");
        assert_eq!(preview["input"][2]["arguments"]["data"], "kept");
        assert!(preview["input"][3]["result"][1]["data"]
            .as_str()
            .unwrap()
            .starts_with("… ("));
    }

    /// 同じ方言なら、別のモデルの思考も送り返す。
    #[test]
    fn accepts_replays_from_any_model_of_the_same_format() {
        let adapter = adapter("http://127.0.0.1:1", "");
        let origin = |api_format, model: &str| AdapterIdentity {
            api_format,
            model: model.to_string(),
            server: "http://127.0.0.1:1".to_string(),
        };
        assert!(adapter.accepts_replay(&origin(ApiFormat::Gemini, "gemini-test")));
        assert!(adapter.accepts_replay(&origin(ApiFormat::Gemini, "gemini-other")));
        assert!(!adapter.accepts_replay(&origin(ApiFormat::Anthropic, "gemini-test")));
    }

    const THOUGHT_AND_CALL: &str = r#"{"id":"v1_x","status":"requires_action","steps":[
        {"type":"thought","signature":"sig","summary":[{"type":"text","text":"plan"}]},
        {"type":"model_output","content":[{"type":"text","text":"adding"}]},
        {"type":"function_call","id":"call_1","name":"add_steps","arguments":{"descriptions":["draft"]}}
    ]}"#;

    #[tokio::test]
    async fn sends_the_key_as_x_goog_api_key_and_returns_the_thought_to_replay() {
        let (base_url, handle) = spawn_server(vec![(200, THOUGHT_AND_CALL)]);
        let mut events = Vec::new();
        let replay = adapter(&base_url, "key-test")
            .send(
                None,
                &[user("hi")],
                ToolOffer::NONE,
                Some(ReasoningEffort::High),
                &mut |e| events.push(e),
            )
            .await
            .unwrap();
        let received = handle.join().unwrap();

        let headers = &received[0].headers;
        assert!(headers.starts_with("post /v1beta/interactions "));
        assert!(headers.contains("x-goog-api-key: key-test"));
        assert!(!headers.contains("authorization"));
        assert_eq!(received[0].body["store"], false);

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
                    id: Some("call_1".to_string()),
                    name: "add_steps".to_string(),
                    arguments: json!({"descriptions": ["draft"]}).into(),
                },
                ResponseEvent::Done {
                    finish_reason: FinishReason::ToolCall
                },
            ]
        );
        let expected: InteractionResponse = serde_json::from_str(THOUGHT_AND_CALL).unwrap();
        let expected: Vec<&str> = expected.steps.iter().map(|s| s.get()).collect();
        let replayed: Vec<&str> = replay.elements().iter().map(|s| s.get()).collect();
        assert_eq!(replayed, expected);
    }

    #[tokio::test]
    async fn reports_an_incomplete_interaction_as_truncated_without_replay() {
        let (base_url, handle) = spawn_server(vec![(
            200,
            r#"{"status":"incomplete","steps":[{"type":"model_output","content":[{"type":"text","text":"cut"}]}]}"#,
        )]);
        let mut events = Vec::new();
        let replay = adapter(&base_url, "")
            .send(None, &[user("hi")], ToolOffer::NONE, None, &mut |e| {
                events.push(e)
            })
            .await
            .unwrap();
        let received = handle.join().unwrap();

        assert!(!received[0].headers.contains("x-goog-api-key"));
        assert_eq!(replay, Replay::default());
        assert_eq!(
            events.last(),
            Some(&ResponseEvent::Done {
                finish_reason: FinishReason::Length
            })
        );
    }

    #[tokio::test]
    async fn a_blocked_interaction_is_a_refusal_without_passing_the_partial_reply() {
        let (base_url, handle) = spawn_server(vec![(
            200,
            r#"{"status":"failed","steps":[{"type":"model_output","content":[{"type":"text","text":"partial"}]}],"errors":[{"code":"safety","message":"blocked"}]}"#,
        )]);
        let mut events = Vec::new();
        let result = adapter(&base_url, "")
            .send(None, &[user("hi")], ToolOffer::NONE, None, &mut |e| {
                events.push(e)
            })
            .await;
        handle.join().unwrap();

        assert!(
            matches!(&result, Err(CoreError::Llm(LlmError::Refused(detail))) if detail.as_str().contains("safety")),
            "{result:?}"
        );
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn retries_with_low_when_the_model_rejects_minimal_thinking() {
        let (base_url, handle) = spawn_server(vec![
            (
                400,
                r#"{"error":{"code":"invalid_request","message":"thinking_level 'minimal' is not supported for this model."}}"#,
            ),
            (
                200,
                r#"{"status":"completed","steps":[{"type":"model_output","content":[{"type":"text","text":"ok"}]}]}"#,
            ),
        ]);
        let mut events = Vec::new();
        adapter(&base_url, "")
            .send(
                None,
                &[user("hi")],
                ToolOffer::NONE,
                Some(ReasoningEffort::Off),
                &mut |e| events.push(e),
            )
            .await
            .unwrap();
        let received = handle.join().unwrap();

        assert_eq!(
            received[0].body["generation_config"]["thinking_level"],
            "minimal"
        );
        assert_eq!(
            received[1].body["generation_config"]["thinking_level"],
            "low"
        );
        assert_eq!(events.len(), 2, "本文とDoneが1回ずつ");
    }

    #[test]
    fn classifies_errors_found_only_in_the_body() {
        let error = |code: &str, message: &str| {
            format!(r#"{{"error":{{"code":"{code}","message":"{message}"}}}}"#)
        };
        assert!(matches!(
            http_error(
                StatusCode::BAD_REQUEST,
                &error("safety", "blocked"),
                &SentSecrets::default(),
                false
            ),
            LlmError::Refused(_)
        ));
        assert!(matches!(
            http_error(
                StatusCode::BAD_REQUEST,
                &error(
                    "invalid_request",
                    "The input token count exceeds the maximum number of tokens allowed."
                ),
                &SentSecrets::default(),
                false
            ),
            LlmError::ContextExceeded(_)
        ));
        assert!(matches!(
            http_error(
                StatusCode::BAD_REQUEST,
                &error("invalid_request", "thinking_level is not supported"),
                &SentSecrets::default(),
                false
            ),
            LlmError::Http(_)
        ));
        assert!(matches!(
            http_error(
                StatusCode::UNAUTHORIZED,
                &error("authentication", "bad key"),
                &SentSecrets::default(),
                true
            ),
            LlmError::Auth(_)
        ));
    }

    /// 無効なキーは400で返るが、`details`の`reason`で見分けて認証の失敗にする。
    #[test]
    fn classifies_an_invalid_key_reported_with_400_as_auth() {
        let body = r#"{"error":{"code":400,"message":"API key not valid. Please pass a valid API key.","status":"INVALID_ARGUMENT","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"API_KEY_INVALID","domain":"googleapis.com"}]}}"#;
        assert!(matches!(
            http_error(
                StatusCode::BAD_REQUEST,
                body,
                &SentSecrets::default(),
                false
            ),
            LlmError::Auth(_)
        ));
        assert!(matches!(
            metadata_error(StatusCode::BAD_REQUEST, body, &SentSecrets::default()),
            LlmError::Auth(_)
        ));
        // `reason`が違う400は認証の失敗にしない。
        let other =
            r#"{"error":{"code":400,"status":"INVALID_ARGUMENT","details":[{"reason":"OTHER"}]}}"#;
        assert!(matches!(
            metadata_error(StatusCode::BAD_REQUEST, other, &SentSecrets::default()),
            LlmError::Http(_)
        ));
    }

    #[tokio::test]
    async fn lists_the_chat_models_across_pages_without_the_prefix() {
        let (base_url, handle) = spawn_server(vec![
            (
                200,
                r#"{"models":[{"name":"models/gemini-b","supportedGenerationMethods":["generateContent"]},{"name":"models/embedding","supportedGenerationMethods":["embedContent"]}],"nextPageToken":"p2"}"#,
            ),
            (
                200,
                r#"{"models":[{"name":"models/gemini-a","supportedGenerationMethods":["generateContent"]}]}"#,
            ),
        ]);
        let names = list_models(
            &base_url,
            &Credentials::key_only(SecretString::from("key-test".to_string())),
        )
        .await
        .unwrap();
        let received = handle.join().unwrap();

        assert_eq!(names, ["gemini-a", "gemini-b"]);
        assert!(received[0].headers.starts_with("get /v1beta/models?"));
        assert!(received[1].headers.contains("pagetoken=p2"));
    }

    #[tokio::test]
    async fn leaves_models_the_server_does_not_know_out_of_the_detection() {
        let (base_url, handle) = spawn_server(vec![
            (
                200,
                r#"{"name":"models/gemini-a","inputTokenLimit":1000,"thinking":true}"#,
            ),
            (
                404,
                r#"{"error":{"code":"model_not_found","message":"not found"}}"#,
            ),
        ]);
        let found = detect(
            &base_url,
            &Credentials::key_only(SecretString::from("key-test".to_string())),
            &["gemini-a".to_string(), "gemini-gone".to_string()],
        )
        .await
        .unwrap();
        let received = handle.join().unwrap();

        assert_eq!(found.len(), 1);
        assert_eq!(found["gemini-a"].context_length, Some(1000));
        assert!(received[1]
            .headers
            .starts_with("get /v1beta/models/gemini-gone "));
    }

    #[tokio::test]
    async fn passes_every_function_call_of_one_response() {
        let (base_url, handle) = spawn_server(vec![(
            200,
            r#"{"status":"requires_action","steps":[
                {"type":"function_call","id":"call_1","name":"a","arguments":{}},
                {"type":"function_call","id":"call_2","name":"b","arguments":{"x":1}}
            ]}"#,
        )]);
        let mut events = Vec::new();
        adapter(&base_url, "")
            .send(None, &[user("hi")], ToolOffer::NONE, None, &mut |e| {
                events.push(e)
            })
            .await
            .unwrap();
        handle.join().unwrap();

        let calls: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                ResponseEvent::ToolCall { id, name, .. } => Some((id.clone(), name.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            calls,
            [
                (Some("call_1".to_string()), "a".to_string()),
                (Some("call_2".to_string()), "b".to_string()),
            ]
        );
    }

    #[test]
    fn reads_the_capabilities_from_the_model_info() {
        let info: ListedModel = serde_json::from_value(json!({
            "name": "models/gemini-test",
            "inputTokenLimit": 1_048_576,
            "thinking": true,
        }))
        .unwrap();
        assert_eq!(
            info.detected(),
            DetectedCapabilities {
                image: None,
                tools: Some(true),
                thinking: Some(true),
                context_length: Some(1_048_576),
            }
        );
    }
}
