use rusqlite::Connection;
use serde_json::json;

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
/// タスク・工程の最新状態JSONを毎ターン組み立てる(docs/spec/principles.md 3節
/// 「最新状態は毎ターン渡す」)。この最新状態は**次ターン以降**の入力履歴を代替するもので、
/// 同一ターン内のツール呼び出しループでの往復は`turn.rs`が別途モデルに返す
/// (docs/spec/rebuild/tools.md 4節「同一ターン内では分類によらず結果を返す」)。
/// このため、旧実装にあった「同一ターン内で実行済みの操作の再掲」はここでは持たない
/// (往復そのものが同じ事実を伝えるため二重になる)。
pub fn build_system_prompt(
    conn: &Connection,
    task_id: i64,
    prompts: &SystemPrompts,
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

    // ユーザー発言を包む予約タグの読み方(Issue #68)。囲みと`sent_at`の意味を伝えないと、
    // モデルはタグを本文の一部と受け取り、応答にそのまま書き写す。
    sections.push(
        "user messages are wrapped as \
         <scitl:user-message sent_at=\"...\">body</scitl:user-message>. \
         sent_at is when the user sent that message (ISO8601 UTC); it is metadata, \
         not part of what the user wrote. Use it to resolve relative dates such as \
         \"tomorrow\". Never write these tags or timestamps in your own reply."
            .to_string(),
    );

    sections.push(format!("current datetime (ISO8601 UTC): {}", now_iso8601()));

    let state = json!({ "task": task, "steps": steps });
    sections.push(format!("current task state:\n{state}"));

    Ok(sections.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[test]
    fn includes_base_prompt_and_state() {
        let conn = db::open_in_memory().unwrap();
        let task_id = db::tasks::create_task(&conn).unwrap().id;
        db::task_steps::add_steps(&conn, task_id, &["買い出し".to_string()]).unwrap();

        let prompts = SystemPrompts {
            base: Some("base prompt"),
            task_chat: None,
        };
        let prompt = build_system_prompt(&conn, task_id, &prompts).unwrap();

        assert!(prompt.contains("base prompt"));
        assert!(prompt.contains("買い出し"));
    }

    #[test]
    fn concatenates_base_and_task_chat_prompts_in_order() {
        let conn = db::open_in_memory().unwrap();
        let task_id = db::tasks::create_task(&conn).unwrap().id;

        let prompts = SystemPrompts {
            base: Some("base prompt"),
            task_chat: Some("task chat prompt"),
        };
        let prompt = build_system_prompt(&conn, task_id, &prompts).unwrap();

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
        let prompt = build_system_prompt(&conn, task_id, &prompts).unwrap();

        assert!(prompt.contains("task chat prompt"));
    }

    #[test]
    fn works_with_no_prompts_at_all() {
        let conn = db::open_in_memory().unwrap();
        let task_id = db::tasks::create_task(&conn).unwrap().id;

        let prompt = build_system_prompt(&conn, task_id, &SystemPrompts::default()).unwrap();

        assert!(prompt.contains("current task state"));
    }
}
