//! 1ターンの応答生成が設定から受け取る文脈。送信・編集・再試行のどの入口も同じ組を
//! 使うため、1つの型にまとめて渡す(principles.md 5節)。

use crate::llm::LlmAdapter;
use crate::orchestration::{McpAccess, SystemPrompts, ToolLimits};

/// 組み立ては[`crate::settings::Snapshot::turn_context`]が行う。`Default`はプロバイダー
/// 未選択・プロンプト無し・外部ツール無し・既定の上限で、テストの出発点に使う。
#[derive(Clone, Copy, Default)]
pub struct TurnContext<'a> {
    /// `None`はプロバイダー未選択。ターンはエラー発言(`no_provider`)で終わる。
    pub adapter: Option<&'a dyn LlmAdapter>,
    pub prompts: SystemPrompts<'a>,
    pub mcp: McpAccess<'a>,
    pub limits: ToolLimits,
}
