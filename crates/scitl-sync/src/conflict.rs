//! 競合の画面に渡すもの(`docs/sync/index.md`「画面とCLI」)。両側の中身を並べて見せ、利用者が選んだ側を
//! 単位ごとに受け取る。中身は書き換えず、長いものを切り詰めるだけにする(相手の端末から来た中身も、
//! 普段の表示と同じく要素の中に閉じ込めて描く。`docs/spec/architecture/sanitize.md`「画面への出力」)。

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use scitl_core::text;

use crate::merge::{Conflict, Pick, Unit};
use crate::transport::{MessageRow, Part, Transport};

/// 1つの発言として見せる長さの上限(文字数)。
const MESSAGE_CHARS: usize = 300;
/// 会話の末尾から見せる発言の数。
const SHOWN_MESSAGES: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct ConflictView {
    /// 選んだ側を返すときに添える値。
    pub id: String,
    pub kind: ConflictKind,
    /// この端末の側。
    pub ours: ConflictSide,
    /// 相手の端末の側。
    pub theirs: ConflictSide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
    Task,
    General,
    Memory,
}

/// 片側の中身。タスクはタスクの行・工程・会話、総合チャットは会話、メモリは本文。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct ConflictSide {
    pub title: Option<String>,
    pub description: Option<String>,
    pub deadline: Option<String>,
    pub archived: bool,
    pub deleted: bool,
    /// 消えていない工程(並びの順)。
    pub steps: Vec<ConflictStep>,
    /// 消えていない通常発言の、末尾のいくつか。
    pub messages: Vec<ConflictMessage>,
    /// `messages`より前にある、消えていない通常発言の数。
    pub earlier_messages: usize,
    /// こちらの側にしか無い(相手の側では消えているか、無い)通常発言の数。選ばないと消える。
    pub only_here: usize,
    /// メモリの本文。
    pub content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct ConflictStep {
    pub description: String,
    pub done: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct ConflictMessage {
    /// `user`・`assistant`・`error`。
    pub role: String,
    pub text: String,
}

/// 利用者が選んだ側。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct ConflictChoice {
    pub id: String,
    pub side: ChosenSide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum ChosenSide {
    /// この端末。
    Ours,
    /// 相手の端末。
    Theirs,
}

impl From<ChosenSide> for Pick {
    fn from(side: ChosenSide) -> Self {
        match side {
            ChosenSide::Ours => Pick::Ours,
            ChosenSide::Theirs => Pick::Theirs,
        }
    }
}

pub(crate) fn view(conflict: &Conflict) -> ConflictView {
    let kind = match conflict.unit {
        Unit::Task(_) => ConflictKind::Task,
        Unit::General => ConflictKind::General,
        Unit::Memory(_) => ConflictKind::Memory,
    };
    ConflictView {
        id: conflict.unit.id(),
        kind,
        ours: side(&conflict.ours, &conflict.theirs),
        theirs: side(&conflict.theirs, &conflict.ours),
    }
}

fn alive_normal(t: &Transport) -> Vec<&MessageRow> {
    t.messages.iter().filter(|m| m.is_alive_normal()).collect()
}

fn side(this: &Transport, other: &Transport) -> ConflictSide {
    let mut view = ConflictSide::default();
    if let Some(task) = this.tasks.first() {
        view.title = task.title.clone();
        view.description = task.description.clone();
        view.deadline = task.deadline.clone();
        view.archived = task.archived_at.is_some();
        view.deleted = task.deleted_at.is_some();
    }
    if let Some(memory) = this.memories.first() {
        view.content = Some(memory.content.clone());
        view.deleted = memory.deleted_at.is_some();
    }
    let mut steps: Vec<_> = this
        .steps
        .iter()
        .filter(|s| s.deleted_at.is_none())
        .collect();
    steps.sort_by(|a, b| (a.order_index, &a.uid).cmp(&(b.order_index, &b.uid)));
    view.steps = steps
        .into_iter()
        .map(|s| ConflictStep {
            description: s.description.clone(),
            done: s.done_at.is_some(),
        })
        .collect();

    let alive = alive_normal(this);
    let alive_there: HashSet<&str> = alive_normal(other).iter().map(|m| m.uid.as_str()).collect();
    view.only_here = alive
        .iter()
        .filter(|m| !alive_there.contains(m.uid.as_str()))
        .count();
    let shown = alive.len().saturating_sub(SHOWN_MESSAGES);
    view.earlier_messages = shown;
    view.messages = alive[shown..]
        .iter()
        .map(|m| ConflictMessage {
            role: m.role.clone(),
            text: text::ellipsize(&message_text(m), MESSAGE_CHARS),
        })
        .collect();
    view
}

/// 発言の本文。返信は中身(`parts`)の本文をつなげる。
fn message_text(m: &MessageRow) -> String {
    match &m.parts {
        Some(parts) => parts
            .iter()
            .filter_map(|p| match p {
                Part::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        None => m.content.clone(),
    }
}
