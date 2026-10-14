//! 行の変換(`docs/sync/index.md` 6節)。送るときに端末の中の連番を`uid`へ、受け取るときに自分の
//! 連番へ置き換える。置き換えるのは外部キー、返信の中身が指す実行記録、実行記録と操作の記録の
//! `content`に載るID。置き換えはここに閉じ、ほかのコードは連番だけを見る。

use std::collections::HashMap;

use serde_json::{json, Value};

use scitl_core::db::messages::ReplyPart;
use scitl_core::db::sync::{self as db_sync, Importer};
use scitl_core::error::Result as CoreResult;
use scitl_core::orchestration::{is_failed, record_ids, RecordIds};
use scitl_core::tools::{IdPosition, IdTable, Segment};
use scitl_core::CoreError;

use crate::transport::{self, Part, Transport, Version, UID_MARKER};
use crate::InvalidRow;

/// 表ごとの連番 → `uid`(送る側)、または`uid` → 連番(受け取る側)。
struct Ids<K, V> {
    tasks: HashMap<K, V>,
    steps: HashMap<K, V>,
    memories: HashMap<K, V>,
    messages: HashMap<K, V>,
    attachments: HashMap<K, V>,
}

impl<K, V> Default for Ids<K, V> {
    fn default() -> Self {
        Self {
            tasks: HashMap::new(),
            steps: HashMap::new(),
            memories: HashMap::new(),
            messages: HashMap::new(),
            attachments: HashMap::new(),
        }
    }
}

impl<K: std::hash::Hash + Eq, V> Ids<K, V> {
    fn table(&self, table: IdTable) -> &HashMap<K, V> {
        match table {
            IdTable::Task => &self.tasks,
            IdTable::Step => &self.steps,
            IdTable::Memory => &self.memories,
            IdTable::Attachment => &self.attachments,
        }
    }
}

/// 送る写しを作る。置き換えられない行(宣言した位置のIDを引けない成功の記録、読めない版など)は
/// すべて集めて返し、写しには元の値のまま入れる(続く検証で、ほかの外れた行もまとめて示すため)。
pub(crate) fn to_transport(rows: &db_sync::Rows) -> (Transport, Vec<InvalidRow>) {
    let ids: Ids<i64, String> = Ids {
        tasks: rows.tasks.iter().map(|r| (r.id, r.uid.clone())).collect(),
        steps: rows.steps.iter().map(|r| (r.id, r.uid.clone())).collect(),
        memories: rows
            .memories
            .iter()
            .map(|r| (r.id, r.uid.clone()))
            .collect(),
        messages: rows
            .messages
            .iter()
            .map(|r| (r.id, r.uid.clone()))
            .collect(),
        attachments: rows
            .attachments
            .iter()
            .map(|r| (r.id, r.uid.clone()))
            .collect(),
    };

    let mut invalid = Vec::new();
    // 外部キーは制約があるので、引けないのは壊れたDBだけ(検証で、指す先が無い行として示す)。
    let lookup = |map: &HashMap<i64, String>, id: i64| map.get(&id).cloned().unwrap_or_default();

    let mut out = Transport::default();
    for r in &rows.tasks {
        let version = parse_version(&r.version).unwrap_or_else(|e| {
            fail(&mut invalid, "tasks", r.id, &r.uid, e);
            Version::default()
        });
        out.tasks.push(transport::TaskRow {
            uid: r.uid.clone(),
            title: r.title.clone(),
            description: r.description.clone(),
            deadline: r.deadline.clone(),
            archived_at: r.archived_at.clone(),
            deleted_at: r.deleted_at.clone(),
            created_at: r.created_at.clone(),
            updated_at: r.updated_at.clone(),
            version,
        });
    }
    for r in &rows.steps {
        let version = parse_version(&r.version).unwrap_or_else(|e| {
            fail(&mut invalid, "task_steps", r.id, &r.uid, e);
            Version::default()
        });
        out.steps.push(transport::StepRow {
            uid: r.uid.clone(),
            task: lookup(&ids.tasks, r.task_id),
            description: r.description.clone(),
            done_at: r.done_at.clone(),
            deleted_at: r.deleted_at.clone(),
            order_index: r.order_index,
            created_at: r.created_at.clone(),
            version,
        });
    }
    for r in &rows.memories {
        let version = parse_version(&r.version).unwrap_or_else(|e| {
            fail(&mut invalid, "memories", r.id, &r.uid, e);
            Version::default()
        });
        out.memories.push(transport::MemoryRow {
            uid: r.uid.clone(),
            content: r.content.clone(),
            created_at: r.created_at.clone(),
            updated_at: r.updated_at.clone(),
            deleted_at: r.deleted_at.clone(),
            version,
        });
    }
    for r in &rows.messages {
        let mut reasons = Vec::new();
        let parts = r.parts.as_deref().map(|parts| {
            parts_to_transport(parts, &ids.messages).unwrap_or_else(|e| {
                reasons.push(e);
                Vec::new()
            })
        });
        let content = if r.kind == "tool_execution" {
            record_to_transport(&r.content, &ids).unwrap_or_else(|e| {
                reasons.push(e);
                r.content.clone()
            })
        } else {
            r.content.clone()
        };
        let version = parse_version(&r.version).unwrap_or_else(|e| {
            reasons.push(e);
            Version::default()
        });
        if !reasons.is_empty() {
            fail(&mut invalid, "messages", r.id, &r.uid, reasons.join("; "));
        }
        out.messages.push(transport::MessageRow {
            uid: r.uid.clone(),
            task: r.task_id.map(|id| lookup(&ids.tasks, id)),
            role: r.role.clone(),
            kind: r.kind.clone(),
            content,
            source: r.source.clone(),
            error_kind: r.error_kind.clone(),
            error_detail: r.error_detail.clone(),
            parts,
            turn_id: r.turn_id.clone(),
            attempt_no: r.attempt_no,
            deleted_at: r.deleted_at.clone(),
            created_at: r.created_at.clone(),
            prev_uid: r.prev_uid.clone(),
            version,
        });
    }
    for r in &rows.attachments {
        out.attachments.push(transport::AttachmentRow {
            uid: r.uid.clone(),
            message: lookup(&ids.messages, r.message_id),
            original_name: r.original_name.clone(),
            mime_type: r.mime_type.clone(),
            kind: r.kind.clone(),
            size_bytes: r.size_bytes,
            content_text: r.content_text.clone(),
            file_hash: r.file_hash.clone(),
            created_at: r.created_at.clone(),
        });
    }
    (out, invalid)
}

