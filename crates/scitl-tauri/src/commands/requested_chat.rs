use tauri::ipc::Channel;
#[cfg(target_os = "android")]
use tauri::Manager;
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
    state.requested_chat.set(on_request);
    deliver(&app);
}

/// 通知を押して届いた、開く会話の頼みがあれば、確かめてから画面へ知らせる
/// (`scitl_core::reply_notification`)。頼みはActivityが持っていて、アプリが前に出たときと、画面が
/// 知らせ先を渡したときに引き取る。画面が知らせ先を渡す前なら、引き取らずに残す。確かめている間に
/// 画面が読み込み直されたら、`AppState::unsent_requested_chat`に残す。Activityに新しい頼みがあれば、
/// 残したものより優先する。
#[cfg(target_os = "android")]
pub fn deliver(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };
        if state.requested_chat.current().is_none() {
            return;
        }
        let unsent = lock_unsent(&state).take();
        let chat = match scitl_core::reply_notification::take_requested_chat() {
            Some(code) => resolve(&state, code).await.or(unsent),
            None => unsent,
        };
        let Some(chat) = chat else {
            return;
        };
        // 知らせ先を取り直してから残すまでを、同じロックの中で行う。渡し直した画面への引き取りが
        // 残したものを見落とさないように。
        let mut unsent = lock_unsent(&state);
        match state.requested_chat.current() {
            Some(channel) => {
                let _ = channel.send(chat);
            }
            None => *unsent = Some(chat),
        }
    });
}

#[cfg(target_os = "android")]
async fn resolve(state: &AppState, code: i64) -> Option<Chat> {
    let resolved = scitl_core::db::with_conn(state.db.clone(), move |conn| {
        Ok(scitl_core::reply_notification::resolve_requested_chat(
            conn, code,
        ))
    })
    .await;
    resolved.unwrap_or_else(|e| {
        scitl_core::diagnostics::report(format_args!("could not check the chat to open: {e}"));
        None
    })
}

#[cfg(target_os = "android")]
fn lock_unsent(state: &AppState) -> std::sync::MutexGuard<'_, Option<Chat>> {
    state
        .unsent_requested_chat
        .lock()
        .expect("unsent requested chat mutex poisoned")
}

/// 通知はAndroidでしか出さないので、ほかでは届くものが無い。
#[cfg(not(target_os = "android"))]
pub fn deliver(_app: &AppHandle) {}
