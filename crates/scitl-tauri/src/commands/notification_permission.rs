//! 通知の許可(Android 13以上)を、OSのダイアログの前に画面で説明する
//! (`architecture/concurrency.md`「応答が終わったことの通知」)。説明するかと、その時機(初めて応答を
//! 生成し終えたとき)はcoreが決め(`scitl_core::reply_notification::offer_permission`)、画面は
//! 知らされたら説明を出し、答えを返すだけ。

use std::sync::{Arc, Mutex};

use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};

use crate::AppState;

/// 説明を出すことの知らせ先。応答生成が終わったことの受け口(`AppState::finished`)からも送るので、
/// 共有して持つ。
pub type OfferWatcher = Arc<Mutex<Option<Channel<()>>>>;

/// 説明を出すことの知らせ先を受け取る。画面が起動時に渡し、渡し直したら置き換える。説明に答える前に
/// 画面が読み込み直されていたら、受け取ったらすぐに知らせ直す。
#[tauri::command]
pub fn watch_notification_offers(state: State<'_, AppState>, on_offer: Channel<()>) {
    *state
        .notification_offer
        .lock()
        .expect("notification offer mutex poisoned") = Some(on_offer);
    if offered() {
        send(&state.notification_offer);
    }
}

/// 説明への答え。`accept`ならOSのダイアログで許可を求める。説明を出していなければ何もしない。
#[tauri::command]
pub fn answer_notification_offer(accept: bool) {
    #[cfg(target_os = "android")]
    scitl_core::reply_notification::answer_permission_offer(accept);
    #[cfg(not(target_os = "android"))]
    let _ = accept;
}

/// 画面が読み込み直されるときに、前の画面の知らせ先を捨てる。
pub fn forget_watcher(app: &AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        *state
            .notification_offer
            .lock()
            .expect("notification offer mutex poisoned") = None;
    }
}

/// 画面へ説明を出すよう知らせる。知らせ先がまだ無ければ、渡されたときに知らせる。
pub fn send(watcher: &OfferWatcher) {
    if let Some(channel) = watcher
        .lock()
        .expect("notification offer mutex poisoned")
        .as_ref()
    {
        let _ = channel.send(());
    }
}

#[cfg(target_os = "android")]
fn offered() -> bool {
    scitl_core::reply_notification::permission_offered()
}

/// 通知はAndroidでしか出さないので、説明することも無い。
#[cfg(not(target_os = "android"))]
fn offered() -> bool {
    false
}
