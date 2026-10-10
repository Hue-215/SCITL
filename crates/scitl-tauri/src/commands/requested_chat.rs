use tauri::ipc::Channel;
use tauri::{AppHandle, State};

use scitl_core::db::messages::Chat;

use crate::AppState;

/// 通知を押して開くことになった会話の知らせ先を受け取る。画面が起動時に渡し、渡し直したら
/// 置き換える。受け取るのは知らせ先だけ。通知を押してアプリが起動した場合は、画面が渡す前に頼みが
/// 届いているので、受け取ったらすぐに届ける。
#[tauri::command]
pub fn watch_requested_chats(
    app: AppHandle,
    state: State<'_, AppState>,
    on_request: Channel<Chat>,
) {
    *state
        .requested_chat
        .lock()
        .expect("requested chat mutex poisoned") = Some(on_request);
    deliver(&app);
}

/// 通知を押して届いた、開く会話の頼みがあれば、確かめてから画面へ知らせる
/// (`scitl_core::reply_notification`)。頼みはActivityが持っていて、アプリが前に出たときと、画面が
/// 知らせ先を渡したときに引き取る。画面が知らせ先を渡す前なら、引き取らずに残す。
#[cfg(target_os = "android")]
pub fn deliver(app: &AppHandle) {
    use tauri::Manager;

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };
        let Some(channel) = state
            .requested_chat
            .lock()
            .expect("requested chat mutex poisoned")
            .clone()
        else {
            return;
        };
        let Some(code) = scitl_core::reply_notification::take_requested_chat() else {
            return;
        };
        let resolved = scitl_core::db::with_conn(state.db.clone(), move |conn| {
            Ok(scitl_core::reply_notification::resolve_requested_chat(
                conn, code,
            ))
        })
        .await;
        if let Ok(Some(chat)) = resolved {
            let _ = channel.send(chat);
        }
    });
}

/// 通知はAndroidでしか出さないので、ほかでは届くものが無い。
#[cfg(not(target_os = "android"))]
pub fn deliver(_app: &AppHandle) {}
