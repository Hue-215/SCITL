//! 応答生成が終わったことの知らせ。画面を見ていない利用者へ伝えるためのもので
//! (`architecture/concurrency.md`「Androidで裏へ回ったとき」)、画面の表示には使わない
//! (画面はコマンドの完了でDBから読み直す)。

use rusqlite::Connection;

use crate::db::messages::{self, Chat, ReplyPart, Role};
use crate::db::tasks;
use crate::error::Result;
use crate::orchestration::turn_error::STOPPED_KIND;

/// 終わった応答生成。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishedTurn {
    pub chat: Chat,
    /// タスクチャットなら、タスクのタイトル(無ければ最初の発言から作った呼び名)。総合チャットと、
    /// どちらも無いタスクは`None`。
    pub task_name: Option<String>,
    pub outcome: TurnOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    /// 返信で終わった。`text`は最後に本文を書いたラウンドの本文(本文を書かずに終わったら空)。
    Replied { text: String },
    /// エラー発言で終わった。`kind`は種別コード(`messages.error_kind`)。
    Failed { kind: String },
}

/// 終わった応答生成の受け口([`crate::orchestration::TurnContext::finished`])。
/// ターンの処理を止めないよう、受け口の側で待たない。
pub type TurnFinished<'a> = &'a (dyn Fn(FinishedTurn) + Send + Sync);

/// 終わったことを知らせない呼び出し元(CLI・テスト等)が渡す受け口。
pub fn discard_finished(_: FinishedTurn) {}

/// 試行の返信の行から、知らせる中身を作る。利用者が止めたターンと、返信の行が無い試行
/// (行を書けなかった・書いたあとに消された)は`None`。
pub(super) fn finished_turn(
    conn: &Connection,
    chat: Chat,
    turn_id: &str,
    attempt_no: i64,
) -> Result<Option<FinishedTurn>> {
    let Some(reply) = messages::reply_of_attempt(conn, turn_id, attempt_no)? else {
        return Ok(None);
    };
    let outcome = match reply.role {
        Role::Error => match reply.error_kind {
            Some(kind) if kind == STOPPED_KIND => return Ok(None),
            Some(kind) => TurnOutcome::Failed { kind },
            None => return Ok(None),
        },
        _ => TurnOutcome::Replied {
            text: last_text(&reply.parts),
        },
    };
    let task_name = match chat {
        Chat::General => None,
        Chat::Task(task_id) => {
            let detail = tasks::get_task_detail_view(conn, task_id)?;
            detail.task.title.or(detail.fallback_label)
        }
    };
    Ok(Some(FinishedTurn {
        chat,
        task_name,
        outcome,
    }))
}

/// 最後に本文を書いたラウンドの本文。ツールを呼ぶ前の前置きではなく、答えにあたるほうを取る。
fn last_text(parts: &[ReplyPart]) -> String {
    parts
        .iter()
        .rev()
        .find_map(|part| match part {
            ReplyPart::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}
