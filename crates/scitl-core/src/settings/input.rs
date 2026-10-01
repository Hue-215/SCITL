//! 設定の入力の検証のうち、複数の操作が共有するもの(名前・数値の範囲・名前の重複)。
//! 画面の入力チェックには頼らず、GUIのコマンドもCLIもここを通る。

use std::collections::HashSet;
use std::fmt::Display;

use secrecy::SecretString;

use super::invalid;
use crate::error::Result;
use crate::text;

pub(super) const PROVIDER_NAME_MAX_CHARS: usize = 64;

/// リポジトリのパスや量子化の種類を含む長い名前(`hf.co/…/…-GGUF:Q4_K_M`等)が収まる長さ。
pub(super) const MODEL_NAME_MAX_CHARS: usize = 256;

/// 応答・ツール実行のタイムアウトの上限(24時間)。
pub(super) const MAX_TIMEOUT_SECS: u64 = 24 * 60 * 60;

pub(super) const MAX_ROUNDS_PER_TURN: u32 = 100;

/// 画面に出す名前(プロバイダー名・モデル名)を検証し、前後の空白を除いて返す。空・
/// `max_chars`文字超・制御文字や見えない書式文字を含む名前を断る。`what`はエラー文の主語。
pub(super) fn name(raw: &str, what: &str, max_chars: usize) -> Result<String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(invalid(format!("{what} must not be empty")));
    }
    if name
        .chars()
        .any(|c| c.is_control() || text::is_invisible_format(c))
    {
        return Err(invalid(format!(
            "{what} must not contain control or invisible characters"
        )));
    }
    if name.chars().count() > max_chars {
        return Err(invalid(format!(
            "{what} must be at most {max_chars} characters"
        )));
    }
    Ok(name.to_string())
}

/// 未設定(`None`)か、1以上`max`以下の値だけを通す。
pub(super) fn bounded<T>(value: Option<T>, what: &str, max: T) -> Result<Option<T>>
where
    T: PartialOrd + Display + From<u8>,
{
    match value {
        Some(v) if v < T::from(1) || v > max => {
            Err(invalid(format!("{what} must be between 1 and {max}")))
        }
        value => Ok(value),
    }
}

/// 秘密情報の組(環境変数・ヘッダー)に同じ名前が2つ無いことを確かめる。同じ名前は最後の
/// 1つしか送られない。`key`は同じとみなす名前を同じ値に写す(ヘッダー名は大文字小文字を
/// 区別しない)。
pub(super) fn unique_names(
    pairs: &[(String, SecretString)],
    what: &str,
    key: fn(&str) -> String,
) -> Result<()> {
    let mut seen = HashSet::new();
    for (name, _) in pairs {
        if !seen.insert(key(name)) {
            return Err(invalid(format!("{what} '{name}' is given more than once")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_trimmed_and_refused_when_blank_invisible_or_too_long() {
        assert_eq!(name(" Local\u{3000}", "name", 8).unwrap(), "Local");
        assert_eq!(name("ローカル 推論", "name", 8).unwrap(), "ローカル 推論");
        for refused in [
            "",
            "  ",
            "\u{200B}",
            "a\u{202E}b",
            "a\nb",
            "a\u{1}",
            "123456789",
        ] {
            assert!(name(refused, "name", 8).is_err(), "{refused:?}");
        }
        // 上限は文字数で数える。
        assert!(name("あいうえおかきく", "name", 8).is_ok());
    }

    #[test]
    fn bounded_accepts_unset_and_values_from_one_to_the_maximum() {
        assert_eq!(bounded(None::<u64>, "n", 10).unwrap(), None);
        assert_eq!(bounded(Some(1u64), "n", 10).unwrap(), Some(1));
        assert_eq!(bounded(Some(10u32), "n", 10).unwrap(), Some(10));
        assert!(bounded(Some(0u64), "n", 10).is_err());
        assert!(bounded(Some(11u32), "n", 10).is_err());
    }

    #[test]
    fn unique_names_compares_by_the_given_key() {
        let pairs = |names: &[&str]| -> Vec<(String, SecretString)> {
            names
                .iter()
                .map(|n| (n.to_string(), SecretString::from("v")))
                .collect()
        };
        let exact: fn(&str) -> String = str::to_string;
        assert!(unique_names(&pairs(&["A", "a"]), "x", exact).is_ok());
        assert!(unique_names(&pairs(&["A", "A"]), "x", exact).is_err());
        assert!(unique_names(&pairs(&["X-Key", "x-key"]), "x", str::to_ascii_lowercase).is_err());
    }
}
