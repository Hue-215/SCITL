//! 添付ファイルの行。テキスト添付は本文をこの表に、それ以外は実体を
//! `attachments::AttachmentStore`に置き、この表はそのハッシュだけを持つ。

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use super::messages::Chat;
use super::{now_iso8601, text_column_enum};
use crate::error::{CoreError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Text,
    Image,
    Other,
}

text_column_enum!(AttachmentKind {
    Text => "text",
    Image => "image",
    Other => "other",
});

/// 添付の中身。テキストとそれ以外が排他であることを
/// `CHECK ((content_text IS NOT NULL) <> (file_hash IS NOT NULL))`と同じく型で表す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentContent {
    Text(String),
    /// 実体のハッシュ(`attachments::AttachmentStore::put`が返す値)。
    File {
        hash: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAttachment {
    pub original_name: String,
    pub mime_type: String,
    pub kind: AttachmentKind,
    pub size_bytes: i64,
    pub content: AttachmentContent,
}

/// 画面に渡す形。テキストの本文と実体は載せない(開いたときに別のコマンドで引く)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct AttachmentView {
    pub id: i64,
    pub original_name: String,
    pub mime_type: String,
    pub kind: AttachmentKind,
    pub size_bytes: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub view: AttachmentView,
    pub content: AttachmentContent,
}

