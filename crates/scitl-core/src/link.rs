//! チャット本文中のリンクを開く前の判定と、OSへの委譲(principles.md 4節「リンクは確認を
//! 挟み、通信方式を制限する」)。確認ダイアログに出す内容(`inspect`)と、実際に開く前の
//! 再検証(`open_confirmed`)が同じ判定を通るよう、判定はこのファイルに閉じる。
//! WebView側の判定結果は信用しない(architecture.md 8節)。

use percent_encoding::{utf8_percent_encode, AsciiSet};
use serde::Serialize;
use url::Url;

use crate::error::{CoreError, Result};
use crate::text::encode_all_but;

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
    /// `verdict`から決まる。「開く」を出すかどうかの判断を画面側に写さないため、
    /// 判定結果として一緒に渡す。
    pub can_open: bool,
    /// 書かれたホストと実際の移動先ホストが食い違う場合だけ、移動先のURL全体
    /// (ホストはpunycode)。見た目の似た文字によるなりすまし(ホモグラフ)や、
    /// `%65vil.com`・全角英字のような、ブラウザが変換してから向かう書き方を見分けるため。
    pub real_url: Option<String>,
    /// URLがユーザー情報(`@`より前)を含む場合だけ、実際の移動先ホスト。
    /// `https://google.com@evil.com/`のように、`@`より前をサイト名に見せかける手口があるため。
    pub userinfo_host: Option<String>,
}

impl LinkInspection {
    fn new(
        url: String,
        verdict: LinkVerdict,
        real_url: Option<String>,
        userinfo_host: Option<String>,
    ) -> Self {
        let can_open = matches!(verdict, LinkVerdict::Web | LinkVerdict::Mail);
        Self {
            url,
            verdict,
            can_open,
            real_url,
            userinfo_host,
        }
    }
}

/// リンクを判定する。WebView側のMarkdown描画はホストの非ASCII文字をパーセント表記に
/// してから渡してくるが、`Url::parse`はブラウザと同じ規則(WHATWG URL・UTS46)で戻して
/// から変換するため、ここでの判定と実際の移動先は一致する。
pub fn inspect(raw: &str) -> LinkInspection {
    let url = raw.trim().to_string();
    let verdict_only = |verdict| LinkInspection::new(url.clone(), verdict, None, None);

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
    // WHATWG URLはホストに`"`等を許すが、OSがブラウザの起動コマンドへURLを差し込む際に
    // 引数の区切りとして解釈されうる。実在するドメイン名に現れない文字は開かない。
    if let Some(url::Host::Domain(domain)) = parsed.host() {
        if !domain
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        {
            return verdict_only(LinkVerdict::Unreadable);
        }
    }

    let real_url = (!written.eq_ignore_ascii_case(host)).then(|| parsed.to_string());
    let userinfo_host =
        (!parsed.username().is_empty() || parsed.password().is_some()).then(|| host.to_string());
    LinkInspection::new(url, LinkVerdict::Web, real_url, userinfo_host)
}

/// 確認ダイアログで承認されたリンクを、判定し直してからOSの既定アプリで開く。
/// 渡すのは解析・正規化後のURLで、判定した対象と開く対象を一致させる。
/// 確認ダイアログを経たこと自体はここでは検証できない(WebView側の呼び出しを信用しない
/// 前提のため、ここで保証するのは開く対象が許可された形であることまで)。
pub fn open_confirmed(raw: &str) -> Result<()> {
    let inspection = inspect(raw);
    if !inspection.can_open {
        return Err(CoreError::Link(format!(
            "refused to open link: {:?}",
            inspection.verdict
        )));
    }
    let mut url = Url::parse(&inspection.url).map_err(|e| CoreError::Link(e.to_string()))?;
    if url.scheme() == "mailto" {
        keep_standard_mailto_fields(&mut url);
    }
    open::that_detached(os_safe(&url)).map_err(|e| CoreError::Link(e.to_string()))
}

/// RFC 3986でURLにそのまま書けない文字(`"`・空白・`<>^`|{}\`等)をパーセント表記にする。
/// `Url`の直列化は、mailtoのパスや特別スキームのクエリにこれらを残すことがあり、
/// OSが起動コマンドへURLを差し込む際に引用の終端や引数の区切りとして解釈されうる。
/// パーセント表記にしても、受け取る側にとってのURLの意味は変わらない。
fn os_safe(url: &Url) -> String {
    // 残すのは非予約文字・予約文字と、既にある符号化の`%`。
    const NOT_URL_CHARACTERS: &AsciiSet = &encode_all_but(b"-._~:/?#[]@!$&'()*+,;=%");
    utf8_percent_encode(url.as_str(), NOT_URL_CHARACTERS).to_string()
}

/// mailtoのクエリを、宛先・件名・本文に関わる標準の項目(RFC 6068)だけに絞る。
/// メールソフトによっては`attach`等でローカルのファイルを添付した下書きを作れるため。
fn keep_standard_mailto_fields(url: &mut Url) {
    let Some(query) = url.query() else { return };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|field| {
            let name = field.split('=').next().unwrap_or("");
            ["to", "cc", "bcc", "subject", "body"]
                .iter()
                .any(|allowed| name.eq_ignore_ascii_case(allowed))
        })
        .collect();
    let kept = kept.join("&");
    url.set_query((!kept.is_empty()).then_some(kept.as_str()));
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
        assert!(i.can_open);
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
            assert!(!i.can_open);
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
            assert!(!i.can_open);
        }
    }

    #[test]
    fn host_with_characters_outside_domain_names_is_unreadable() {
        let i = inspect("http://a%22b.com/");
        assert_eq!(i.verdict, LinkVerdict::Unreadable);
        assert!(!i.can_open);
    }

    #[test]
    fn os_safe_escapes_characters_that_could_split_arguments() {
        let url = Url::parse("mailto:a\" -x b@c.com").unwrap();
        assert_eq!(os_safe(&url), "mailto:a%22%20-x%20b@c.com");
        let url = Url::parse("https://e.com/p|^?q=a|b^c{d}`e").unwrap();
        assert_eq!(
            os_safe(&url),
            "https://e.com/p%7C%5E?q=a%7Cb%5Ec%7Bd%7D%60e"
        );
    }

    #[test]
    fn os_safe_keeps_ordinary_urls_unchanged() {
        for s in [
            "https://example.com/a/b?x=1&y=%E3%81%82#frag",
            "http://[::1]:8080/",
            "mailto:someone@example.com?subject=hi",
        ] {
            let url = Url::parse(s).unwrap();
            assert_eq!(os_safe(&url), url.as_str());
        }
    }

    #[test]
    fn mailto_keeps_only_standard_fields() {
        let mut url =
            Url::parse("mailto:a@b.com?subject=hi&attach=/etc/passwd&Body=x&cc=c@d.com").unwrap();
        keep_standard_mailto_fields(&mut url);
        assert_eq!(url.as_str(), "mailto:a@b.com?subject=hi&Body=x&cc=c@d.com");
        let mut url = Url::parse("mailto:a@b.com?attach=/etc/passwd").unwrap();
        keep_standard_mailto_fields(&mut url);
        assert_eq!(url.as_str(), "mailto:a@b.com");
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
