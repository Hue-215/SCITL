//! 1ターンの応答生成が受け取る文脈。送信・編集・再試行のどの入口も同じ組を使うため、
//! 1つの型にまとめて渡す。

use crate::attachments::Attachments;
use crate::config::ReasoningEffort;
use crate::db::messages::Chat;
use crate::in_flight::InFlightSet;
use crate::llm::{LlmAdapter, ModelCapabilities};
use crate::orchestration::{
    McpAccess, SystemPrompts, ToolLimits, TurnEvents, TurnFailure, TurnFinished,
};

/// 組み立ては[`crate::settings::Snapshot::turn_context`]が行う。
#[derive(Clone)]
pub struct TurnContext<'a> {
    /// 使えるアダプタ、または使えない理由(プロバイダー未選択・組み立てられない・設定ファイルを
    /// 読めない)。使えなければ、ターンはその理由のエラー発言で終わる。
    pub adapter: Result<&'a dyn LlmAdapter, TurnFailure>,
    /// `adapter`のプロバイダーの表示名([`crate::orchestration::TurnEvent::AwaitingModel`])。
    pub provider_name: &'a str,
    pub prompts: SystemPrompts<'a>,
    /// 聞き取りから始まった会話で、最初の返信が答えた発言([`crate::orchestration::create_task`])。
    pub opening_message: &'a str,
    /// 使うモデルの能力(`llm::resolve_capabilities`で解決済み)。
    pub capabilities: ModelCapabilities,
    /// リクエストで指定する思考の強さ。思考に対応しないモデルでは`None`
    /// ([`LlmAdapter::send`]の契約)。
    pub reasoning_effort: Option<ReasoningEffort>,
    pub mcp: McpAccess<'a>,
    pub limits: ToolLimits,
    /// 応答を生成中の会話。アプリの起動中ずっと同じ集合を渡す。同じ会話で2つの
    /// ターンが並ぶと、発言と実行記録の行が混ざり合うため、ターンの入口で1本に絞る。
    pub generating: &'a InFlightSet<Chat>,
    /// 送信前の添付と実体の置き場所。アプリの起動中ずっと同じものを渡す。
    pub attachments: &'a Attachments,
    /// ターンの途中経過の受け口([`crate::orchestration::TurnEvent`])。
    pub events: TurnEvents<'a>,
    /// 応答生成が終わったことの受け口([`crate::orchestration::FinishedTurn`])。
    pub finished: TurnFinished<'a>,
}

/// ターンの文脈を作れる、設定の写しとターンの材料の組([`crate::orchestration::create_task`])。
/// 写しを取る時点を入口の中で決めるときに使う。文脈は写しを借りるので、写しと材料を1つにして
/// 渡す。
pub trait TurnSource {
    fn turn_context(&self) -> TurnContext<'_>;
}

/// 写しを取り直さない呼び出し(試験等)は、作った文脈をそのまま渡す。
impl TurnSource for TurnContext<'_> {
    fn turn_context(&self) -> TurnContext<'_> {
        self.clone()
    }
}
