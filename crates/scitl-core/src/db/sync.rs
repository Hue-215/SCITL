//! 同期の層(`scitl-sync`)がDBに触る入口(`docs/sync/index.md`)。行は端末の中の連番のまま読み書き
//! する。運ぶ写しの連番を`uid`へ置き換えるのは同期の層で、ここは知らない(6節)。どの行を書くかを
//! 決める規則も同期の層が持ち、ここは言われた行を書くだけ。

use rusqlite::{Connection, OptionalExtension, Row};

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

/// 端末IDと、資格情報ストアの端末IDの項目と同じ値(`docs/sync/index.md` 10節)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceToken {
    pub device_id: String,
    pub token: String,
}

pub fn device_token(conn: &Connection) -> Result<DeviceToken> {
    Ok(conn.query_row(
        "SELECT device_id, device_token FROM sync_state WHERE id = 1",
        [],
        device_token_row,
    )?)
}

/// 項目の値を替え、替える前と後を返す。`renew_id`なら端末IDも作り直す(それまでの版とカーソルは
/// そのまま使える)。
pub fn replace_device_token(
    conn: &Connection,
    renew_id: bool,
) -> Result<(DeviceToken, DeviceToken)> {
    in_transaction(conn, |conn| {
        let before = device_token(conn)?;
        let replaced = conn.query_row(
            "UPDATE sync_state
             SET device_id = CASE WHEN ?1 THEN lower(hex(randomblob(16))) ELSE device_id END,
                 device_token = lower(hex(randomblob(16)))
             WHERE id = 1
             RETURNING device_id, device_token",
            [renew_id],
            device_token_row,
        )?;
        Ok((before, replaced))
    })
}

/// [`replace_device_token`]で`replaced`にしたものを、資格情報ストアに書けなかったときに`before`へ戻す。
/// その後に別プロセスが替えていれば、そちらを残す。
pub fn restore_device_token(
    conn: &Connection,
    before: &DeviceToken,
    replaced: &DeviceToken,
) -> Result<()> {
    conn.execute(
        "UPDATE sync_state SET device_id = ?1, device_token = ?2
         WHERE id = 1 AND device_id = ?3 AND device_token = ?4",
        rusqlite::params![
            before.device_id,
            before.token,
            replaced.device_id,
            replaced.token
        ],
    )?;
    Ok(())
}

fn device_token_row(row: &Row<'_>) -> rusqlite::Result<DeviceToken> {
    Ok(DeviceToken {
        device_id: row.get(0)?,
        token: row.get(1)?,
    })
}

/// 相手ごとのカーソル(`sync_cursors`)。`sent_seq`は相手へ渡した位置(自分の番号)、
/// `received_seq`は相手から受け取った位置(相手の番号)。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cursor {
    pub sent_seq: i64,
    pub received_seq: i64,
}

pub fn record_cursor(conn: &Connection, peer_device_id: &str, cursor: Cursor) -> Result<()> {
    conn.execute(
        "INSERT INTO sync_cursors (peer_device_id, sent_seq, received_seq) VALUES (?1, ?2, ?3)
         ON CONFLICT (peer_device_id) DO UPDATE
         SET sent_seq = excluded.sent_seq, received_seq = excluded.received_seq",
        rusqlite::params![peer_device_id, cursor.sent_seq, cursor.received_seq],
    )?;
    Ok(())
}

/// 相手ごとのカーソル。初めての相手は0から(全部を送る)。
pub fn cursor(conn: &Connection, peer_device_id: &str) -> Result<Cursor> {
    Ok(conn
        .query_row(
            "SELECT sent_seq, received_seq FROM sync_cursors WHERE peer_device_id = ?1",
            [peer_device_id],
            |row| {
                Ok(Cursor {
                    sent_seq: row.get(0)?,
                    received_seq: row.get(1)?,
                })
            },
        )
        .optional()?
        .unwrap_or_default())
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
    /// 試行(`turn_id`と`attempt_no`の組)の最初の行の`uid`。試行の番号は端末の中の値なので
    /// (`docs/sync/index.md` 9節)、端末を跨いで試行を指すのに使う。書くときは見ない。
    pub attempt_key: Option<String>,
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

/// 同期する行。各表とも`id`の順。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rows {
    pub tasks: Vec<TaskRow>,
    pub steps: Vec<StepRow>,
    pub memories: Vec<MemoryRow>,
    pub messages: Vec<MessageRow>,
    pub attachments: Vec<AttachmentRow>,
}

