//! 設定の入力の検証のうち、複数の操作が共有するもの(名前・数値の範囲・名前の重複)。
//! 画面の入力チェックには頼らず、GUIのコマンドもCLIもここを通る。

use std::collections::HashSet;

use secrecy::{ExposeSecret, SecretString};

use super::invalid;
use super::rejection::{refuse_if_any, InputRejection};
use crate::error::Result;
use crate::text;

pub(super) const PROVIDER_NAME_MAX_CHARS: usize = 64;

/// リポジトリのパスや量子化の種類を含む長い名前(`hf.co/…/…-GGUF:Q4_K_M`等)が収まる長さ。
pub(super) const MODEL_NAME_MAX_CHARS: usize = 256;

/// 応答・ツール実行のタイムアウトの上限。
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

/// 数値の欄の文字列を解釈する。空欄(空白だけを含む)は未設定(`None`)、それ以外は1以上
/// `max`以下の整数だけを通す。IMEを切り忘れて打った全角の数字も受け付ける。断った理由は
/// 画面が欄の近くに出す種類で返す([`InputRejection`])。
pub(super) fn positive_integer<T>(text: &str, max: T) -> Result<Option<T>>
where
    T: Copy + Into<u64> + TryFrom<u64>,
{
    let digits: String = text
        .trim()
        .chars()
        .map(|c| match c {
            '０'..='９' => char::from(b'0' + (c as u32 - '０' as u32) as u8),
            c => c,
        })
        .collect();
    if digits.is_empty() {
        return Ok(None);
    }
    let reject = |reason| refuse_if_any(vec![reason]).map(|()| None);
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return reject(InputRejection::NotPositiveInteger);
    }
    let max_value: u64 = max.into();
    // 桁が多すぎて`u64`に収まらないものも、上限を超えたものとして断る。
    match digits.parse::<u64>() {
        Ok(0) => reject(InputRejection::NotPositiveInteger),
        Ok(n) if n <= max_value => Ok(T::try_from(n).ok()),
        _ => reject(InputRejection::NumberTooLarge { max: max_value }),
    }
}

/// HTTPヘッダーの入力。画面はヘッダーの欄の文字列(1行1件、`NAME=VALUE`)のまま送り、ここで
/// 分ける([`Self::into_pairs`])。値は秘密情報なので、欄全体を秘密情報として受け取る
/// (`architecture/network-secrets.md`「秘密情報」)。CLIは環境変数から読んだ組で渡す。
pub enum HeaderInput {
    Lines(SecretString),
    Pairs(Vec<(String, SecretString)>),
}

impl Default for HeaderInput {
    fn default() -> Self {
        Self::Pairs(Vec::new())
    }
}

impl FromIterator<(String, SecretString)> for HeaderInput {
    fn from_iter<I: IntoIterator<Item = (String, SecretString)>>(pairs: I) -> Self {
        Self::Pairs(pairs.into_iter().collect())
    }
}

/// IPCでは欄の文字列として届く。
impl<'de> serde::Deserialize<'de> for HeaderInput {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        SecretString::deserialize(deserializer).map(Self::Lines)
    }
}

impl HeaderInput {
    /// 名前と値の組にする。欄の各行は前後の空白を除いてから見て、空行は飛ばし、最初の`=`で
    /// 名前と値に分ける(名前・値の前後の空白も除く)。`=`が無い・名前が空の行はすべて
    /// 行の番号で断る(行の中身は秘密情報を含みうるので返さない)。名前と値の検証は呼び出し側。
    /// 断った理由は、他の欄の理由とまとめられるよう、並びのまま返す。
    pub(super) fn into_pairs(
        self,
    ) -> std::result::Result<Vec<(String, SecretString)>, Vec<InputRejection>> {
        let text = match self {
            Self::Pairs(pairs) => return Ok(pairs),
            Self::Lines(text) => text,
        };
        let mut pairs = Vec::new();
        let mut reasons = Vec::new();
        for (i, line) in text.expose_secret().split('\n').enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match line.split_once('=') {
                Some((name, value)) if !name.trim().is_empty() => pairs.push((
                    name.trim().to_string(),
                    SecretString::from(value.trim().to_string()),
                )),
                _ => reasons.push(InputRejection::HeaderLineInvalid { line_no: i + 1 }),
            }
        }
        if reasons.is_empty() {
            Ok(pairs)
        } else {
            Err(reasons)
        }
    }
}

/// 秘密情報の組(ヘッダー)に同じ名前が2つ無いことを確かめる。同じ名前は最後の
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

    fn rejection(text: &str, max: u32) -> InputRejection {
        match positive_integer(text, max) {
            Err(crate::error::CoreError::Rejected(r)) => r.0[0].clone(),
            other => panic!("expected a rejection for {text:?}, got {other:?}"),
        }
    }

    #[test]
    fn positive_integer_reads_blank_as_unset_and_full_width_digits() {
        assert_eq!(positive_integer("", 10u32).unwrap(), None);
        assert_eq!(positive_integer(" \u{3000}", 10u64).unwrap(), None);
        assert_eq!(positive_integer("1", 10u32).unwrap(), Some(1));
        assert_eq!(positive_integer(" 10 ", 10u64).unwrap(), Some(10));
        assert_eq!(positive_integer("１０", 10u32).unwrap(), Some(10));
    }

    #[test]
    fn positive_integer_refuses_other_text_and_values_out_of_range() {
        for text in ["0", "00", "-1", "+1", "1.5", "1e3", "abc", "1 0"] {
            assert_eq!(
                rejection(text, 10),
                InputRejection::NotPositiveInteger,
                "{text:?}"
            );
        }
        for text in ["11", "99999999999999999999999"] {
            assert_eq!(
                rejection(text, 10),
                InputRejection::NumberTooLarge { max: 10 },
                "{text:?}"
            );
        }
    }

    #[test]
    fn header_lines_are_split_at_the_first_equals_sign_and_bad_lines_are_numbered() {
        let lines = |text: &str| HeaderInput::Lines(SecretString::from(text.to_string()));
        let pairs = lines("\n X-Key = a=b \r\n\nAuthorization=Bearer t\n")
            .into_pairs()
            .unwrap();
        let shown: Vec<_> = pairs
            .iter()
            .map(|(n, v)| (n.as_str(), v.expose_secret()))
            .collect();
        assert_eq!(shown, [("X-Key", "a=b"), ("Authorization", "Bearer t")]);
        assert!(lines("").into_pairs().unwrap().is_empty());

        let reasons = lines("A=1\nsecret\n=v\n B =").into_pairs().unwrap_err();
        assert_eq!(
            reasons,
            [
                InputRejection::HeaderLineInvalid { line_no: 2 },
                InputRejection::HeaderLineInvalid { line_no: 3 },
            ]
        );
        // 行の中身(秘密情報を含みうる)はエラー文にも出さない。
        let reasons = lines("secret").into_pairs().unwrap_err();
        let err = refuse_if_any(reasons).unwrap_err();
        assert!(!err.to_string().contains("secret"));
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
