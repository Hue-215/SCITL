//! チャット本文中のリンクを開く前の判定と、OSへの委譲(principles.md 4節「リンクは確認を
//! 挟み、通信方式を制限する」)。確認ダイアログに出す内容(`inspect`)と、実際に開く前の
//! 再検証(`open_confirmed`)が同じ判定を通るよう、判定はこのファイルに閉じる。
//! WebView側の判定結果は信用しない(architecture.md 8節)。

use serde::Serialize;
use url::Url;

use crate::db::error::{CoreError, Result};

/// 開いてよいかどうかと、その理由。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LinkVerdict {
    Web,
    /// メールソフトが起動する旨を添えて開く。
    Mail,
    /// URLとして解釈できない(相対パス・不正なホスト等)。開かない。
    Unreadable,
    /// 許可リスト外の通信方式。`file:`や独自スキームはOSに関連付けられたアプリを
    /// 起動でき、チャット本文から開かせる必要も無いため開かない。
    SchemeBlocked {
        scheme: String,
    },
}

/// 確認ダイアログに出す内容。`url`は受け取った文字列をそのまま返す(表示用)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkInspection {
    pub url: String,
    pub verdict: LinkVerdict,
    /// 書かれたホストと実際の移動先ホストが食い違う場合だけ、移動先のURL全体
    /// (ホストはpunycode)。見た目の似た文字によるなりすまし(ホモグラフ)や、
    /// `%65vil.com`・全角英字のような、ブラウザが変換してから向かう書き方を見分けるため。
    pub real_url: Option<String>,
    /// URLがユーザー情報(`@`より前)を含む場合だけ、実際の移動先ホスト。
    /// `https://google.com@evil.com/`のように、`@`より前をサイト名に見せかける手口があるため。
    pub userinfo_host: Option<String>,
}

impl LinkInspection {
    pub fn can_open(&self) -> bool {
        matches!(self.verdict, LinkVerdict::Web | LinkVerdict::Mail)
    }
}

/// リンクを判定する。WebView側のMarkdown描画はホストの非ASCII文字をパーセント表記に
/// してから渡してくるが、`Url::parse`はブラウザと同じ規則(WHATWG URL・UTS46)で戻して
/// から変換するため、ここでの判定と実際の移動先は一致する。
pub fn inspect(raw: &str) -> LinkInspection {
    let url = raw.trim().to_string();
    let verdict_only = |verdict| LinkInspection {
        url: url.clone(),
        verdict,
        real_url: None,
        userinfo_host: None,
    };

    let Ok(parsed) = Url::parse(&url) else {
        return verdict_only(LinkVerdict::Unreadable);
    };
    match parsed.scheme() {
        "http" | "https" => {}
        "mailto" => return verdict_only(LinkVerdict::Mail),
        other => {
            return verdict_only(LinkVerdict::SchemeBlocked {
                scheme: other.to_string(),
            })
        }
    }
    let (Some(host), Some(written)) = (parsed.host_str(), written_host(&url)) else {
        return verdict_only(LinkVerdict::Unreadable);
    };

    LinkInspection {
        verdict: LinkVerdict::Web,
        real_url: (!written.eq_ignore_ascii_case(host)).then(|| parsed.to_string()),
        userinfo_host: (!parsed.username().is_empty() || parsed.password().is_some())
            .then(|| host.to_string()),
        url,
    }
}

/// 確認ダイアログで承認されたリンクを、判定し直してからOSの既定アプリで開く。
/// 渡すのは解析・正規化後のURLで、判定した対象と開く対象を一致させる。
pub fn open_confirmed(raw: &str) -> Result<()> {
    let inspection = inspect(raw);
    if !inspection.can_open() {
        return Err(CoreError::Link(format!(
            "refused to open link: {:?}",
            inspection.verdict
        )));
    }
    let url = Url::parse(&inspection.url).map_err(|e| CoreError::Link(e.to_string()))?;
    open::that_detached(url.as_str()).map_err(|e| CoreError::Link(e.to_string()))
}

