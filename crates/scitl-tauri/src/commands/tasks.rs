use tauri::ipc::Channel;
use tauri::State;

use scitl_core::db::messages::OperationSource;
use scitl_core::orchestration::{self, operations, TaskCreation, TaskOpeningEvent};

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

/// 新規タスク追加ボタン。作ったら続けて聞き取りを始める([`orchestration::create_task`])。
/// 作ったタスクと聞き取りの途中経過は、この順で`on_event`へ送る。送れなくても(画面が
/// 閉じた等)聞き取りは最後まで走らせるので、送信の失敗は捨てる。チャットを使えない間は
/// 作らずに理由を返す。
#[tauri::command]
pub async fn create_task(
    state: State<'_, AppState>,
    on_event: Channel<TaskOpeningEvent>,
) -> CommandResult<TaskCreation> {
    let snapshot = state.settings.snapshot_for_turn().await;
    let events = |event| {
        let _ = on_event.send(TaskOpeningEvent::Turn { event });
    };
    Ok(orchestration::create_task(
        state.db.clone(),
        &snapshot.turn_context(&state.generating, &state.attachments, &events),
        |task| {
            let _ = on_event.send(TaskOpeningEvent::Created { task: task.clone() });
        },
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
