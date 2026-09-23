use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db::error::Result;
use crate::db::tasks::{self, TaskStatus, TaskUpdate};
use crate::llm::ToolSchema;

use super::args::Args;

pub const NAME: &str = "update_task";

const KNOWN_ARGS: &[&str] = &["title", "description", "deadline", "status"];

/// タスクチャット版のスキーマ。`task_id`を引数に含めない
/// (docs/spec/rebuild/tools.md 1節「確定方針」)。
pub fn schema() -> ToolSchema {
    ToolSchema {
        name: NAME.to_string(),
        description: "現在開いているタスクを更新する".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "title": { "type": "string" },
                "description": { "type": "string" },
                "deadline": {
                    "type": "string",
                    "format": "date",
                    // 形式を満たさない値は`db::tasks::update_task`が弾く。スキーマ側にも
                    // 明示しておき、モデルが日時形式を渡して往復を1回無駄にするのを減らす。
                    "description": "締切日(YYYY-MM-DD)"
                },
                "status": { "type": "string", "enum": ["archived", "unarchived"] }
            },
            "additionalProperties": false
        }),
    }
}

pub fn execute(conn: &Connection, task_id: i64, arguments: &Value) -> Result<Value> {
    let args = Args::parse(arguments, KNOWN_ARGS)?;

    let title = args.optional_string("title")?;
    let description = args.optional_string("description")?;
    let deadline = args.optional_string("deadline")?;
    let status = args
        .optional_string("status")?
        .map(|s| TaskStatus::parse(&s))
        .transpose()?;

    let updated = tasks::update_task(
        conn,
        task_id,
        TaskUpdate {
            title,
            description,
            deadline,
            status,
        },
    )?;

    Ok(serde_json::to_value(updated).expect("Task serialization cannot fail"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::db::error::CoreError;

    fn seed_task(conn: &Connection) -> i64 {
        let now = db::now_iso8601();
        conn.execute(
            "INSERT INTO tasks (created_at, updated_at) VALUES (?1, ?1)",
            [&now],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn rejects_unknown_argument() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let err = execute(&conn, task_id, &json!({ "task_id": 1 })).unwrap_err();
        assert!(matches!(err, CoreError::UnknownArgument(_)));
    }

    #[test]
    fn rejects_wrong_type_without_coercion() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let err = execute(&conn, task_id, &json!({ "title": 123 })).unwrap_err();
        assert!(matches!(err, CoreError::InvalidArgument { .. }));
    }

    #[test]
    fn applies_valid_update() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let result = execute(&conn, task_id, &json!({ "title": "買い物" })).unwrap();
        assert_eq!(result["title"], "買い物");
    }
}
