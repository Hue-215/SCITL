pub mod chat;
pub mod link;
pub mod mcp;
pub mod settings;
pub mod tasks;

use std::sync::Arc;

use scitl_core::db::{self, error::Result, Connection};
use scitl_core::settings::Settings;
use tauri::State;

use crate::AppState;

/// コマンドからリポジトリ層を1回呼ぶ定型。ロックの扱いは`db::with_conn`に閉じており、
/// ここではフロントエンドへ返す形(`String`のエラー)に揃えるだけを足す。
async fn with_db<F, T>(state: &State<'_, AppState>, f: F) -> std::result::Result<T, String>
where
    F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    db::with_conn(state.db.clone(), f)
        .await
        .map_err(|e| e.to_string())
}

/// コマンドから設定操作を1回呼ぶ定型。設定操作は資格情報ストアとファイルのI/Oを伴うため、
/// メインスレッド(同期コマンドの実行先)でもランタイムのワーカーでもなく
/// `blocking::run`で呼ぶ(architecture.md 4節)。
async fn with_settings<F, T>(state: &State<'_, AppState>, f: F) -> std::result::Result<T, String>
where
    F: FnOnce(&Settings) -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let settings = Arc::clone(&state.settings);
    scitl_core::blocking::run(move || f(&settings))
        .await
        .map_err(|e| e.to_string())
}
