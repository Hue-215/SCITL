use tauri::State;

use scitl_core::llm::ResponseEvent;
use scitl_core::orchestration::run_turn;

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
    // ロックはアダプタの`Arc`とシステムプロンプトの複製を取るまでだけ持つ(main.rsの
    // `AppState::runtime`のドキュメント参照)。`run_turn`のawaitをロック保持中にまたがせない。
    let (adapter, system_prompt) = {
        let runtime = state.runtime.lock().expect("runtime mutex poisoned");
        (runtime.adapter.clone(), runtime.config.general.system_prompt.clone())
    };
    let adapter =
        adapter.ok_or_else(|| "no LLM provider is configured; add one in settings".to_string())?;

    run_turn(
        state.db.clone(),
        adapter.as_ref(),
        task_id,
        text,
        system_prompt.as_deref(),
    )
    .await
    .map_err(|e| e.to_string())
}
