use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db::error::{CoreError, Result};
use crate::db::tasks;
use crate::llm::ToolSchema;

pub const NAME: &str = "get_task_list";

/// 引数なし。文脈から決まる情報を持たないため面によらず同一のスキーマ
/// (docs/spec/rebuild/tools.md 2節)。
pub fn schema() -> ToolSchema {
    ToolSchema {
        name: NAME.to_string(),
        description: "タスクの一覧を取得する".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
    }
}

pub fn execute(conn: &Connection, arguments: &Value) -> Result<Value> {
    let object = arguments
        .as_object()
        .ok_or_else(|| CoreError::InvalidArgument {
            name: "arguments".to_string(),
            reason: "expected a JSON object".to_string(),
        })?;
    if let Some(key) = object.keys().next() {
        return Err(CoreError::UnknownArgument(key.clone()));
    }

    // 表示側のフォールバック(Issue #61)はモデルには渡さない。`title: null` が「未設定」を
    // 意味する状態をそのまま見せる(docs/spec/rebuild/tools.md「変更点の詳細」)。
    let tasks: Vec<_> = tasks::list_tasks(conn)?
        .into_iter()
        .map(|t| t.summary)
        .collect();
    Ok(serde_json::to_value(tasks).expect("TaskSummary serialization cannot fail"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[test]
    fn rejects_unknown_argument() {
        let conn = db::open_in_memory().unwrap();
        let err = execute(&conn, &json!({ "task_id": 1 })).unwrap_err();
        assert!(matches!(err, CoreError::UnknownArgument(_)));
    }

    #[test]
    fn returns_task_summaries() {
        let conn = db::open_in_memory().unwrap();
        db::tasks::create_task(&conn).unwrap();
        let result = execute(&conn, &json!({})).unwrap();
        assert_eq!(result.as_array().unwrap().len(), 1);
    }
}
