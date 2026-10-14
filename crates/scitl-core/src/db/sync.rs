//! 同期の層(`scitl-sync`)がDBに触る入口(`docs/sync/index.md`)。行は端末の中の連番のまま読み書き
//! する。運ぶ写しの連番を`uid`へ置き換えるのは同期の層で、ここは知らない(6節)。

use rusqlite::{Connection, Row};

use super::in_transaction;
use crate::error::Result;

/// DBに1つの端末IDとカウンター(`sync_state`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceState {
    pub device_id: String,
    pub counter: i64,
}

pub fn device_state(conn: &Connection) -> Result<DeviceState> {
    Ok(conn.query_row(
        "SELECT device_id, counter FROM sync_state WHERE id = 1",
        [],
        |row| {
            Ok(DeviceState {
                device_id: row.get(0)?,
                counter: row.get(1)?,
            })
        },
    )?)
}

/// 端末IDを作り直す(`docs/sync/index.md` 10節)。それまでの版とカーソルはそのまま使える。
pub fn renew_device_id(conn: &Connection) -> Result<String> {
    Ok(conn.query_row(
        "UPDATE sync_state SET device_id = lower(hex(randomblob(16))) WHERE id = 1
         RETURNING device_id",
        [],
        |row| row.get(0),
    )?)
}

/// タスク・発言・メモリが1行でもあるか(削除した行も数える)。添付は発言に付くので見ない。
pub fn has_data(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM tasks)
             OR EXISTS (SELECT 1 FROM messages)
             OR EXISTS (SELECT 1 FROM memories)",
        [],
        |row| row.get(0),
    )?)
}

/// 相手ごとのカーソルを書く(`sync_cursors`)。`sent_seq`は相手へ渡した位置(自分の番号)、
/// `received_seq`は相手から受け取った位置(相手の番号)。
pub fn record_cursor(
    conn: &Connection,
    peer_device_id: &str,
    sent_seq: i64,
    received_seq: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO sync_cursors (peer_device_id, sent_seq, received_seq) VALUES (?1, ?2, ?3)
         ON CONFLICT (peer_device_id) DO UPDATE
         SET sent_seq = excluded.sent_seq, received_seq = excluded.received_seq",
        rusqlite::params![peer_device_id, sent_seq, received_seq],
    )?;
    Ok(())
}

