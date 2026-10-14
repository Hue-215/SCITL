//! 運ぶ行が、本体が書く行と同じ形かを確かめる(`docs/sync/index.md` 5節)。送る側は送る前に、
//! 受け取る側は書く前に、同じ規則で確かめる。外れた行は直さず、すべて集めて返す。DBの制約と
//! トリガーで守られるもの(列の組み合わせ等)は、書くときにそのまま掛かるのでここでは見ない。

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use scitl_core::db::sync::rules;

use crate::convert::{marker_uid, positions_to_convert, visit};
use crate::transport::{Part, Transport, Version};
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
    let messages_uids = uids(
        t.messages.iter().map(|r| r.uid.as_str()),
        "messages",
        &mut fail,
    );
    let attachments = uids(
        t.attachments.iter().map(|r| r.uid.as_str()),
        "attachments",
        &mut fail,
    );
    // 発言の`uid` → (会話, 実行記録か)。指す先が同じ会話にあるかを見る。
    let messages: HashMap<&str, (Option<&str>, bool)> = t
        .messages
        .iter()
        .map(|r| {
            (
                r.uid.as_str(),
                (r.task.as_deref(), r.kind == "tool_execution"),
            )
        })
        .collect();
    debug_assert_eq!(messages.len(), messages_uids.len());

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
    for r in &t.messages {
        let result = (|| {
            if let Some(task) = &r.task {
                if !tasks.contains(task.as_str()) {
                    return Err("points at a missing task".to_string());
                }
            }
            let same_chat = |uid: &str| {
                messages
                    .get(uid)
                    .map(|(chat, record)| (*chat == r.task.as_deref(), *record))
            };
            if let Some(prev) = &r.prev_uid {
                match same_chat(prev) {
                    Some((true, false)) => {}
                    _ => {
                        return Err(
                            "prev_uid does not point at a message of the same chat".to_string()
                        )
                    }
                }
            }
            for part in r.parts.iter().flatten() {
                if let Part::Tool { record, .. } = part {
                    if same_chat(record) != Some((true, true)) {
                        return Err("reply parts point at a missing tool record".to_string());
                    }
                }
            }
            if r.kind == "tool_execution" {
                record(&r.content, &ids)?;
            } else if r.role == "user" {
                rules::user_message_content(&r.content).map_err(|e| e.to_string())?;
            }
            if r.attempt_no.is_some_and(|n| n < 1) || r.turn_id.as_deref() == Some("") {
                return Err("malformed turn".to_string());
            }
            timestamps([Some(&r.created_at), r.deleted_at.as_ref()])?;
            version(&r.version)
        })();
        if let Err(e) = result {
            fail("messages", &r.uid, e);
        }
    }
    for r in &t.attachments {
        let result = (|| {
            if !messages.contains_key(r.message.as_str()) {
                return Err("points at a missing message".to_string());
            }
            match (r.kind.as_str(), &r.content_text, &r.file_hash) {
                ("text", Some(text), None) => {
                    if usize::try_from(r.size_bytes) != Ok(text.len()) {
                        return Err("size does not match the text".to_string());
                    }
                }
                ("image" | "other", None, Some(hash)) => {
                    if !is_sha256(hash) {
                        return Err("malformed file hash".to_string());
                    }
                    if r.size_bytes < 0 {
                        return Err("negative size".to_string());
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
            fail(table, uid, "malformed uid".to_string());
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
                scitl_core::tools::IdTable::Task => ids.tasks,
                scitl_core::tools::IdTable::Step => ids.steps,
                scitl_core::tools::IdTable::Memory => ids.memories,
                scitl_core::tools::IdTable::Attachment => ids.attachments,
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

pub(crate) fn is_sha256(hash: &str) -> bool {
    is_hex(hash, 64)
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}
