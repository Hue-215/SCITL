use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db::memories::{self, MAX_MEMORIES, MAX_MEMORY_CHARS};
use crate::error::Result;

use super::args::Args;
use super::get_memories::memory_list;
use super::Run;

internal_tool! {
    /// 本文は常に配列で受ける(工程の追加と同じ)。
    name: "add_memories",
    run: Run::Write(execute),
    "Save facts about the user that will also help in other tasks and conversations, such as \
     their schedule, habits and preferences. Write one fact per item as a short sentence. Do \
     not save details that belong to a single task, secrets such as passwords or API keys, or \
     anything the user asked you not to remember.",
    json!({
        "type": "object",
        "properties": {
            "contents": {
                "type": "array",
                "items": { "type": "string", "maxLength": MAX_MEMORY_CHARS },
                "minItems": 1,
                // 上限は追加する件数ではなく持てる件数なので、maxItemsでは書けない。
                "description": format!("At most {MAX_MEMORIES} memories can be kept.")
            }
        },
        "required": ["contents"],
        "additionalProperties": false
    }),
}

pub fn execute(conn: &Connection, arguments: &Value) -> Result<Value> {
    let args = Args::parse(arguments, schema())?;

    let contents = args.required_string_array("contents")?;

    memories::add(conn, &contents)?;
    memory_list(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::error::CoreError;

    #[test]
    fn rejects_non_array() {
        let conn = db::open_in_memory().unwrap();
        let err = execute(&conn, &json!({ "contents": "朝型" })).unwrap_err();
        assert!(matches!(err, CoreError::InvalidArgument { .. }));
    }

    #[test]
    fn adds_and_returns_all_memories() {
        let conn = db::open_in_memory().unwrap();
        db::memories::add(&conn, &["朝型".to_string()]).unwrap();

        let result = execute(&conn, &json!({ "contents": ["締切は2日前に置く"] })).unwrap();

        assert_eq!(result.as_array().unwrap().len(), 2);
    }
}
