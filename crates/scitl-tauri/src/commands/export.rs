//! Markdownエクスポートのコマンド。どちらもパスを受け取らず、書き出し先は起動時に決めた
//! `AppState::export_dir`だけ。

use std::sync::Arc;

use scitl_core::export::ExportSummary;
use tauri::State;

use super::CommandResult;
use crate::AppState;

#[tauri::command]
pub async fn export_markdown(state: State<'_, AppState>) -> CommandResult<ExportSummary> {
    let attachments = Arc::clone(&state.attachments);
    Ok(scitl_core::export::export_markdown(
        state.db.clone(),
        &attachments,
        state.export_dir.clone(),
    )
    .await?)
}

/// 書き出し先のフォルダを開く。外部プロセスの起動を伴うため`blocking::run`で呼ぶ。
#[tauri::command]
pub async fn open_export_folder(state: State<'_, AppState>) -> CommandResult<()> {
    let root = state.export_dir.clone();
    Ok(scitl_core::blocking::run(move || scitl_core::export::open_folder(&root)).await?)
}
