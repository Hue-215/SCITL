pub mod update_task;

use rusqlite::Connection;
use serde_json::Value;

use crate::db::error::{CoreError, Result};
use crate::llm::ToolSchema;

/// タスクチャット向けの公開ツール一覧(docs/spec/rebuild/tools.md 2節)。
/// `task_id`はターン開始時にオーケストレーション層が束縛するため、
/// ここでは引数として公開しない(architecture.md 7節)。
pub fn task_chat_tools() -> Vec<ToolSchema> {
    vec![update_task::schema()]
}

/// タスクチャット面でのツール実行。`task_id`は呼び出し元(orchestration)が
/// 文脈から渡す(モデルには公開しない)。
pub fn execute_task_chat_tool(
    conn: &Connection,
    task_id: i64,
    tool_name: &str,
    arguments: &Value,
) -> Result<Value> {
    match tool_name {
        update_task::NAME => update_task::execute(conn, task_id, arguments),
        other => Err(CoreError::UnknownArgument(format!("unknown tool: {other}"))),
    }
}
