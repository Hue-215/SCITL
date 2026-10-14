//! 運ぶ形(`docs/sync/index.md` 6節)。端末の中の連番を持たず、行どうしは`uid`で指す。相手を
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
    pub attempt_no: Option<i64>,
    pub deleted_at: Option<String>,
    pub created_at: String,
    pub prev_uid: Option<String>,
    pub version: Version,
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

/// 運ぶ行のすべて。各表とも送り側の`id`の順(会話の並びはこの順で書く)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Transport {
    pub tasks: Vec<TaskRow>,
    pub steps: Vec<StepRow>,
    pub memories: Vec<MemoryRow>,
    pub messages: Vec<MessageRow>,
    pub attachments: Vec<AttachmentRow>,
}

/// 写した行の数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub tasks: usize,
    pub steps: usize,
    pub memories: usize,
    pub messages: usize,
    pub attachments: usize,
    /// 運んだ添付の実体(画像・その他)の数。受け取る側に既にあったものは数えない。
    pub files: usize,
}

impl Transport {
    pub(crate) fn counts(&self) -> Counts {
        Counts {
            tasks: self.tasks.len(),
            steps: self.steps.len(),
            memories: self.memories.len(),
            messages: self.messages.len(),
            attachments: self.attachments.len(),
            files: 0,
        }
    }
}

/// 相手を確かめたあとのメッセージ。
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Message {
    /// 端末IDと、写すデータがあるか。相手を確かめる前には出さない。
    Intro {
        device_id: String,
        has_data: bool,
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
    /// 行の終わり。`counter`は写す側が行を読んだ時点のカウンター(受け取る側のカーソルに書く)。
    RowsEnd {
        counter: i64,
    },
    /// 受け取る側に無い添付の実体。
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
    /// 取り込みを終えた。`counter`は受け取った側の、書き終えた時点のカウンター。
    Done {
        counter: i64,
    },
    /// やめる理由を相手に伝える。
    Abort {
        reason: AbortReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbortReason {
    /// 運ぶ行が、本体が書く行の形に合わない。
    InvalidRows,
    /// 受け取る側に、途中でデータができた。
    NotEmpty,
    /// それ以外の失敗。
    Failed,
}
