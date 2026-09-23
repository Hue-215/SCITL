//! 1ターンの応答生成が受け取る文脈。送信・編集・再試行のどの入口も同じ組を使うため、
//! 1つの型にまとめて渡す(principles.md 5節)。

use crate::in_flight::InFlightSet;
use crate::llm::LlmAdapter;
use crate::orchestration::{McpAccess, SystemPrompts, ToolLimits};

/// 組み立ては[`crate::settings::Snapshot::turn_context`]が行う。
#[derive(Clone, Copy)]
pub struct TurnContext<'a> {
    /// `None`はプロバイダー未選択。ターンはエラー発言(`no_provider`)で終わる。
    pub adapter: Option<&'a dyn LlmAdapter>,
    pub prompts: SystemPrompts<'a>,
    pub mcp: McpAccess<'a>,
    pub limits: ToolLimits,
    /// 応答を生成中のタスク。アプリの起動中ずっと同じ集合を渡す。同じタスクで2つの
    /// ターンが並ぶと、発言と実行記録の行が混ざり合うため、ターンの入口で1本に絞る。
    pub generating: &'a InFlightSet<i64>,
}
