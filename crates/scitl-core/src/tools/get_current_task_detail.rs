use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db::error::Result;
use crate::db::{task_steps, tasks};
use crate::llm::ToolSchema;

use super::args::Args;

pub const NAME: &str = "get_current_task_detail";

/// 引数なし。`task_id`はターン開始時にオーケストレーション層が束縛するため公開しない
/// (docs/spec/rebuild/tools.md 1節)。
pub fn schema() -> ToolSchema {
    ToolSchema {
        name: NAME.to_string(),
        description: "現在開いているタスクの詳細(本文・工程を含む)を取得する".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
    }
}

pub fn execute(conn: &Connection, task_id: i64, arguments: &Value) -> Result<Value> {
    Args::parse(arguments, &[])?;

    task_detail(conn, task_id)
}

/// 工程の追加・更新・削除ツールも、変更後の全体をこの形で返す
/// (docs/spec/principles.md 3節「書き込み系は変更後の全体を返す」)。
pub fn task_detail(conn: &Connection, task_id: i64) -> Result<Value> {
    let task = tasks::get_task(conn, task_id)?;
    let steps = task_steps::list_for_task(conn, task_id)?;
    Ok(json!({ "task": task, "steps": steps }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::db::error::CoreError;

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
