//! 運ぶ行が、本体が書く行と同じ形かを確かめる(`docs/sync/index.md` 5節)。送る側は送る前に、
//! 受け取る側は書く前に、同じ規則で確かめる。外れた行は直さず、すべて集めて返す。DBの制約と
//! トリガーで守られるもの(列の組み合わせ等)は、書くときにそのまま掛かるのでここでは見ない。
//! 添付の実体と行の突き合わせは`files`。
//!
//! 運ぶのは差分なので、行が指す先は、運ぶ行の中か、確かめる側に既にある行([`Known`])にある。

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use scitl_core::attachments::LIMITS;
use scitl_core::db::sync::{rules, Tx};
use scitl_core::error::Result as CoreResult;
use scitl_core::tools::IdTable;

use crate::convert::{marker_uid, positions_to_convert, visit, Ids};
use crate::files::MAX_FILE_BYTES;
use crate::transport::{MessageRow, Part, Transport, Version};
use crate::InvalidRow;

/// 確かめる側に既にある行。発言は、指す先として確かめるのに要る列だけ。
#[derive(Default)]
pub(crate) struct Known {
    tasks: HashSet<String>,
    steps: HashSet<String>,
    memories: HashSet<String>,
    attachments: HashSet<String>,
    messages: HashMap<String, KnownMessage>,
}

struct KnownMessage {
    task: Option<String>,
    kind: String,
    turn_id: Option<String>,
    attempt_no: Option<i64>,
}

impl Known {
    pub(crate) fn load(tx: &Tx<'_>) -> CoreResult<Self> {
        let uids = tx.uids()?;
        let ids = Ids::<i64, String>::from_uids(&uids);
        let values = |pairs: &[(i64, String)]| pairs.iter().map(|(_, u)| u.clone()).collect();
        let messages = tx
            .message_refs()?
            .into_iter()
            .map(|m| {
                let task = m.task_id.and_then(|id| ids.tasks.get(&id).cloned());
                (
                    m.uid,
                    KnownMessage {
                        task,
                        kind: m.kind,
                        turn_id: m.turn_id,
                        attempt_no: m.attempt_no,
                    },
                )
            })
            .collect();
        Ok(Self {
            tasks: values(&uids.tasks),
            steps: values(&uids.steps),
            memories: values(&uids.memories),
            attachments: values(&uids.attachments),
            messages,
        })
    }
}

/// 確かめる行の中と、既にある行([`Known`])の`uid`。
struct IdSets<'a> {
    tasks: HashSet<&'a str>,
    steps: HashSet<&'a str>,
    memories: HashSet<&'a str>,
    attachments: HashSet<&'a str>,
    known: &'a Known,
}

impl IdSets<'_> {
    fn has(&self, table: IdTable, uid: &str) -> bool {
        match table {
            IdTable::Task => self.tasks.contains(uid) || self.known.tasks.contains(uid),
            IdTable::Step => self.steps.contains(uid) || self.known.steps.contains(uid),
            IdTable::Memory => self.memories.contains(uid) || self.known.memories.contains(uid),
            IdTable::Attachment => {
                self.attachments.contains(uid) || self.known.attachments.contains(uid)
            }
        }
    }
}

