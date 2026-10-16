//! 行の変換(`docs/sync/index.md`「IDと行の変換」)。送るときに端末の中の連番を`uid`へ、受け取るときに自分の
//! 連番へ置き換える。置き換えるのは外部キー、返信の中身が指す実行記録、実行記録と操作の記録の
//! `content`に載るID。置き換えはここに閉じ、ほかのコードは連番だけを見る。
//!
//! 運ぶのは差分なので、行が指す先は運ぶ行の中にも、受け取る側に既にある行の中にもありうる。
//! どちらも`uid`で引く。

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use scitl_core::db::messages::ReplyPart;
use scitl_core::db::sync::{self as db_sync, MessageRef, Tx};
use scitl_core::error::Result as CoreResult;
use scitl_core::orchestration::{is_error_result, record_ids, RecordIds};
use scitl_core::tools::{IdPosition, IdTable, Segment};
use scitl_core::CoreError;

use crate::transport::{self, Counts, Part, Transport, Version, UID_MARKER};
use crate::InvalidRow;

/// 表ごとの連番 → `uid`(送る側)、または`uid` → 連番(受け取る側)。
pub(crate) struct Ids<K, V> {
    pub tasks: HashMap<K, V>,
    pub steps: HashMap<K, V>,
    pub memories: HashMap<K, V>,
    pub messages: HashMap<K, V>,
    pub attachments: HashMap<K, V>,
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

impl Ids<i64, String> {
    pub(crate) fn from_uids(uids: &db_sync::Uids) -> Self {
        let map = |pairs: &[(i64, String)]| pairs.iter().cloned().collect();
        Self {
            tasks: map(&uids.tasks),
            steps: map(&uids.steps),
            memories: map(&uids.memories),
            messages: map(&uids.messages),
            attachments: map(&uids.attachments),
        }
    }
}

impl Ids<String, i64> {
    fn inverse(ids: &Ids<i64, String>) -> Self {
        let flip = |map: &HashMap<i64, String>| map.iter().map(|(i, u)| (u.clone(), *i)).collect();
        Self {
            tasks: flip(&ids.tasks),
            steps: flip(&ids.steps),
            memories: flip(&ids.memories),
            messages: flip(&ids.messages),
            attachments: flip(&ids.attachments),
        }
    }
}

/// この端末のすべての行の連番 → `uid`と、発言の一部(指す先の確かめと試行の番号に使う)。どちらも
/// 表をすべて読むので、1つのトランザクションでは書き込みの前と後に1度ずつだけ読む。
pub(crate) struct Snapshot {
    pub(crate) ids: Ids<i64, String>,
    pub(crate) messages: Vec<MessageRef>,
}

impl Snapshot {
    pub(crate) fn load(tx: &Tx<'_>) -> CoreResult<Self> {
        Ok(Self {
            ids: Ids::<i64, String>::from_uids(&tx.uids()?),
            messages: tx.message_refs()?,
        })
    }
}

/// 運ぶ写しを作る。`ids`はこの端末のすべての行の連番 → `uid`。置き換えられない行(宣言した
/// 位置のIDを引けない成功の記録、読めない版など)はすべて集めて返し、写しには元の値のまま
/// 入れる(続く検証で、ほかの外れた行もまとめて示すため)。
pub(crate) fn to_transport(
    rows: &db_sync::Rows,
    ids: &Ids<i64, String>,
) -> (Transport, Vec<InvalidRow>) {
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
            record_to_transport(&r.content, ids).unwrap_or_else(|e| {
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
            attempt: r.attempt_key.clone(),
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

/// [`read_with`]の、`ids`をその場で読む形。
#[cfg(test)]
pub(crate) fn read(
    tx: &Tx<'_>,
    filter: db_sync::Filter<'_>,
) -> CoreResult<(Transport, Vec<InvalidRow>)> {
    read_with(tx, filter, &Ids::<i64, String>::from_uids(&tx.uids()?))
}

/// 自分の行を運ぶ形で読む。置き換えられない行は、検証で外れた行と同じく扱う。`ids`は同じ
/// トランザクションで読んだもの([`Snapshot`])。
pub(crate) fn read_with(
    tx: &Tx<'_>,
    filter: db_sync::Filter<'_>,
    ids: &Ids<i64, String>,
) -> CoreResult<(Transport, Vec<InvalidRow>)> {
    Ok(to_transport(&tx.rows(filter)?, ids))
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
    let record = map_declared_ids(content, &mut |key, position, leaf| {
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
    Ok(record.map_or_else(|| content.to_string(), |r| r.to_string()))
}

/// 実行記録の`content`の、宣言した位置の値すべてに`f`を掛け、掛けたあとの記録を返す(`f`には、
/// `arguments`と`result`のどちらかと、位置の宣言も渡す)。失敗の記録と外部ツールの記録は触らず`None`。
pub(crate) fn map_declared_ids(
    content: &str,
    f: &mut dyn FnMut(&str, &IdPosition, &mut Value) -> Result<(), String>,
) -> Result<Option<Value>, String> {
    let mut record: Value =
        serde_json::from_str(content).map_err(|e| format!("unreadable tool record: {e}"))?;
    let Some(positions) = positions_to_convert(&record)? else {
        return Ok(None);
    };
    for (key, positions) in [
        ("arguments", positions.arguments),
        ("result", positions.result),
    ] {
        let Some(value) = record.get_mut(key) else {
            continue;
        };
        for position in positions {
            visit(value, position.path, &mut |leaf| f(key, position, leaf))?;
        }
    }
    Ok(Some(record))
}

/// 置き換える記録なら、その宣言。失敗の記録と外部ツールは`None`。
fn positions_to_convert(
    record: &Value,
) -> Result<Option<&'static scitl_core::tools::IdPositions>, String> {
    let tool = record
        .get("tool")
        .and_then(Value::as_str)
        .ok_or("tool record has no tool name")?;
    if record.get("result").is_some_and(is_error_result) {
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
fn visit(
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

/// 受け取った行を書く。行は検証済み(`validate::check`)なので、引けない`uid`は失敗にする
/// (起きないはずの不整合)。発言は先に仮の`content`・`parts`で書き、指す先(実行記録・添付)を
/// すべて書いてから[`Writer::finish`]で直す。
///
/// 試行の番号は振り直す(`docs/sync/index.md`「会話の枝と、競合の単位」)。試行の最初の行が既にあれば、その行の番号に
/// 合わせ、無ければ空いた番号を取る。
pub(crate) struct Writer<'t, 'c> {
    tx: &'t Tx<'c>,
    ids: Ids<String, i64>,
    /// 発言の`uid` → この端末での試行の番号(既にある行と、書いた行)。
    attempt_nos: HashMap<String, i64>,
    /// 試行(最初の行の`uid`) → この回に振った番号。
    attempts: HashMap<String, i64>,
    pending: Vec<(i64, transport::MessageRow)>,
    counts: Counts,
    touched_tasks: HashSet<String>,
}

impl<'t, 'c> Writer<'t, 'c> {
    /// `snapshot`は、書き始める前に同じトランザクションで読んだもの。
    pub(crate) fn new(tx: &'t Tx<'c>, snapshot: &Snapshot) -> Self {
        let attempt_nos = snapshot
            .messages
            .iter()
            .filter_map(|m| Some((m.uid.clone(), m.attempt_no?)))
            .collect();
        Self {
            tx,
            ids: Ids::inverse(&snapshot.ids),
            attempt_nos,
            attempts: HashMap::new(),
            pending: Vec::new(),
            counts: Counts::default(),
            touched_tasks: HashSet::new(),
        }
    }

    fn id(map: &HashMap<String, i64>, what: &str, uid: &str) -> CoreResult<i64> {
        map.get(uid)
            .copied()
            .ok_or_else(|| CoreError::Internal(format!("unresolved {what} {uid}")))
    }

    pub(crate) fn task(&mut self, r: &transport::TaskRow) -> CoreResult<()> {
        let mut row = db_sync::TaskRow {
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
        };
        match self.ids.tasks.get(&r.uid) {
            Some(&id) => {
                row.id = id;
                self.tx.update_task(&row)?;
            }
            None => {
                let id = self.tx.insert_task(&row)?;
                self.ids.tasks.insert(r.uid.clone(), id);
            }
        }
        self.touched_tasks.insert(r.uid.clone());
        Ok(())
    }

    pub(crate) fn step(&mut self, r: &transport::StepRow) -> CoreResult<()> {
        let mut row = db_sync::StepRow {
            id: 0,
            uid: r.uid.clone(),
            task_id: Self::id(&self.ids.tasks, "task", &r.task)?,
            description: r.description.clone(),
            done_at: r.done_at.clone(),
            deleted_at: r.deleted_at.clone(),
            order_index: r.order_index,
            created_at: r.created_at.clone(),
            version: version_json(&r.version),
        };
        match self.ids.steps.get(&r.uid) {
            Some(&id) => {
                row.id = id;
                self.tx.update_step(&row)?;
            }
            None => {
                let id = self.tx.insert_step(&row)?;
                self.ids.steps.insert(r.uid.clone(), id);
            }
        }
        self.counts.steps += 1;
        self.touched_tasks.insert(r.task.clone());
        Ok(())
    }

    pub(crate) fn memory(&mut self, r: &transport::MemoryRow) -> CoreResult<()> {
        let mut row = db_sync::MemoryRow {
            id: 0,
            uid: r.uid.clone(),
            content: r.content.clone(),
            created_at: r.created_at.clone(),
            updated_at: r.updated_at.clone(),
            deleted_at: r.deleted_at.clone(),
            version: version_json(&r.version),
        };
        match self.ids.memories.get(&r.uid) {
            Some(&id) => {
                row.id = id;
                self.tx.update_memory(&row)?;
            }
            None => {
                let id = self.tx.insert_memory(&row)?;
                self.ids.memories.insert(r.uid.clone(), id);
            }
        }
        self.counts.memories += 1;
        Ok(())
    }

    /// 既にある発言は、`deleted_at`と版だけを書き直す(ほかの列は書いたあと変わらない)。
    pub(crate) fn message(&mut self, r: &transport::MessageRow) -> CoreResult<()> {
        self.counts.messages += 1;
        if let Some(&id) = self.ids.messages.get(&r.uid) {
            return self.tx.update_message(&db_sync::MessageRow {
                id,
                deleted_at: r.deleted_at.clone(),
                version: version_json(&r.version),
                ..placeholder_message()
            });
        }
        let task_id = r
            .task
            .as_ref()
            .map(|uid| Self::id(&self.ids.tasks, "task", uid))
            .transpose()?;
        let attempt_no = match (&r.turn_id, &r.attempt) {
            (Some(turn_id), Some(attempt)) => Some(self.attempt_no(turn_id, attempt)?),
            _ => None,
        };
        let is_record = r.kind == "tool_execution";
        let id = self.tx.insert_message(&db_sync::MessageRow {
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
            attempt_no,
            attempt_key: None,
            deleted_at: r.deleted_at.clone(),
            created_at: r.created_at.clone(),
            prev_uid: r.prev_uid.clone(),
            version: version_json(&r.version),
        })?;
        self.ids.messages.insert(r.uid.clone(), id);
        if let Some(n) = attempt_no {
            self.attempt_nos.insert(r.uid.clone(), n);
        }
        if is_record || r.parts.is_some() {
            self.pending.push((id, r.clone()));
        }
        Ok(())
    }

    fn attempt_no(&mut self, turn_id: &str, attempt: &str) -> CoreResult<i64> {
        if let Some(&n) = self.attempts.get(attempt) {
            return Ok(n);
        }
        let n = match self.attempt_nos.get(attempt) {
            Some(&n) => n,
            None => self.tx.next_attempt_no(turn_id)?,
        };
        self.attempts.insert(attempt.to_string(), n);
        Ok(n)
    }

    /// 添付は書いたあと変わらないので、無いものだけを書く。
    pub(crate) fn attachment(&mut self, r: &transport::AttachmentRow) -> CoreResult<()> {
        if self.ids.attachments.contains_key(&r.uid) {
            return Ok(());
        }
        let message_id = Self::id(&self.ids.messages, "message", &r.message)?;
        let id = self.tx.insert_attachment(&db_sync::AttachmentRow {
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
        self.ids.attachments.insert(r.uid.clone(), id);
        self.counts.attachments += 1;
        Ok(())
    }

    /// 結合後のタスクの全体の記録を、その会話の末尾に置く。同じ`uid`の記録が既にあれば
    /// (別の2台が同じ結合をした)置かない。
    pub(crate) fn task_state_record(
        &mut self,
        task: &str,
        uid: &str,
        created_at: &str,
    ) -> CoreResult<()> {
        if self.ids.messages.contains_key(uid) {
            return Ok(());
        }
        let task_id = Self::id(&self.ids.tasks, "task", task)?;
        let id = self.tx.insert_task_state_record(task_id, uid, created_at)?;
        self.ids.messages.insert(uid.to_string(), id);
        self.counts.messages += 1;
        Ok(())
    }

    /// 仮の値で書いた発言の`content`・`parts`を直し、書いた数を返す。
    pub(crate) fn finish(self) -> CoreResult<Counts> {
        for (id, r) in &self.pending {
            let content = if r.kind == "tool_execution" {
                record_from_transport(&r.content, &self.ids).map_err(CoreError::Internal)?
            } else {
                r.content.clone()
            };
            let parts = r
                .parts
                .as_ref()
                .map(|parts| parts_from_transport(parts, &self.ids.messages))
                .transpose()
                .map_err(CoreError::Internal)?;
            self.tx.set_message_body(*id, &content, parts.as_deref())?;
        }
        Ok(Counts {
            tasks: self.touched_tasks.len(),
            ..self.counts
        })
    }
}

fn placeholder_message() -> db_sync::MessageRow {
    db_sync::MessageRow {
        id: 0,
        uid: String::new(),
        task_id: None,
        role: String::new(),
        content: String::new(),
        kind: String::new(),
        source: None,
        error_kind: None,
        error_detail: None,
        parts: None,
        turn_id: None,
        attempt_no: None,
        attempt_key: None,
        deleted_at: None,
        created_at: String::new(),
        prev_uid: None,
        version: String::new(),
    }
}

pub(crate) fn version_json(version: &Version) -> String {
    serde_json::to_string(version).expect("a version serializes")
}

fn record_from_transport(content: &str, ids: &Ids<String, i64>) -> Result<String, String> {
    let record = map_declared_ids(content, &mut |_, position, leaf| {
        let uid = marker_uid(leaf).ok_or("expected a replaced ID")?;
        let id = *ids
            .table(position.table)
            .get(uid)
            .ok_or_else(|| format!("unresolved ID {uid}"))?;
        *leaf = json!(id);
        Ok(())
    })?;
    Ok(record.map_or_else(|| content.to_string(), |r| r.to_string()))
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
