use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db::error::Result;
use crate::db::task_steps;
use crate::llm::ToolSchema;

use super::args::Args;
use super::get_current_task_detail::task_detail;

pub const NAME: &str = "add_steps";

const KNOWN_ARGS: &[&str] = &["descriptions"];

/// タスクチャット版のスキーマ。`task_id`を引数に含めない
/// (docs/spec/rebuild/tools.md 1節)。説明は常に配列のみを受ける
/// (同2節「変更点の詳細」— 文字列/配列の多相引数をやめる)。
pub fn schema() -> ToolSchema {
    ToolSchema {
        name: NAME.to_string(),
        description: "Add steps to the currently open task.".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "descriptions": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1
                }
            },
            "required": ["descriptions"],
            "additionalProperties": false
        }),
    }
}

pub fn execute(conn: &Connection, task_id: i64, arguments: &Value) -> Result<Value> {
    let args = Args::parse(arguments, KNOWN_ARGS)?;

    let descriptions = args.required_string_array("descriptions")?;

    task_steps::add_steps(conn, task_id, &descriptions)?;
    task_detail(conn, task_id)
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
    fn rejects_non_array() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let err = execute(&conn, task_id, &json!({ "descriptions": "買い出し" })).unwrap_err();
        assert!(matches!(err, CoreError::InvalidArgument { .. }));
    }

    #[test]
    fn adds_steps_and_returns_full_detail() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let result = execute(
            &conn,
            task_id,
            &json!({ "descriptions": ["買い出し", "調理"] }),
        )
        .unwrap();
        assert_eq!(result["steps"].as_array().unwrap().len(), 2);
    }
}
