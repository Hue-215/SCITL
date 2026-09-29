//! ターンの途中経過の通知。表示のためだけに
//! 使い、発言として保存するのは1ターン分を組み立て終えてから`turn`が行う。

use serde::Serialize;

use crate::llm::ResponseEvent;
use crate::orchestration::ToolExecutionView;

/// どのタスクのイベントかは持たない。受け口はターンの呼び出しごとに渡されるので、
/// 呼び出し側が知っている。
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnEvent {
    /// アダプタが渡したイベントをそのまま転送する(`llm::LlmAdapter::send`の約束事に従う)。
    Response { event: ResponseEvent },
    /// ツールを1件実行し、実行記録を保存した。`id`は保存した行のid、`execution`はその行を
    /// 会話の一覧([`crate::orchestration::list_chat`])で読んだときと同じ表示。
    ToolExecuted {
        id: i64,
        execution: ToolExecutionView,
    },
}

/// ターンのイベントの受け口([`crate::orchestration::TurnContext::events`])。
/// ターンの処理を止めないよう、受け口の側で待たない。
pub type TurnEvents<'a> = &'a (dyn Fn(TurnEvent) + Send + Sync);

/// 途中経過を見ない呼び出し元(テスト等)が渡す受け口。
pub fn discard_events(_: TurnEvent) {}