/// 相手ごとのカーソル。無ければ`None`。
pub fn cursor(conn: &Connection, peer_device_id: &str) -> Result<Option<(i64, i64)>> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row(
            "SELECT sent_seq, received_seq FROM sync_cursors WHERE peer_device_id = ?1",
            [peer_device_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

/// 行の写し。`id`と外部キーは端末の中の連番、`version`は版のJSONの文字列。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    pub id: i64,
    pub uid: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub deadline: Option<String>,
    pub archived_at: Option<String>,
    pub deleted_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRow {
    pub id: i64,
    pub uid: String,
    pub task_id: i64,
    pub description: String,
    pub done_at: Option<String>,
    pub deleted_at: Option<String>,
    pub order_index: i64,
    pub created_at: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRow {
    pub id: i64,
    pub uid: String,
    pub content: String,
    pub created_at: String,
    pub updated_at: String,
    pub deleted_at: Option<String>,
    pub version: String,
}

/// `role`・`kind`は列の値のまま持つ(読めない値も、検証で行を示せるように)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRow {
    pub id: i64,
    pub uid: String,
    pub task_id: Option<i64>,
    pub role: String,
    pub content: String,
    pub kind: String,
    pub source: Option<String>,
    pub error_kind: Option<String>,
    pub error_detail: Option<String>,
    pub parts: Option<String>,
    pub turn_id: Option<String>,
    pub attempt_no: Option<i64>,
    pub deleted_at: Option<String>,
    pub created_at: String,
    pub prev_uid: Option<String>,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentRow {
    pub id: i64,
    pub uid: String,
    pub message_id: i64,
    pub original_name: String,
    pub mime_type: String,
    pub kind: String,
    pub size_bytes: i64,
    pub content_text: Option<String>,
    pub file_hash: Option<String>,
    pub created_at: String,
}

/// 同期する行のすべて。各表とも`id`の順。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rows {
    pub tasks: Vec<TaskRow>,
    pub steps: Vec<StepRow>,
    pub memories: Vec<MemoryRow>,
    pub messages: Vec<MessageRow>,
    pub attachments: Vec<AttachmentRow>,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub device: DeviceState,
    pub rows: Rows,
}

/// 版を持つ表。版と印の付け方は同じ。
const VERSIONED: &[&str] = &["tasks", "task_steps", "memories", "messages"];

/// 印のある行に、この端末の分の版を振って印を外し(`docs/sync/index.md` 8節)、そのうえで全部の
/// 行を読む。印を残したまま写すと、写した側だけがあとでその印に版を振り、受け取った側で変えた
/// 行と並んで競合になる(12節)。振ることと読むことを1つのトランザクションにし、振ったあとに
/// 別プロセスが変えた行を、振る前の状態で読まない。
///
/// 振る番号は、カウンターを進めて取った値で、この回の行すべてに同じ値を使う。
pub fn stamp_and_snapshot(conn: &Connection) -> Result<Snapshot> {
    in_transaction(conn, |conn| {
        let device_id: String = conn.query_row(
            "UPDATE sync_state SET counter = counter + 1 WHERE id = 1 RETURNING device_id",
            [],
            |row| row.get(0),
        )?;
        let number: i64 =
            conn.query_row("SELECT counter FROM sync_state WHERE id = 1", [], |row| {
                row.get(0)
            })?;
        let path = format!("$.\"{device_id}\"");
        for table in VERSIONED {
            conn.execute(
                &format!(
                    "UPDATE {table} SET sync_version = json_set(sync_version, ?1, ?2), sync_changed = 0
                     WHERE sync_changed = 1"
                ),
                rusqlite::params![path, number],
            )?;
        }
        Ok(Snapshot {
            device: device_state(conn)?,
            rows: read_rows(conn)?,
        })
    })
}

fn read_rows(conn: &Connection) -> Result<Rows> {
    Ok(Rows {
        tasks: select(
            conn,
            "SELECT id, uid, title, description, deadline, archived_at, deleted_at, created_at,
                    updated_at, sync_version
             FROM tasks ORDER BY id",
            |row| {
                Ok(TaskRow {
                    id: row.get(0)?,
                    uid: row.get(1)?,
                    title: row.get(2)?,
                    description: row.get(3)?,
                    deadline: row.get(4)?,
                    archived_at: row.get(5)?,
                    deleted_at: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                    version: row.get(9)?,
                })
            },
        )?,
        steps: select(
            conn,
            "SELECT id, uid, task_id, description, done_at, deleted_at, order_index, created_at,
                    sync_version
             FROM task_steps ORDER BY id",
            |row| {
                Ok(StepRow {
                    id: row.get(0)?,
                    uid: row.get(1)?,
                    task_id: row.get(2)?,
                    description: row.get(3)?,
                    done_at: row.get(4)?,
                    deleted_at: row.get(5)?,
                    order_index: row.get(6)?,
                    created_at: row.get(7)?,
                    version: row.get(8)?,
                })
            },
        )?,
        memories: select(
            conn,
            "SELECT id, uid, content, created_at, updated_at, deleted_at, sync_version
             FROM memories ORDER BY id",
            |row| {
                Ok(MemoryRow {
                    id: row.get(0)?,
                    uid: row.get(1)?,
                    content: row.get(2)?,
                    created_at: row.get(3)?,
                    updated_at: row.get(4)?,
                    deleted_at: row.get(5)?,
                    version: row.get(6)?,
                })
            },
        )?,
        messages: select(
            conn,
            "SELECT id, uid, task_id, role, content, kind, source, error_kind, error_detail, parts,
                    turn_id, attempt_no, deleted_at, created_at, prev_uid, sync_version
             FROM messages ORDER BY id",
            |row| {
                Ok(MessageRow {
                    id: row.get(0)?,
                    uid: row.get(1)?,
                    task_id: row.get(2)?,
                    role: row.get(3)?,
                    content: row.get(4)?,
                    kind: row.get(5)?,
                    source: row.get(6)?,
                    error_kind: row.get(7)?,
                    error_detail: row.get(8)?,
                    parts: row.get(9)?,
                    turn_id: row.get(10)?,
                    attempt_no: row.get(11)?,
                    deleted_at: row.get(12)?,
                    created_at: row.get(13)?,
                    prev_uid: row.get(14)?,
                    version: row.get(15)?,
                })
            },
        )?,
        attachments: select(
            conn,
            "SELECT id, uid, message_id, original_name, mime_type, kind, size_bytes, content_text,
                    file_hash, created_at
             FROM attachments ORDER BY id",
            |row| {
                Ok(AttachmentRow {
                    id: row.get(0)?,
                    uid: row.get(1)?,
                    message_id: row.get(2)?,
                    original_name: row.get(3)?,
                    mime_type: row.get(4)?,
                    kind: row.get(5)?,
                    size_bytes: row.get(6)?,
                    content_text: row.get(7)?,
                    file_hash: row.get(8)?,
                    created_at: row.get(9)?,
                })
            },
        )?,
    })
}

fn select<T>(
    conn: &Connection,
    sql: &str,
    map: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map([], map)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// 受け取った行を書く。[`import`]の中でだけ作る。行の`id`は見ずに振り直し、外部キーは
/// 呼び出し側がこの端末の連番に置き換えて渡す。`uid`・版・`prev_uid`は渡した値をそのまま書く。
pub struct Importer<'c> {
    conn: &'c Connection,
}

/// 1つのトランザクションで取り込む。`f`が失敗すれば何も残さない。
pub fn import<T>(conn: &Connection, f: impl FnOnce(&Importer<'_>) -> Result<T>) -> Result<T> {
    in_transaction(conn, |conn| f(&Importer { conn }))
}

impl Importer<'_> {
    pub fn has_data(&self) -> Result<bool> {
        has_data(self.conn)
    }

    pub fn device_state(&self) -> Result<DeviceState> {
        device_state(self.conn)
    }

    pub fn insert_task(&self, row: &TaskRow) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO tasks (uid, title, description, deadline, archived_at, deleted_at,
                                created_at, updated_at, sync_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                row.uid,
                row.title,
                row.description,
                row.deadline,
                row.archived_at,
                row.deleted_at,
                row.created_at,
                row.updated_at,
                row.version,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn insert_step(&self, row: &StepRow) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO task_steps (uid, task_id, description, done_at, deleted_at, order_index,
                                     created_at, sync_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                row.uid,
                row.task_id,
                row.description,
                row.done_at,
                row.deleted_at,
                row.order_index,
                row.created_at,
                row.version,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn insert_memory(&self, row: &MemoryRow) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO memories (uid, content, created_at, updated_at, deleted_at, sync_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                row.uid,
                row.content,
                row.created_at,
                row.updated_at,
                row.deleted_at,
                row.version,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn insert_message(&self, row: &MessageRow) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO messages (uid, task_id, role, content, kind, source, error_kind,
                                   error_detail, parts, turn_id, attempt_no, deleted_at, created_at,
                                   prev_uid, sync_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            rusqlite::params![
                row.uid,
                row.task_id,
                row.role,
                row.content,
                row.kind,
                row.source,
                row.error_kind,
                row.error_detail,
                row.parts,
                row.turn_id,
                row.attempt_no,
                row.deleted_at,
                row.created_at,
                row.prev_uid,
                row.version,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// 書いた発言の`content`と`parts`を書き直す。実行記録と返信の中身が指すIDは、指す先の行を
    /// 書いてからでないと決まらない(添付は発言のあとに書く)ので、発言を先に仮の値で書いておく。
    pub fn set_message_body(&self, id: i64, content: &str, parts: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE messages SET content = ?1, parts = ?2 WHERE id = ?3",
            rusqlite::params![content, parts, id],
        )?;
        Ok(())
    }

    pub fn insert_attachment(&self, row: &AttachmentRow) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO attachments (uid, message_id, original_name, mime_type, kind, size_bytes,
                                      content_text, file_hash, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                row.uid,
                row.message_id,
                row.original_name,
                row.mime_type,
                row.kind,
                row.size_bytes,
                row.content_text,
                row.file_hash,
                row.created_at,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// 取り込みの終わり。空のDBへ写したあとに呼ぶ(印はどれも取り込んだ行のもの)。取り込んだ行の
    /// 印を外し(`docs/sync/index.md` 1節)、相手のカーソルを書く。相手へ渡した位置は、書き終えた
    /// 時点のカウンター(相手は、取り込んだ行をすべて持っている)で、それを返す。
    pub fn finish(&self, peer_device_id: &str, received_seq: i64) -> Result<i64> {
        for table in VERSIONED {
            self.conn.execute(
                &format!("UPDATE {table} SET sync_changed = 0 WHERE sync_changed = 1"),
                [],
            )?;
        }
        let counter = device_state(self.conn)?.counter;
        record_cursor(self.conn, peer_device_id, counter, received_seq)?;
        Ok(counter)
    }
}

/// 取り込む行が、本体が書く行と同じ形かを確かめる規則(`docs/sync/index.md` 5節)。保存の前の
/// 正規化を通して値が変わらないこと、形式が保存のときの形であることを見る。件数の上限は見ない
/// (2台で足した分を合わせると超えうる。上限は追加のときにだけ掛ける)。
pub mod rules {
    use crate::db::{memories, task_steps, tasks};
    use crate::error::{CoreError, Result};

    fn unchanged(name: &str, normalized: String, value: &str) -> Result<()> {
        if normalized == value {
            Ok(())
        } else {
            Err(CoreError::InvalidArgument {
                name: name.to_string(),
                reason: "is not in the form this app stores".to_string(),
            })
        }
    }

    pub fn title(value: &str) -> Result<()> {
        unchanged("title", tasks::normalize_title(value)?, value)
    }

    pub fn task_description(value: &str) -> Result<()> {
        tasks::check_description(value)
    }

    pub fn deadline(value: &str) -> Result<()> {
        tasks::validate_deadline(value)
    }

    pub fn step_description(value: &str) -> Result<()> {
        unchanged(
            "description",
            task_steps::normalize_description(value, "description")?,
            value,
        )
    }

    pub fn memory_content(value: &str) -> Result<()> {
        unchanged(
            "content",
            memories::normalize_content(value, "content")?,
            value,
        )
    }

    /// ユーザー発言の本文は前後の空白を落として保存する(添付だけの発言は空)。
    pub fn user_message_content(value: &str) -> Result<()> {
        unchanged("content", value.trim().to_string(), value)
    }

    /// 日時の列の形(`YYYY-MM-DDTHH:MM:SSZ`。`db::now_iso8601`)。
    pub fn timestamp(value: &str) -> Result<()> {
        let parsed = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%SZ")
            .ok()
            .map(|at| crate::db::iso8601(at.and_utc()));
        if parsed.as_deref() == Some(value) {
            Ok(())
        } else {
            Err(CoreError::InvalidArgument {
                name: "timestamp".to_string(),
                reason: "expected YYYY-MM-DDTHH:MM:SSZ".to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests;
