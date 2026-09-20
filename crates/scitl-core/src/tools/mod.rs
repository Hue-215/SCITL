pub mod add_steps;
pub mod delete_step;
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
/// (検索・外部MCPツール等、Issue #11)は本Issueの範囲外で、値としてはまだ登場しない。
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
        other => Err(CoreError::UnknownArgument(format!("unknown tool: {other}"))),
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
