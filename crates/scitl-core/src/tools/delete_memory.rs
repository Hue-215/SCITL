use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db::memories;
use crate::error::Result;

use super::args::Args;
use super::get_memories::memory_list;
use super::Run;

internal_tool! {
    name: "delete_memory",
    run: Run::Write(execute),
    "Delete a memory that is wrong or no longer true, or that the user asked you to forget.",
    json!({
        "type": "object",
        "properties": {
            "memory_id": { "type": "integer" }
        },
        "required": ["memory_id"],
        "additionalProperties": false
    }),
}

pub fn execute(conn: &Connection, arguments: &Value) -> Result<Value> {
    let args = Args::parse(arguments, schema())?;

    let memory_id = args.required_i64("memory_id")?;

    memories::delete(conn, memory_id)?;
    memory_list(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::error::CoreError;

    #[test]
    fn deletes_and_returns_the_rest() {
        let conn = db::open_in_memory().unwrap();
        let ids: Vec<i64> = db::memories::add(&conn, &["朝型".to_string(), "猫が好き".to_string()])
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();

        let result = execute(&conn, &json!({ "memory_id": ids[0] })).unwrap();

        assert_eq!(result.as_array().unwrap().len(), 1);
        assert_eq!(result[0]["content"], "猫が好き");
    }

    #[test]
    fn unknown_memory_is_not_found() {
        let conn = db::open_in_memory().unwrap();
        let err = execute(&conn, &json!({ "memory_id": 99 })).unwrap_err();
        assert!(matches!(err, CoreError::MemoryNotFound(99)));
    }
}
