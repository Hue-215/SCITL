use tauri::State;

use scitl_core::db::messages::OperationSource;
use scitl_core::orchestration::{self, discard_events, operations, TaskCreation};

use super::{with_db, CommandResult};
use crate::AppState;

/// フロントエンド向けIPCコマンド。`task_id`は表示中のタスクとしてフロントエンドが渡す
/// (ターン開始時に対象を固定するツール`get_current_task_detail`とは別の境界)。
#[tauri::command]
pub async fn get_task_detail(
    state: State<'_, AppState>,
    task_id: i64,
) -> CommandResult<scitl_core::db::tasks::TaskDetailView> {
    with_db(&state, move |conn| {
        scitl_core::db::tasks::get_task_detail_view(conn, task_id)
    })
    .await
}

/// サイドバー向けのタスク一覧(アーカイブ済み/未アーカイブの振り分けはフロントエンド側)。
#[tauri::command]
pub async fn list_tasks(
    state: State<'_, AppState>,
) -> CommandResult<Vec<scitl_core::db::tasks::TaskListItem>> {
    with_db(&state, scitl_core::db::tasks::list_tasks).await
}

/// 新規タスク追加ボタン。作れたら、画面は続けて`open_task_chat`で聞き取りを始める。
/// チャットを使えない間は作らずに理由を返す。
#[tauri::command]
pub async fn create_task(state: State<'_, AppState>) -> CommandResult<TaskCreation> {
    // 使えるかどうかは設定だけで決まるので、推論サーバーへ問い合わせる`snapshot_for_turn`は
    // 使わない。
    let snapshot = state.settings.snapshot();
    Ok(orchestration::create_task(
        state.db.clone(),
        &snapshot.turn_context(&state.generating, &state.attachments, &discard_events),
    )
    .await?)
}

/// ヘッダーからタイトルを変更し、会話ログに記録する。
#[tauri::command]
pub async fn rename_task(
    state: State<'_, AppState>,
    task_id: i64,
    title: String,
) -> CommandResult<()> {
    Ok(operations::rename_task(
        state.db.clone(),
        &state.generating,
        OperationSource::Ui,
        task_id,
        title,
    )
    .await?)
}

#[tauri::command]
pub async fn set_task_archived(
    state: State<'_, AppState>,
    task_id: i64,
    archived: bool,
) -> CommandResult<()> {
    Ok(operations::set_task_archived(
        state.db.clone(),
        &state.generating,
        OperationSource::Ui,
        task_id,
        archived,
    )
    .await?)
}

/// タスクの論理削除。モデルには公開しない操作で、画面・CLIからのみ行う。
#[tauri::command]
pub async fn delete_task(state: State<'_, AppState>, task_id: i64) -> CommandResult<()> {
    Ok(operations::delete_task(
        state.db.clone(),
        &state.generating,
        OperationSource::Ui,
        task_id,
    )
    .await?)
}