/// 読む行の範囲。
#[derive(Debug, Clone, Copy)]
pub enum Filter<'a> {
    All,
    /// `sync_seq`がこれより大きい行(前回相手へ渡した位置より後に変わった行。7節)。
    Since(i64),
    /// `uid`がこれらの行(表を問わない)。
    Uids(&'a [String]),
    /// タスク(行・工程・会話と、その発言の添付)と、総合チャット(`general`)のすべての行。
    Units {
        tasks: &'a [String],
        general: bool,
    },
}

/// 各表の、連番と`uid`の組のすべて。運ぶ写しの中のIDを置き換えるのに使う。
#[derive(Debug, Clone, Default)]
pub struct Uids {
    pub tasks: Vec<(i64, String)>,
    pub steps: Vec<(i64, String)>,
    pub memories: Vec<(i64, String)>,
    pub messages: Vec<(i64, String)>,
    pub attachments: Vec<(i64, String)>,
}

/// 発言が指す先を確かめるのに要る、発言の行の一部。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRef {
    pub uid: String,
    pub task_id: Option<i64>,
    pub role: String,
    pub kind: String,
    pub turn_id: Option<String>,
    pub attempt_no: Option<i64>,
}

/// 版を持つ表。版と印の付け方は同じ。
const VERSIONED: &[&str] = &["tasks", "task_steps", "memories", "messages"];

/// 同期の層の読み書き。[`transaction`]の中でだけ作る。
pub struct Tx<'c> {
    conn: &'c Connection,
}

