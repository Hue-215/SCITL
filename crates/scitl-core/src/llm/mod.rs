pub mod providers;

use serde::{Deserialize, Serialize};

use crate::db::error::CoreError;

/// アダプタ層が上位に返す形は完成した応答1つではなくイベントの並び
/// (docs/spec/principles.md 3節「応答はイベントの並びとして受け取る」、Issue #8)。
/// ストリーミングしないプロバイダーも各イベントを1回ずつ返せば同じ経路に乗る。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseEvent {
    TextDelta { text: String },
    ToolCall { name: String, arguments: serde_json::Value },
    Done { finish_reason: FinishReason },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolCall,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub role: &'static str,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// 具象プロバイダの境界。`orchestration::turn`はこのtraitのみを知り、
/// プロバイダ固有の癖は各実装内に閉じ込める(architecture.md 3節)。
#[async_trait::async_trait]
pub trait LlmAdapter: Send + Sync {
    async fn send(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSchema],
    ) -> Result<Vec<ResponseEvent>, CoreError>;
}
