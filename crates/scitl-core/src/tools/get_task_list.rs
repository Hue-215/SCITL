use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db::error::Result;
use crate::db::tasks;
use crate::llm::ToolSchema;

use super::args::Args;

pub const NAME: &str = "get_task_list";

/// 引数なし。文脈から決まる情報を持たないため面によらず同一のスキーマ
/// (docs/spec/rebuild/tools.md 2節)。
pub fn schema() -> ToolSchema {
    ToolSchema {
        name: NAME.to_string(),
        description: "List the tasks that are not archived.".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
    }
}

pub fn execute(conn: &Connection, arguments: &Value) -> Result<Value> {
    Args::parse(arguments, &[])?;

    // 表示側のフォールバック(Issue #61)はモデルには渡さない。`title: null` が「未設定」を
    // 意味する状態をそのまま見せる(docs/spec/rebuild/tools.md「変更点の詳細」)。
    // アーカイブ済みは返さない(旧実装と同じ。docs/spec/rebuild/tools.md 2節)。アーカイブは
    // 溜まる一方で、返し続けるとトークンが増え続け、優先度の相談ではノイズになる。
    let tasks: Vec<_> = tasks::list_tasks(conn)?
        .into_iter()
        .map(|t| t.summary)
        .filter(|t| t.archived_at.is_none())
        .collect();
    Ok(serde_json::to_value(tasks).expect("TaskSummary serialization cannot fail"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::db::error::CoreError;

    #[test]
    fn rejects_unknown_argument() {
        let conn = db::open_in_memory().unwrap();
        let err = execute(&conn, &json!({ "task_id": 1 })).unwrap_err();
        assert!(matches!(err, CoreError::UnknownArgument(_)));
    }

    #[test]
    fn excludes_archived_tasks() {
        let conn = db::open_in_memory().unwrap();
        let kept = db::tasks::create_task(&conn).unwrap().id;
        let archived = db::tasks::create_task(&conn).unwrap().id;
        db::tasks::update_task(
            &conn,
            archived,
            db::tasks::TaskUpdate {
                status: Some(db::tasks::TaskStatus::Archived),
                ..Default::default()
            },
        )
        .unwrap();

        let result = execute(&conn, &json!({})).unwrap();
        let ids: Vec<_> = result
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_i64().unwrap())
            .collect();
        assert_eq!(ids, vec![kept]);
    }

    #[test]
    fn returns_task_summaries() {
        let conn = db::open_in_memory().unwrap();
        db::tasks::create_task(&conn).unwrap();
        let result = execute(&conn, &json!({})).unwrap();
        assert_eq!(result.as_array().unwrap().len(), 1);
    }
}
