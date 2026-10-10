//! チャット本文中のリンクを開く前の判定と、OSへの委譲。確認のダイアログに出す内容(`inspect`・
//! `dialog`)と、実際に開く前の再検証(`confirmed_target`)が同じ判定を通るよう、判定はこのファイルに
//! 閉じる。確認はGUIのシェルがRust側からネイティブのダイアログで出し(画面は「開きたい」と伝える
//! だけ)、WebView側の判定結果は受け取らない(`architecture/webview-boundary.md`の外部リンクの項)。

use percent_encoding::{utf8_percent_encode, AsciiSet};
use serde::Serialize;
use url::Url;

use crate::error::{CoreError, Result};
use crate::i18n::{self, Language};
use crate::text::{self, encode_all_but};

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

/// 確認のダイアログに出す内容。`url`は受け取った文字列をそのまま返す(表示用)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkInspection {
    pub url: String,
    pub verdict: LinkVerdict,
    /// `verdict`から決まる(「開く」を出すか)。
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

/// 確認のダイアログに出す見出しのURLの長さの上限(超えた分は「…」)。
const MAX_SHOWN_URL_CHARS: usize = 300;

/// リンクを開く前に出すネイティブのダイアログの文面(表示言語)。`open_label`が無ければ開けない
/// リンクで、理由を知らせて閉じるだけにする。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkDialog {
    pub title: String,
    pub message: String,
    /// 見た目の紛らわしいURL(ホモグラフ・ユーザー情報)への警告を含む。ダイアログの種類を変える。
    pub warning: bool,
    pub open_label: Option<String>,
    pub close_label: String,
}

/// 判定の結果を、ネイティブのダイアログの文面にする。ダイアログは見た目を画面に揃えられないので、
/// 警告は見出しの行と本文の行を空行で区切って並べる。URLは1行にし、見えない文字を`\uXXXX`の形にする
/// (改行で文を差し込ませない。`architecture/sanitize.md`「ネイティブのダイアログ」)。
pub fn dialog(lang: Language, inspection: &LinkInspection) -> LinkDialog {
    let line = |s: &str| text::reveal_invisible_line(&text::ellipsize(s, MAX_SHOWN_URL_CHARS));
    let t = |key| i18n::text(lang, key).to_string();
    // 見出しのURLは、読めるものは開くときと同じ解釈で正規化した形にする(入力のままだと、空白の
    // 並びで別の行の文に見せたり、長さの上限で切ってホストを隠したりできる)。開けるWebのリンクは、
    // 移動先のホストを必ず別の行に出す。
    let parsed = Url::parse(&inspection.url).ok();
    let mut head = line(parsed.as_ref().map_or(&inspection.url, |url| url.as_str()));
    if let (LinkVerdict::Web, Some(host)) = (
        &inspection.verdict,
        parsed.as_ref().and_then(|url| url.host_str()),
    ) {
        head.push('\n');
        head.push_str(&i18n::format(
            lang,
            "link.host_label",
            &[("host", &line(host))],
        ));
    }
    let mut sections = vec![head];
    if let Some(real_url) = &inspection.real_url {
        sections.push(format!(
            "{}\n{}\n{}",
            t("link.special_char_title"),
            t("link.special_char_body"),
            i18n::format(lang, "link.real_url_label", &[("url", &line(real_url))]),
        ));
    }
    if let Some(host) = &inspection.userinfo_host {
        sections.push(format!(
            "{}\n{}\n{}",
            t("link.userinfo_title"),
            t("link.userinfo_body"),
            i18n::format(
                lang,
                "link.userinfo_domain_label",
                &[("domain", &line(host))]
            ),
        ));
    }
    let warning = inspection.real_url.is_some() || inspection.userinfo_host.is_some();
    match &inspection.verdict {
        LinkVerdict::Unreadable => sections.push(t("link.unreadable")),
        LinkVerdict::SchemeBlocked { scheme } => sections.push(i18n::format(
            lang,
            "link.scheme_blocked",
            &[("scheme", &line(scheme))],
        )),
        LinkVerdict::Mail => sections.push(t("link.mailto_note")),
        LinkVerdict::Web if !warning => sections.push(t("link.generic_warning")),
        LinkVerdict::Web => {}
    }
    LinkDialog {
        title: t("link.dialog_title"),
        message: sections.join("\n\n"),
        warning,
        open_label: inspection.can_open.then(|| t("link.open")),
        close_label: t(if inspection.can_open {
            "common.cancel"
        } else {
            "common.close"
        }),
    }
}

/// 開くと承認されたリンクを判定し直し、OSへ渡す形(解析・正規化したURL)にする。判定した対象と
/// 開く対象を一致させる。確認のダイアログはRust側が出すので、呼ぶのは承認されたあとだけ。
pub fn confirmed_target(raw: &str) -> Result<String> {
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
    Ok(os_safe(&url))
}

/// 承認されたリンクをOSの既定アプリで開く(デスクトップ。Androidでは`open`クレートが動かないので、
/// GUIのシェルが[`confirmed_target`]の結果を`tauri-plugin-opener`へ渡す)。
pub fn open_confirmed(raw: &str) -> Result<()> {
    open::that_detached(confirmed_target(raw)?).map_err(|e| CoreError::Link(e.to_string()))
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
    fn the_dialog_offers_to_open_only_allowed_links_and_lists_the_warnings() {
        let plain = dialog(Language::En, &inspect("https://example.com/a"));
        assert!(plain.open_label.is_some());
        assert!(!plain.warning);
        assert!(plain.message.starts_with("https://example.com/a"));

        let disguised = dialog(Language::Ja, &inspect("https://google.com@evil.com/"));
        assert!(disguised.warning);
        assert!(
            disguised.message.contains("evil.com"),
            "{}",
            disguised.message
        );

        let blocked = dialog(Language::En, &inspect("file:///etc/passwd"));
        assert!(blocked.open_label.is_none());
        assert!(blocked.message.contains("file:"), "{}", blocked.message);

        // URLの中の改行で、文を差し込ませない(URLは1行に収める)。
        let injected = dialog(Language::En, &inspect("javascript:x\n\nThis link is safe."));
        assert!(!injected.message.lines().any(|l| l == "This link is safe."));
        // 空白の並びは正規化で`%20`になり、長い道筋でもホストは別の行に出る。
        let spaced = dialog(
            Language::En,
            &inspect("https://evil.example/       Checked by SCITL."),
        );
        assert!(!spaced.message.contains("       "), "{}", spaced.message);
        let slashes = format!("https:{}evil.example/", "/".repeat(300));
        let hidden = dialog(Language::En, &inspect(&slashes));
        assert!(
            hidden.message.contains("evil.example"),
            "{}",
            hidden.message
        );
        assert!(!plain.message.contains('{') && !disguised.message.contains('{'));
    }

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
