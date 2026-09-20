use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use super::{now_iso8601, CoreError, Result};

#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub id: i64,
    pub title: Option<String>,
    pub description: Option<String>,
    pub deadline: Option<String>,
    pub archived_at: Option<String>,
    pub deleted_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// `update_task`ツールが受け付ける引数。`status`は列挙のみを許す
/// (docs/spec/rebuild/tools.md「変更点の詳細」— 削除操作をここから漏らさない)。
#[derive(Debug, Default)]
pub struct TaskUpdate {
    pub title: Option<String>,
    pub description: Option<String>,
    pub deadline: Option<String>,
    pub status: Option<TaskStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Archived,
    Unarchived,
}

impl TaskStatus {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "archived" => Ok(Self::Archived),
            "unarchived" => Ok(Self::Unarchived),
            other => Err(CoreError::InvalidArgument {
                name: "status".to_string(),
                reason: format!("unknown value: {other}"),
            }),
        }
    }
}

pub fn get_task(conn: &Connection, task_id: i64) -> Result<Task> {
    conn.query_row(
        "SELECT id, title, description, deadline, archived_at, deleted_at, created_at, updated_at
         FROM tasks WHERE id = ?1 AND deleted_at IS NULL",
        [task_id],
        row_to_task,
    )
    .optional()?
    .ok_or(CoreError::TaskNotFound(task_id))
}

pub fn update_task(conn: &Connection, task_id: i64, update: TaskUpdate) -> Result<Task> {
    // 存在確認(未削除)を先に行い、TaskNotFoundを一貫して返す。
    get_task(conn, task_id)?;

    let now = now_iso8601();
    let archived_at_clause = update.status.map(|status| match status {
        TaskStatus::Archived => Some(now.clone()),
        TaskStatus::Unarchived => None,
    });

    conn.execute(
        "UPDATE tasks SET
            title = COALESCE(?1, title),
            description = COALESCE(?2, description),
            deadline = COALESCE(?3, deadline),
            archived_at = CASE WHEN ?4 THEN ?5 ELSE archived_at END,
            updated_at = ?6
         WHERE id = ?7",
        rusqlite::params![
            update.title,
            update.description,
            update.deadline,
            archived_at_clause.is_some(),
            archived_at_clause.flatten(),
            now,
            task_id,
        ],
    )?;

    get_task(conn, task_id)
}

fn row_to_task(row: &rusqlite::Row) -> rusqlite::Result<Task> {
    Ok(Task {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        deadline: row.get(3)?,
        archived_at: row.get(4)?,
        deleted_at: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn seed_task(conn: &Connection) -> i64 {
        let now = now_iso8601();
        conn.execute(
            "INSERT INTO tasks (title, created_at, updated_at) VALUES (NULL, ?1, ?1)",
            [&now],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn update_task_sets_only_given_fields() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);

        let updated = update_task(
            &conn,
            id,
            TaskUpdate {
                title: Some("買い物".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(updated.title.as_deref(), Some("買い物"));
        assert!(updated.deadline.is_none());
        assert!(updated.archived_at.is_none());
    }

    #[test]
    fn update_task_archives_and_unarchives() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);

        let archived = update_task(
            &conn,
            id,
            TaskUpdate {
                status: Some(TaskStatus::Archived),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(archived.archived_at.is_some());

        let unarchived = update_task(
            &conn,
            id,
            TaskUpdate {
                status: Some(TaskStatus::Unarchived),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(unarchived.archived_at.is_none());
    }

    #[test]
    fn update_task_missing_returns_not_found() {
        let conn = db::open_in_memory().unwrap();
        let err = update_task(&conn, 999, TaskUpdate::default()).unwrap_err();
        assert!(matches!(err, CoreError::TaskNotFound(999)));
    }

    #[test]
    fn status_parse_rejects_unknown_value() {
        assert!(TaskStatus::parse("deleted").is_err());
    }
}
