pub mod add_steps;
mod args;
pub mod delete_step;
pub mod external;
pub mod get_current_task_detail;
pub mod get_task_list;
pub mod update_step;
pub mod update_task;

use rusqlite::Connection;
use serde_json::Value;

use crate::db::error::{CoreError, Result};
use crate::db::task_steps;
use crate::llm::ToolSchema;

/// ツール実行結果を毎ターンの入力履歴に残すか否かの分類(docs/spec/rebuild/tools.md 4節)。
/// 状態系は次ターンの最新状態JSONで完全に代替できるため履歴に残さない。事実系
/// (検索・外部MCPツール等)を次ターン以降の履歴に残す仕組み自体はIssue #11の範囲で、
/// 外部ツール(Issue #44)の結果も現時点では同一ターン内に閉じる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    State,
}

pub struct ToolDefinition {
    pub schema: ToolSchema,
    pub kind: ToolKind,
}

/// 公開面。総合/MCPは枠のみ(docs/spec/rebuild/tools.md 1節、Issue #38範囲外)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    General,
    Task,
    Mcp,
}

/// 面ごとの公開ツール定義。実装関数は1つのまま、公開するスキーマだけを面で分ける
/// (docs/spec/rebuild/tools.md 1節「確定方針」)。
pub fn tool_definitions(surface: Surface) -> Vec<ToolDefinition> {
    match surface {
        Surface::Task => vec![
            ToolDefinition {
                schema: get_task_list::schema(),
                kind: ToolKind::State,
            },
            ToolDefinition {
                schema: get_current_task_detail::schema(),
                kind: ToolKind::State,
            },
            ToolDefinition {
                schema: update_task::schema(),
                kind: ToolKind::State,
            },
            ToolDefinition {
                schema: add_steps::schema(),
                kind: ToolKind::State,
            },
            ToolDefinition {
                schema: update_step::schema(),
                kind: ToolKind::State,
            },
            ToolDefinition {
                schema: delete_step::schema(),
                kind: ToolKind::State,
            },
        ],
        Surface::General | Surface::Mcp => Vec::new(),
    }
}

/// タスクチャット面で公開する内部ツールの名前。外部ツールの名前空間化で衝突を
/// 避けるために使う(`external::ExternalToolset::build`)。
pub fn task_chat_tool_names() -> Vec<String> {
    task_chat_tools()
        .iter()
        .map(|t| t.name().to_string())
        .collect()
}

/// タスクチャット向けの公開ツール一覧(docs/spec/rebuild/tools.md 2節)。
/// `task_id`はターン開始時にオーケストレーション層が束縛するため、
/// ここでは引数として公開しない(architecture.md 7節)。
pub fn task_chat_tools() -> Vec<ToolSchema> {
    tool_definitions(Surface::Task)
        .into_iter()
        .map(|def| def.schema)
        .collect()
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
        get_task_list::NAME => get_task_list::execute(conn, arguments),
        get_current_task_detail::NAME => get_current_task_detail::execute(conn, task_id, arguments),
        update_task::NAME => update_task::execute(conn, task_id, arguments),
        add_steps::NAME => add_steps::execute(conn, task_id, arguments),
        update_step::NAME => update_step::execute(conn, task_id, arguments),
        delete_step::NAME => delete_step::execute(conn, task_id, arguments),
        other => Err(CoreError::UnknownTool(other.to_string())),
    }
}

/// `step_id`はタスクIDと違いモデルの文脈に頼らず渡させる引数のため
/// (docs/spec/rebuild/tools.md 1節)、対象タスクの取り違え(同節が修正した過去の不具合)を
/// 防ぐには呼び出し側で所属チェックが要る。工程の更新・削除ツールで共有する。
fn require_step_in_task(conn: &Connection, task_id: i64, step_id: i64) -> Result<()> {
    let belongs = task_steps::list_for_task(conn, task_id)?
        .iter()
        .any(|step| step.id == step_id);
    if belongs {
        Ok(())
    } else {
        Err(CoreError::TaskStepNotFound(step_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[test]
    fn unknown_tool_is_reported_as_unknown_tool() {
        // モデルに返る文言が「未知の引数」にならないこと(ツール名の誤りだと伝える)。
        let conn = db::open_in_memory().unwrap();
        let task_id = db::tasks::create_task(&conn).unwrap().id;
        let err = execute_task_chat_tool(&conn, task_id, "no_such_tool", &serde_json::json!({}))
            .unwrap_err();
        assert!(matches!(&err, CoreError::UnknownTool(name) if name == "no_such_tool"));
    }
}
