//! Rust側から出すネイティブのダイアログ(公式の`tauri-plugin-dialog`)。画面(WebView)には
//! プラグインの権限(capabilities)を与えず、乗っ取られた画面からは開けも閉じもできない
//! 確認の手段として使う(`architecture/webview-boundary.md`「CSP / Tauri権限設定」)。
//!
//! どれもダイアログを閉じるまで待つので、メインスレッドでもランタイムのワーカーでもなく
//! `blocking::run`の中で呼ぶ。

use std::sync::atomic::{AtomicBool, Ordering};

use scitl_core::link::LinkDialog;
use scitl_core::settings::DestinationDialog;
use tauri::AppHandle;
#[cfg(desktop)]
use tauri::Manager;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

#[cfg(desktop)]
use crate::MAIN_WINDOW;

/// ダイアログを出している間は真。
static SHOWING: AtomicBool = AtomicBool::new(false);

/// [`SHOWING`]を立てている間持つ。ダイアログの表示に失敗して抜けても(panicを含む)外す。
struct Showing;

impl Showing {
    fn begin() -> Option<Self> {
        (!SHOWING.swap(true, Ordering::AcqRel)).then_some(Self)
    }
}

impl Drop for Showing {
    fn drop(&mut self) {
        SHOWING.store(false, Ordering::Release);
    }
}

/// ダイアログを出し、承認(1つ目のボタン)が押されたら真。ダイアログは同時に1つだけ出し、出して
/// いる間に届いたものは出さずに`false`とする。乗っ取られた画面がコマンドを並べて呼び、ダイアログを
/// 積み重ねたり、待つスレッドで処理を詰まらせたりするのを防ぐ。
fn show(
    app: &AppHandle,
    title: &str,
    message: &str,
    kind: MessageDialogKind,
    buttons: MessageDialogButtons,
) -> bool {
    let Some(_showing) = Showing::begin() else {
        return false;
    };
    let builder = app
        .dialog()
        .message(message_text(message))
        .title(title)
        .kind(kind)
        .buttons(buttons);
    // 窓の前に出し、閉じるまで窓を操作させない(モバイルのダイアログは元から画面の前に出る)。
    #[cfg(desktop)]
    let builder = match app.get_webview_window(MAIN_WINDOW) {
        Some(window) => builder.parent(&window),
        None => builder,
    };
    builder.blocking_show()
}

/// 新しい通信先の登録を確かめる受け口(`Settings::add_provider`・`add_mcp_server`)。承認された
/// ときだけ真。
pub fn confirm_destination(
    app: &AppHandle,
) -> impl FnOnce(&DestinationDialog) -> bool + Send + 'static {
    let app = app.clone();
    move |dialog| {
        show(
            &app,
            &dialog.title,
            &dialog.message,
            MessageDialogKind::Warning,
            MessageDialogButtons::OkCancelCustom(
                dialog.confirm_label.clone(),
                dialog.cancel_label.clone(),
            ),
        )
    }
}

/// 本文中のリンクを開く前の確認。開けるリンクは「開く」で承認されたときだけ真で、開けないリンクは
/// 理由を知らせて閉じるだけにする(いつも偽)。
pub fn confirm_link(app: &AppHandle, dialog: &LinkDialog) -> bool {
    let kind = if dialog.warning {
        MessageDialogKind::Warning
    } else {
        MessageDialogKind::Info
    };
    match &dialog.open_label {
        Some(open) => show(
            app,
            &dialog.title,
            &dialog.message,
            kind,
            MessageDialogButtons::OkCancelCustom(open.clone(), dialog.close_label.clone()),
        ),
        None => {
            show(
                app,
                &dialog.title,
                &dialog.message,
                MessageDialogKind::Warning,
                MessageDialogButtons::OkCustom(dialog.close_label.clone()),
            );
            false
        }
    }
}

/// 知らせるだけのダイアログ(閉じるボタン1つ)。
pub fn notify(app: &AppHandle, title: &str, message: &str, close_label: &str) {
    show(
        app,
        title,
        message,
        MessageDialogKind::Error,
        MessageDialogButtons::OkCustom(close_label.to_string()),
    );
}

/// GTKのダイアログ(Linux等。`rfd`のgtk3)は、本文をprintfの書式として渡す
/// (`gtk_message_dialog_format_secondary_text`に後続の引数なしで渡す。rfd 0.16で確認)。
/// URLや名前に含まれる`%`が書式指定として読まれ、表示が化けるうえ、`%s`・`%n`で落ちる・
/// メモリを書き換えられるので、`%`を`%%`にして文字として出させる。見出しは`"%s"`で渡るので要らない。
fn message_text(message: &str) -> String {
    if cfg!(all(desktop, not(any(windows, target_os = "macos")))) {
        message.replace('%', "%%")
    } else {
        message.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_signs_are_not_format_directives_on_gtk() {
        let shown = message_text("https://example.com/%s%n%2F 50%");
        if cfg!(all(desktop, not(any(windows, target_os = "macos")))) {
            assert_eq!(shown, "https://example.com/%%s%%n%%2F 50%%");
        } else {
            assert_eq!(shown, "https://example.com/%s%n%2F 50%");
        }
    }
}
