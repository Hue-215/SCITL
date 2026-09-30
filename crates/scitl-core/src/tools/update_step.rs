use std::sync::LazyLock;

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db::task_steps;
use crate::error::Result;
use crate::llm::ToolSchema;

use super::args::Args;
use super::get_current_task_detail::task_detail;
use super::{InternalTool, Run};

pub const NAME: &str = "update_step";

pub(super) const TOOL: InternalTool = InternalTool {
    schema,
    run: Run::UpdateTask(execute),
};

/// タスクチャット版のスキーマ。`task_id`を引数に含めない。
pub fn schema() -> &'static ToolSchema {
    static SCHEMA: LazyLock<ToolSchema> = LazyLock::new(|| {
        ToolSchema::internal(
            NAME,
            "Update a step of the currently open task.",
            json!({
                "type": "object",
                "properties": {
                    "step_id": { "type": "integer" },
                    "description": { "type": "string" },
                    "done": { "type": "boolean" }
                },
                "required": ["step_id"],
                "additionalProperties": false
            }),
        )
    });
    &SCHEMA
}

pub fn execute(conn: &Connection, task_id: i64, arguments: &Value) -> Result<Value> {
    let args = Args::parse(arguments, schema())?;

    let step_id = args.required_i64("step_id")?;
    super::require_step_in_task(conn, task_id, step_id)?;

    let description = args.optional_string("description")?;
    let done = args.optional_bool("done")?;

    task_steps::update_step(conn, step_id, description, done)?;
    task_detail(conn, task_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::error::CoreError;

    fn seed_task_with_step(conn: &Connection) -> (i64, i64) {
        let task_id = db::tasks::create_task(conn).unwrap().id;
        let step = db::task_steps::add_steps(conn, task_id, &["買い出し".to_string()])
            .unwrap()
            .remove(0);
        (task_id, step.id)
    }

    #[test]
    fn rejects_unknown_argument() {
        let conn = db::open_in_memory().unwrap();
        let (task_id, step_id) = seed_task_with_step(&conn);
        let err =
            execute(&conn, task_id, &json!({ "step_id": step_id, "task_id": 1 })).unwrap_err();
        assert!(matches!(err, CoreError::UnknownArgument(_)));
    }

    #[test]
    fn rejects_step_from_another_task() {
        let conn = db::open_in_memory().unwrap();
        let (_task_id, step_id) = seed_task_with_step(&conn);
        let other_task_id = db::tasks::create_task(&conn).unwrap().id;

        let err = execute(&conn, other_task_id, &json!({ "step_id": step_id })).unwrap_err();
        assert!(matches!(err, CoreError::TaskStepNotFound(_)));
    }

    #[test]
    fn marks_step_done() {
        let conn = db::open_in_memory().unwrap();
        let (task_id, step_id) = seed_task_with_step(&conn);

        let result = execute(&conn, task_id, &json!({ "step_id": step_id, "done": true })).unwrap();
        assert!(result["steps"][0]["done_at"].is_string());
    }
}
