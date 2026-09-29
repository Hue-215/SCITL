//! Markdownエクスポートのコマンド。どちらもパスを受け取らず、書き出し先は起動時に決めた
//! `AppState::export_dir`だけ。

use std::sync::Arc;

use scitl_core::export::ExportSummary;
use tauri::State;

use crate::AppState;

#[tauri::command]
pub async fn export_markdown(state: State<'_, AppState>) -> Result<ExportSummary, String> {
    let attachments = Arc::clone(&state.attachments);
    scitl_core::export::export_markdown(state.db.clone(), &attachments, state.export_dir.clone())
        .await
        .map_err(|e| e.to_string())
}

/// 書き出し先のフォルダを開く。外部プロセスの起動を伴うため`blocking::run`で呼ぶ。
#[tauri::command]
pub async fn open_export_folder(state: State<'_, AppState>) -> Result<(), String> {
    let root = state.export_dir.clone();
    scitl_core::blocking::run(move || scitl_core::export::open_folder(&root))
        .await
        .map_err(|e| e.to_string())
}