fn fail(invalid: &mut Vec<InvalidRow>, table: &'static str, id: i64, uid: &str, reason: String) {
    invalid.push(InvalidRow {
        table,
        id: Some(id),
        uid: uid.to_string(),
        reason,
    });
}

fn parse_version(json: &str) -> Result<Version, String> {
    serde_json::from_str(json).map_err(|e| format!("unreadable version: {e}"))
}

fn parts_to_transport(json: &str, messages: &HashMap<i64, String>) -> Result<Vec<Part>, String> {
    let parts: Vec<ReplyPart> =
        serde_json::from_str(json).map_err(|e| format!("unreadable reply parts: {e}"))?;
    parts
        .into_iter()
        .map(|part| {
            Ok(match part {
                ReplyPart::Reasoning { round, text } => Part::Reasoning { round, text },
                ReplyPart::Text { round, text } => Part::Text { round, text },
                ReplyPart::Truncated { round } => Part::Truncated { round },
                ReplyPart::Tool { round, record } => Part::Tool {
                    round,
                    record: messages
                        .get(&record)
                        .cloned()
                        .ok_or_else(|| format!("reply parts point at missing record {record}"))?,
                },
            })
        })
        .collect()
}

/// 実行記録の`content`のうち、宣言した位置のIDを`uid`に置き換える。失敗の記録と外部ツールの
/// 記録はそのまま。
fn record_to_transport(content: &str, ids: &Ids<i64, String>) -> Result<String, String> {
    let mut record: Value =
        serde_json::from_str(content).map_err(|e| format!("unreadable tool record: {e}"))?;
    let Some(positions) = positions_to_convert(&record)? else {
        return Ok(content.to_string());
    };
    for (key, positions) in [
        ("arguments", positions.arguments),
        ("result", positions.result),
    ] {
        let Some(value) = record.get_mut(key) else {
            continue;
        };
        for position in positions {
            visit(value, position.path, &mut |leaf| {
                let id = leaf
                    .as_i64()
                    .ok_or_else(|| format!("{key} has a non-integer ID at {:?}", position.path))?;
                let uid = ids
                    .table(position.table)
                    .get(&id)
                    .ok_or_else(|| format!("{key} points at a missing row {id}"))?;
                *leaf = json!({ UID_MARKER: uid });
                Ok(())
            })?;
        }
    }
    Ok(record.to_string())
}

