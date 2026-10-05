use rusqlite::Connection;
use serde_json::{json, Value};

use crate::error::Result;

use super::args::Args;
use super::get_current_task_detail::task_detail;
use super::Run;

internal_tool! {
    /// 総合チャット版。総合チャットは特定のタスクに紐づかず、対象を文脈から決められないので、
    /// `task_id`をモデルに選ばせる。読み取り専用なので、取り違えても書き込みは起きない。
    name: "get_task_detail",
    run: Run::Read(execute),
    "Get the details of a task, including its description and steps.",
    json!({
        "type": "object",
        "properties": {
            "task_id": { "type": "integer" }
        },
        "required": ["task_id"],
        "additionalProperties": false
    }),
}

/// アーカイブ済みのタスクも引ける(一覧には出ないが、会話で名前が挙がることはある)。
pub fn execute(conn: &Connection, arguments: &Value) -> Result<Value> {
    let args = Args::parse(arguments, schema())?;
    task_detail(conn, args.required_i64("task_id")?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::error::CoreError;

    #[test]
    fn returns_the_named_task_and_its_steps() {
        let conn = db::open_in_memory().unwrap();
        let task_id = db::tasks::create_task(&conn).unwrap().id;
        db::task_steps::add_steps(&conn, task_id, &["買い出し".to_string()]).unwrap();

        let result = execute(&conn, &json!({ "task_id": task_id })).unwrap();
        assert_eq!(result["task"]["id"], task_id);
        assert_eq!(result["steps"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn rejects_a_missing_or_non_integer_task_id() {
        let conn = db::open_in_memory().unwrap();
        let task_id = db::tasks::create_task(&conn).unwrap().id;
        for arguments in [
            json!({}),
            json!({ "task_id": task_id.to_string() }),
            json!({ "task_id": 1.5 }),
        ] {
            let err = execute(&conn, &arguments).unwrap_err();
            assert!(matches!(err, CoreError::InvalidArgument { .. }));
        }
    }

    #[test]
    fn reports_a_deleted_task_as_not_found() {
        let conn = db::open_in_memory().unwrap();
        let task_id = db::tasks::create_task(&conn).unwrap().id;
        db::tasks::delete_task(&conn, task_id).unwrap();
        let err = execute(&conn, &json!({ "task_id": task_id })).unwrap_err();
        assert!(matches!(err, CoreError::TaskNotFound(id) if id == task_id));
    }
}
