use scitl_core::paths::DataDirError;
use tauri::{AppHandle, Manager};

use crate::StartupFailure;

/// 起動時にデータディレクトリを開けなかった理由。開けていればnull。画面は描く前に1度だけ呼ぶ。
#[tauri::command]
pub fn get_startup_failure(app: AppHandle) -> Option<DataDirError> {
    app.try_state::<StartupFailure>()
        .map(|failure| failure.0.clone())
}
