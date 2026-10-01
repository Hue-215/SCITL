use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db::{task_steps, tasks};
use crate::error::Result;

use super::args::Args;
use super::Run;

internal_tool! {
    /// 引数なし。`task_id`はターン開始時にオーケストレーション層が束縛するため公開しない。
    name: "get_current_task_detail",
    run: Run::ReadTask(execute),
    "Get the details of the currently open task, including its description and steps.",
    json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false
    }),
}

pub fn execute(conn: &Connection, task_id: i64, arguments: &Value) -> Result<Value> {
    Args::parse(arguments, schema())?;

    task_detail(conn, task_id)
}

/// 工程の追加・更新・削除ツールも、変更後の全体をこの形で返す。
pub fn task_detail(conn: &Connection, task_id: i64) -> Result<Value> {
    let task = tasks::get_task(conn, task_id)?;
    let steps = task_steps::list_for_task(conn, task_id)?;
    Ok(json!({ "task": task, "steps": steps }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::error::CoreError;

    fn seed_task(conn: &Connection) -> i64 {
        db::tasks::create_task(conn).unwrap().id
    }

    #[test]
    fn rejects_unknown_argument() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let err = execute(&conn, task_id, &json!({ "task_id": task_id })).unwrap_err();
        assert!(matches!(err, CoreError::UnknownArgument(_)));
    }

    #[test]
    fn returns_task_and_steps() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        db::task_steps::add_steps(&conn, task_id, &["買い出し".to_string()]).unwrap();

        let result = execute(&conn, task_id, &json!({})).unwrap();
        assert_eq!(result["task"]["id"], task_id);
        assert_eq!(result["steps"].as_array().unwrap().len(), 1);
    }
}
