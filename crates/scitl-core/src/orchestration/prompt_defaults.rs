//! 設定画面で書き換えられるプロンプトの既定の文面と、未設定時の解釈。既定の文面は言語
//! ファイルに置き、設定ファイルには書き写さない。モデルへ渡す固定文言は英語で書くが、
//! この既定の文面は利用者が書き換える起点なので表示言語で持つ。ターン・設定画面の表示・
//! 保存時の正規化はどれもここを読む。

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
    /// 設定から作る組。タスクチャット用が未設定なら既定の文面にする(総合チャットは
    /// タスクチャット用を使わない。`system_prompt::build_system_prompt`)。
    pub fn from_config(general: &'a GeneralConfig) -> Self {
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

/// 設定画面から保存するプロンプトの値。空白だけの値と既定の文面は未設定にする(既定の文面を
/// 改めたときに追従させるため)。既定の文面を持たないプロンプト(基本のシステムプロンプト)は
/// `default`に`None`を渡す。
pub(crate) fn stored_prompt(value: Option<String>, default: Option<&str>) -> Option<String> {
    value.filter(|v| !is_blank(v) && Some(v.as_str()) != default)
}

/// 空白だけの値は画面からは保存されないが、手で編集した`config.toml`が同じ経路を通るため、
/// ここでも未設定として受け止める。
fn or_default<'a>(value: Option<&'a str>, default: &'a str) -> &'a str {
    value.filter(|v| !is_blank(v)).unwrap_or(default)
}

/// 空白だけのプロンプトは未設定(設定画面の「空欄にすると既定の文面に戻ります」)。
fn is_blank(value: &str) -> bool {
    value.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_or_empty_settings_fall_back_to_the_defaults_of_the_display_language() {
        let general = GeneralConfig {
            task_opening_message: Some(String::new()),
            language: Some(Language::En.code().to_string()),
            ..GeneralConfig::default()
        };
        assert_eq!(
            SystemPrompts::from_config(&general).task_chat,
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
            SystemPrompts::from_config(&general).task_chat,
            Some("custom")
        );
        assert_eq!(opening_message(&general), "hello");
    }

    #[test]
    fn blank_settings_fall_back_to_the_defaults() {
        let general = GeneralConfig {
            task_chat_system_prompt: Some(" \n".to_string()),
            task_opening_message: Some("\t".to_string()),
            language: Some(Language::En.code().to_string()),
            ..GeneralConfig::default()
        };
        assert_eq!(
            SystemPrompts::from_config(&general).task_chat,
            Some(default_task_chat_prompt(Language::En))
        );
        assert_eq!(
            opening_message(&general),
            default_opening_message(Language::En)
        );
    }

    #[test]
    fn stored_prompts_leave_blank_and_default_values_unset() {
        let stored = |value: &str, default| stored_prompt(Some(value.to_string()), default);
        assert_eq!(stored("", None), None);
        assert_eq!(stored("  \n", None), None);
        assert_eq!(stored("既定", Some("既定")), None);
        assert_eq!(stored(" 既定 ", Some("既定")).as_deref(), Some(" 既定 "));
        assert_eq!(stored("custom", Some("既定")).as_deref(), Some("custom"));
        assert_eq!(stored_prompt(None, Some("既定")), None);
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
