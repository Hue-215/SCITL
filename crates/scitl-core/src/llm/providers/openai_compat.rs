use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::db::error::CoreError;
use crate::llm::{ChatMessage, FinishReason, LlmAdapter, ResponseEvent, ToolSchema};

// reqwestの既定はタイムアウト無制限。応答しないエンドポイント1つでターンが
// 永久に固まるのを避ける(生成が長い非ストリーミング応答も想定し余裕を持たせる)。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// LLMプロバイダ第一弾: OpenAI互換チャットコンプリーションAPI
/// (docs/spec/rebuild/architecture.md 2節)。方言吸収はこのファイル内に閉じ込め、
/// `orchestration::turn`は本アダプタの存在を知らない。
pub struct OpenAiCompatAdapter {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl OpenAiCompatAdapter {
    /// `api_key`は呼び出し元(`secrets.rs`経由)から平文で受け取る。
    /// このアダプタ自身はkeyringに触れない(architecture.md 6節)。
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>, model: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .no_proxy()
            // architecture.md 5節が求めるのは「クロスホストのリダイレクトを拒否」だが、
            // チャットコンプリーションAPIが正当な理由でリダイレクトを返すことは
            // 想定していないため、同一ホスト内も含めて一律拒否する方が単純で安全。
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("reqwest client construction failed");
        Self {
            client,
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
        }
    }
}

#[derive(Serialize)]
struct RequestBody<'a> {
    model: &'a str,
    messages: Vec<RequestMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<RequestTool>,
    stream: bool,
}

#[derive(Serialize)]
struct RequestMessage {
    role: &'static str,
    content: String,
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
}

#[derive(Deserialize)]
struct ResponseToolCall {
    function: ResponseFunctionCall,
}

#[derive(Deserialize)]
struct ResponseFunctionCall {
    name: String,
    arguments: String,
}

#[async_trait::async_trait]
impl LlmAdapter for OpenAiCompatAdapter {
    async fn send(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSchema],
    ) -> Result<Vec<ResponseEvent>, CoreError> {
        let body = RequestBody {
            model: &self.model,
            messages: messages
                .iter()
                .map(|m| RequestMessage {
                    role: m.role,
                    content: m.content.clone(),
                })
                .collect(),
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

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| CoreError::Llm(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(CoreError::Llm(format!("http {status}: {text}")));
        }

        let parsed: CompletionResponse = response
            .json()
            .await
            .map_err(|e| CoreError::Llm(e.to_string()))?;

        let choice = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| CoreError::Llm("empty choices".to_string()))?;

        let mut events = Vec::new();
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
                name: call.function.name,
                arguments,
            });
        }

        let finish_reason = match choice.finish_reason.as_deref() {
            Some("tool_calls") => FinishReason::ToolCall,
            Some("stop") | None => FinishReason::Stop,
            Some(_) => FinishReason::Stop,
        };
        events.push(ResponseEvent::Done { finish_reason });

        Ok(events)
    }
}
