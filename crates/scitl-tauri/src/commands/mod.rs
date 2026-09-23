pub mod chat;
pub mod mcp;
pub mod settings;
pub mod tasks;

use scitl_core::db::{self, error::Result, Connection};
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
