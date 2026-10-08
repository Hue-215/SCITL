pub mod attachments;
pub mod chat;
pub mod export;
pub mod link;
pub mod mcp;
pub mod memories;
pub mod settings;
pub mod startup;
pub mod tasks;

use std::sync::Arc;

use scitl_core::db::{self, Connection};
use scitl_core::error::{CoreError, Result};
use scitl_core::settings::Settings;
use tauri::ipc::InvokeError;
use tauri::State;

use crate::AppState;

/// コマンドの失敗。画面には`CoreError`の表示文を文字列として渡す。コマンドは`?`で返すだけに
/// して、画面へ渡す形をここ1箇所で決める。
#[derive(Debug)]
pub struct CommandError(String);

impl From<CoreError> for CommandError {
    fn from(error: CoreError) -> Self {
        Self(error.to_string())
    }
}

impl From<&str> for CommandError {
    fn from(message: &str) -> Self {
        Self(message.to_string())
    }
}

impl From<CommandError> for InvokeError {
    fn from(error: CommandError) -> Self {
        Self::from(error.0)
    }
}

pub type CommandResult<T> = std::result::Result<T, CommandError>;

/// コマンドからリポジトリ層を1回呼ぶ定型。ロックの扱いは`db::with_conn`に閉じており、
/// ここではフロントエンドへ返す形([`CommandError`])に揃えるだけを足す。
async fn with_db<F, T>(state: &State<'_, AppState>, f: F) -> CommandResult<T>
where
    F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    Ok(db::with_conn(state.db.clone(), f).await?)
}

/// コマンドから設定操作を1回呼ぶ定型。設定操作は資格情報ストアとファイルのI/Oを伴うため、
/// メインスレッド(同期コマンドの実行先)でもランタイムのワーカーでもなく
/// `blocking::run`で呼ぶ。
async fn with_settings<F, T>(state: &State<'_, AppState>, f: F) -> CommandResult<T>
where
    F: FnOnce(&Settings) -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let settings = Arc::clone(&state.settings);
    Ok(scitl_core::blocking::run(move || f(&settings)).await?)
}