pub fn insert(conn: &Connection, message_id: i64, new: &NewAttachment) -> Result<i64> {
    let (content_text, file_hash) = match &new.content {
        AttachmentContent::Text(text) => (Some(text.as_str()), None),
        AttachmentContent::File { hash } => (None, Some(hash.as_str())),
    };
    conn.execute(
        "INSERT INTO attachments
            (message_id, original_name, mime_type, kind, size_bytes, content_text, file_hash, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            message_id,
            new.original_name,
            new.mime_type,
            new.kind,
            new.size_bytes,
            content_text,
            file_hash,
            now_iso8601(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 1つの会話(`?1`)の、論理削除していない発言に付いた添付に絞る`FROM`・`WHERE`。
const IN_CHAT: &str = "FROM attachments a
    JOIN messages m ON m.id = a.message_id
    WHERE m.task_id IS ?1 AND m.deleted_at IS NULL";
/// [`view_from_row`]の並び。`concat!`に渡すため、定数ではなくマクロで持つ。
macro_rules! view_columns {
    () => {
        "a.id, a.original_name, a.mime_type, a.kind, a.size_bytes"
    };
}
const VIEW_COLUMNS: &str = view_columns!();
/// [`attachment_from_row`]の並び(表示用の列に中身を足したもの)。
const CONTENT_COLUMNS: &str = concat!(view_columns!(), ", a.content_text, a.file_hash");

/// 1つの会話の、論理削除していない発言に付いた添付を、中身ごと発言ごとにまとめる。添付の
/// 並びは付けた順。表示から外れる行(古い試行等)の分も入るが、引く側が自分の行の分だけを使う。
pub fn for_chat(conn: &Connection, chat: Chat) -> Result<HashMap<i64, Vec<Attachment>>> {
    group_in_chat(conn, chat, CONTENT_COLUMNS, attachment_from_row)
}

/// [`for_chat`]の、画面に渡す形。中身は引かない(会話を読み直すたびに呼ばれるため)。
pub fn views_for_chat(conn: &Connection, chat: Chat) -> Result<HashMap<i64, Vec<AttachmentView>>> {
    group_in_chat(conn, chat, VIEW_COLUMNS, view_from_row)
}

/// `columns`を2列目から並べて引き、発言ごとにまとめる。
fn group_in_chat<T>(
    conn: &Connection,
    chat: Chat,
    columns: &str,
    from_row: fn(&rusqlite::Row, usize) -> rusqlite::Result<T>,
) -> Result<HashMap<i64, Vec<T>>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT a.message_id, {columns} {IN_CHAT} ORDER BY a.id"
    ))?;
    let rows = stmt.query_map([chat.task_id()], |row| {
        Ok((row.get::<_, i64>(0)?, from_row(row, 1)?))
    })?;
    let mut out: HashMap<i64, Vec<T>> = HashMap::new();
    for row in rows {
        let (message_id, item) = row?;
        out.entry(message_id).or_default().push(item);
    }
    Ok(out)
}

/// 1つの発言の添付。付けた順。
pub fn views_for_message(conn: &Connection, message_id: i64) -> Result<Vec<AttachmentView>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {VIEW_COLUMNS} FROM attachments a WHERE a.message_id = ?1 ORDER BY a.id"
    ))?;
    let rows = stmt
        .query_map([message_id], |row| view_from_row(row, 0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn get(conn: &Connection, id: i64) -> Result<Attachment> {
    conn.query_row(
        &format!("SELECT {CONTENT_COLUMNS} FROM attachments a WHERE a.id = ?1"),
        [id],
        |row| attachment_from_row(row, 0),
    )
    .optional()?
    .ok_or(CoreError::AttachmentNotFound(id))
}

/// 1つの会話の添付を1件引く。他の会話の添付と、論理削除した発言の添付は、見つからない
/// ものとして扱う([`for_chat`]と同じ範囲)。モデルが添付IDを選ぶ経路で、対象の会話を
/// 文脈から固定するため。
pub fn get_in_chat(conn: &Connection, chat: Chat, id: i64) -> Result<Attachment> {
    conn.query_row(
        &format!("SELECT {CONTENT_COLUMNS} {IN_CHAT} AND a.id = ?2"),
        rusqlite::params![chat.task_id(), id],
        |row| attachment_from_row(row, 0),
    )
    .optional()?
    .ok_or(CoreError::AttachmentNotFound(id))
}

/// 添付の行を、付いた発言の情報と一緒に見せる形。画面の会話からは見えない行(論理削除した
/// 発言の添付)も含めて調べるためのもの。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttachmentRecord {
    #[serde(flatten)]
    pub view: AttachmentView,
    pub message_id: i64,
    /// 付いた発言の会話。総合チャットなら`None`。
    pub task_id: Option<i64>,
    /// 付いた発言を論理削除した日時。
    pub message_deleted_at: Option<String>,
    /// 実体のハッシュ。テキストの添付は本文を行に持つので`None`。
    pub file_hash: Option<String>,
    pub created_at: String,
}

/// すべての添付の行。付けた順。
pub fn list_all(conn: &Connection) -> Result<Vec<AttachmentRecord>> {
    let mut stmt = conn.prepare(concat!(
        "SELECT ",
        view_columns!(),
        ", a.message_id, m.task_id, m.deleted_at, a.file_hash, a.created_at
         FROM attachments a JOIN messages m ON m.id = a.message_id ORDER BY a.id"
    ))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(AttachmentRecord {
                view: view_from_row(row, 0)?,
                message_id: row.get(5)?,
                task_id: row.get(6)?,
                message_deleted_at: row.get(7)?,
                file_hash: row.get(8)?,
                created_at: row.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// 添付の行が指している実体のハッシュ。論理削除した発言の添付の分も含む(行は消さないので、
/// その実体も残す)。
pub fn file_hashes(conn: &Connection) -> Result<HashSet<String>> {
    let mut stmt =
        conn.prepare("SELECT DISTINCT file_hash FROM attachments WHERE file_hash IS NOT NULL")?;
    let hashes = stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<HashSet<String>>>()?;
    Ok(hashes)
}

/// 編集で新しい発言へ添付を引き継ぐ。実体は共有し、行だけを写す。写した数を返す。
pub fn copy_to_message(
    conn: &Connection,
    from_message_id: i64,
    to_message_id: i64,
) -> Result<usize> {
    let copied = conn.execute(
        "INSERT INTO attachments
            (message_id, original_name, mime_type, kind, size_bytes, content_text, file_hash, created_at)
         SELECT ?2, original_name, mime_type, kind, size_bytes, content_text, file_hash, ?3
         FROM attachments WHERE message_id = ?1 ORDER BY id",
        rusqlite::params![from_message_id, to_message_id, now_iso8601()],
    )?;
    Ok(copied)
}

/// `SELECT`の`start`列目から`id, original_name, mime_type, kind, size_bytes, content_text,
/// file_hash`が並んでいる前提。
fn attachment_from_row(row: &rusqlite::Row, start: usize) -> rusqlite::Result<Attachment> {
    let view = view_from_row(row, start)?;
    let content = match (
        row.get::<_, Option<String>>(start + 5)?,
        row.get(start + 6)?,
    ) {
        (Some(text), None) => AttachmentContent::Text(text),
        (None, Some(hash)) => AttachmentContent::File { hash },
        // CHECK制約で排他が保証されているので、ここには来ない。
        _ => {
            return Err(rusqlite::Error::InvalidColumnType(
                start + 5,
                "content_text".to_string(),
                rusqlite::types::Type::Null,
            ))
        }
    };
    Ok(Attachment { view, content })
}

/// `SELECT`の`start`列目から`id, original_name, mime_type, kind, size_bytes`が並んでいる前提。
fn view_from_row(row: &rusqlite::Row, start: usize) -> rusqlite::Result<AttachmentView> {
    Ok(AttachmentView {
        id: row.get(start)?,
        original_name: row.get(start + 1)?,
        mime_type: row.get(start + 2)?,
        kind: row.get(start + 3)?,
        size_bytes: row.get(start + 4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::db::messages::{self, Kind, NewMessage, Origin, Role};

    fn user_message(conn: &Connection, task_id: i64) -> i64 {
        messages::insert_message(
            conn,
            NewMessage {
                chat: Chat::Task(task_id),
                role: Role::User,
                content: "見て",
                kind: Kind::Normal,
                origin: Origin::User,
                error_kind: None,
                error_detail: None,
                partial_reply: None,
                reasoning: None,
            },
        )
        .unwrap()
    }

    fn text(name: &str, body: &str) -> NewAttachment {
        NewAttachment {
            original_name: name.to_string(),
            mime_type: "text/plain".to_string(),
            kind: AttachmentKind::Text,
            size_bytes: body.len() as i64,
            content: AttachmentContent::Text(body.to_string()),
        }
    }

    fn image(name: &str) -> NewAttachment {
        NewAttachment {
            original_name: name.to_string(),
            mime_type: "image/png".to_string(),
            kind: AttachmentKind::Image,
            size_bytes: 3,
            content: AttachmentContent::File {
                hash: "a".repeat(64),
            },
        }
    }

    #[test]
    fn stores_text_and_file_contents_and_reads_them_back() {
        let conn = db::open_in_memory().unwrap();
        let task_id = db::tasks::create_task(&conn).unwrap().id;
        let message_id = user_message(&conn, task_id);

        let text_id = insert(&conn, message_id, &text("memo.txt", "本文")).unwrap();
        let image_id = insert(&conn, message_id, &image("photo.png")).unwrap();

        assert_eq!(
            get(&conn, text_id).unwrap().content,
            AttachmentContent::Text("本文".to_string())
        );
        let stored = get(&conn, image_id).unwrap();
        assert_eq!(stored.view.kind, AttachmentKind::Image);
        assert_eq!(
            stored.content,
            AttachmentContent::File {
                hash: "a".repeat(64)
            }
        );
        assert!(matches!(
            get(&conn, image_id + 1),
            Err(CoreError::AttachmentNotFound(_))
        ));
    }

    #[test]
    fn groups_views_by_message_and_leaves_out_deleted_messages() {
        let conn = db::open_in_memory().unwrap();
        let task_id = db::tasks::create_task(&conn).unwrap().id;
        let first = user_message(&conn, task_id);
        let second = user_message(&conn, task_id);
        insert(&conn, first, &text("a.txt", "a")).unwrap();
        insert(&conn, first, &image("b.png")).unwrap();
        insert(&conn, second, &text("c.txt", "c")).unwrap();
        messages::soft_delete_message(&conn, second).unwrap();

        let views = views_for_chat(&conn, Chat::Task(task_id)).unwrap();
        let names: Vec<&str> = views[&first]
            .iter()
            .map(|v| v.original_name.as_str())
            .collect();
        assert_eq!(names, ["a.txt", "b.png"]);
        assert!(!views.contains_key(&second));
        assert!(views_for_chat(&conn, Chat::General).unwrap().is_empty());
    }

    #[test]
    fn gets_an_attachment_only_within_its_chat() {
        let conn = db::open_in_memory().unwrap();
        let task_id = db::tasks::create_task(&conn).unwrap().id;
        let other_task = db::tasks::create_task(&conn).unwrap().id;
        let kept = insert(&conn, user_message(&conn, task_id), &text("a.txt", "a")).unwrap();
        let deleted_message = user_message(&conn, task_id);
        let deleted = insert(&conn, deleted_message, &text("b.txt", "b")).unwrap();
        messages::soft_delete_message(&conn, deleted_message).unwrap();

        let chat = Chat::Task(task_id);
        assert_eq!(get_in_chat(&conn, chat, kept).unwrap().view.id, kept);
        for (chat, id) in [
            (chat, deleted),
            (Chat::Task(other_task), kept),
            (Chat::General, kept),
        ] {
            assert!(
                matches!(get_in_chat(&conn, chat, id), Err(CoreError::AttachmentNotFound(n)) if n == id),
                "{chat}: {id}"
            );
        }
    }

    #[test]
    fn copies_attachments_to_another_message_sharing_the_contents() {
        let conn = db::open_in_memory().unwrap();
        let task_id = db::tasks::create_task(&conn).unwrap().id;
        let from = user_message(&conn, task_id);
        let to = user_message(&conn, task_id);
        insert(&conn, from, &text("a.txt", "a")).unwrap();
        insert(&conn, from, &image("b.png")).unwrap();

        assert_eq!(copy_to_message(&conn, from, to).unwrap(), 2);

        let copied = views_for_message(&conn, to).unwrap();
        assert_eq!(copied.len(), 2);
        assert_eq!(copied[0].original_name, "a.txt");
        assert_eq!(
            get(&conn, copied[1].id).unwrap().content,
            AttachmentContent::File {
                hash: "a".repeat(64)
            }
        );
        assert_eq!(views_for_message(&conn, from).unwrap().len(), 2);
    }

    #[test]
    fn lists_every_row_with_its_message_and_the_hashes_they_point_at() {
        let conn = db::open_in_memory().unwrap();
        let task_id = db::tasks::create_task(&conn).unwrap().id;
        let kept = user_message(&conn, task_id);
        let deleted = user_message(&conn, task_id);
        insert(&conn, kept, &text("a.txt", "a")).unwrap();
        insert(&conn, deleted, &image("b.png")).unwrap();
        messages::soft_delete_message(&conn, deleted).unwrap();

        let records = list_all(&conn).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].view.original_name, "a.txt");
        assert_eq!(records[0].task_id, Some(task_id));
        assert_eq!(records[0].file_hash, None);
        assert_eq!(records[0].message_deleted_at, None);
        assert_eq!(records[1].message_id, deleted);
        assert_eq!(records[1].file_hash, Some("a".repeat(64)));
        assert!(records[1].message_deleted_at.is_some());

        // 論理削除した発言の添付が指す実体も、指されているものに数える。
        assert_eq!(file_hashes(&conn).unwrap(), HashSet::from(["a".repeat(64)]));
    }
}
