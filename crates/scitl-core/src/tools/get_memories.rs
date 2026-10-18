use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db::memories;
use crate::error::Result;

use super::args::Args;
use super::Run;
use super::{IdPosition, IdTable, Segment, NO_IDS};

internal_tool! {
    /// メモリはどの会話にも属さないので、会話によらず同一のスキーマ。
    name: "get_memories",
    run: Run::Read(execute),
    ids: super::IdPositions { arguments: NO_IDS, result: MEMORY_LIST },
    "List the memories.",
    json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false
    }),
}

/// [`memory_list`]の形のうち、IDの入る位置。
pub(super) const MEMORY_LIST: &[IdPosition] = &[IdPosition {
    path: &[Segment::Each, Segment::Key("id")],
    table: IdTable::Memory,
}];

pub fn execute(conn: &Connection, arguments: &Value) -> Result<Value> {
    Args::parse(arguments, schema())?;

    memory_list(conn)
}

/// メモリの全体。読み取りと書き込み系のツールが同じ形で返す。
pub(super) fn memory_list(conn: &Connection) -> Result<Value> {
    let memories = memories::list(conn)?;
    Ok(serde_json::to_value(memories).expect("Memory serialization cannot fail"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::error::CoreError;

    #[test]
    fn rejects_unknown_argument() {
        let conn = db::open_in_memory().unwrap();
        let err = execute(&conn, &json!({ "task_id": 1 })).unwrap_err();
        assert!(matches!(err, CoreError::UnknownArgument(_)));
    }

    #[test]
    fn lists_memories() {
        let conn = db::open_in_memory().unwrap();
        db::memories::add(&conn, &["朝型".to_string()]).unwrap();

        let result = execute(&conn, &json!({})).unwrap();

        assert_eq!(result[0]["content"], "朝型");
    }
}