/// 置き換える記録なら、その宣言。失敗の記録と外部ツールは`None`。
pub(crate) fn positions_to_convert(
    record: &Value,
) -> Result<Option<&'static scitl_core::tools::IdPositions>, String> {
    let tool = record
        .get("tool")
        .and_then(Value::as_str)
        .ok_or("tool record has no tool name")?;
    if record.get("result").is_some_and(is_failed) {
        return Ok(None);
    }
    match record_ids(tool) {
        RecordIds::Declared(positions) => Ok(Some(positions)),
        RecordIds::External => Ok(None),
        RecordIds::Unknown => Err(format!("tool {tool} has no declaration of its IDs")),
    }
}

/// 宣言した位置の値すべてに`f`を掛ける。途中の段が無いのは、その欄を持たない記録(省ける
/// 引数など)なので飛ばす。段の型が違うのは、宣言に合わない記録。
pub(crate) fn visit(
    value: &mut Value,
    path: &[Segment],
    f: &mut dyn FnMut(&mut Value) -> Result<(), String>,
) -> Result<(), String> {
    match path.split_first() {
        None => f(value),
        Some((Segment::Key(key), rest)) => match value {
            Value::Object(map) => match map.get_mut(*key) {
                Some(child) => visit(child, rest, f),
                None => Ok(()),
            },
            _ => Err(format!(
                "tool record does not match its declaration at {key}"
            )),
        },
        Some((Segment::Each, rest)) => match value {
            Value::Array(items) => items.iter_mut().try_for_each(|item| visit(item, rest, f)),
            _ => Err("tool record does not match its declaration (expected a list)".to_string()),
        },
    }
}

/// 置き換えた値の`uid`。形が違えば`None`。
pub(crate) fn marker_uid(value: &Value) -> Option<&str> {
    let map = value.as_object()?;
    if map.len() != 1 {
        return None;
    }
    map.get(UID_MARKER)?.as_str()
}