/// http/httpsのURL文字列から、書かれたままのホスト部分を取り出す。区切りの解釈は
/// WHATWG URLの特別スキームに合わせる(`\`も`/`と同じ扱い、ユーザー情報は最後の`@`まで)。
/// `Url::parse`が成功した文字列にだけ使う前提。
fn written_host(url: &str) -> Option<&str> {
    let after_scheme = &url[url.find(':')? + 1..];
    let authority_start = after_scheme.trim_start_matches(['/', '\\']);
    let authority_end = authority_start
        .find(['/', '\\', '?', '#'])
        .unwrap_or(authority_start.len());
    let authority = &authority_start[..authority_end];
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    // IPv6リテラルの`[...]`内の`:`はポートの区切りではない
    let host = match host_port.rfind(':') {
        Some(i) if !host_port[i..].contains(']') => &host_port[..i],
        _ => host_port,
    };
    Some(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_https_has_no_warnings() {
        let i = inspect("https://example.com/path?q=1#frag");
        assert_eq!(i.verdict, LinkVerdict::Web);
        assert_eq!(i.real_url, None);
        assert_eq!(i.userinfo_host, None);
    }

    #[test]
    fn host_case_and_port_are_not_warnings() {
        let i = inspect("HTTPS://Example.COM:8443/");
        assert_eq!(i.verdict, LinkVerdict::Web);
        assert_eq!(i.real_url, None);
    }

    #[test]
    fn non_ascii_host_shows_punycode_destination() {
        // 先頭の「а」はキリル文字
        let i = inspect("https://аpple.com/login");
        assert_eq!(i.verdict, LinkVerdict::Web);
        assert_eq!(
            i.real_url.as_deref(),
            Some("https://xn--pple-43d.com/login")
        );
    }

    #[test]
    fn percent_encoded_host_is_detected() {
        // Markdown描画側がパーセント表記にして渡してくる形と、意図的に隠す形の両方
        let i = inspect("https://%D0%B0pple.com/");
        assert_eq!(i.real_url.as_deref(), Some("https://xn--pple-43d.com/"));
        let i = inspect("https://%65vil.com/");
        assert_eq!(i.real_url.as_deref(), Some("https://evil.com/"));
    }

    #[test]
    fn fullwidth_and_numeric_ip_hosts_are_detected() {
        let i = inspect("https://ｇｏｏｇｌｅ.com/");
        assert_eq!(i.real_url.as_deref(), Some("https://google.com/"));
        let i = inspect("http://0x7f.1/");
        assert_eq!(i.real_url.as_deref(), Some("http://127.0.0.1/"));
    }

    #[test]
    fn ipv6_literal_with_port_is_not_a_warning() {
        let i = inspect("http://[::1]:8080/");
        assert_eq!(i.verdict, LinkVerdict::Web);
        assert_eq!(i.real_url, None);
    }

    #[test]
    fn userinfo_reports_real_host() {
        let i = inspect("https://google.com@evil.com/");
        assert_eq!(i.userinfo_host.as_deref(), Some("evil.com"));
        let i = inspect("https://user:pass@evil.com/");
        assert_eq!(i.userinfo_host.as_deref(), Some("evil.com"));
    }

    #[test]
    fn at_sign_in_path_is_not_userinfo() {
        let i = inspect("https://medium.com/@someone");
        assert_eq!(i.userinfo_host, None);
        assert_eq!(i.real_url, None);
    }

    #[test]
    fn backslash_separator_matches_browser_parsing() {
        let i = inspect("https:\\\\example.com\\path");
        assert_eq!(i.verdict, LinkVerdict::Web);
        assert_eq!(i.real_url, None);
    }

    #[test]
    fn mailto_is_allowed_with_note() {
        let i = inspect("mailto:someone@example.com");
        assert_eq!(i.verdict, LinkVerdict::Mail);
        assert_eq!(i.userinfo_host, None);
        assert!(i.can_open());
    }

    #[test]
    fn other_schemes_are_blocked() {
        for (url, scheme) in [
            ("javascript:alert(1)", "javascript"),
            ("file:///etc/passwd", "file"),
            ("vscode://file/x", "vscode"),
            ("data:text/html,<p>x</p>", "data"),
        ] {
            let i = inspect(url);
            assert_eq!(
                i.verdict,
                LinkVerdict::SchemeBlocked {
                    scheme: scheme.to_string()
                },
                "{url}"
            );
            assert!(!i.can_open());
        }
    }

    #[test]
    fn unparseable_is_blocked() {
        for url in [
            "",
            "/relative/path",
            "#anchor",
            "https://",
            "https://exa mple.com/",
        ] {
            let i = inspect(url);
            assert_eq!(i.verdict, LinkVerdict::Unreadable, "{url:?}");
            assert!(!i.can_open());
        }
    }

    #[test]
    fn open_confirmed_rejects_blocked_links_without_opening() {
        assert!(matches!(
            open_confirmed("javascript:alert(1)"),
            Err(CoreError::Link(_))
        ));
        assert!(matches!(
            open_confirmed("/relative"),
            Err(CoreError::Link(_))
        ));
    }
}
