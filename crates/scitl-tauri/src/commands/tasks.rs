use tauri::State;

use crate::AppState;

/// フロントエンド向けIPCコマンド。`task_id`は表示中のタスクとしてフロントエンドが渡す
/// (ツール`get_current_task_detail`の「ターン開始時に束縛される引数無し」とは異なる境界。
/// architecture.md 7節)。
#[tauri::command]
pub async fn get_task_detail(
    state: State<'_, AppState>,
    task_id: i64,
) -> Result<scitl_core::db::tasks::Task, String> {
    let db = state.db.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let conn = db.lock().expect("db mutex poisoned");
        scitl_core::db::tasks::get_task(&conn, task_id)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}