/// 受け取った行を書く。行は検証済み(`validate::check`)なので、引けない`uid`は起きない。
/// 発言は先に仮の`content`・`parts`で書き、指す先(実行記録・添付)をすべて書いてから直す。
pub(crate) fn import(importer: &Importer<'_>, t: &Transport) -> CoreResult<()> {
    let missing = |what: &str, uid: &str| CoreError::Internal(format!("unresolved {what} {uid}"));
    let mut ids: Ids<String, i64> = Ids::default();

    for r in &t.tasks {
        let id = importer.insert_task(&db_sync::TaskRow {
            id: 0,
            uid: r.uid.clone(),
            title: r.title.clone(),
            description: r.description.clone(),
            deadline: r.deadline.clone(),
            archived_at: r.archived_at.clone(),
            deleted_at: r.deleted_at.clone(),
            created_at: r.created_at.clone(),
            updated_at: r.updated_at.clone(),
            version: version_json(&r.version),
        })?;
        ids.tasks.insert(r.uid.clone(), id);
    }
    for r in &t.steps {
        let task_id = *ids
            .tasks
            .get(&r.task)
            .ok_or_else(|| missing("task", &r.task))?;
        let id = importer.insert_step(&db_sync::StepRow {
            id: 0,
            uid: r.uid.clone(),
            task_id,
            description: r.description.clone(),
            done_at: r.done_at.clone(),
            deleted_at: r.deleted_at.clone(),
            order_index: r.order_index,
            created_at: r.created_at.clone(),
            version: version_json(&r.version),
        })?;
        ids.steps.insert(r.uid.clone(), id);
    }
    for r in &t.memories {
        let id = importer.insert_memory(&db_sync::MemoryRow {
            id: 0,
            uid: r.uid.clone(),
            content: r.content.clone(),
            created_at: r.created_at.clone(),
            updated_at: r.updated_at.clone(),
            deleted_at: r.deleted_at.clone(),
            version: version_json(&r.version),
        })?;
        ids.memories.insert(r.uid.clone(), id);
    }
    let mut pending = Vec::new();
    for r in &t.messages {
        let task_id = r
            .task
            .as_ref()
            .map(|uid| {
                ids.tasks
                    .get(uid)
                    .copied()
                    .ok_or_else(|| missing("task", uid))
            })
            .transpose()?;
        let is_record = r.kind == "tool_execution";
        let id = importer.insert_message(&db_sync::MessageRow {
            id: 0,
            uid: r.uid.clone(),
            task_id,
            role: r.role.clone(),
            content: if is_record {
                "{}".to_string()
            } else {
                r.content.clone()
            },
            kind: r.kind.clone(),
            source: r.source.clone(),
            error_kind: r.error_kind.clone(),
            error_detail: r.error_detail.clone(),
            parts: r.parts.as_ref().map(|_| "[]".to_string()),
            turn_id: r.turn_id.clone(),
            attempt_no: r.attempt_no,
            deleted_at: r.deleted_at.clone(),
            created_at: r.created_at.clone(),
            prev_uid: r.prev_uid.clone(),
            version: version_json(&r.version),
        })?;
        ids.messages.insert(r.uid.clone(), id);
        if is_record || r.parts.is_some() {
            pending.push((id, r));
        }
    }
    for r in &t.attachments {
        let message_id = *ids
            .messages
            .get(&r.message)
            .ok_or_else(|| missing("message", &r.message))?;
        let id = importer.insert_attachment(&db_sync::AttachmentRow {
            id: 0,
            uid: r.uid.clone(),
            message_id,
            original_name: r.original_name.clone(),
            mime_type: r.mime_type.clone(),
            kind: r.kind.clone(),
            size_bytes: r.size_bytes,
            content_text: r.content_text.clone(),
            file_hash: r.file_hash.clone(),
            created_at: r.created_at.clone(),
        })?;
        ids.attachments.insert(r.uid.clone(), id);
    }
    for (id, r) in pending {
        let content = if r.kind == "tool_execution" {
            record_from_transport(&r.content, &ids).map_err(CoreError::Internal)?
        } else {
            r.content.clone()
        };
        let parts = r
            .parts
            .as_ref()
            .map(|parts| parts_from_transport(parts, &ids.messages))
            .transpose()
            .map_err(CoreError::Internal)?;
        importer.set_message_body(id, &content, parts.as_deref())?;
    }
    Ok(())
}

fn version_json(version: &Version) -> String {
    serde_json::to_string(version).expect("a version serializes")
}

fn record_from_transport(content: &str, ids: &Ids<String, i64>) -> Result<String, String> {
    let mut record: Value = serde_json::from_str(content).map_err(|e| e.to_string())?;
    let Some(positions) = positions_to_convert(&record)? else {
        return Ok(content.to_string());
    };
    for (key, positions) in [
        ("arguments", positions.arguments),
        ("result", positions.result),
    ] {
        let Some(value) = record.get_mut(key) else {
            continue;
        };
        for position in positions {
            convert_back(value, position, ids)?;
        }
    }
    Ok(record.to_string())
}

fn convert_back(
    value: &mut Value,
    position: &IdPosition,
    ids: &Ids<String, i64>,
) -> Result<(), String> {
    visit(value, position.path, &mut |leaf| {
        let uid = marker_uid(leaf).ok_or("expected a replaced ID")?;
        let id = ids
            .table(position.table)
            .get(uid)
            .ok_or_else(|| format!("unresolved ID {uid}"))?;
        *leaf = json!(id);
        Ok(())
    })
}

fn parts_from_transport(parts: &[Part], messages: &HashMap<String, i64>) -> Result<String, String> {
    let parts: Vec<ReplyPart> = parts
        .iter()
        .map(|part| {
            Ok(match part {
                Part::Reasoning { round, text } => ReplyPart::Reasoning {
                    round: *round,
                    text: text.clone(),
                },
                Part::Text { round, text } => ReplyPart::Text {
                    round: *round,
                    text: text.clone(),
                },
                Part::Truncated { round } => ReplyPart::Truncated { round: *round },
                Part::Tool { round, record } => ReplyPart::Tool {
                    round: *round,
                    record: *messages
                        .get(record)
                        .ok_or_else(|| format!("unresolved record {record}"))?,
                },
            })
        })
        .collect::<Result<_, String>>()?;
    Ok(serde_json::to_string(&parts).expect("reply parts serialize"))
}
