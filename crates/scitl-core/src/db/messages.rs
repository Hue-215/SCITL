use std::collections::HashMap;
use std::fmt;

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::attachments::{self, AttachmentView};
use super::{now_iso8601, text_column_enum};
use crate::error::{CoreError, Result};

/// 発言が属する会話。`messages.task_id`がNULLなら総合チャット。`Option<i64>`で持たないのは、
/// 渡し忘れの`None`が総合チャットへの書き込みに化けるのを型で防ぐため。画面とは
/// `{"kind":"general"}`・`{"kind":"task","task_id":1}`の形でやり取りする。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "kind", content = "task_id", rename_all = "snake_case")]
pub enum Chat {
    General,
    Task(i64),
}

impl Chat {
    /// `messages.task_id`の値。
    pub fn task_id(self) -> Option<i64> {
        match self {
            Self::General => None,
            Self::Task(id) => Some(id),
        }
    }
}

impl fmt::Display for Chat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::General => f.write_str("the general chat"),
            Self::Task(id) => write!(f, "task {id}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    Tool,
    Error,
}

text_column_enum!(Role {
    User => "user",
    Assistant => "assistant",
    Tool => "tool",
    Error => "error",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Normal,
    ToolExecution,
}

text_column_enum!(Kind {
    Normal => "normal",
    ToolExecution => "tool_execution",
});

/// 行の出どころ。`source`と`turn_id`/`attempt_no`の組み合わせはこれだけから決まり、
/// 取り違えた組み合わせは書けない。操作の記録が実行記録であることまでは型で縛らず、
/// DBのトリガー(`0004_message_origin.sql`)が止める。
#[derive(Debug, Clone, Copy)]
pub enum Origin<'a> {
    User,
    /// SCITLの応答生成の1試行に属する行。応答生成の途中で外部のツールサーバーを呼んだ記録も
    /// ここに入る(`source`は「外部と通信した」印ではない)。
    Turn {
        turn_id: &'a str,
        attempt_no: i64,
    },
    /// 応答生成以外の経路での操作の記録。
    Operation(OperationSource),
}

/// 応答生成以外の経路の印(`messages.source`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationSource {
    Ui,
    Cli,
}

text_column_enum!(OperationSource {
    Ui => "ui",
    Cli => "cli",
});

pub struct NewMessage<'a> {
    pub chat: Chat,
    pub role: Role,
    pub content: &'a str,
    pub kind: Kind,
    pub origin: Origin<'a>,
    /// `role`が`Error`のときのみ`Some`(`CHECK ((role = 'error') = (error_kind IS NOT NULL))`)。
    pub error_kind: Option<&'a str>,
    /// エラー発言の詳細(`orchestration::TurnFailure::detail`)。画面の「詳細を表示」専用で、
    /// モデル入力・エクスポートには使わない。
    pub error_detail: Option<&'a str>,
    /// ターンの返信の行(`Origin::Turn`の通常発言)だけが持ち、それには必ず持つ
    /// (`0008_reply_parts.sql`のトリガー)。
    pub parts: Option<&'a [ReplyPart]>,
}

/// ターンの返信の行が持つ、そのターンの中身(`messages.parts`)の1要素。起きた順に並べ、
/// 本文はラウンドごとに分けたまま持つ(`docs/spec/data-model/messages.md`「1ターン内の往復で
/// 保存するもの」)。`round`は1始まりのラウンドの番号で、ツールだけのラウンドが続いても
/// 区切れるように要素ごとに持つ。
///
/// ツールは実行記録の行をidで指す。記録の行は実行した時点で書くログで、呼び出しと結果は
/// そちらにだけ持つ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReplyPart {
    /// 思考の表示用のテキスト。表示専用で、モデルへの入力にもエクスポートにも出さない。
    Reasoning {
        round: u32,
        text: String,
    },
    Text {
        round: u32,
        text: String,
    },
    Tool {
        round: u32,
        record: i64,
    },
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct Message {
    pub id: i64,
    pub task_id: Option<i64>,
    pub role: Role,
    pub content: String,
    pub kind: Kind,
    pub source: Option<String>,
    pub error_kind: Option<String>,
    pub error_detail: Option<String>,
    /// ターンの返信の行の中身([`ReplyPart`])。ほかの行は空。ツールは行のidを指すだけなので、
    /// 画面へは指した記録を解いた形で渡す(`orchestration::list_chat`)。
    #[serde(skip)]
    pub parts: Vec<ReplyPart>,
    pub turn_id: Option<String>,
    pub attempt_no: Option<i64>,
    pub created_at: String,
    /// 発言に付いた添付。付けた順。
    pub attachments: Vec<AttachmentView>,
}

/// 返信の行の中身([`ReplyPart`])の1要素を、ツールなら指した実行記録の行を引いた形にしたもの。
#[derive(Debug, Clone, Copy)]
pub(crate) enum ResolvedPart<'a> {
    Reasoning { round: u32, text: &'a str },
    Text { round: u32, text: &'a str },
    Tool { round: u32, record: &'a Message },
}

impl ResolvedPart<'_> {
    pub(crate) fn round(&self) -> u32 {
        match self {
            Self::Reasoning { round, .. } | Self::Text { round, .. } | Self::Tool { round, .. } => {
                *round
            }
        }
    }
}

/// 会話の行のうち、返信の行の中身から指されている実行記録の行。表示・エクスポート・履歴は、
/// これらの記録を独立した行としては扱わず、指した返信の中身の位置に並べる。
pub(crate) struct ReplyRecords<'a>(HashMap<i64, &'a Message>);

impl<'a> ReplyRecords<'a> {
    /// 指した記録は、返信と同じ試行の、`rows`にある(消していない)ものだけを引く。
    pub(crate) fn of(rows: &'a [Message]) -> Self {
        let by_id: HashMap<i64, &Message> = rows
            .iter()
            .filter(|m| m.kind == Kind::ToolExecution && m.turn_id.is_some())
            .map(|m| (m.id, m))
            .collect();
        let mut referenced = HashMap::new();
        for reply in rows {
            for part in &reply.parts {
                let ReplyPart::Tool { record, .. } = part else {
                    continue;
                };
                if let Some(m) = by_id.get(record).filter(|m| same_attempt(m, reply)) {
                    referenced.insert(*record, *m);
                }
            }
        }
        Self(referenced)
    }

