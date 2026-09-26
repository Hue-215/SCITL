use tauri::State;

use scitl_core::orchestration::{self, discard_events, TaskCreation};

use super::with_db;
use crate::AppState;

/// フロントエンド向けIPCコマンド。`task_id`は表示中のタスクとしてフロントエンドが渡す
/// (ツール`get_current_task_detail`の「ターン開始時に束縛される引数無し」とは異なる境界。
/// architecture.md 7節)。
#[tauri::command]
pub async fn get_task_detail(
    state: State<'_, AppState>,
    task_id: i64,
) -> Result<scitl_core::db::tasks::TaskDetailView, String> {
    with_db(&state, move |conn| {
        scitl_core::db::tasks::get_task_detail_view(conn, task_id)
    })
    .await
}

/// サイドバー向けのタスク一覧(アーカイブ済み/未アーカイブの振り分けはフロントエンド側)。
#[tauri::command]
pub async fn list_tasks(
    state: State<'_, AppState>,
) -> Result<Vec<scitl_core::db::tasks::TaskListItem>, String> {
    with_db(&state, scitl_core::db::tasks::list_tasks).await
}

/// 新規タスク追加ボタン。作れたら、画面は続けて`open_task_chat`で聞き取りを始める
/// (legacy/frontend.md 1節)。チャットを使えない間は作らずに理由を返す。
#[tauri::command]
pub async fn create_task(state: State<'_, AppState>) -> Result<TaskCreation, String> {
    // 使えるかどうかは設定だけで決まるので、推論サーバーへ問い合わせる`snapshot_for_turn`は
    // 使わない。
    let snapshot = state.settings.snapshot();
    orchestration::create_task(
        state.db.clone(),
        &snapshot.turn_context(&state.generating, &discard_events),
    )
    .await
    .map_err(|e| e.to_string())
}
