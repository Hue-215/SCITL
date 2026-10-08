use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db::memories::{self, MAX_MEMORY_CHARS};
use crate::error::Result;

use super::args::Args;
use super::get_memories::memory_list;
use super::Run;

internal_tool! {
    name: "update_memory",
    run: Run::Write(execute),
    "Rewrite a memory, for example when the fact has changed.",
    json!({
        "type": "object",
        "properties": {
            "memory_id": { "type": "integer" },
            "content": { "type": "string", "maxLength": MAX_MEMORY_CHARS }
        },
        "required": ["memory_id", "content"],
        "additionalProperties": false
    }),
}

pub fn execute(conn: &Connection, arguments: &Value) -> Result<Value> {
    let args = Args::parse(arguments, schema())?;

    let memory_id = args.required_i64("memory_id")?;
    let content = args.required_string("content")?;

    memories::update(conn, memory_id, &content)?;
    memory_list(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::error::CoreError;

    #[test]
    fn requires_content() {
        let conn = db::open_in_memory().unwrap();
        let id = db::memories::add(&conn, &["朝型".to_string()]).unwrap()[0].id;

        let err = execute(&conn, &json!({ "memory_id": id })).unwrap_err();

        assert!(matches!(err, CoreError::InvalidArgument { .. }));
    }

    #[test]
    fn rewrites_and_returns_all_memories() {
        let conn = db::open_in_memory().unwrap();
        let id = db::memories::add(&conn, &["朝型".to_string()]).unwrap()[0].id;

        let result = execute(&conn, &json!({ "memory_id": id, "content": "夜型" })).unwrap();

        assert_eq!(result[0]["content"], "夜型");
    }

    #[test]
    fn unknown_memory_is_not_found() {
        let conn = db::open_in_memory().unwrap();
        let err = execute(&conn, &json!({ "memory_id": 99, "content": "夜型" })).unwrap_err();
        assert!(matches!(err, CoreError::MemoryNotFound(99)));
    }
}
