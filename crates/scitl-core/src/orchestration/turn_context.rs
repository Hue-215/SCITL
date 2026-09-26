//! 1ターンの応答生成が受け取る文脈。送信・編集・再試行のどの入口も同じ組を使うため、
//! 1つの型にまとめて渡す(principles.md 5節)。

use crate::config::ReasoningEffort;
use crate::in_flight::InFlightSet;
use crate::llm::{LlmAdapter, ModelCapabilities};
use crate::orchestration::{McpAccess, SystemPrompts, ToolLimits, TurnEvents, TurnFailure};

/// 組み立ては[`crate::settings::Snapshot::turn_context`]が行う。
#[derive(Clone)]
pub struct TurnContext<'a> {
    /// 使えるアダプタ、または使えない理由(プロバイダー未選択・組み立てられない・設定ファイルを
    /// 読めない)。使えなければ、ターンはその理由のエラー発言で終わる。
    pub adapter: Result<&'a dyn LlmAdapter, TurnFailure>,
    pub prompts: SystemPrompts<'a>,
    /// 使うモデルの能力(`llm::resolve_capabilities`で解決済み)。ツールに対応しなければ
    /// ツールを渡さない。
    pub capabilities: ModelCapabilities,
    /// リクエストで指定する思考の強さ。思考に対応しないモデルでは`None`
    /// ([`LlmAdapter::send`]の契約)。
    pub reasoning_effort: Option<ReasoningEffort>,
    pub mcp: McpAccess<'a>,
    pub limits: ToolLimits,
    /// 応答を生成中のタスク。アプリの起動中ずっと同じ集合を渡す。同じタスクで2つの
    /// ターンが並ぶと、発言と実行記録の行が混ざり合うため、ターンの入口で1本に絞る。
    pub generating: &'a InFlightSet<i64>,
    /// ターンの途中経過の受け口([`crate::orchestration::TurnEvent`])。
    pub events: TurnEvents<'a>,
}
