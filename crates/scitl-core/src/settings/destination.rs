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

impl NewDestination<'_> {
    /// 名前とURLは、見えない文字を`\uXXXX`の形にして見せる(除くと隠されていたことも消える。
    /// `architecture/sanitize.md`の実行記録の行と同じ)。URLは検証で正規化した形を渡す。
    pub(super) fn dialog(&self, lang: Language) -> DestinationDialog {
        let message = match self {
            Self::Provider {
                name,
                api_format,
                base_url,
            } => i18n::format(
                lang,
                "destination_dialog.provider_message",
                &[
                    ("name", &text::reveal_invisible(name)),
                    ("format", i18n::text(lang, format_key(*api_format))),
                    ("url", &text::reveal_invisible(base_url)),
                ],
            ),
            Self::McpServer { name, url } => i18n::format(
                lang,
                "destination_dialog.mcp_message",
                &[
                    ("name", &text::reveal_invisible(name)),
                    ("url", &text::reveal_invisible(url)),
                ],
            ),
        };
        DestinationDialog {
            title: i18n::text(lang, "destination_dialog.title").to_string(),
            message,
            confirm_label: i18n::text(lang, "destination_dialog.confirm").to_string(),
            cancel_label: i18n::text(lang, "common.cancel").to_string(),
        }
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
}
