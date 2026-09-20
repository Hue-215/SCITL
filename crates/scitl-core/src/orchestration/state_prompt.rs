use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db::error::Result;
use crate::db::{now_iso8601, task_steps, tasks};

/// システムプロンプト3種のうち今回扱う2種(docs/spec/legacy/data-model.md 3節)。
/// タイトル生成用プロンプトはIssue #46の担当。総合チャットとタスクチャットでは
/// 公開ツールが異なるため(docs/spec/rebuild/tools.md 5節)、`base`と`task_chat`を
/// 分けて持つ。`Option<&str>`を2つ並べて渡すと取り違えの余地が生まれるため
/// (tools.md 1節が修正した「対象タスクの取り違え」と同種の事故)、名前で縛る。
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemPrompts<'a> {
    pub base: Option<&'a str>,
    pub task_chat: Option<&'a str>,
}

/// 基本システムプロンプト + タスクチャット用システムプロンプト + 現在日時 +
/// タスク・工程の最新状態JSON + 同一ターン内で実行済みの操作の再掲、を毎ターン組み立てる
/// (docs/spec/legacy/backend.md 4節 手順2、docs/spec/principles.md 3節
/// 「最新状態は毎ターン渡す」)。状態系ツールの実行結果はこの再構築で完全に代替できるため、
/// 会話履歴には別途投入しない(docs/spec/rebuild/tools.md 4節)。
pub fn build_system_prompt(
    conn: &Connection,
    task_id: i64,
    prompts: &SystemPrompts,
    executed_ops: &[Value],
) -> Result<String> {
    let task = tasks::get_task(conn, task_id)?;
    let steps = task_steps::list_for_task(conn, task_id)?;

    let mut sections = Vec::new();
    if let Some(prompt) = prompts.base.filter(|p| !p.is_empty()) {
        sections.push(prompt.to_string());
    }
    if let Some(prompt) = prompts.task_chat.filter(|p| !p.is_empty()) {
        sections.push(prompt.to_string());
    }

    sections.push(format!("current datetime (ISO8601 UTC): {}", now_iso8601()));

    let state = json!({ "task": task, "steps": steps });
    sections.push(format!("current task state:\n{state}"));

    if !executed_ops.is_empty() {
        sections.push(format!(
            "operations already executed this turn (do not repeat them):\n{}",
            json!(executed_ops)
        ));
    }

    Ok(sections.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[test]
    fn includes_base_prompt_state_and_executed_ops() {
        let conn = db::open_in_memory().unwrap();
        let task_id = db::tasks::create_task(&conn).unwrap().id;
        db::task_steps::add_steps(&conn, task_id, &["買い出し".to_string()]).unwrap();

        let executed_ops = vec![json!({ "tool": "add_steps", "result": "ok" })];
        let prompts = SystemPrompts {
            base: Some("base prompt"),
            task_chat: None,
        };
        let prompt = build_system_prompt(&conn, task_id, &prompts, &executed_ops).unwrap();

        assert!(prompt.contains("base prompt"));
        assert!(prompt.contains("買い出し"));
        assert!(prompt.contains("add_steps"));
    }

    #[test]
    fn omits_executed_ops_section_when_empty() {
        let conn = db::open_in_memory().unwrap();
        let task_id = db::tasks::create_task(&conn).unwrap().id;

        let prompt = build_system_prompt(&conn, task_id, &SystemPrompts::default(), &[]).unwrap();

        assert!(!prompt.contains("already executed"));
    }

    #[test]
    fn concatenates_base_and_task_chat_prompts_in_order() {
        let conn = db::open_in_memory().unwrap();
        let task_id = db::tasks::create_task(&conn).unwrap().id;

        let prompts = SystemPrompts {
            base: Some("base prompt"),
            task_chat: Some("task chat prompt"),
        };
        let prompt = build_system_prompt(&conn, task_id, &prompts, &[]).unwrap();

        let base_pos = prompt.find("base prompt").unwrap();
        let task_chat_pos = prompt.find("task chat prompt").unwrap();
        assert!(base_pos < task_chat_pos);
    }

    #[test]
    fn works_with_only_task_chat_prompt() {
        let conn = db::open_in_memory().unwrap();
        let task_id = db::tasks::create_task(&conn).unwrap().id;

        let prompts = SystemPrompts {
            base: None,
            task_chat: Some("task chat prompt"),
        };
        let prompt = build_system_prompt(&conn, task_id, &prompts, &[]).unwrap();

        assert!(prompt.contains("task chat prompt"));
    }
}
