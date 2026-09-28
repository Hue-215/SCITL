use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use super::{now_iso8601, tasks};
use crate::error::{CoreError, Result};

#[derive(Debug, Clone, Serialize)]
pub struct TaskStep {
    pub id: i64,
    pub task_id: i64,
    pub description: String,
    pub done_at: Option<String>,
    pub order_index: i64,
    pub created_at: String,
}

/// 削除済み(deleted_at)を除く工程を`order_index`順で返す。
pub fn list_for_task(conn: &Connection, task_id: i64) -> Result<Vec<TaskStep>> {
    let mut stmt = conn.prepare(
        "SELECT id, task_id, description, done_at, order_index, created_at
         FROM task_steps
         WHERE task_id = ?1 AND deleted_at IS NULL
         ORDER BY order_index ASC",
    )?;
    let rows = stmt
        .query_map([task_id], row_to_step)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// 工程の説明の正規化。前後の空白を落とし(落とさないと重複排除が効かない)、空なら
/// エラーにする。追加と更新で同じ規則を通す。
fn normalize_description(raw: &str, arg_name: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(CoreError::InvalidArgument {
            name: arg_name.to_string(),
            reason: "must not be empty".to_string(),
        });
    }
    Ok(trimmed.to_string())
}

/// 工程の追加。`descriptions`内の重複、および既存の未削除工程と同一の説明は
/// 除外する(docs/spec/legacy/backend.md 5節の棚卸しを踏まえた確定方針)。
/// 空の説明が1つでもあれば、1件も追加せずにエラーを返す。
/// `order_index`は連番で既存の最大値の続きから振る。戻り値は新規に追加された工程のみ。
pub fn add_steps(
    conn: &Connection,
    task_id: i64,
    descriptions: &[String],
) -> Result<Vec<TaskStep>> {
    super::in_transaction(conn, |conn| {
        tasks::get_task(conn, task_id)?;
        let descriptions = descriptions
            .iter()
            .map(|d| normalize_description(d, "descriptions"))
            .collect::<Result<Vec<_>>>()?;

        let mut stmt = conn.prepare(
            "SELECT description FROM task_steps WHERE task_id = ?1 AND deleted_at IS NULL",
        )?;
        let existing: HashSet<String> = stmt
            .query_map([task_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        drop(stmt);

        let mut next_order_index: i64 = conn.query_row(
            "SELECT COALESCE(MAX(order_index), -1) + 1 FROM task_steps WHERE task_id = ?1",
            [task_id],
            |row| row.get(0),
        )?;

        let mut seen = existing;
        let mut created = Vec::new();
        let now = now_iso8601();
        for description in &descriptions {
            if !seen.insert(description.clone()) {
                continue;
            }
            conn.execute(
                "INSERT INTO task_steps (task_id, description, order_index, created_at)
             VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![task_id, description, next_order_index, now],
            )?;
            created.push(get_step(conn, conn.last_insert_rowid())?);
            next_order_index += 1;
        }

        if !created.is_empty() {
            tasks::touch(conn, task_id)?;
        }
        Ok(created)
    })
}

/// `description`・`done`はどちらも省略可(渡された分だけ更新する)。
/// 完了は`done_at`の有無で表現する: `done=true`で未完了なら現在時刻を設定し、
/// 既に完了済みなら元の完了日時を保つ(冪等)。`done=false`は`done_at`を消す。
pub fn update_step(
    conn: &Connection,
    step_id: i64,
    description: Option<String>,
    done: Option<bool>,
) -> Result<TaskStep> {
    super::in_transaction(conn, |conn| {
        let current = get_step(conn, step_id)?;
        let description = description
            .map(|d| normalize_description(&d, "description"))
            .transpose()?;

        let done_at = match done {
            Some(true) => Some(current.done_at.clone().unwrap_or_else(now_iso8601)),
            Some(false) => None,
            None => current.done_at.clone(),
        };

        conn.execute(
            "UPDATE task_steps SET description = COALESCE(?1, description), done_at = ?2
         WHERE id = ?3",
            rusqlite::params![description, done_at, step_id],
        )?;
        tasks::touch(conn, current.task_id)?;

        get_step(conn, step_id)
    })
}

pub fn delete_step(conn: &Connection, step_id: i64) -> Result<()> {
    super::in_transaction(conn, |conn| {
        let step = get_step(conn, step_id)?;
        conn.execute(
            "UPDATE task_steps SET deleted_at = ?1 WHERE id = ?2",
            rusqlite::params![now_iso8601(), step_id],
        )?;
        tasks::touch(conn, step.task_id)
    })
}

fn get_step(conn: &Connection, step_id: i64) -> Result<TaskStep> {
    conn.query_row(
        "SELECT id, task_id, description, done_at, order_index, created_at
         FROM task_steps WHERE id = ?1 AND deleted_at IS NULL",
        [step_id],
        row_to_step,
    )
    .optional()?
    .ok_or(CoreError::TaskStepNotFound(step_id))
}

fn row_to_step(row: &rusqlite::Row) -> rusqlite::Result<TaskStep> {
    Ok(TaskStep {
        id: row.get(0)?,
        task_id: row.get(1)?,
        description: row.get(2)?,
        done_at: row.get(3)?,
        order_index: row.get(4)?,
        created_at: row.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn seed_task(conn: &Connection) -> i64 {
        db::tasks::create_task(conn).unwrap().id
    }

    #[test]
    fn add_steps_assigns_sequential_order_index() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);

        let created = add_steps(
            &conn,
            task_id,
            &["買い出し".to_string(), "調理".to_string()],
        )
        .unwrap();

        assert_eq!(created.len(), 2);
        assert_eq!(created[0].order_index, 0);
        assert_eq!(created[1].order_index, 1);
    }

    #[test]
    fn add_steps_dedupes_within_batch_and_against_existing() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);

        add_steps(&conn, task_id, &["買い出し".to_string()]).unwrap();
        let created = add_steps(
            &conn,
            task_id,
            &[
                "買い出し".to_string(),
                "調理".to_string(),
                "調理".to_string(),
            ],
        )
        .unwrap();

        assert_eq!(created.len(), 1);
        assert_eq!(created[0].description, "調理");
        assert_eq!(list_for_task(&conn, task_id).unwrap().len(), 2);
    }

    #[test]
    fn update_step_toggles_done_at_and_keeps_original_completion_time() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let step = add_steps(&conn, task_id, &["買い出し".to_string()])
            .unwrap()
            .remove(0);

        let done = update_step(&conn, step.id, None, Some(true)).unwrap();
        assert!(done.done_at.is_some());
        let first_done_at = done.done_at.clone().unwrap();

        let still_done = update_step(&conn, step.id, None, Some(true)).unwrap();
        assert_eq!(still_done.done_at, Some(first_done_at));

        let undone = update_step(&conn, step.id, None, Some(false)).unwrap();
        assert!(undone.done_at.is_none());
    }

    #[test]
    fn add_steps_trims_before_deduping_and_rejects_blank_without_adding_any() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let created = add_steps(
            &conn,
            task_id,
            &[" 買い出し".to_string(), "買い出し ".to_string()],
        )
        .unwrap();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].description, "買い出し");

        let err = add_steps(&conn, task_id, &["調理".to_string(), "  ".to_string()]).unwrap_err();
        assert!(matches!(&err, CoreError::InvalidArgument { name, .. } if name == "descriptions"));
        assert_eq!(list_for_task(&conn, task_id).unwrap().len(), 1);
    }

    #[test]
    fn update_step_rejects_blank_description() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let step = add_steps(&conn, task_id, &["買い出し".to_string()])
            .unwrap()
            .remove(0);
        assert!(update_step(&conn, step.id, Some(" ".to_string()), None).is_err());
        let updated = update_step(&conn, step.id, Some(" 調理 ".to_string()), None).unwrap();
        assert_eq!(updated.description, "調理");
    }

    #[test]
    fn step_changes_update_the_task_updated_at() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let reset = || {
            conn.execute(
                "UPDATE tasks SET updated_at = '2000-01-01T00:00:00Z' WHERE id = ?1",
                [task_id],
            )
            .unwrap();
        };
        let updated_at = || db::tasks::get_task(&conn, task_id).unwrap().updated_at;

        reset();
        let step = add_steps(&conn, task_id, &["買い出し".to_string()])
            .unwrap()
            .remove(0);
        assert_ne!(updated_at(), "2000-01-01T00:00:00Z");

        reset();
        update_step(&conn, step.id, None, Some(true)).unwrap();
        assert_ne!(updated_at(), "2000-01-01T00:00:00Z");

        reset();
        delete_step(&conn, step.id).unwrap();
        assert_ne!(updated_at(), "2000-01-01T00:00:00Z");
    }

    #[test]
    fn update_step_missing_returns_not_found() {
        let conn = db::open_in_memory().unwrap();
        let err = update_step(&conn, 999, None, None).unwrap_err();
        assert!(matches!(err, CoreError::TaskStepNotFound(999)));
    }

    #[test]
    fn delete_step_is_excluded_from_listing() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let step = add_steps(&conn, task_id, &["買い出し".to_string()])
            .unwrap()
            .remove(0);

        delete_step(&conn, step.id).unwrap();

        assert!(list_for_task(&conn, task_id).unwrap().is_empty());
        assert!(matches!(
            delete_step(&conn, step.id).unwrap_err(),
            CoreError::TaskStepNotFound(_)
        ));
    }
}
