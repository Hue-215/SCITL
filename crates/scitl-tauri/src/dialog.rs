//! Rust側から出すネイティブのダイアログ(公式の`tauri-plugin-dialog`)。画面(WebView)には
//! プラグインの権限(capabilities)を与えず、乗っ取られた画面からは開けも閉じもできない
//! 確認の手段として使う(`architecture/webview-boundary.md`「CSP / Tauri権限設定」)。

use scitl_core::settings::DestinationDialog;
use tauri::AppHandle;
#[cfg(desktop)]
use tauri::Manager;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

#[cfg(desktop)]
use crate::MAIN_WINDOW;

/// 新しい通信先の登録を確かめる受け口(`Settings::add_provider`・`add_mcp_server`)。承認された
/// ときだけ真。ダイアログを閉じるまで待つので、メインスレッドでもランタイムのワーカーでもなく
/// `blocking::run`の中で呼ばれる(coreの設定操作はそこで動く)。
pub fn confirm_destination(
    app: &AppHandle,
) -> impl FnOnce(&DestinationDialog) -> bool + Send + 'static {
    let app = app.clone();
    move |dialog| {
        let builder = app
            .dialog()
            .message(&dialog.message)
            .title(&dialog.title)
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom(
                dialog.confirm_label.clone(),
                dialog.cancel_label.clone(),
            ));
        // 窓の前に出し、閉じるまで窓を操作させない(モバイルのダイアログは元から画面の前に出る)。
        #[cfg(desktop)]
        let builder = match app.get_webview_window(MAIN_WINDOW) {
            Some(window) => builder.parent(&window),
            None => builder,
        };
        builder.blocking_show()
    }
}
