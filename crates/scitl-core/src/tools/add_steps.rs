use std::sync::LazyLock;

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db::task_steps::{self, MAX_STEPS, MAX_STEP_DESCRIPTION_CHARS};
use crate::error::Result;
use crate::llm::ToolSchema;

use super::args::Args;
use super::get_current_task_detail::task_detail;
use super::{InternalTool, Run};

pub const NAME: &str = "add_steps";

pub(super) const TOOL: InternalTool = InternalTool {
    schema,
    run: Run::UpdateTask(execute),
};

/// タスクチャット版のスキーマ。`task_id`を引数に含めない。説明は常に配列で受ける。
pub fn schema() -> &'static ToolSchema {
    static SCHEMA: LazyLock<ToolSchema> = LazyLock::new(|| {
        ToolSchema::internal(
            NAME,
            "Add steps to the currently open task.",
            json!({
                "type": "object",
                "properties": {
                    "descriptions": {
                        "type": "array",
                        "items": { "type": "string", "maxLength": MAX_STEP_DESCRIPTION_CHARS },
                        "minItems": 1,
                        // 上限は追加する件数ではなくタスクが持つ件数なので、maxItemsでは書けない。
                        "description": format!("A task can have at most {MAX_STEPS} steps.")
                    }
                },
                "required": ["descriptions"],
                "additionalProperties": false
            }),
        )
    });
    &SCHEMA
}

pub fn execute(conn: &Connection, task_id: i64, arguments: &Value) -> Result<Value> {
    let args = Args::parse(arguments, schema())?;

    let descriptions = args.required_string_array("descriptions")?;

    task_steps::add_steps(conn, task_id, &descriptions)?;
    task_detail(conn, task_id)
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
