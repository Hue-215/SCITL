//! チャット本文中のリンクの確認と、確認後にOSへ委譲するIPCコマンド。判定は
//! `scitl_core::link`にあり、開く側でも同じ判定をやり直す(WebView側の確認結果は信用しない)。

use scitl_core::link::LinkInspection;

use super::CommandResult;

/// 確認ダイアログに出す内容を返す。純粋な計算のみでI/Oを伴わないため同期コマンドにする。
#[tauri::command]
pub fn inspect_link(url: String) -> LinkInspection {
    scitl_core::link::inspect(&url)
}

/// 確認ダイアログで承認されたリンクを開く。外部プロセスの起動を伴うため`blocking::run`で呼ぶ。
#[tauri::command]
pub async fn open_confirmed_link(url: String) -> CommandResult<()> {
    Ok(scitl_core::blocking::run(move || scitl_core::link::open_confirmed(&url)).await?)
}
