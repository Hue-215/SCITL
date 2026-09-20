use rusqlite::Connection;
use serde::Serialize;

use super::{now_iso8601, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    Tool,
    Error,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
            Role::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Normal,
    ToolExecution,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Normal => "normal",
            Kind::ToolExecution => "tool_execution",
        }
    }
}

/// `docs/spec/rebuild/data-model.md`「ターン境界」の3分類。
/// ユーザー発言・外部経由の記録は`None`、SCITL自身の応答生成に属する行は`Some`。
pub struct NewMessage<'a> {
    pub task_id: Option<i64>,
    pub role: Role,
    pub content: &'a str,
    pub kind: Kind,
    pub source: Option<&'a str>,
    pub turn: Option<(&'a str, i64)>,
    /// `role`が`Error`のときのみ`Some`(`CHECK ((role = 'error') = (error_kind IS NOT NULL))`)。
    pub error_kind: Option<&'a str>,
    /// モデルの思考(reasoning)。表示・エクスポート専用で、APIへの入力には使わない
    /// (`docs/spec/rebuild/data-model.md` messagesテーブル、Issue #42)。
    pub reasoning: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub id: i64,
    pub task_id: Option<i64>,
    pub role: String,
    pub content: String,
    pub kind: String,
    pub source: Option<String>,
    pub reasoning: Option<String>,
    pub error_kind: Option<String>,
    pub turn_id: Option<String>,
    pub attempt_no: Option<i64>,
    pub created_at: String,
}

pub fn insert_message(conn: &Connection, msg: NewMessage) -> Result<i64> {
    let (turn_id, attempt_no) = match msg.turn {
        Some((id, attempt)) => (Some(id), Some(attempt)),
        None => (None, None),
    };
    conn.execute(
        "INSERT INTO messages
            (task_id, role, content, kind, source, reasoning, error_kind, turn_id, attempt_no, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            msg.task_id,
            msg.role.as_str(),
            msg.content,
            msg.kind.as_str(),
            msg.source,
            msg.reasoning,
            msg.error_kind,
            turn_id,
            attempt_no,
            now_iso8601(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// タスクチャンネル分の発言取得(支配的クエリ)。
/// ターンを持つ行は`turn_id`ごとの最新試行のみに絞る
/// (data-model.md「ターン境界」— 外部経由の記録はturn_idを持たないため常に残る)。
pub fn list_for_task(conn: &Connection, task_id: i64) -> Result<Vec<Message>> {
    let mut stmt = conn.prepare(
        "SELECT id, task_id, role, content, kind, source, reasoning, error_kind, turn_id, attempt_no, created_at
         FROM messages
         WHERE task_id = ?1
           AND deleted_at IS NULL
           AND (
             turn_id IS NULL
             OR attempt_no = (
               SELECT MAX(attempt_no) FROM messages m2
               WHERE m2.turn_id = messages.turn_id AND m2.deleted_at IS NULL
             )
           )
         ORDER BY created_at ASC, id ASC",
    )?;
    let rows = stmt
        .query_map([task_id], |row| {
            Ok(Message {
                id: row.get(0)?,
                task_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                kind: row.get(4)?,
                source: row.get(5)?,
                reasoning: row.get(6)?,
                error_kind: row.get(7)?,
                turn_id: row.get(8)?,
                attempt_no: row.get(9)?,
                created_at: row.get(10)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
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
                task_id: Some(task_id),
                role: Role::User,
                content: "こんにちは",
                kind: Kind::Normal,
                source: None,
                turn: None,
                error_kind: None,
                reasoning: None,
            },
        )
        .unwrap();

        insert_message(
            &conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::Error,
                content: "モデルからの応答が空でした",
                kind: Kind::Normal,
                source: None,
                turn: Some(("turn-1", 1)),
                error_kind: Some("empty_response"),
                reasoning: None,
            },
        )
        .unwrap();

        insert_message(
            &conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::Assistant,
                content: "再試行後の応答",
                kind: Kind::Normal,
                source: None,
                turn: Some(("turn-1", 2)),
                error_kind: None,
                reasoning: None,
            },
        )
        .unwrap();

        let messages = list_for_task(&conn, task_id).unwrap();
        let contents: Vec<_> = messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, vec!["こんにちは", "再試行後の応答"]);
    }

    #[test]
    fn external_records_without_turn_are_always_shown() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);

        insert_message(
            &conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::Tool,
                content: "{}",
                kind: Kind::ToolExecution,
                source: Some("mcp:external-client"),
                turn: None,
                error_kind: None,
                reasoning: None,
            },
        )
        .unwrap();

        let messages = list_for_task(&conn, task_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].source.as_deref(), Some("mcp:external-client"));
    }

    #[test]
    fn error_role_round_trips_with_error_kind() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);

        insert_message(
            &conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::Error,
                content: "APIキーが設定されていません",
                kind: Kind::Normal,
                source: None,
                turn: Some(("turn-1", 1)),
                error_kind: Some("no_api_key"),
                reasoning: None,
            },
        )
        .unwrap();

        let messages = list_for_task(&conn, task_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "error");
        assert_eq!(messages[0].error_kind.as_deref(), Some("no_api_key"));
    }

    #[test]
    fn error_role_without_error_kind_is_rejected_by_check_constraint() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);

        let result = insert_message(
            &conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::Error,
                content: "壊れた呼び出し",
                kind: Kind::Normal,
                source: None,
                turn: None,
                error_kind: None,
                reasoning: None,
            },
        );
        assert!(result.is_err());
    }
}
