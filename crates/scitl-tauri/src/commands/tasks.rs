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

/// サイドバー向けのタスク一覧(アーカイブ済み/未アーカイブの振り分けはフロントエンド側)。
#[tauri::command]
pub async fn list_tasks(
    state: State<'_, AppState>,
) -> Result<Vec<scitl_core::db::tasks::TaskListItem>, String> {
    let db = state.db.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let conn = db.lock().expect("db mutex poisoned");
        scitl_core::db::tasks::list_tasks(&conn)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// 新規タスク追加ボタン。ユーザーの発言なしにAI側が聞き取りを開始する挙動(legacy/frontend.md
/// 1節)は、この後にフロントエンドが最初のダミー発言を送る形で実現する(別Issue)。
#[tauri::command]
pub async fn create_task(
    state: State<'_, AppState>,
) -> Result<scitl_core::db::tasks::Task, String> {
    let db = state.db.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let conn = db.lock().expect("db mutex poisoned");
        scitl_core::db::tasks::create_task(&conn)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}
