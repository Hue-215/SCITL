//! WebViewがアプリ自身の画面以外へ遷移するのを止める。本文中のリンクは
//! `commands::link`の確認を経てOSのブラウザで開くのが唯一の経路であり、WebView自体が
//! 外部のページを読み込む必要は無い。リンクのクリック処理が漏れた場合にも止まるよう、
//! 描画側の対策と併用する。

use tauri::plugin::{Builder, TauriPlugin};
use tauri::{Manager, Runtime, Url};

pub fn guard<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("navigation-guard")
        .on_navigation(|webview, url| is_app_url(url, webview.config().build.dev_url.as_ref()))
        .build()
}

/// アプリ自身の画面のURLか。本番は埋め込み資源を配るカスタムプロトコル
/// (`tauri://localhost`、Windowsでは`http(s)://tauri.localhost`)、開発時はVite開発サーバー。
fn is_app_url(url: &Url, dev_url: Option<&Url>) -> bool {
    if tauri::is_dev() {
        if let Some(dev_url) = dev_url {
            return url.origin() == dev_url.origin();
        }
    }
    match url.scheme() {
        "tauri" => url.host_str() == Some("localhost"),
        "http" | "https" => url.host_str() == Some("tauri.localhost"),
        _ => false,
    }
}
