//! チャット本文中のリンクを開くIPCコマンド。画面は「このURLを開きたい」と伝えるだけで、判定
//! (`scitl_core::link`)も確認のダイアログもRust側で行う。確認はRust側から出すネイティブの
//! ダイアログなので、乗っ取られた画面から呼ばれても、利用者が「開く」を押さない限り開かない。

use tauri::{AppHandle, State};

use scitl_core::i18n;
use scitl_core::link;

use super::CommandResult;
use crate::{dialog, AppState};

/// リンクを判定して確認のダイアログを出し、「開く」が押されたらOSへ渡す。開けないリンクは理由を
/// 知らせるだけ。OSへ渡すのに失敗したら、その理由もダイアログで知らせる(画面は結果を待つだけ)。
/// ダイアログを閉じるまで待つので`blocking::run`で呼ぶ。
#[tauri::command]
pub async fn open_link(
    app: AppHandle,
    state: State<'_, AppState>,
    url: String,
) -> CommandResult<()> {
    let lang = state.settings.display_language();
    Ok(scitl_core::blocking::run(move || {
        let text = link::dialog(lang, &link::inspect(&url));
        if !dialog::confirm_link(&app, &text) {
            return Ok(());
        }
        if let Err(e) = open(&app, &url) {
            let message = i18n::format(
                lang,
                "link.open_failed",
                &[(
                    "error",
                    &scitl_core::text::reveal_invisible_line(&e.to_string()),
                )],
            );
            dialog::notify(
                &app,
                &text.title,
                &message,
                i18n::text(lang, "common.close"),
            );
        }
        Ok(())
    })
    .await?)
}

/// 承認されたリンクを開く。デスクトップはcoreが`open`クレートで開き、Androidでは`open`クレートが
/// 動かないので`tauri-plugin-opener`(`ACTION_VIEW`のIntent)へ渡す。どちらもcoreが判定し直して
/// 正規化したURLだけを渡す。
#[cfg(not(target_os = "android"))]
fn open(_app: &AppHandle, url: &str) -> scitl_core::error::Result<()> {
    link::open_confirmed(url)
}

#[cfg(target_os = "android")]
fn open(app: &AppHandle, url: &str) -> scitl_core::error::Result<()> {
    use tauri_plugin_opener::OpenerExt;
    let target = link::confirmed_target(url)?;
    app.opener()
        .open_url(target, None::<&str>)
        .map_err(|e| scitl_core::error::CoreError::Link(e.to_string()))
}
