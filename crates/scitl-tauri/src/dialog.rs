//! Rust側から出すネイティブのダイアログ(公式の`tauri-plugin-dialog`)。画面(WebView)には
//! プラグインの権限(capabilities)を与えず、乗っ取られた画面からは開けも閉じもできない
//! 確認の手段として使う(`architecture/webview-boundary.md`「CSP / Tauri権限設定」)。

use std::sync::atomic::{AtomicBool, Ordering};

use scitl_core::settings::DestinationDialog;
use tauri::AppHandle;
#[cfg(desktop)]
use tauri::Manager;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

#[cfg(desktop)]
use crate::MAIN_WINDOW;

/// 確認のダイアログを出している間は真。
static CONFIRMING: AtomicBool = AtomicBool::new(false);

/// [`CONFIRMING`]を立てている間持つ。ダイアログの表示に失敗して抜けても(panicを含む)外す。
struct Confirming;

impl Confirming {
    fn begin() -> Option<Self> {
        (!CONFIRMING.swap(true, Ordering::AcqRel)).then_some(Self)
    }
}

impl Drop for Confirming {
    fn drop(&mut self) {
        CONFIRMING.store(false, Ordering::Release);
    }
}

/// 新しい通信先の登録を確かめる受け口(`Settings::add_provider`・`add_mcp_server`)。承認された
/// ときだけ真。ダイアログを閉じるまで待つので、メインスレッドでもランタイムのワーカーでもなく
/// `blocking::run`の中で呼ばれる(coreの設定操作はそこで動く)。
///
/// 確認は同時に1つだけ出し、出している間に届いた登録は取りやめとして扱う。乗っ取られた画面が
/// 登録のコマンドを並べて呼び、ダイアログを積み重ねたり、待つスレッドで処理を詰まらせたり
/// するのを防ぐ。
pub fn confirm_destination(
    app: &AppHandle,
) -> impl FnOnce(&DestinationDialog) -> bool + Send + 'static {
    let app = app.clone();
    move |dialog| {
        let Some(_confirming) = Confirming::begin() else {
            return false;
        };
        let builder = app
            .dialog()
            .message(message_text(&dialog.message))
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
