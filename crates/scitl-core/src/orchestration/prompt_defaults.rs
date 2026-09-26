//! 設定画面で書き換えられるプロンプトの既定の文面と、未設定時の解釈(Issue #63・#76)。
//! 既定の文面の実体は言語ファイルに置き、設定ファイルには書き写さない(`ToolLimits`と同じ理由)。
//! ターン・設定画面の表示・保存時の正規化はどれもここを読む。
//! 既定の文面は表示言語で持つ(英語で書く規則の例外。architecture.md 3節「聞き取りの開始」)。

use crate::config::GeneralConfig;
use crate::i18n::{self, Language};
use crate::orchestration::SystemPrompts;

const TASK_CHAT_PROMPT_KEY: &str = "task_chat.system_prompt";
const OPENING_MESSAGE_KEY: &str = "task_chat.opening_message";

/// タスクチャット用プロンプトの既定の文面。
pub fn default_task_chat_prompt(language: Language) -> &'static str {
    i18n::text(language, TASK_CHAT_PROMPT_KEY)
}

/// 聞き取りを始めるときに、ユーザーの代わりに送る発言の既定の文面。
pub fn default_opening_message(language: Language) -> &'static str {
    i18n::text(language, OPENING_MESSAGE_KEY)
}

impl<'a> SystemPrompts<'a> {
    /// タスクチャットで使う組。タスクチャット用が未設定なら既定の文面にする。
    pub fn for_task_chat(general: &'a GeneralConfig) -> Self {
        Self {
            base: general.system_prompt.as_deref(),
            task_chat: Some(or_default(
                general.task_chat_system_prompt.as_deref(),
                default_task_chat_prompt(general.language()),
            )),
        }
    }
}

/// 聞き取りの開始に使う発言。未設定なら既定の文面。
pub fn opening_message(general: &GeneralConfig) -> &str {
    or_default(
        general.task_opening_message.as_deref(),
        default_opening_message(general.language()),
    )
}

/// 空は画面からは保存されないが、手で編集した`config.toml`が同じ経路を通るため、ここでも
/// 未設定として受け止める。
fn or_default<'a>(value: Option<&'a str>, default: &'a str) -> &'a str {
    value.filter(|v| !v.is_empty()).unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_or_empty_settings_fall_back_to_the_defaults_of_the_display_language() {
        let general = GeneralConfig {
            task_opening_message: Some(String::new()),
            language: Some(Language::En),
            ..GeneralConfig::default()
        };
        assert_eq!(
            SystemPrompts::for_task_chat(&general).task_chat,
            Some(default_task_chat_prompt(Language::En))
        );
        assert_eq!(
            opening_message(&general),
            default_opening_message(Language::En)
        );

        let general = GeneralConfig {
            task_chat_system_prompt: Some("custom".to_string()),
            task_opening_message: Some("hello".to_string()),
            ..GeneralConfig::default()
        };
        assert_eq!(
            SystemPrompts::for_task_chat(&general).task_chat,
            Some("custom")
        );
        assert_eq!(opening_message(&general), "hello");
    }

    #[test]
    fn every_language_has_its_own_defaults() {
        for (key, default) in [
            (
                TASK_CHAT_PROMPT_KEY,
                default_task_chat_prompt as fn(Language) -> &'static str,
            ),
            (OPENING_MESSAGE_KEY, default_opening_message),
        ] {
            for language in Language::ALL {
                assert_ne!(default(language), key);
            }
            assert_ne!(default(Language::Ja), default(Language::En));
        }
    }
}