    /// 行が、返信の中身から指されている実行記録か。
    pub(crate) fn contains(&self, id: i64) -> bool {
        self.0.contains_key(&id)
    }

    /// 返信の行の中身を、起きた順のまま解く。引けない記録(消した記録)を指す要素は飛ばす。
    pub(crate) fn resolve(&self, reply: &'a Message) -> Vec<ResolvedPart<'a>> {
        reply
            .parts
            .iter()
            .filter_map(|part| match part {
                ReplyPart::Reasoning { round, text } => Some(ResolvedPart::Reasoning {
                    round: *round,
                    text,
                }),
                ReplyPart::Text { round, text } => Some(ResolvedPart::Text {
                    round: *round,
                    text,
                }),
                ReplyPart::Tool { round, record } => self
                    .0
                    .get(record)
                    .filter(|m| same_attempt(m, reply))
                    .map(|m| ResolvedPart::Tool {
                        round: *round,
                        record: m,
                    }),
            })
            .collect()
    }
}

fn same_attempt(a: &Message, b: &Message) -> bool {
    a.turn_id == b.turn_id && a.attempt_no == b.attempt_no
}

pub fn insert_message(conn: &Connection, msg: NewMessage) -> Result<i64> {
    let (source, turn_id, attempt_no) = match msg.origin {
        Origin::User => (None, None, None),
        Origin::Turn {
            turn_id,
            attempt_no,
        } => (None, Some(turn_id), Some(attempt_no)),
        Origin::Operation(source) => (Some(source), None, None),
    };
    let parts = msg
        .parts
        .map(|parts| serde_json::to_string(parts).expect("reply parts serialize"));
    conn.execute(
        "INSERT INTO messages
            (task_id, role, content, kind, source, error_kind, error_detail, parts, turn_id, attempt_no, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        rusqlite::params![
            msg.chat.task_id(),
            msg.role,
            msg.content,
            msg.kind,
            source,
            msg.error_kind,
            msg.error_detail,
            parts,
            turn_id,
            attempt_no,
            now_iso8601(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 1つの会話の発言を取得する。ターンを持つ行は`turn_id`ごとの最新試行に絞り、通常発言が
/// 1行も残っていないターン(編集・再試行・削除で破棄されたターン)は丸ごと除く。
/// 応答生成以外の経路での操作の記録は`turn_id`を持たないので、常に残る。
/// 編集・再試行で作り直すターンの、破棄された試行のツール実行記録はDBに残すが
/// ([`soft_delete_normal_from`])、会話に並べると直後の編集後の発言が新規送信と見分けられなく
/// なるため、ここで外す。
pub fn list_for_chat(conn: &Connection, chat: Chat) -> Result<Vec<Message>> {
    let mut rows = list_rows_for_chat(conn, chat)?;
    let mut attached = attachments::views_for_chat(conn, chat)?;
    for row in &mut rows {
        row.attachments = attached.remove(&row.id).unwrap_or_default();
    }
    Ok(rows)
}

/// [`list_for_chat`]の、添付を埋めない形。添付を中身ごと別に引く呼び出し側
/// (`orchestration::history`)が、同じ添付を2回引かないために使う。
pub(crate) fn list_rows_for_chat(conn: &Connection, chat: Chat) -> Result<Vec<Message>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {MESSAGE_COLUMNS}
         FROM messages
         WHERE task_id IS ?1
           AND deleted_at IS NULL
           AND (
             turn_id IS NULL
             OR (
               attempt_no = (
                 SELECT MAX(attempt_no) FROM messages m2
                 WHERE m2.turn_id = messages.turn_id AND m2.deleted_at IS NULL
               )
               AND EXISTS (
                 SELECT 1 FROM messages m3
                 WHERE m3.turn_id = messages.turn_id
                   AND m3.attempt_no = messages.attempt_no
                   AND m3.kind = 'normal'
                   AND m3.deleted_at IS NULL
               )
             )
           )
         ORDER BY created_at ASC, id ASC"
    ))?;
    let rows = stmt
        .query_map([chat.task_id()], message_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 会話のツール実行記録すべて。[`list_rows_for_chat`]と違い、捨てた試行(古い試行・通常発言の
/// 生き残っていないターン)の記録も含む。履歴に載らない試行で実行したことをモデルに伝えるため
/// (`orchestration::history`)。
pub(crate) fn list_tool_records_for_chat(conn: &Connection, chat: Chat) -> Result<Vec<Message>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {MESSAGE_COLUMNS}
         FROM messages
         WHERE task_id IS ?1
           AND deleted_at IS NULL
           AND kind = 'tool_execution'
         ORDER BY created_at ASC, id ASC"
    ))?;
    let rows = stmt
        .query_map([chat.task_id()], message_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// idで1件取得する(論理削除済みは対象外)。
pub fn find_message(conn: &Connection, id: i64) -> Result<Option<Message>> {
    let found = conn
        .query_row(
            &format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE id = ?1 AND deleted_at IS NULL"),
            [id],
            message_from_row,
        )
        .optional()?;
    found
        .map(|mut m| {
            m.attachments = attachments::views_for_message(conn, m.id)?;
            Ok(m)
        })
        .transpose()
}

/// [`message_from_row`]が読む列の並び。
const MESSAGE_COLUMNS: &str = "id, task_id, role, content, kind, source, error_kind, error_detail, parts, turn_id, attempt_no, created_at";

/// 添付は呼び出し側が埋める。
fn message_from_row(row: &rusqlite::Row) -> rusqlite::Result<Message> {
    Ok(Message {
        id: row.get(0)?,
        task_id: row.get(1)?,
        role: row.get(2)?,
        content: row.get(3)?,
        kind: row.get(4)?,
        source: row.get(5)?,
        error_kind: row.get(6)?,
        error_detail: row.get(7)?,
        parts: match row.get::<_, Option<String>>(8)? {
            Some(json) => serde_json::from_str(&json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, e.into())
            })?,
            None => Vec::new(),
        },
        turn_id: row.get(9)?,
        attempt_no: row.get(10)?,
        created_at: row.get(11)?,
        attachments: Vec::new(),
    })
}

/// 発言を1件だけ論理削除する。対象はユーザー発言とターンの返信(アシスタント発言・
/// エラー発言)の通常発言のみで、ツール実行記録は消さない。`deleted_at`を立てるだけなので、
/// NULLに戻せば復元できる。
///
/// 発言の削除の入口(`orchestration::delete_message`)には使わない。途中の発言だけを消すと、その
/// 後ろに送った会話の前提が変わるため、削除は[`soft_delete_normal_from`]で以降をまとめて消す。
pub fn soft_delete_message(conn: &Connection, id: i64) -> Result<()> {
    let msg = find_message(conn, id)?.ok_or(CoreError::MessageNotFound(id))?;
    if msg.kind != Kind::Normal || !matches!(msg.role, Role::User | Role::Assistant | Role::Error) {
        return Err(CoreError::InvalidMessageOperation(
            "delete is only allowed for normal user/assistant/error messages".to_string(),
        ));
    }
    let updated = conn.execute(
        "UPDATE messages SET deleted_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
        rusqlite::params![now_iso8601(), id],
    )?;
    if updated == 0 {
        return Err(CoreError::MessageNotFound(id));
    }
    Ok(())
}

/// `from_id`以降(自身を含む)の通常発言を一括で論理削除する(編集・再試行で、以降を
/// 作り直す前に使う)。
///
/// ツール実行記録は消さない。残った記録は[`list_for_chat`]が会話から外す。実行記録の保全を
/// 優先しているので、表示を合わせるために記録の側を消してはならない。消えたターンの記録は、
/// 続けて[`soft_delete_turn_records_after`]か[`soft_delete_trailing_turn_records`]で消す。
pub fn soft_delete_normal_from(conn: &Connection, chat: Chat, from_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE messages SET deleted_at = ?1
         WHERE task_id IS ?2 AND id >= ?3 AND kind = 'normal' AND deleted_at IS NULL",
        rusqlite::params![now_iso8601(), chat.task_id(), from_id],
    )?;
    Ok(())
}

/// ユーザー発言`user_message_id`に答えたターンの`turn_id`(行の順)。発言の直後から次の
/// ユーザー発言の手前までに行のあるターンが当たる。返信の無いまま終わったターンのあとに
/// 応答を生成し直すと(`orchestration::generate_reply`)、1つの発言に複数のターンが答える。
///
/// 論理削除した行も見る。答えたターンの行が削除で消えていても、その後ろの発言に答えたターンを
/// 取り違えないため。同じ理由で、区切りの次のユーザー発言も削除したものを含める。
pub fn turns_answering(conn: &Connection, chat: Chat, user_message_id: i64) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT turn_id FROM messages
         WHERE task_id IS ?1 AND id > ?2 AND turn_id IS NOT NULL
           AND NOT EXISTS (
             SELECT 1 FROM messages u
             WHERE u.task_id IS ?1 AND u.role = 'user' AND u.id > ?2 AND u.id < messages.id
           )
         GROUP BY turn_id
         ORDER BY MIN(id) ASC",
    )?;
    let turns = stmt
        .query_map(rusqlite::params![chat.task_id(), user_message_id], |row| {
            row.get(0)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(turns)
}

/// 行`after_id`より後ろにある、`kept_turns`以外のターンのツール実行記録を論理削除する。
/// 編集・再試行で、作り直す地点より後ろのターンの記録を以後モデルに送らないため
/// (`docs/spec/data-model/messages.md`「ターン境界」)。作り直すターン自身の記録は`kept_turns`で
/// 残し、捨てた試行の記録として伝える。
///
/// 応答生成以外の経路での操作の記録(`turn_id`が無い)は会話に並ぶ行なので消さない。
pub fn soft_delete_turn_records_after(
    conn: &Connection,
    chat: Chat,
    after_id: i64,
    kept_turns: &[String],
) -> Result<()> {
    let kept = serde_json::to_string(kept_turns).expect("turn ids serialize");
    conn.execute(
        "UPDATE messages SET deleted_at = ?1
         WHERE task_id IS ?2
           AND kind = 'tool_execution'
           AND turn_id IS NOT NULL
           AND turn_id NOT IN (SELECT value FROM json_each(?4))
           AND deleted_at IS NULL
           AND id > ?3",
        rusqlite::params![now_iso8601(), chat.task_id(), after_id, kept],
    )?;
    Ok(())
}

/// 生き残っている最後の通常発言より後ろの、ターンのツール実行記録を論理削除する(通常発言が
/// 1つも残っていなければ、会話のターンの記録すべて)。発言の削除で、
/// [`soft_delete_normal_from`]のあとに使う。編集・再試行と違い、消した発言のターン自身の記録も
/// 消す。
///
/// 消した発言のターンの記録は、再試行で捨てた試行の分も含め、すべてこの範囲に入る。編集で
/// 置き換える前の発言のターンの記録も、置き換えたあとの発言より前にあるので、その発言を消せば
/// 入る。
pub fn soft_delete_trailing_turn_records(conn: &Connection, chat: Chat) -> Result<()> {
    let last_normal: Option<i64> = conn.query_row(
        "SELECT MAX(id) FROM messages
         WHERE task_id IS ?1 AND kind = 'normal' AND deleted_at IS NULL",
        [chat.task_id()],
        |row| row.get(0),
    )?;
    soft_delete_turn_records_after(conn, chat, last_normal.unwrap_or(0), &[])
}

/// タスクの会話を始めた側。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opener {
    User,
    /// ユーザー発言より先に、SCITLの応答生成が始まった(聞き取りの開始)。
    Reply,
}

/// タスクの会話を始めた側。まだ1行も無ければ`None`。
///
/// 論理削除した行も含めた最初の行で決める。最初のユーザー発言を消しても編集で置き換えても
/// 行は残るので、答えが変わらない。応答生成以外の経路での操作の記録はこの会話の発言ではないので
/// 見ない。
pub fn opener(conn: &Connection, task_id: i64) -> Result<Option<Opener>> {
    let role: Option<Role> = conn
        .query_row(
            "SELECT role FROM messages
             WHERE task_id = ?1 AND (role = 'user' OR turn_id IS NOT NULL)
             ORDER BY created_at ASC, id ASC
             LIMIT 1",
            [task_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(role.map(|role| {
        if role == Role::User {
            Opener::User
        } else {
            Opener::Reply
        }
    }))
}

/// 指定`turn_id`の次の試行番号を採番する。削除された古い試行と番号が重ならないよう、
/// 論理削除済みの試行も`MAX`の対象に含める。
pub fn next_attempt_no(conn: &Connection, turn_id: &str) -> Result<i64> {
    let max: Option<i64> = conn.query_row(
        "SELECT MAX(attempt_no) FROM messages WHERE turn_id = ?1",
        [turn_id],
        |row| row.get(0),
    )?;
    Ok(max.unwrap_or(0) + 1)
}

/// テストで返信の行を書くときの中身。試行のそれまでの実行記録を記録ごとに1ラウンドとして指し、
/// 最後のラウンドに`text`を置く(`0008_reply_parts.sql`が移した形と同じ)。
#[cfg(test)]
pub(crate) fn parts_for_reply(
    conn: &Connection,
    turn_id: &str,
    attempt_no: i64,
    text: Option<&str>,
) -> Vec<ReplyPart> {
    let records: Vec<i64> = conn
        .prepare(
            "SELECT id FROM messages
             WHERE kind = 'tool_execution' AND turn_id = ?1 AND attempt_no = ?2
               AND deleted_at IS NULL
             ORDER BY id",
        )
        .unwrap()
        .query_map(rusqlite::params![turn_id, attempt_no], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let last = u32::try_from(records.len()).unwrap() + 1;
    let mut parts: Vec<ReplyPart> = records
        .into_iter()
        .zip(1..)
        .map(|(record, round)| ReplyPart::Tool { round, record })
        .collect();
    parts.extend(
        text.filter(|t| !t.trim().is_empty())
            .map(|t| ReplyPart::Text {
                round: last,
                text: t.to_string(),
            }),
    );
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn seed_task(conn: &Connection) -> i64 {
        let now = now_iso8601();
        conn.execute(
            "INSERT INTO tasks (created_at, updated_at) VALUES (?1, ?1)",
            [&now],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn retry_only_shows_latest_attempt() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);

        insert_message(
            &conn,
            NewMessage {
                chat: Chat::Task(task_id),
                role: Role::User,
                content: "こんにちは",
                kind: Kind::Normal,
                origin: Origin::User,
                error_kind: None,
                error_detail: None,
                parts: None,
            },
        )
        .unwrap();

        insert_message(
            &conn,
            NewMessage {
                chat: Chat::Task(task_id),
                role: Role::Error,
                content: "モデルからの応答が空でした",
                kind: Kind::Normal,
                origin: Origin::Turn {
                    turn_id: "turn-1",
                    attempt_no: 1,
                },
                error_kind: Some("empty_response"),
                error_detail: None,
                parts: Some(&[]),
            },
        )
        .unwrap();

        insert_message(
            &conn,
            NewMessage {
                chat: Chat::Task(task_id),
                role: Role::Assistant,
                content: "再試行後の応答",
                kind: Kind::Normal,
                origin: Origin::Turn {
                    turn_id: "turn-1",
                    attempt_no: 2,
                },
                error_kind: None,
                error_detail: None,
                parts: Some(&[]),
            },
        )
        .unwrap();

        let messages = list_for_chat(&conn, Chat::Task(task_id)).unwrap();
        let contents: Vec<_> = messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, vec!["こんにちは", "再試行後の応答"]);
    }

    #[test]
    fn operation_records_without_turn_are_always_shown() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);

        insert_message(
            &conn,
            NewMessage {
                chat: Chat::Task(task_id),
                role: Role::Tool,
                content: "{}",
                kind: Kind::ToolExecution,
                origin: Origin::Operation(OperationSource::Ui),
                error_kind: None,
                error_detail: None,
                parts: None,
            },
        )
        .unwrap();

        let messages = list_for_chat(&conn, Chat::Task(task_id)).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].source.as_deref(), Some("ui"));
    }

    #[test]
    fn only_operation_records_outside_a_turn_carry_a_source() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let insert = |role, kind, origin| {
            insert_message(
                &conn,
                NewMessage {
                    chat: Chat::Task(task_id),
                    role,
                    content: "{}",
                    kind,
                    origin,
                    error_kind: None,
                    error_detail: None,
                    parts: (kind == Kind::Normal).then_some(&[]),
                },
            )
        };
        let raw = |source: Option<&str>, turn_id: Option<&str>| {
            conn.execute(
                "INSERT INTO messages (task_id, role, content, kind, source, turn_id, attempt_no, created_at)
                 VALUES (?1, 'tool', '{}', 'tool_execution', ?2, ?3, ?4, '2026-01-01T00:00:00Z')",
                rusqlite::params![task_id, source, turn_id, turn_id.map(|_| 1)],
            )
        };

        let record = insert(
            Role::Tool,
            Kind::ToolExecution,
            Origin::Operation(OperationSource::Ui),
        )
        .unwrap();
        // 経路の印を発言に付けることはできない(CLIからの発言もただのユーザー発言)。
        assert!(insert(
            Role::User,
            Kind::Normal,
            Origin::Operation(OperationSource::Ui)
        )
        .is_err());
        // ターンの記録に印を付けること、印の無い記録をターンの外に書くことはできない。
        assert!(raw(Some("ui"), Some("turn-1")).is_err());
        assert!(raw(None, None).is_err());
        assert!(conn
            .execute(
                "UPDATE messages SET turn_id = 'turn-1', attempt_no = 1 WHERE id = ?1",
                [record]
            )
            .is_err());
        // 論理削除は印に関わらない。
        conn.execute(
            "UPDATE messages SET deleted_at = '2026-01-01T00:00:00Z' WHERE id = ?1",
            [record],
        )
        .unwrap();
    }

    #[test]
    fn tool_role_is_reserved_for_tool_execution_records() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let insert = |role, kind, content| {
            insert_message(
                &conn,
                NewMessage {
                    chat: Chat::Task(task_id),
                    role,
                    content,
                    kind,
                    origin: Origin::Turn {
                        turn_id: "turn-1",
                        attempt_no: 1,
                    },
                    error_kind: None,
                    error_detail: None,
                    parts: (kind == Kind::Normal).then_some(&[]),
                },
            )
        };

        assert!(insert(Role::Assistant, Kind::ToolExecution, "{}").is_err());
        assert!(insert(Role::Tool, Kind::Normal, "結果").is_err());
        let id = insert(Role::Tool, Kind::ToolExecution, "{}").unwrap();
        assert!(conn
            .execute("UPDATE messages SET kind = 'normal' WHERE id = ?1", [id])
            .is_err());
    }

    #[test]
    fn error_role_round_trips_with_error_kind() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);

        insert_message(
            &conn,
            NewMessage {
                chat: Chat::Task(task_id),
                role: Role::Error,
                content: "APIキーが設定されていません",
                kind: Kind::Normal,
                origin: Origin::Turn {
                    turn_id: "turn-1",
                    attempt_no: 1,
                },
                error_kind: Some("no_api_key"),
                error_detail: Some("HTTP 401: invalid key"),
                parts: Some(&[]),
            },
        )
        .unwrap();

        let messages = list_for_chat(&conn, Chat::Task(task_id)).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::Error);
        assert_eq!(messages[0].error_kind.as_deref(), Some("no_api_key"));
        assert_eq!(
            messages[0].error_detail.as_deref(),
            Some("HTTP 401: invalid key")
        );
    }

    /// 詳細を持てるのはエラー発言だけで、空文字は未設定(NULL)と区別させない
    /// (`0003_error_detail.sql`のトリガー)。
    #[test]
    fn error_detail_is_rejected_outside_error_messages_and_when_empty() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);

        let on_user = insert_message(
            &conn,
            NewMessage {
                chat: Chat::Task(task_id),
                role: Role::User,
                content: "こんにちは",
                kind: Kind::Normal,
                origin: Origin::User,
                error_kind: None,
                error_detail: Some("HTTP 500: boom"),
                parts: None,
            },
        );
        assert!(on_user.is_err());

        let empty = insert_message(
            &conn,
            NewMessage {
                chat: Chat::Task(task_id),
                role: Role::Error,
                content: "LLMプロバイダーとの通信に失敗しました。",
                kind: Kind::Normal,
                origin: Origin::User,
                error_kind: Some("provider"),
                error_detail: Some(""),
                parts: None,
            },
        );
        assert!(empty.is_err());

        let id = insert_message(
            &conn,
            NewMessage {
                chat: Chat::Task(task_id),
                role: Role::Error,
                content: "LLMプロバイダーとの通信に失敗しました。",
                kind: Kind::Normal,
                origin: Origin::User,
                error_kind: Some("provider"),
                error_detail: Some("HTTP 500: boom"),
                parts: None,
            },
        )
        .unwrap();
        assert!(conn
            .execute(
                "UPDATE messages SET role = 'assistant', error_kind = NULL WHERE id = ?1",
                [id]
            )
            .is_err());
    }

    /// 中身を持つのはターンの返信の行だけで、それには必ず持つ(`0008_reply_parts.sql`のトリガー)。
    #[test]
    fn parts_are_held_by_exactly_the_replies_of_turns() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let turn = Origin::Turn {
            turn_id: "turn-1",
            attempt_no: 1,
        };
        let parts = [ReplyPart::Text {
            round: 1,
            text: "本文".to_string(),
        }];
        let insert = |role, kind, origin, parts| {
            insert_message(
                &conn,
                NewMessage {
                    chat: Chat::Task(task_id),
                    role,
                    content: "{}",
                    kind,
                    origin,
                    error_kind: (role == Role::Error).then_some("provider"),
                    error_detail: None,
                    parts,
                },
            )
        };

        assert!(insert(Role::Assistant, Kind::Normal, turn, None).is_err());
        assert!(insert(Role::User, Kind::Normal, Origin::User, Some(&parts)).is_err());
        assert!(insert(Role::Tool, Kind::ToolExecution, turn, Some(&parts)).is_err());
        insert(Role::Error, Kind::Normal, turn, Some(&[])).unwrap();
        let id = insert(Role::Assistant, Kind::Normal, turn, Some(&parts)).unwrap();
        assert_eq!(find_message(&conn, id).unwrap().unwrap().parts, parts);
        assert!(conn
            .execute("UPDATE messages SET parts = '{}' WHERE id = ?1", [id])
            .is_err());
        assert!(conn
            .execute("UPDATE messages SET parts = NULL WHERE id = ?1", [id])
            .is_err());
    }

    #[test]
    fn error_role_without_error_kind_is_rejected_by_check_constraint() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);

        let result = insert_message(
            &conn,
            NewMessage {
                chat: Chat::Task(task_id),
                role: Role::Error,
                content: "壊れた呼び出し",
                kind: Kind::Normal,
                origin: Origin::User,
                error_kind: None,
                error_detail: None,
                parts: None,
            },
        );
        assert!(result.is_err());
    }

    /// 複数回再試行したターンを、さらに前方の発言の編集で丸ごと破棄した場合。旧試行の行は
    /// `MAX(attempt_no)`で、最新試行の行は「通常発言が残っていない」判定で外れる。
    #[test]
    fn a_retried_turn_discarded_by_a_later_edit_disappears_from_every_attempt() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);

        let user_id = insert_message(
            &conn,
            NewMessage {
                chat: Chat::Task(task_id),
                role: Role::User,
                content: "工程を作って",
                kind: Kind::Normal,
                origin: Origin::User,
                error_kind: None,
                error_detail: None,
                parts: None,
            },
        )
        .unwrap();

        // 同じturn_idのまま2回試行し、どちらもツール実行記録と通常応答を残す。
        for attempt in 1..=2 {
            insert_message(
                &conn,
                NewMessage {
                    chat: Chat::Task(task_id),
                    role: Role::Tool,
                    content: r#"{"tool":"add_steps"}"#,
                    kind: Kind::ToolExecution,
                    origin: Origin::Turn {
                        turn_id: "turn-1",
                        attempt_no: attempt,
                    },
                    error_kind: None,
                    error_detail: None,
                    parts: None,
                },
            )
            .unwrap();
            insert_message(
                &conn,
                NewMessage {
                    chat: Chat::Task(task_id),
                    role: Role::Assistant,
                    content: "追加しました",
                    kind: Kind::Normal,
                    origin: Origin::Turn {
                        turn_id: "turn-1",
                        attempt_no: attempt,
                    },
                    error_kind: None,
                    error_detail: None,
                    parts: Some(&[]),
                },
            )
            .unwrap();
        }

        // 前方のユーザー発言を編集した場合。通常発言は全試行分が消える。
        soft_delete_normal_from(&conn, Chat::Task(task_id), user_id).unwrap();

        // 旧試行・最新試行のどちらのツール実行記録も会話には出ない。
        let remaining = list_for_chat(&conn, Chat::Task(task_id)).unwrap();
        assert!(
            remaining.is_empty(),
            "unexpected remaining rows: {remaining:?}"
        );

        // 記録自体は全試行分がDBに残る。
        let kept: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages
                 WHERE kind = 'tool_execution' AND deleted_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, 2);
    }

    #[test]
    fn soft_delete_message_hides_it_but_keeps_the_row() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let id = insert_message(
            &conn,
            NewMessage {
                chat: Chat::Task(task_id),
                role: Role::User,
                content: "こんにちは",
                kind: Kind::Normal,
                origin: Origin::User,
                error_kind: None,
                error_detail: None,
                parts: None,
            },
        )
        .unwrap();

        soft_delete_message(&conn, id).unwrap();

        assert!(list_for_chat(&conn, Chat::Task(task_id))
            .unwrap()
            .is_empty());
        // 物理削除ではないことを確認する(deleted_atを無視すれば行は残っている)。
        let deleted_at: Option<String> = conn
            .query_row("SELECT deleted_at FROM messages WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(deleted_at.is_some());
    }

    #[test]
    fn soft_delete_message_rejects_tool_execution_rows() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let id = insert_message(
            &conn,
            NewMessage {
                chat: Chat::Task(task_id),
                role: Role::Tool,
                content: r#"{"tool":"add_steps"}"#,
                kind: Kind::ToolExecution,
                origin: Origin::Turn {
                    turn_id: "turn-1",
                    attempt_no: 1,
                },
                error_kind: None,
                error_detail: None,
                parts: None,
            },
        )
        .unwrap();

        let result = soft_delete_message(&conn, id);
        assert!(matches!(result, Err(CoreError::InvalidMessageOperation(_))));
    }

    #[test]
    fn soft_delete_message_on_missing_id_is_not_found() {
        let conn = db::open_in_memory().unwrap();
        let result = soft_delete_message(&conn, 999);
        assert!(matches!(result, Err(CoreError::MessageNotFound(999))));
    }

    #[test]
    fn soft_delete_normal_from_cascades_but_spares_tool_execution_rows() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);

        let user_id = insert_message(
            &conn,
            NewMessage {
                chat: Chat::Task(task_id),
                role: Role::User,
                content: "工程を追加して",
                kind: Kind::Normal,
                origin: Origin::User,
                error_kind: None,
                error_detail: None,
                parts: None,
            },
        )
        .unwrap();

        insert_message(
            &conn,
            NewMessage {
                chat: Chat::Task(task_id),
                role: Role::Tool,
                content: r#"{"tool":"add_steps"}"#,
                kind: Kind::ToolExecution,
                origin: Origin::Turn {
                    turn_id: "turn-1",
                    attempt_no: 1,
                },
                error_kind: None,
                error_detail: None,
                parts: None,
            },
        )
        .unwrap();

        let assistant_id = insert_message(
            &conn,
            NewMessage {
                chat: Chat::Task(task_id),
                role: Role::Assistant,
                content: "追加しました",
                kind: Kind::Normal,
                origin: Origin::Turn {
                    turn_id: "turn-1",
                    attempt_no: 1,
                },
                error_kind: None,
                error_detail: None,
                parts: Some(&[]),
            },
        )
        .unwrap();

        // ユーザー発言以降(自身を含む)をすべて論理削除する(編集と同じ形)。
        soft_delete_normal_from(&conn, Chat::Task(task_id), user_id).unwrap();

        // 通常発言はすべて消え、ツール実行記録は消えないが、ターンごと会話から外れる。
        let remaining = list_for_chat(&conn, Chat::Task(task_id)).unwrap();
        assert!(
            remaining.is_empty(),
            "unexpected remaining rows: {remaining:?}"
        );

        // ツール実行記録の行自体はDBに残る。
        let tool_deleted_at: Option<String> = conn
            .query_row(
                "SELECT deleted_at FROM messages WHERE kind = 'tool_execution'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(tool_deleted_at.is_none());

        let assistant_deleted_at: Option<String> = conn
            .query_row(
                "SELECT deleted_at FROM messages WHERE id = ?1",
                [assistant_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(assistant_deleted_at.is_some());
    }

    /// 生き残っている最後の通常発言より後ろのターンの記録だけを消す。それより前のターンの記録、
    /// 応答生成以外の経路での操作の記録、別の会話の記録は消さない。
    #[test]
    fn soft_delete_trailing_turn_records_removes_only_turn_records_after_the_last_message() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let chat = Chat::Task(task_id);
        let insert = |chat, role, origin| {
            insert_message(
                &conn,
                NewMessage {
                    chat,
                    role,
                    content: if role == Role::Tool { "{}" } else { "本文" },
                    kind: if role == Role::Tool {
                        Kind::ToolExecution
                    } else {
                        Kind::Normal
                    },
                    origin,
                    error_kind: None,
                    error_detail: None,
                    parts: (role != Role::Tool && matches!(origin, Origin::Turn { .. }))
                        .then_some(&[]),
                },
            )
            .unwrap()
        };
        let turn = |turn_id, attempt_no| Origin::Turn {
            turn_id,
            attempt_no,
        };
        let operation = Origin::Operation(OperationSource::Ui);

        insert(chat, Role::User, Origin::User);
        let kept = insert(chat, Role::Tool, turn("turn-1", 1));
        insert(chat, Role::Assistant, turn("turn-1", 1));
        let second = insert(chat, Role::User, Origin::User);
        let retried = insert(chat, Role::Tool, turn("turn-2", 1));
        insert(chat, Role::Assistant, turn("turn-2", 1));
        let by_ui = insert(chat, Role::Tool, operation);
        let latest = insert(chat, Role::Tool, turn("turn-2", 2));
        insert(chat, Role::Assistant, turn("turn-2", 2));
        let general = insert(Chat::General, Role::Tool, turn("turn-3", 1));

        soft_delete_normal_from(&conn, chat, second).unwrap();
        soft_delete_trailing_turn_records(&conn, chat).unwrap();

        let alive = |id| find_message(&conn, id).unwrap().is_some();
        assert!(alive(kept));
        assert!(!alive(retried));
        assert!(alive(by_ui));
        assert!(!alive(latest));
        assert!(alive(general));
        assert!(list_tool_records_for_chat(&conn, chat)
            .unwrap()
            .iter()
            .all(|m| m.id == kept || m.id == by_ui));
    }

    /// 生き残った発言より前にある記録は、返信を持たないターンのものでも消さない。失敗したターン
    /// (エラー発言が残る)の記録と、編集で置き換える前の発言のターンの記録が当たる。
    #[test]
    fn soft_delete_trailing_turn_records_spares_records_before_a_surviving_message() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let chat = Chat::Task(task_id);
        let insert = |role, origin| {
            insert_message(
                &conn,
                NewMessage {
                    chat,
                    role,
                    content: if role == Role::Tool { "{}" } else { "本文" },
                    kind: if role == Role::Tool {
                        Kind::ToolExecution
                    } else {
                        Kind::Normal
                    },
                    origin,
                    error_kind: (role == Role::Error).then_some("provider"),
                    error_detail: None,
                    parts: (role != Role::Tool && matches!(origin, Origin::Turn { .. }))
                        .then_some(&[]),
                },
            )
            .unwrap()
        };
        let turn = |turn_id| Origin::Turn {
            turn_id,
            attempt_no: 1,
        };

        insert(Role::User, Origin::User);
        let failed = insert(Role::Tool, turn("turn-1"));
        insert(Role::Error, turn("turn-1"));
        let original = insert(Role::User, Origin::User);
        let before_edit = insert(Role::Tool, turn("turn-2"));
        insert(Role::Assistant, turn("turn-2"));
        soft_delete_normal_from(&conn, chat, original).unwrap();
        insert(Role::User, Origin::User);
        let after_edit = insert(Role::Tool, turn("turn-3"));
        let reply = insert(Role::Assistant, turn("turn-3"));

        soft_delete_normal_from(&conn, chat, reply).unwrap();
        soft_delete_trailing_turn_records(&conn, chat).unwrap();

        let alive = |id| find_message(&conn, id).unwrap().is_some();
        assert!(alive(failed));
        assert!(alive(before_edit));
        assert!(!alive(after_edit));
    }

    /// 通常発言が1つも残っていなければ、会話のターンの記録すべてを消す。
    #[test]
    fn soft_delete_trailing_turn_records_removes_every_turn_record_of_an_emptied_chat() {
        let conn = db::open_in_memory().unwrap();
        let insert = |role, kind, origin| {
            insert_message(
                &conn,
                NewMessage {
                    chat: Chat::General,
                    role,
                    content: "{}",
                    kind,
                    origin,
                    error_kind: None,
                    error_detail: None,
                    parts: (matches!(origin, Origin::Turn { .. }) && kind == Kind::Normal)
                        .then_some(&[]),
                },
            )
            .unwrap()
        };
        let turn = Origin::Turn {
            turn_id: "turn-1",
            attempt_no: 1,
        };
        // 以前に消した発言のターンの記録が残っている会話。
        let stale = insert(Role::Tool, Kind::ToolExecution, turn);
        let user = insert(Role::User, Kind::Normal, Origin::User);

        soft_delete_normal_from(&conn, Chat::General, user).unwrap();
        soft_delete_trailing_turn_records(&conn, Chat::General).unwrap();

        assert!(find_message(&conn, stale).unwrap().is_none());
    }

    /// 編集・再試行で消すのは、作り直す地点より後ろの、残すターン以外の記録だけ。残すターンの
    /// 前の試行の記録、それより前のターンの記録、操作の記録は消さない。
    #[test]
    fn soft_delete_turn_records_after_spares_the_kept_turn() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let chat = Chat::Task(task_id);
        let insert = |role, origin| {
            insert_message(
                &conn,
                NewMessage {
                    chat,
                    role,
                    content: if role == Role::Tool { "{}" } else { "本文" },
                    kind: if role == Role::Tool {
                        Kind::ToolExecution
                    } else {
                        Kind::Normal
                    },
                    origin,
                    error_kind: None,
                    error_detail: None,
                    parts: (role != Role::Tool && matches!(origin, Origin::Turn { .. }))
                        .then_some(&[]),
                },
            )
            .unwrap()
        };
        let turn = |turn_id, attempt_no| Origin::Turn {
            turn_id,
            attempt_no,
        };

        insert(Role::User, Origin::User);
        let earlier = insert(Role::Tool, turn("turn-1", 1));
        insert(Role::Assistant, turn("turn-1", 1));
        let user = insert(Role::User, Origin::User);
        let old_attempt = insert(Role::Tool, turn("turn-2", 1));
        insert(Role::Assistant, turn("turn-2", 1));
        let kept = insert(Role::Tool, turn("turn-2", 2));
        let reply = insert(Role::Assistant, turn("turn-2", 2));
        insert(Role::User, Origin::User);
        let later = insert(Role::Tool, turn("turn-3", 1));
        let by_ui = insert(Role::Tool, Origin::Operation(OperationSource::Ui));
        insert(Role::Assistant, turn("turn-3", 1));

        // `user`を編集する。答えたターン(`turn-2`)の記録は`user`より後ろにあっても残す。
        let answered_by = turns_answering(&conn, chat, user).unwrap();
        assert_eq!(answered_by, vec!["turn-2".to_string()]);
        soft_delete_normal_from(&conn, chat, user).unwrap();
        soft_delete_turn_records_after(&conn, chat, user, &answered_by).unwrap();

        let alive = |id| find_message(&conn, id).unwrap().is_some();
        assert!(alive(earlier));
        assert!(alive(old_attempt));
        assert!(alive(kept));
        assert!(!alive(later));
        assert!(alive(by_ui));
        assert!(find_message(&conn, reply).unwrap().is_none());

        // 残すターンが無ければ(答えのない発言の編集)、後ろのターンの記録はすべて消す。
        soft_delete_turn_records_after(&conn, chat, user, &[]).unwrap();
        assert!(!alive(kept));
        assert!(alive(earlier));
    }

    /// 答えたターンの行が削除で消えていても、後ろの発言に答えたターンを返さない。答えたターンの
    /// 行が1つも無ければ空。
    #[test]
    fn turns_answering_stops_at_the_next_user_message() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let chat = Chat::Task(task_id);
        let insert = |role, origin| {
            insert_message(
                &conn,
                NewMessage {
                    chat,
                    role,
                    content: "本文",
                    kind: Kind::Normal,
                    origin,
                    error_kind: None,
                    error_detail: None,
                    parts: matches!(origin, Origin::Turn { .. }).then_some(&[]),
                },
            )
            .unwrap()
        };
        let turn = |turn_id| Origin::Turn {
            turn_id,
            attempt_no: 1,
        };
        let first = insert(Role::User, Origin::User);
        let deleted = insert(Role::Assistant, turn("turn-1"));
        soft_delete_message(&conn, deleted).unwrap();
        // 返信の無いまま終わったあとに生成し直したターンも、同じ発言に答えている。
        insert(Role::Assistant, turn("turn-1b"));
        let unanswered = insert(Role::User, Origin::User);
        insert(Role::User, Origin::User);
        insert(Role::Assistant, turn("turn-2"));
        let last = insert(Role::User, Origin::User);

        let answering = |id| turns_answering(&conn, chat, id).unwrap();
        assert_eq!(answering(first), vec!["turn-1", "turn-1b"]);
        assert!(answering(unanswered).is_empty());
        assert!(answering(last).is_empty());
    }

    #[test]
    fn next_attempt_no_increments_and_ignores_deleted_rows() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);

        assert_eq!(next_attempt_no(&conn, "turn-1").unwrap(), 1);

        let id = insert_message(
            &conn,
            NewMessage {
                chat: Chat::Task(task_id),
                role: Role::Assistant,
                content: "1回目の応答",
                kind: Kind::Normal,
                origin: Origin::Turn {
                    turn_id: "turn-1",
                    attempt_no: 1,
                },
                error_kind: None,
                error_detail: None,
                parts: Some(&[]),
            },
        )
        .unwrap();
        assert_eq!(next_attempt_no(&conn, "turn-1").unwrap(), 2);

        soft_delete_message(&conn, id).unwrap();
        // 削除済みでも採番は巻き戻らない。
        assert_eq!(next_attempt_no(&conn, "turn-1").unwrap(), 2);
    }

    #[test]
    fn find_message_ignores_deleted_rows() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let id = insert_message(
            &conn,
            NewMessage {
                chat: Chat::Task(task_id),
                role: Role::User,
                content: "こんにちは",
                kind: Kind::Normal,
                origin: Origin::User,
                error_kind: None,
                error_detail: None,
                parts: None,
            },
        )
        .unwrap();

        assert!(find_message(&conn, id).unwrap().is_some());
        soft_delete_message(&conn, id).unwrap();
        assert!(find_message(&conn, id).unwrap().is_none());
    }

    #[test]
    fn opener_is_decided_by_the_first_row_even_after_it_is_deleted() {
        let conn = db::open_in_memory().unwrap();
        let insert = |task_id, role, origin| {
            insert_message(
                &conn,
                NewMessage {
                    chat: Chat::Task(task_id),
                    role,
                    content: if role == Role::Tool { "{}" } else { "本文" },
                    kind: if role == Role::Tool {
                        Kind::ToolExecution
                    } else {
                        Kind::Normal
                    },
                    origin,
                    error_kind: None,
                    error_detail: None,
                    parts: (role != Role::Tool && matches!(origin, Origin::Turn { .. }))
                        .then_some(&[]),
                },
            )
            .unwrap()
        };

        let by_user = seed_task(&conn);
        assert_eq!(opener(&conn, by_user).unwrap(), None);
        let turn = |turn_id| Origin::Turn {
            turn_id,
            attempt_no: 1,
        };
        let first = insert(by_user, Role::User, Origin::User);
        insert(by_user, Role::Assistant, turn("turn-1"));
        soft_delete_message(&conn, first).unwrap();
        assert_eq!(opener(&conn, by_user).unwrap(), Some(Opener::User));

        let by_reply = seed_task(&conn);
        // 応答生成以外の経路での操作の記録は会話の発言ではない。
        insert(by_reply, Role::Tool, Origin::Operation(OperationSource::Ui));
        let reply = insert(by_reply, Role::Assistant, turn("turn-2"));
        insert(by_reply, Role::User, Origin::User);
        soft_delete_normal_from(&conn, Chat::Task(by_reply), reply).unwrap();
        assert_eq!(opener(&conn, by_reply).unwrap(), Some(Opener::Reply));
    }
    #[test]
    fn general_and_task_chats_do_not_see_each_others_messages() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let insert = |chat: Chat, content| {
            insert_message(
                &conn,
                NewMessage {
                    chat,
                    role: Role::User,
                    content,
                    kind: Kind::Normal,
                    origin: Origin::User,
                    error_kind: None,
                    error_detail: None,
                    parts: None,
                },
            )
            .unwrap()
        };
        let general_first = insert(Chat::General, "総合1");
        insert(Chat::Task(task_id), "タスク");
        insert(Chat::General, "総合2");

        let contents = |chat| -> Vec<String> {
            list_for_chat(&conn, chat)
                .unwrap()
                .into_iter()
                .map(|m| m.content)
                .collect()
        };
        assert_eq!(contents(Chat::General), vec!["総合1", "総合2"]);
        assert_eq!(contents(Chat::Task(task_id)), vec!["タスク"]);

        // 編集のカスケードも会話の中に留まる。
        soft_delete_normal_from(&conn, Chat::General, general_first).unwrap();
        assert!(contents(Chat::General).is_empty());
        assert_eq!(contents(Chat::Task(task_id)), vec!["タスク"]);
    }

    #[test]
    fn chat_is_exchanged_as_a_tagged_object() {
        assert_eq!(
            serde_json::to_value(Chat::General).unwrap(),
            serde_json::json!({ "kind": "general" })
        );
        assert_eq!(
            serde_json::to_value(Chat::Task(3)).unwrap(),
            serde_json::json!({ "kind": "task", "task_id": 3 })
        );
        let general: Chat =
            serde_json::from_value(serde_json::json!({ "kind": "general" })).unwrap();
        assert_eq!(general, Chat::General);
        // nullや数値だけでは総合チャットにもタスクにもならない。
        assert!(serde_json::from_value::<Chat>(serde_json::json!(null)).is_err());
        assert!(serde_json::from_value::<Chat>(serde_json::json!(3)).is_err());
        assert!(serde_json::from_value::<Chat>(serde_json::json!({ "kind": "task" })).is_err());
    }
}
