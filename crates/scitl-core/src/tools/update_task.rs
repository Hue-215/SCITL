use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db::error::{CoreError, Result};
use crate::db::tasks::{self, FieldChange, TaskStatus, TaskUpdate};
use crate::llm::ToolSchema;

use super::args::Args;

pub const NAME: &str = "update_task";

const KNOWN_ARGS: &[&str] = &["title", "description", "deadline", "status", "clear"];

/// `clear`で消せる項目。タイトルは未設定に戻す操作を持たないので含めない。
const CLEARABLE: &[&str] = &["deadline", "description"];

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
                "description": {
                    "type": "string",
                    "description": "本文。空にはできない(消すときはclearを使う)"
                },
                "deadline": {
                    "type": "string",
                    "format": "date",
                    // 形式を満たさない値は`db::tasks::update_task`が弾く。スキーマ側にも
                    // 明示しておき、モデルが日時形式を渡して往復を1回無駄にするのを減らす。
                    "description": "締切日(YYYY-MM-DD)"
                },
                "status": { "type": "string", "enum": ["archived", "unarchived"] },
                "clear": {
                    "type": "array",
                    "items": { "type": "string", "enum": CLEARABLE },
                    "uniqueItems": true,
                    // 省略・nullは「変えない」。値を消すのはこの引数だけにする(nullを消去の
                    // 意味にすると、型に緩いモデルが変えないつもりの項目を消してしまう)。
                    "description": "消す項目。同じ項目を同時に設定することはできない"
                }
            },
            "additionalProperties": false
        }),
    }
}

pub fn execute(conn: &Connection, task_id: i64, arguments: &Value) -> Result<Value> {
    let args = Args::parse(arguments, KNOWN_ARGS)?;

    let clear = args.optional_string_array("clear")?.unwrap_or_default();
    for (i, field) in clear.iter().enumerate() {
        let reason = if !CLEARABLE.contains(&field.as_str()) {
            format!("cannot clear: {field}")
        } else if clear[..i].contains(field) {
            format!("duplicate item: {field}")
        } else {
            continue;
        };
        return Err(CoreError::InvalidArgument {
            name: "clear".to_string(),
            reason,
        });
    }
    let change = |name: &str| -> Result<FieldChange> {
        let value = args.optional_string(name)?;
        let cleared = clear.iter().any(|c| c == name);
        match (value, cleared) {
            (Some(_), true) => Err(CoreError::InvalidArgument {
                name: name.to_string(),
                reason: "cannot both set and clear the same field".to_string(),
            }),
            (Some(value), false) => Ok(FieldChange::Set(value)),
            (None, true) => Ok(FieldChange::Clear),
            (None, false) => Ok(FieldChange::Keep),
        }
    };

    let title = args.optional_string("title")?;
    let description = change("description")?;
    let deadline = change("deadline")?;
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
    fn clear_removes_listed_fields() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        execute(
            &conn,
            task_id,
            &json!({ "deadline": "2026-10-01", "description": "牛乳" }),
        )
        .unwrap();

        // nullは「変えない」。
        let result = execute(&conn, task_id, &json!({ "deadline": null })).unwrap();
        assert_eq!(result["deadline"], "2026-10-01");

        let result = execute(&conn, task_id, &json!({ "clear": ["deadline"] })).unwrap();
        assert!(result["deadline"].is_null());
        assert_eq!(result["description"], "牛乳");
    }

    #[test]
    fn rejects_setting_and_clearing_the_same_field() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let err = execute(
            &conn,
            task_id,
            &json!({ "deadline": "2026-10-01", "clear": ["deadline"] }),
        )
        .unwrap_err();
        assert!(matches!(err, CoreError::InvalidArgument { .. }));
    }

    #[test]
    fn rejects_clearing_title_unknown_or_duplicate_field() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        for clear in [
            json!(["title"]),
            json!(["status"]),
            json!(["deadline", "deadline"]),
        ] {
            let err = execute(&conn, task_id, &json!({ "clear": clear })).unwrap_err();
            assert!(matches!(err, CoreError::InvalidArgument { .. }));
        }
    }

    #[test]
    fn applies_valid_update() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let result = execute(&conn, task_id, &json!({ "title": "買い物" })).unwrap();
        assert_eq!(result["title"], "買い物");
    }
}
