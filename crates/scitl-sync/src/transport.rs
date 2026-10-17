//! 運ぶ形(`docs/sync/index.md`「IDと行の変換」)。端末の中の連番を持たず、行どうしは`uid`で指す。相手を
//! 確かめたあとのメッセージ([`Message`])の中身。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 版(端末ID → その端末での番号)。
pub(crate) type Version = BTreeMap<String, i64>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskRow {
    pub uid: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub deadline: Option<String>,
    pub archived_at: Option<String>,
    pub deleted_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub version: Version,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StepRow {
    pub uid: String,
    pub task: String,
    pub description: String,
    pub done_at: Option<String>,
    pub deleted_at: Option<String>,
    pub order_index: i64,
    pub created_at: String,
    pub version: Version,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MemoryRow {
    pub uid: String,
    pub content: String,
    pub created_at: String,
    pub updated_at: String,
    pub deleted_at: Option<String>,
    pub version: Version,
}

/// `content`は、実行記録ならIDを[`UID_MARKER`]の形に置き換えたJSONの文字列。
///
/// 試行の番号は端末の中の値なので運ばず、試行の最初の行の`uid`(`attempt`)で試行を指す。
/// 受け取る側が自分の番号を振り直す(`docs/sync/index.md`「会話の枝と、競合の単位」「試行の番号」)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MessageRow {
    pub uid: String,
    pub task: Option<String>,
    pub role: String,
    pub kind: String,
    pub content: String,
    pub source: Option<String>,
    pub error_kind: Option<String>,
    pub error_detail: Option<String>,
    pub parts: Option<Vec<Part>>,
    pub turn_id: Option<String>,
    pub attempt: Option<String>,
    pub deleted_at: Option<String>,
    pub created_at: String,
    pub prev_uid: Option<String>,
    pub version: Version,
}

impl MessageRow {
    /// 消えていない通常発言(会話の鎖に入る行。`docs/sync/index.md`「会話の枝と、競合の単位」)。
    pub(crate) fn is_alive_normal(&self) -> bool {
        self.kind == "normal" && self.deleted_at.is_none()
    }
}

/// 返信の行の中身の1要素(`db::messages::ReplyPart`)。ツールは実行記録を`uid`で指す。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Part {
    Reasoning { round: u32, text: String },
    Text { round: u32, text: String },
    Tool { round: u32, record: String },
    Truncated { round: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AttachmentRow {
    pub uid: String,
    pub message: String,
    pub original_name: String,
    pub mime_type: String,
    pub kind: String,
    pub size_bytes: i64,
    pub content_text: Option<String>,
    pub file_hash: Option<String>,
    pub created_at: String,
}

/// 実行記録の中でIDを置き換えた値の形(`{"$uid": "<uid>"}`)。置き換えなかった数と見分ける。
pub(crate) const UID_MARKER: &str = "$uid";

/// 運ぶ行。各表とも送り側の`id`の順(会話の並びはこの順で書く)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Transport {
    pub tasks: Vec<TaskRow>,
    pub steps: Vec<StepRow>,
    pub memories: Vec<MemoryRow>,
    pub messages: Vec<MessageRow>,
    pub attachments: Vec<AttachmentRow>,
}

/// この端末で書いた(足した・変えた)行の数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct Counts {
    /// タスクの行か工程を書いたタスクの数。
    pub tasks: usize,
    /// 書いた会話の数(タスクの会話と総合チャット)。
    pub conversations: usize,
    pub steps: usize,
    pub memories: usize,
    pub attachments: usize,
    /// 受け取った添付の実体(画像・その他)の数。既にあったものは数えない。
    pub files: usize,
}

impl Transport {
    /// すべての行の`uid`(表を問わない)。
    pub(crate) fn uids(&self) -> Vec<String> {
        self.tasks
            .iter()
            .map(|r| r.uid.clone())
            .chain(self.steps.iter().map(|r| r.uid.clone()))
            .chain(self.memories.iter().map(|r| r.uid.clone()))
            .chain(self.messages.iter().map(|r| r.uid.clone()))
            .chain(self.attachments.iter().map(|r| r.uid.clone()))
            .collect()
    }
}

/// 相手を確かめたあとのメッセージ。流れは`docs/sync/index.md`「同期1回の流れ」(つなぐ側がB、待ち受ける側がA)。
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Message {
    /// 端末ID。相手を確かめる前には出さない。
    Intro {
        device_id: String,
    },
    /// カーソルの突き合わせに要るもの(「差分」)。`cursor`は自分が相手について持つカーソル、
    /// `seen_of_you`は自分が知る相手の番号の最大(カーソルと版から)。
    State {
        counter: i64,
        sent_seq: i64,
        received_seq: i64,
        seen_of_you: i64,
    },
    Tasks {
        rows: Vec<TaskRow>,
    },
    Steps {
        rows: Vec<StepRow>,
    },
    Memories {
        rows: Vec<MemoryRow>,
    },
    Messages {
        rows: Vec<MessageRow>,
    },
    Attachments {
        rows: Vec<AttachmentRow>,
    },
    /// Bの差分の終わり。
    RowsEnd,
    /// AがBに、単位の行をすべて求める(両方が変えた単位。「版と結合」)。
    NeedUnits {
        tasks: Vec<String>,
        general: bool,
    },
    UnitsEnd,
    /// 相手に求める添付の実体。
    NeedFiles {
        hashes: Vec<String>,
    },
    /// 実体の一部(base64)。`last`で1つの実体が終わる。
    File {
        hash: String,
        data: String,
        last: bool,
    },
    FilesEnd,
    /// Aが、競合を利用者に選ばせている。
    Choosing,
    /// Aが、書き込む前の確認を見せている(その間、Bは待って出る)。
    Confirming,
    /// Aの結果の終わり。`counter`はAが書き終えた時点のカウンター、`digest`は照合の値。
    Merged {
        counter: i64,
        digest: String,
        device_id: String,
    },
    /// Bが書き終えた。`counter`はBが書き終えた時点のカウンター。
    Done {
        counter: i64,
        device_id: String,
    },
    /// やめる理由を相手に伝える。
    Abort {
        reason: AbortReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum AbortReason {
    /// 運ぶ行が、本体が書く行の形に合わない。
    #[error("some rows cannot be synced")]
    InvalidRows,
    /// 同期の途中で、相手のデータが別の書き込みで変わった。
    #[error("its data changed during the sync")]
    Changed,
    /// 書き終えた結果が、相手の結果と一致しない。
    #[error("the results did not match")]
    Mismatch,
    /// 利用者がやめた。
    #[error("the user stopped the sync")]
    Cancelled,
    /// それ以外の失敗。
    #[error("it failed")]
    Failed,
}