pub(crate) fn check(t: &Transport, known: &Known) -> Result<(), Vec<InvalidRow>> {
    let mut invalid = Vec::new();
    let mut fail = |table: &'static str, uid: &str, reason: String| {
        invalid.push(InvalidRow {
            table,
            id: None,
            uid: uid.to_string(),
            reason,
        });
    };

    let ids = IdSets {
        tasks: uids(t.tasks.iter().map(|r| r.uid.as_str()), "tasks", &mut fail),
        steps: uids(
            t.steps.iter().map(|r| r.uid.as_str()),
            "task_steps",
            &mut fail,
        ),
        memories: uids(
            t.memories.iter().map(|r| r.uid.as_str()),
            "memories",
            &mut fail,
        ),
        attachments: uids(
            t.attachments.iter().map(|r| r.uid.as_str()),
            "attachments",
            &mut fail,
        ),
        known,
    };
    uids(
        t.messages.iter().map(|r| r.uid.as_str()),
        "messages",
        &mut fail,
    );
    // 発言の`uid` → 運ぶ順の位置と行。指す先が同じ会話の、前に書く行かを見る。
    let messages: HashMap<&str, (usize, &MessageRow)> = t
        .messages
        .iter()
        .enumerate()
        .map(|(i, r)| (r.uid.as_str(), (i, r)))
        .collect();

    for r in &t.tasks {
        let result = (|| {
            if let Some(title) = &r.title {
                rules::title(title).map_err(|e| e.to_string())?;
            }
            if let Some(description) = &r.description {
                rules::task_description(description).map_err(|e| e.to_string())?;
            }
            if let Some(deadline) = &r.deadline {
                rules::deadline(deadline).map_err(|e| e.to_string())?;
            }
            timestamps([
                Some(&r.created_at),
                Some(&r.updated_at),
                r.archived_at.as_ref(),
                r.deleted_at.as_ref(),
            ])?;
            version(&r.version)
        })();
        if let Err(e) = result {
            fail("tasks", &r.uid, e);
        }
    }
    for r in &t.steps {
        let result = (|| {
            if !ids.has(IdTable::Task, &r.task) {
                return Err("points at a missing task".to_string());
            }
            rules::step_description(&r.description).map_err(|e| e.to_string())?;
            timestamps([
                Some(&r.created_at),
                r.done_at.as_ref(),
                r.deleted_at.as_ref(),
            ])?;
            version(&r.version)
        })();
        if let Err(e) = result {
            fail("task_steps", &r.uid, e);
        }
    }
    for r in &t.memories {
        let result = (|| {
            rules::memory_content(&r.content).map_err(|e| e.to_string())?;
            timestamps([
                Some(&r.created_at),
                Some(&r.updated_at),
                r.deleted_at.as_ref(),
            ])?;
            version(&r.version)
        })();
        if let Err(e) = result {
            fail("memories", &r.uid, e);
        }
    }
    for (position, r) in t.messages.iter().enumerate() {
        if let Err(e) = message(r, position, &messages, &ids) {
            fail("messages", &r.uid, e);
        }
    }
    let mut file_sizes: HashMap<&str, i64> = HashMap::new();
    for r in &t.attachments {
        let result = (|| {
            // 添付は発言と同じトランザクションで書くので、運ぶ添付の発言も運ぶ行の中にある。既にある
            // 発言に添付を足させない(過去のターンの入力を書き換えさせない)。
            match messages.get(r.message.as_str()) {
                Some((_, m)) if m.role == "user" && m.kind == "normal" => {}
                _ => return Err("does not belong to a user message sent with it".to_string()),
            }
            match (r.kind.as_str(), &r.content_text, &r.file_hash) {
                ("text", Some(text), None) => {
                    if usize::try_from(r.size_bytes) != Ok(text.len()) {
                        return Err("the size does not match the text".to_string());
                    }
                    if text.len() as u64 > LIMITS.text_bytes {
                        return Err("the text is larger than an attachment can be".to_string());
                    }
                }
                ("image" | "other", None, Some(hash)) => {
                    if !is_hex(hash, 64) {
                        return Err("malformed file hash".to_string());
                    }
                    if !(0..=MAX_FILE_BYTES as i64).contains(&r.size_bytes) {
                        return Err("the file is too large to sync".to_string());
                    }
                    if *file_sizes.entry(hash).or_insert(r.size_bytes) != r.size_bytes {
                        return Err(
                            "the size differs from another attachment of the same file".to_string()
                        );
                    }
                }
                _ => return Err("kind does not match the content".to_string()),
            }
            timestamps([Some(&r.created_at)])
        })();
        if let Err(e) = result {
            fail("attachments", &r.uid, e);
        }
    }
    if invalid.is_empty() {
        Ok(())
    } else {
        Err(invalid)
    }
}

/// 指す先の発言。運ぶ行の中なら、運ぶ順で前の行に限る(1台の中では、指す先が先に書かれる)。
struct Target<'a> {
    task: Option<&'a str>,
    kind: &'a str,
    turn_id: Option<&'a str>,
    /// 運ぶ行なら試行の最初の行の`uid`、既にある行ならこの端末での試行の番号。
    attempt: Attempt<'a>,
}

#[derive(PartialEq, Eq)]
enum Attempt<'a> {
    Key(Option<&'a str>),
    No(Option<i64>),
}

