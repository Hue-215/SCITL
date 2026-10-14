//! 運ぶ行が、本体が書く行と同じ形かを確かめる(`docs/sync/index.md` 5節)。送る側は送る前に、
//! 受け取る側は書く前に、同じ規則で確かめる。外れた行は直さず、すべて集めて返す。DBの制約と
//! トリガーで守られるもの(列の組み合わせ等)は、書くときにそのまま掛かるのでここでは見ない。
//! 添付の実体と行の突き合わせは`files`。

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use scitl_core::attachments::LIMITS;
use scitl_core::db::sync::rules;
use scitl_core::tools::IdTable;

use crate::convert::{marker_uid, positions_to_convert, visit};
use crate::files::MAX_FILE_BYTES;
use crate::transport::{MessageRow, Part, Transport, Version};
use crate::InvalidRow;

pub(crate) fn check(t: &Transport) -> Result<(), Vec<InvalidRow>> {
    let mut invalid = Vec::new();
    let mut fail = |table: &'static str, uid: &str, reason: String| {
        invalid.push(InvalidRow {
            table,
            id: None,
            uid: uid.to_string(),
            reason,
        });
    };

    let tasks = uids(t.tasks.iter().map(|r| r.uid.as_str()), "tasks", &mut fail);
    let steps = uids(
        t.steps.iter().map(|r| r.uid.as_str()),
        "task_steps",
        &mut fail,
    );
    let memories = uids(
        t.memories.iter().map(|r| r.uid.as_str()),
        "memories",
        &mut fail,
    );
    uids(
        t.messages.iter().map(|r| r.uid.as_str()),
        "messages",
        &mut fail,
    );
    let attachments = uids(
        t.attachments.iter().map(|r| r.uid.as_str()),
        "attachments",
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
            if !tasks.contains(r.task.as_str()) {
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
    let ids = IdSets {
        tasks: &tasks,
        steps: &steps,
        memories: &memories,
        attachments: &attachments,
    };
    for (position, r) in t.messages.iter().enumerate() {
        if let Err(e) = message(r, position, &messages, &ids) {
            fail("messages", &r.uid, e);
        }
    }
    let mut file_sizes: HashMap<&str, i64> = HashMap::new();
    for r in &t.attachments {
        let result = (|| {
            match messages.get(r.message.as_str()) {
                Some((_, m)) if m.role == "user" && m.kind == "normal" => {}
                _ => return Err("does not belong to a user message".to_string()),
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

fn message(
    r: &MessageRow,
    position: usize,
    messages: &HashMap<&str, (usize, &MessageRow)>,
    ids: &IdSets<'_>,
) -> Result<(), String> {
    if let Some(task) = &r.task {
        if !ids.tasks.contains(task.as_str()) {
            return Err("points at a missing task".to_string());
        }
    }
    // 指す先は同じ会話の、運ぶ順で前の行(1台の中では、指す先が先に書かれる)。
    let earlier_in_chat = |uid: &str| {
        messages
            .get(uid)
            .filter(|(i, m)| *i < position && m.task == r.task)
            .map(|(_, m)| *m)
    };
    if let Some(prev) = &r.prev_uid {
        if !earlier_in_chat(prev).is_some_and(|m| m.kind == "normal") {
            return Err(
                "prev_uid does not point at an earlier message of the same chat".to_string(),
            );
        }
    }
    for part in r.parts.iter().flatten() {
        if let Part::Tool { record, .. } = part {
            let same_attempt = earlier_in_chat(record).is_some_and(|m| {
                m.kind == "tool_execution" && m.turn_id == r.turn_id && m.attempt_no == r.attempt_no
            });
            if !same_attempt {
                return Err("reply parts point at a missing tool record".to_string());
            }
        }
    }
    if let Some(source) = &r.source {
        if !matches!(source.as_str(), "ui" | "cli") {
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
    if r.attempt_no.is_some_and(|n| n < 1) || r.turn_id.as_deref() == Some("") {
        return Err("malformed turn".to_string());
    }
    timestamps([Some(&r.created_at), r.deleted_at.as_ref()])?;
    version(&r.version)
}

struct IdSets<'a> {
    tasks: &'a HashSet<&'a str>,
    steps: &'a HashSet<&'a str>,
    memories: &'a HashSet<&'a str>,
    attachments: &'a HashSet<&'a str>,
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

/// 実行記録の`content`。宣言した位置には、運ぶ行の中の`uid`に置き換えた値がある。
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
            let set = match position.table {
                IdTable::Task => ids.tasks,
                IdTable::Step => ids.steps,
                IdTable::Memory => ids.memories,
                IdTable::Attachment => ids.attachments,
            };
            visit(value, position.path, &mut |leaf| match marker_uid(leaf) {
                Some(uid) if set.contains(uid) => Ok(()),
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

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}
