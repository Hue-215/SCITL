//! WebViewがアプリ自身の画面以外へ遷移するのを止める。本文中のリンクは
//! `commands::link`の確認を経てOSのブラウザで開くのが唯一の経路であり、WebView自体が
//! 外部のページを読み込む必要は無い。リンクのクリック処理が漏れた場合にも止まるよう、
//! 描画側の対策と併用する。

use tauri::plugin::{Builder, TauriPlugin};
use tauri::{Manager, Runtime, Url};

pub fn guard<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("navigation-guard")
        .on_navigation(|webview, url| {
            let dev_url = webview.config().build.dev_url.as_ref();
            is_app_url(url, dev_url.filter(|_| tauri::is_dev()))
        })
        .build()
}

/// `http(s)://tauri.localhost`がカスタムプロトコルとして横取りされるか。WindowsとAndroidだけで、
/// ほかでは通常の読み込みになる(`*.localhost`を手元へ解決する環境では、手元のサーバーの画面が出る)。
const HTTP_APP_PROTOCOL: bool = cfg!(any(windows, target_os = "android"));

/// アプリ自身の画面のURLか。本番は埋め込み資源を配るカスタムプロトコル
/// (`tauri://localhost`、Windows・Androidでは`http(s)://tauri.localhost`)、開発時(`dev_url`が
/// ある)はそれに加えてVite開発サーバー。モバイルの開発時は、Tauriが画面をカスタムプロトコルで
/// 開き、開発サーバーへはRust側から中継するので、開発時もカスタムプロトコルを通す。
fn is_app_url(url: &Url, dev_url: Option<&Url>) -> bool {
    if dev_url.is_some_and(|dev_url| url.origin() == dev_url.origin()) {
        return true;
    }
    // ユーザー情報やポートの付いたものは、カスタムプロトコルの画面ではない(wryは
    // `http://tauri.`の前方一致で横取りを決めるので、通常の読み込みになりうる)。
    if !url.username().is_empty() || url.password().is_some() || url.port().is_some() {
        return false;
    }
    match url.scheme() {
        "tauri" => url.host_str() == Some("localhost"),
        "http" | "https" if HTTP_APP_PROTOCOL => url.host_str() == Some("tauri.localhost"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn app_protocol_passes_with_or_without_dev_server() {
        let dev = url("http://localhost:1420");
        for dev_url in [None, Some(&dev)] {
            assert!(is_app_url(&url("tauri://localhost/"), dev_url));
            for http in [
                "http://tauri.localhost/",
                "https://tauri.localhost/index.html",
            ] {
                assert_eq!(is_app_url(&url(http), dev_url), HTTP_APP_PROTOCOL, "{http}");
            }
        }
    }

    #[test]
    fn dev_server_passes_only_by_its_origin() {
        let dev = url("http://localhost:1420");
        assert!(is_app_url(
            &url("http://localhost:1420/src/main.tsx"),
            Some(&dev)
        ));
        assert!(!is_app_url(&url("http://localhost:1420/"), None));
        for other in [
            "http://localhost:1421/",
            "https://localhost:1420/",
            "http://127.0.0.1:1420/",
            "http://localhost.:1420/",
        ] {
            assert!(!is_app_url(&url(other), Some(&dev)), "{other}");
        }
    }

    #[test]
    fn other_urls_are_stopped() {
        let dev = url("http://localhost:1420");
        for dev_url in [None, Some(&dev)] {
            for other in [
                "https://example.com/",
                "http://tauri.localhost.example.com/",
                "http://tauri.localhost:8080/",
                "http://x@tauri.localhost/",
                "tauri://localhost:8080/",
                "tauri://x@localhost/",
                "tauri://example.com/",
                "file:///etc/passwd",
            ] {
                assert!(!is_app_url(&url(other), dev_url), "{other}");
            }
        }
    }
}
