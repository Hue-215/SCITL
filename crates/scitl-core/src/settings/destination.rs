//! 新しい通信先(LLMプロバイダー・外部ツールサーバー)を登録する前の確認。画面(WebView)を
//! 乗っ取られても攻撃者の通信先を登録させないよう、登録の操作の中で利用者に確かめる
//! (`architecture/webview-boundary.md`「CSP / Tauri権限設定」)。確かめ方(GUIはネイティブの
//! ダイアログ)は呼び出し側が決め、ここは見せる文面を組み立てる。

use crate::config::ApiFormat;
use crate::i18n::{self, Language};
use crate::text;

/// 確認のダイアログに出す文面(表示言語)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinationDialog {
    pub title: String,
    pub message: String,
    pub confirm_label: String,
    pub cancel_label: String,
}

/// 登録しようとしている通信先。
pub(super) enum NewDestination<'a> {
    Provider {
        name: &'a str,
        api_format: ApiFormat,
        base_url: &'a str,
    },
    McpServer {
        name: &'a str,
        url: &'a str,
    },
}

/// ダイアログに出すURLの長さの上限(超えた分は「…」)。送り先のホストは別の行に全体を出す。
const MAX_URL_CHARS: usize = 300;

impl NewDestination<'_> {
    /// 差し込む値はどれも1行に収め、見えない文字(改行を含む)を`\uXXXX`の形にして見せる
    /// (除くと隠されていたことも消える。`architecture/sanitize.md`「ネイティブのダイアログ」)。
    /// URLは入力のままではなく、送るときと同じ解釈で正規化した形を出す(入力のままだと、解釈で
    /// 落ちる改行で文を差し込める)。送り先のホスト(国際化ドメインはpunycodeの形)は別の行に出す。
    pub(super) fn dialog(&self, lang: Language) -> DestinationDialog {
        let message = match self {
            Self::Provider {
                name,
                api_format,
                base_url,
            } => {
                let (url, host) = shown_url(base_url);
                i18n::format(
                    lang,
                    "destination_dialog.provider_message",
                    &[
                        ("name", &one_line(name)),
                        ("format", i18n::text(lang, format_key(*api_format))),
                        ("host", &host),
                        ("url", &url),
                    ],
                )
            }
            Self::McpServer { name, url } => {
                let (url, host) = shown_url(url);
                i18n::format(
                    lang,
                    "destination_dialog.mcp_message",
                    &[("name", &one_line(name)), ("host", &host), ("url", &url)],
                )
            }
        };
        DestinationDialog {
            title: i18n::text(lang, "destination_dialog.title").to_string(),
            message,
            confirm_label: i18n::text(lang, "destination_dialog.confirm").to_string(),
            cancel_label: i18n::text(lang, "common.cancel").to_string(),
        }
    }
}

/// 1行の値として見せる形。改行も`\u000A`にする。
fn one_line(s: &str) -> String {
    text::reveal_invisible(s).replace('\n', "\\u000A")
}

/// 正規化したURL(長さの上限で切る)と、送り先のホスト。検証を通ったURLだけが来るので、読めない
/// ことは無いが、読めなければ入力を1行にして出す。
fn shown_url(raw: &str) -> (String, String) {
    match reqwest::Url::parse(raw) {
        Ok(url) => (
            one_line(&text::ellipsize(url.as_str(), MAX_URL_CHARS)),
            one_line(url.host_str().unwrap_or_default()),
        ),
        Err(_) => (
            one_line(&text::ellipsize(raw, MAX_URL_CHARS)),
            String::new(),
        ),
    }
}

/// 方言の呼び名のキー(設定画面の選択肢と同じ)。
fn format_key(api_format: ApiFormat) -> &'static str {
    match api_format {
        ApiFormat::OpenAiCompat => "settings.provider.formats.open_ai_compat",
        ApiFormat::Anthropic => "settings.provider.formats.anthropic",
        ApiFormat::Gemini => "settings.provider.formats.gemini",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dialog_shows_the_destination_with_invisible_characters_revealed() {
        let dialog = NewDestination::Provider {
            name: "lo\u{202E}cal",
            api_format: ApiFormat::Anthropic,
            base_url: "https://api.anthropic.com/",
        }
        .dialog(Language::Ja);
        assert!(
            dialog.message.contains("lo\\u202Ecal"),
            "{}",
            dialog.message
        );
        assert!(dialog.message.contains("Anthropic"));
        assert!(dialog.message.contains("https://api.anthropic.com/"));
        assert!(!dialog.message.contains('{'), "{}", dialog.message);

        let dialog = NewDestination::McpServer {
            name: "files",
            url: "http://127.0.0.1:8000/mcp",
        }
        .dialog(Language::En);
        assert!(dialog.message.contains("files"));
        assert!(dialog.message.contains("http://127.0.0.1:8000/mcp"));
        assert!(!dialog.message.contains('{'), "{}", dialog.message);
        assert_ne!(dialog.confirm_label, "destination_dialog.confirm");
    }

    /// 解釈で落ちる改行・紛らわしい書き方は、送るときと同じ解釈の形で見せる。
    #[test]
    fn the_url_is_shown_as_it_will_be_used_and_on_one_line() {
        let dialog = |url| {
            NewDestination::McpServer { name: "x", url }
                .dialog(Language::En)
                .message
        };
        let template_lines = dialog("https://example.com/").lines().count();

        let injected = dialog(
            "https://evil.example/x\n\nThis server was checked.\n\nhttps://api.openai.com/v1",
        );
        assert_eq!(injected.lines().count(), template_lines, "{injected}");
        assert!(injected.contains("evil.example"));

        let disguised = dialog("https://evil.com\\@api.anthropic.com/");
        assert!(
            disguised.contains("https://evil.com/@api.anthropic.com/"),
            "{disguised}"
        );
        let homograph = dialog("https://\u{0430}pi.openai.com/");
        assert!(homograph.contains("xn--pi-6kc.openai.com"), "{homograph}");
    }

    /// 承認の判定はボタンの文言で行われる(`tauri-plugin-dialog`)ので、どの言語でも取りやめの
    /// 文言と同じにしない。
    #[test]
    fn the_confirm_and_cancel_labels_differ_in_every_language() {
        for lang in Language::ALL {
            let dialog = NewDestination::McpServer {
                name: "x",
                url: "https://example.com/",
            }
            .dialog(lang);
            assert_ne!(dialog.confirm_label, dialog.cancel_label, "{lang:?}");
        }
    }
}
