//! ターンの途中経過の通知。表示のためだけに使い、発言として保存するのは1ターン分を組み立て
//! 終えてから`turn`が行う。

use serde::Serialize;

use crate::llm::ResponseEvent;
use crate::orchestration::ToolExecutionView;

/// どのタスクのイベントかは持たない。受け口はターンの呼び出しごとに渡されるので、
/// 呼び出し側が知っている。
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnEvent {
    /// モデルの応答を待ち始めた。ターンの始め(外部ツールの接続と履歴の組み立ての前)と、
    /// ラウンドごとにモデルを呼ぶ前に送る。`provider`はこの応答生成が使うプロバイダーの
    /// 表示名(利用者が登録したもの)。画面は、これから本文が届き始めるまで「入力中」を出す
    /// (Issue #561)。生成中にモデルを切り替えても、ターンの始めに取った設定の名前のまま。
    AwaitingModel { provider: String },
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
