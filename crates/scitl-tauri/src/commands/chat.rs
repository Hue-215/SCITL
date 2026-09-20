use tauri::State;

use scitl_core::llm::ResponseEvent;
use scitl_core::orchestration::{run_turn, SystemPrompts};

use crate::AppState;

/// タスクチャットへの発言送信。`task_id`は文脈(表示中のタスク)から決まる引数であり、
/// モデルへのツール引数には出てこない(update_taskのタスクチャット版と同じ区別。
/// docs/spec/rebuild/tools.md 1節)。
#[tauri::command]
pub async fn send_task_chat_message(
    state: State<'_, AppState>,
    task_id: i64,
    text: String,
) -> Result<Vec<ResponseEvent>, String> {
    // ロックはアダプタの`Arc`と2種のシステムプロンプトの複製を取るまでだけ持つ(main.rsの
    // `AppState::runtime`のドキュメント参照)。`run_turn`のawaitをロック保持中にまたがせない。
    let (adapter, system_prompt, task_chat_system_prompt) = {
        let runtime = state.runtime.lock().expect("runtime mutex poisoned");
        (
            runtime.adapter.clone(),
            runtime.config.general.system_prompt.clone(),
            runtime.config.general.task_chat_system_prompt.clone(),
        )
    };
    let adapter =
        adapter.ok_or_else(|| "no LLM provider is configured; add one in settings".to_string())?;

    let prompts = SystemPrompts {
        base: system_prompt.as_deref(),
        task_chat: task_chat_system_prompt.as_deref(),
    };

    run_turn(state.db.clone(), adapter.as_ref(), task_id, text, &prompts)
        .await
        .map_err(|e| e.to_string())
}

/// タスクチャンネルの発言履歴取得(#37)。`commands::tasks`と同じ
/// `spawn_blocking` + ロックの型を踏襲する。
#[tauri::command]
pub async fn list_task_messages(
    state: State<'_, AppState>,
    task_id: i64,
) -> Result<Vec<scitl_core::db::messages::Message>, String> {
    let db = state.db.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let conn = db.lock().expect("db mutex poisoned");
        scitl_core::db::messages::list_for_task(&conn, task_id)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}
