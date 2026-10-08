use tauri::State;

use scitl_core::db::memories::{self, Memory};

use super::{with_db, CommandResult};
use crate::AppState;

/// 設定のメモリタブの一覧。
#[tauri::command]
pub async fn list_memories(state: State<'_, AppState>) -> CommandResult<Vec<Memory>> {
    with_db(&state, memories::list).await
}

/// メモリタブからの書き足し。規則はツールと同じ(`db::memories::add`)。
#[tauri::command]
pub async fn add_memory(state: State<'_, AppState>, content: String) -> CommandResult<()> {
    with_db(&state, move |conn| {
        memories::add(conn, &[content]).map(drop)
    })
    .await
}

/// メモリタブからの編集(`db::memories::update`)。
#[tauri::command]
pub async fn update_memory(
    state: State<'_, AppState>,
    memory_id: i64,
    content: String,
) -> CommandResult<()> {
    with_db(&state, move |conn| {
        memories::update(conn, memory_id, &content).map(drop)
    })
    .await
}

/// メモリタブからの削除(論理削除。`db::memories::delete`)。
#[tauri::command]
pub async fn delete_memory(state: State<'_, AppState>, memory_id: i64) -> CommandResult<()> {
    with_db(&state, move |conn| memories::delete(conn, memory_id)).await
}
