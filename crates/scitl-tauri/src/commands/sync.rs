//! 端末間の同期(`docs/sync/index.md` 11節)。どちらの役も、始める前にRust側からネイティブの
//! ダイアログで確かめる。途中経過と競合は`Channel`で知らせ、結果は失敗も含めて`SyncReport`で返す。

use scitl_sync::{ConflictChoice, Request, SyncEvent, SyncReport};
use tauri::ipc::Channel;
use tauri::{AppHandle, State};

use crate::{dialog, AppState};

#[tauri::command]
pub async fn sync_listen(
    app: AppHandle,
    state: State<'_, AppState>,
    on_event: Channel<SyncEvent>,
) -> Result<SyncReport, ()> {
    Ok(scitl_sync::listen(
        request(&state),
        dialog::confirm_sync(&app),
        forward(on_event),
    )
    .await)
}

/// `address`と`code`は利用者が打ったまま渡す(読むのも確かめるのもRust側)。
#[tauri::command]
pub async fn sync_connect(
    app: AppHandle,
    state: State<'_, AppState>,
    address: String,
    code: String,
    on_event: Channel<SyncEvent>,
) -> Result<SyncReport, ()> {
    Ok(scitl_sync::connect_to(
        request(&state),
        &address,
        &code,
        dialog::confirm_sync(&app),
        forward(on_event),
    )
    .await)
}

/// カメラで待ち受ける側のQRを読み、その相手へつないで同期する。読み取りはRust側からOSに頼み、
/// 読んだ中身は画面へ渡さない。
#[tauri::command]
pub async fn sync_connect_scanned(
    app: AppHandle,
    state: State<'_, AppState>,
    on_event: Channel<SyncEvent>,
) -> Result<SyncReport, ()> {
    Ok(scitl_sync::connect_scanned(
        request(&state),
        scitl_core::qr_scan::scan(),
        dialog::confirm_sync(&app),
        forward(on_event),
    )
    .await)
}

/// この端末でQRを読めるか(読めなければ、画面は読むボタンを出さない)。
#[tauri::command]
pub fn can_scan_sync_qr() -> bool {
    scitl_core::qr_scan::available()
}

/// 進んでいる同期を止める。同期していなければ何もしない。
#[tauri::command]
pub fn cancel_sync(state: State<'_, AppState>) {
    scitl_sync::cancel(&state.generating);
}

/// 見せている競合の、選んだ側を渡す。見せている競合のすべてに1つずつ選んでいなければ受け取らず、
/// `false`を返す。
#[tauri::command]
pub fn resolve_sync_conflicts(state: State<'_, AppState>, choices: Vec<ConflictChoice>) -> bool {
    scitl_sync::answer(&state.sync_answers, choices)
}

/// 途中経過を画面へ流す。画面が閉じていて届かなくても、同期は続ける。
fn forward(on_event: Channel<SyncEvent>) -> impl Fn(SyncEvent) + Sync {
    move |event| {
        let _ = on_event.send(event);
    }
}

fn request<'a>(state: &'a State<'_, AppState>) -> Request<'a> {
    Request {
        ctx: &state.sync,
        generating: &state.generating,
        answers: &state.sync_answers,
        lang: state.settings.display_language(),
    }
}