/// 1つのトランザクションで読み書きする。`f`が失敗すれば何も残さない。通信と利用者の選択を
/// 跨がないこと(`docs/sync/index.md` 4節)は呼び出し側が守る。
pub fn transaction<T>(conn: &Connection, f: impl FnOnce(&Tx<'_>) -> Result<T>) -> Result<T> {
    in_transaction(conn, |conn| f(&Tx { conn }))
}

impl Tx<'_> {
    pub fn device_state(&self) -> Result<DeviceState> {
        device_state(self.conn)
    }

    pub fn cursor(&self, peer_device_id: &str) -> Result<Cursor> {
        cursor(self.conn, peer_device_id)
    }

    pub fn record_cursor(&self, peer_device_id: &str, cursor: Cursor) -> Result<()> {
        record_cursor(self.conn, peer_device_id, cursor)
    }

    /// 版の中の、端末`device_id`の番号の最大(無ければ0)。相手の番号の巻き戻りを見るのに使う
    /// (`docs/sync/index.md` 10節)。
    pub fn seen_of(&self, device_id: &str) -> Result<i64> {
        let path = version_path(device_id);
        let mut seen = 0;
        for table in VERSIONED {
            let max: Option<i64> = self.conn.query_row(
                &format!("SELECT MAX(json_extract(sync_version, ?1)) FROM {table}"),
                [&path],
                |row| row.get(0),
            )?;
            seen = seen.max(max.unwrap_or(0));
        }
        Ok(seen)
    }

    /// 印のある行に、この端末の分の版を振って印を外す(`docs/sync/index.md` 8節)。振る番号は
    /// カウンターを進めて取った値で、この回の行すべてに同じ値を使う。
    pub fn stamp(&self) -> Result<()> {
        let number = self.next_number()?;
        let path = version_path(&self.device_state()?.device_id);
        for table in VERSIONED {
            self.conn.execute(
                &format!(
                    "UPDATE {table} SET sync_version = json_set(sync_version, ?1, ?2), sync_changed = 0
                     WHERE sync_changed = 1"
                ),
                rusqlite::params![path, number],
            )?;
        }
        Ok(())
    }

    /// カウンターを進めて取った番号。同期の層が版に振る(同じ番号を2度使わない)。
    pub fn next_number(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "UPDATE sync_state SET counter = counter + 1 WHERE id = 1 RETURNING counter",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn rows(&self, filter: Filter<'_>) -> Result<Rows> {
        read_rows(self.conn, filter)
    }

    pub fn uids(&self) -> Result<Uids> {
        let pairs = |table: &str| {
            select(
                self.conn,
                &format!("SELECT id, uid FROM {table} ORDER BY id"),
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
        };
        Ok(Uids {
            tasks: pairs("tasks")?,
            steps: pairs("task_steps")?,
            memories: pairs("memories")?,
            messages: pairs("messages")?,
            attachments: pairs("attachments")?,
        })
    }

    pub fn message_refs(&self) -> Result<Vec<MessageRef>> {
        select(
            self.conn,
            "SELECT uid, task_id, role, kind, turn_id, attempt_no FROM messages ORDER BY id",
            |row| {
                Ok(MessageRef {
                    uid: row.get(0)?,
                    task_id: row.get(1)?,
                    role: row.get(2)?,
                    kind: row.get(3)?,
                    turn_id: row.get(4)?,
                    attempt_no: row.get(5)?,
                })
            },
        )
    }

    /// 消えていないメモリのすべて(`id`の順)。同じ本文を畳むのに使う(`docs/sync/index.md` 8節)。
    pub fn alive_memories(&self) -> Result<Vec<MemoryRow>> {
        select(
            self.conn,
            "SELECT id, uid, content, created_at, updated_at, deleted_at, sync_version
             FROM memories WHERE deleted_at IS NULL ORDER BY id",
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
        )
    }

    /// 試行の番号を新しく取る。消えた行も含めた最大の続き(`db::messages::next_attempt_no`と同じ)。
    pub fn next_attempt_no(&self, turn_id: &str) -> Result<i64> {
        super::messages::next_attempt_no(self.conn, turn_id)
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

    /// `attempt_key`は見ない(番号は`attempt_no`で渡す)。
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

    /// 書いたあと変わりうる列と版を書き直す(`id`で指す)。ほかの列は見ない。
    pub fn update_task(&self, row: &TaskRow) -> Result<()> {
        self.conn.execute(
            "UPDATE tasks SET title = ?1, description = ?2, deadline = ?3, archived_at = ?4,
                              deleted_at = ?5, updated_at = ?6, sync_version = ?7
             WHERE id = ?8",
            rusqlite::params![
                row.title,
                row.description,
                row.deadline,
                row.archived_at,
                row.deleted_at,
                row.updated_at,
                row.version,
                row.id,
            ],
        )?;
        Ok(())
    }

    pub fn update_step(&self, row: &StepRow) -> Result<()> {
        self.conn.execute(
            "UPDATE task_steps SET description = ?1, done_at = ?2, deleted_at = ?3,
                                   sync_version = ?4
             WHERE id = ?5",
            rusqlite::params![
                row.description,
                row.done_at,
                row.deleted_at,
                row.version,
                row.id
            ],
        )?;
        Ok(())
    }

    pub fn update_memory(&self, row: &MemoryRow) -> Result<()> {
        self.conn.execute(
            "UPDATE memories SET content = ?1, deleted_at = ?2, updated_at = ?3, sync_version = ?4
             WHERE id = ?5",
            rusqlite::params![
                row.content,
                row.deleted_at,
                row.updated_at,
                row.version,
                row.id
            ],
        )?;
        Ok(())
    }

    pub fn update_message(&self, row: &MessageRow) -> Result<()> {
        self.conn.execute(
            "UPDATE messages SET deleted_at = ?1, sync_version = ?2 WHERE id = ?3",
            rusqlite::params![row.deleted_at, row.version, row.id],
        )?;
        Ok(())
    }

    /// 結合後のタスクの全体を、操作の記録としてその会話の末尾に置く(`docs/sync/index.md` 8節)。
    /// `uid`と`created_at`は呼び出し側が決める(別の2台が同じ結合をしても同じ行になるように)。
    pub fn insert_task_state_record(
        &self,
        task_id: i64,
        uid: &str,
        created_at: &str,
    ) -> Result<i64> {
        let content = crate::orchestration::operations::task_state_record(self.conn, task_id)?;
        self.insert_message(&MessageRow {
            id: 0,
            uid: uid.to_string(),
            task_id: Some(task_id),
            role: "tool".to_string(),
            content,
            kind: "tool_execution".to_string(),
            source: Some("sync".to_string()),
            error_kind: None,
            error_detail: None,
            parts: None,
            turn_id: None,
            attempt_no: None,
            attempt_key: None,
            deleted_at: None,
            created_at: created_at.to_string(),
            prev_uid: None,
            version: "{}".to_string(),
        })
    }

    /// 印をすべて外す。同期の層の書き込み(取り込み・版を振った書き込み)のあとに、同じ
    /// トランザクションで呼ぶ(`docs/sync/index.md` 1節)。
    pub fn clear_changed(&self) -> Result<()> {
        for table in VERSIONED {
            self.conn.execute(
                &format!("UPDATE {table} SET sync_changed = 0 WHERE sync_changed = 1"),
                [],
            )?;
        }
        Ok(())
    }
}

fn version_path(device_id: &str) -> String {
    format!("$.\"{device_id}\"")
}

/// `filter`の各表の条件(`?1`に`uid`の一覧のJSON、`?2`に番号か総合チャットの有無が入る)。
struct Where {
    tasks: &'static str,
    steps: &'static str,
    memories: &'static str,
    messages: &'static str,
    attachments: &'static str,
}

impl Where {
    /// どの表にも同じ条件。
    const fn every_table(clause: &'static str) -> Self {
        Self {
            tasks: clause,
            steps: clause,
            memories: clause,
            messages: clause,
            attachments: clause,
        }
    }
}

const ALL: Where = Where::every_table("1");
const SINCE: Where = Where::every_table("sync_seq > ?2");
const UIDS: Where = Where::every_table("uid IN (SELECT value FROM json_each(?1))");

const UNITS: Where = Where {
    tasks: "uid IN (SELECT value FROM json_each(?1))",
    steps: "task_id IN (SELECT id FROM tasks WHERE uid IN (SELECT value FROM json_each(?1)))",
    memories: "0",
    messages: "(task_id IN (SELECT id FROM tasks WHERE uid IN (SELECT value FROM json_each(?1)))
                OR (?2 AND task_id IS NULL))",
    attachments: "message_id IN (
                    SELECT id FROM messages
                    WHERE task_id IN (SELECT id FROM tasks WHERE uid IN (SELECT value FROM json_each(?1)))
                       OR (?2 AND task_id IS NULL))",
};

/// どの条件の文にも`?1`と`?2`を渡す(条件が使わない値も、使ったことにする末尾の式を足す)。
fn read_rows(conn: &Connection, filter: Filter<'_>) -> Result<Rows> {
    let (clauses, uids, number): (&Where, &[String], i64) = match filter {
        Filter::All => (&ALL, &[], 0),
        Filter::Since(seq) => (&SINCE, &[], seq),
        Filter::Uids(uids) => (&UIDS, uids, 0),
        Filter::Units { tasks, general } => (&UNITS, tasks, i64::from(general)),
    };
    let uids = serde_json::to_string(uids).expect("uids serialize");
    let params = rusqlite::params![uids, number];
    Ok(Rows {
        tasks: select_with(
            conn,
            &format!(
                "SELECT id, uid, title, description, deadline, archived_at, deleted_at, created_at,
                        updated_at, sync_version
                 FROM tasks WHERE {} AND (?1 IS NOT NULL OR ?2 IS NOT NULL) ORDER BY id",
                clauses.tasks
            ),
            params,
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
        steps: select_with(
            conn,
            &format!(
                "SELECT id, uid, task_id, description, done_at, deleted_at, order_index, created_at,
                        sync_version
                 FROM task_steps WHERE {} AND (?1 IS NOT NULL OR ?2 IS NOT NULL) ORDER BY id",
                clauses.steps
            ),
            params,
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
        memories: select_with(
            conn,
            &format!(
                "SELECT id, uid, content, created_at, updated_at, deleted_at, sync_version
                 FROM memories WHERE {} AND (?1 IS NOT NULL OR ?2 IS NOT NULL) ORDER BY id",
                clauses.memories
            ),
            params,
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
        messages: select_with(
            conn,
            &format!(
                "SELECT id, uid, task_id, role, content, kind, source, error_kind, error_detail, parts,
                        turn_id, attempt_no,
                        (SELECT f.uid FROM messages f
                         WHERE f.turn_id = messages.turn_id AND f.attempt_no = messages.attempt_no
                         ORDER BY f.id LIMIT 1),
                        deleted_at, created_at, prev_uid, sync_version
                 FROM messages WHERE {} AND (?1 IS NOT NULL OR ?2 IS NOT NULL) ORDER BY id",
                clauses.messages
            ),
            params,
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
                    attempt_key: row.get(12)?,
                    deleted_at: row.get(13)?,
                    created_at: row.get(14)?,
                    prev_uid: row.get(15)?,
                    version: row.get(16)?,
                })
            },
        )?,
        attachments: select_with(
            conn,
            &format!(
                "SELECT id, uid, message_id, original_name, mime_type, kind, size_bytes, content_text,
                        file_hash, created_at
                 FROM attachments WHERE {} AND (?1 IS NOT NULL OR ?2 IS NOT NULL) ORDER BY id",
                clauses.attachments
            ),
            params,
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
    select_with(conn, sql, [], map)
}

fn select_with<T>(
    conn: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
    map: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map(params, map)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
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
