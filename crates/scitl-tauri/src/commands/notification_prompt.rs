use tauri::ipc::Channel;
use tauri::State;

use crate::AppState;

/// 通知の許可を求める説明を出す知らせ先を受け取る。画面が起動時に渡し、渡し直したら置き換える。
/// 知らせるのは応答生成が終わったとき(`lib.rs`の`finished_sink`)。
#[tauri::command]
pub fn watch_notification_prompt(state: State<'_, AppState>, on_prompt: Channel<()>) {
    state.notification_prompt.set(on_prompt);
}

/// 説明への答え([`scitl_core::permissions::answer_notification_prompt`])。
#[tauri::command]
pub fn answer_notification_prompt(allow: bool) {
    scitl_core::permissions::answer_notification_prompt(allow);
}
