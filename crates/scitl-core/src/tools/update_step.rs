use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db::error::{CoreError, Result};
use crate::db::task_steps;
use crate::llm::ToolSchema;

use super::get_current_task_detail::task_detail;

pub const NAME: &str = "update_step";

const KNOWN_ARGS: &[&str] = &["step_id", "description", "done"];

/// タスクチャット版のスキーマ。`task_id`を引数に含めない
/// (docs/spec/rebuild/tools.md 1節)。
pub fn schema() -> ToolSchema {
    ToolSchema {
        name: NAME.to_string(),
        description: "現在開いているタスクの工程を更新する".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "step_id": { "type": "integer" },
                "description": { "type": "string" },
                "done": { "type": "boolean" }
            },
            "required": ["step_id"],
            "additionalProperties": false
        }),
    }
}

pub fn execute(conn: &Connection, task_id: i64, arguments: &Value) -> Result<Value> {
    let object = arguments
        .as_object()
        .ok_or_else(|| CoreError::InvalidArgument {
            name: "arguments".to_string(),
            reason: "expected a JSON object".to_string(),
        })?;

    for key in object.keys() {
        if !KNOWN_ARGS.contains(&key.as_str()) {
            return Err(CoreError::UnknownArgument(key.clone()));
        }
    }

    let step_id = extract_step_id(object)?;
    super::require_step_in_task(conn, task_id, step_id)?;

    let description = extract_string(object, "description")?;
    let done = extract_bool(object, "done")?;

    task_steps::update_step(conn, step_id, description, done)?;
    task_detail(conn, task_id)
}

fn extract_step_id(object: &serde_json::Map<String, Value>) -> Result<i64> {
    let value = object.get("step_id").ok_or_else(|| CoreError::InvalidArgument {
        name: "step_id".to_string(),
        reason: "required".to_string(),
    })?;
    value.as_i64().ok_or_else(|| CoreError::InvalidArgument {
        name: "step_id".to_string(),
        reason: "expected an integer".to_string(),
    })
}

fn extract_string(object: &serde_json::Map<String, Value>, name: &str) -> Result<Option<String>> {
    match object.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(CoreError::InvalidArgument {
            name: name.to_string(),
            reason: "expected a string".to_string(),
        }),
    }
}

fn extract_bool(object: &serde_json::Map<String, Value>, name: &str) -> Result<Option<bool>> {
    match object.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(CoreError::InvalidArgument {
            name: name.to_string(),
            reason: "expected a boolean".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

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