fn message(
    r: &MessageRow,
    position: usize,
    messages: &HashMap<&str, (usize, &MessageRow)>,
    ids: &IdSets<'_>,
) -> Result<(), String> {
    if let Some(task) = &r.task {
        if !ids.has(IdTable::Task, task) {
            return Err("points at a missing task".to_string());
        }
    }
    let target = |uid: &str| -> Option<Target<'_>> {
        if let Some((i, m)) = messages.get(uid) {
            return (*i < position).then_some(Target {
                task: m.task.as_deref(),
                kind: m.kind.as_str(),
                turn_id: m.turn_id.as_deref(),
                attempt: Attempt::Key(m.attempt.as_deref()),
            });
        }
        ids.known.messages.get(uid).map(|m| Target {
            task: m.task.as_deref(),
            kind: m.kind.as_str(),
            turn_id: m.turn_id.as_deref(),
            attempt: Attempt::No(m.attempt_no),
        })
    };
    let in_chat = |uid: &str| target(uid).filter(|m| m.task == r.task.as_deref());

    // 試行は、同じターンの行を指す(運ぶ行の中で前か自身、または既にある行)。
    let attempt = match (&r.turn_id, &r.attempt) {
        (None, None) => None,
        (Some(turn_id), Some(key)) => {
            let same_turn = if key == &r.uid {
                true
            } else {
                in_chat(key).is_some_and(|m| m.turn_id == Some(turn_id.as_str()))
            };
            if !same_turn {
                return Err("the attempt does not point at a row of the same turn".to_string());
            }
            Some(key.as_str())
        }
        _ => return Err("malformed turn".to_string()),
    };
    if let Some(prev) = &r.prev_uid {
        if !in_chat(prev).is_some_and(|m| m.kind == "normal") {
            return Err(
                "prev_uid does not point at an earlier message of the same chat".to_string(),
            );
        }
    }
    for part in r.parts.iter().flatten() {
        if let Part::Tool { record, .. } = part {
            let same_attempt = in_chat(record).is_some_and(|m| {
                m.kind == "tool_execution"
                    && m.turn_id == r.turn_id.as_deref()
                    && match m.attempt {
                        Attempt::Key(key) => key == attempt,
                        // 既にある記録は、試行の最初の行も既にあり、同じ番号を持つときだけ同じ試行。
                        Attempt::No(no) => {
                            attempt.and_then(|key| ids.known.messages.get(key)?.attempt_no) == no
                        }
                    }
            });
            if !same_attempt {
                return Err("reply parts point at a missing tool record".to_string());
            }
        }
    }
    if let Some(source) = &r.source {
        if !matches!(source.as_str(), "ui" | "cli" | "sync") {
            return Err("unknown source".to_string());
        }
    }
    match (r.role.as_str(), r.kind.as_str()) {
        (_, "tool_execution") => record(&r.content, ids)?,
        ("user", _) => rules::user_message_content(&r.content).map_err(|e| e.to_string())?,
        // ターンの返信の本文は`parts`にだけ持つ。
        ("assistant", _) if !r.content.is_empty() => {
            return Err("a reply keeps its text only in its parts".to_string())
        }
        _ => {}
    }
    if r.turn_id.as_deref() == Some("") {
        return Err("malformed turn".to_string());
    }
    timestamps([Some(&r.created_at), r.deleted_at.as_ref()])?;
    version(&r.version)
}

/// `uid`の形と、表の中で重ならないこと。
fn uids<'a>(
    rows: impl Iterator<Item = &'a str>,
    table: &'static str,
    fail: &mut impl FnMut(&'static str, &str, String),
) -> HashSet<&'a str> {
    let mut seen = HashSet::new();
    for uid in rows {
        if !is_hex(uid, 32) {
            // 相手から届いた値なので、長さを切って示す。
            let shown: String = uid.chars().take(32).collect();
            fail(table, &shown, "malformed uid".to_string());
        } else if !seen.insert(uid) {
            fail(table, uid, "duplicate uid".to_string());
        }
    }
    seen
}

/// 実行記録の`content`。宣言した位置には、運ぶ行か既にある行の`uid`に置き換えた値がある。
fn record(content: &str, ids: &IdSets<'_>) -> Result<(), String> {
    let mut record: Value =
        serde_json::from_str(content).map_err(|e| format!("unreadable tool record: {e}"))?;
    let Some(positions) = positions_to_convert(&record)? else {
        return Ok(());
    };
    for (key, positions) in [
        ("arguments", positions.arguments),
        ("result", positions.result),
    ] {
        let Some(value) = record.get_mut(key) else {
            continue;
        };
        for position in positions {
            visit(value, position.path, &mut |leaf| match marker_uid(leaf) {
                Some(uid) if ids.has(position.table, uid) => Ok(()),
                Some(_) => Err(format!("{key} points at a missing row")),
                None => Err(format!("{key} has an ID that was not replaced")),
            })?;
        }
    }
    Ok(())
}

fn timestamps<const N: usize>(values: [Option<&String>; N]) -> Result<(), String> {
    values
        .into_iter()
        .flatten()
        .try_for_each(|v| rules::timestamp(v).map_err(|e| e.to_string()))
}

fn version(version: &Version) -> Result<(), String> {
    if version
        .iter()
        .all(|(device, n)| is_hex(device, 32) && *n >= 1)
    {
        Ok(())
    } else {
        Err("malformed version".to_string())
    }
}

pub(crate) fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}
