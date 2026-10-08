//! 画面文言の言語ファイル(`lang/*.json`)を引く。
//!
//! 言語ファイルはフロントエンドと共有し、画面はフロントエンドが引く。core側で引くのは、
//! 画面を通らずに残る・使われる文言(エラー発言の`content`と、プロンプトの既定の文面
//! `task_chat.*`)だけ。ファイルはビルド時に埋め込むので、実行時にファイルを探さない。

use std::collections::HashMap;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

/// 表示言語。値は言語ファイルの名前(`lang/{code}.json`)と、画面の`lang`属性に使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Ja,
    En,
}

impl Language {
    pub const ALL: [Language; 2] = [Language::Ja, Language::En];

    /// 未設定のときの表示言語で、他の言語に文言が無いときの引き先。すべてのキーを持つ正本。
    pub const DEFAULT: Language = Language::Ja;

    /// `code`の言語。知らないコードは`None`。
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|language| language.code() == code)
    }

    pub fn code(self) -> &'static str {
        match self {
            Language::Ja => "ja",
            Language::En => "en",
        }
    }

    fn source(self) -> &'static str {
        match self {
            Language::Ja => {
                include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../lang/ja.json"))
            }
            Language::En => {
                include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../lang/en.json"))
            }
        }
    }
}

type Catalog = HashMap<String, String>;

static CATALOGS: LazyLock<HashMap<Language, Catalog>> = LazyLock::new(|| {
    Language::ALL
        .into_iter()
        .map(|lang| {
            // 埋め込んだファイルの形はテストで確かめているので、ここで壊れていたらビルドの誤り。
            let catalog = serde_json::from_str(lang.source()).unwrap_or_else(|e| {
                panic!("lang/{}.json is not a flat JSON object: {e}", lang.code())
            });
            (lang, catalog)
        })
        .collect()
});

/// `key`の文言。`lang`に無ければ[`Language::DEFAULT`]、そこにも無ければキーそのものを返す。
pub fn text(lang: Language, key: &str) -> &str {
    [lang, Language::DEFAULT]
        .into_iter()
        .find_map(|l| CATALOGS[&l].get(key))
        .map_or(key, String::as_str)
}

/// `key`の文言の`{名前}`を`values`の値に置き換える。1回の走査で置き換えるので、差し込んだ値に
/// `{…}`が含まれていても置き換えない。渡さなかった名前は`{名前}`のまま残る
/// (`architecture/i18n.md`「言語ファイル」。画面の`t()`と同じ規則)。
pub fn format(lang: Language, key: &str, values: &[(&str, &str)]) -> String {
    let template = text(lang, key);
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let name_len = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(after.len());
        let value = (after[name_len..].starts_with('}'))
            .then(|| values.iter().find(|(n, _)| *n == &after[..name_len]))
            .flatten();
        match value {
            Some((_, value)) => {
                out.push_str(value);
                rest = &after[name_len + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fmt;

    use serde::de::{Deserializer, MapAccess, Visitor};

    use super::*;

    /// 書かれた順のままのエントリ。`HashMap`に読むと重複したキーが黙って後勝ちになるため、
    /// 検査はこれで読む。
    struct Entries(Vec<(String, String)>);

    impl<'de> Deserialize<'de> for Entries {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct EntriesVisitor;
            impl<'de> Visitor<'de> for EntriesVisitor {
                type Value = Entries;
                fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                    f.write_str("a flat JSON object of strings")
                }
                fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Entries, A::Error> {
                    let mut entries = Vec::new();
                    while let Some(entry) = map.next_entry::<String, String>()? {
                        entries.push(entry);
                    }
                    Ok(Entries(entries))
                }
            }
            deserializer.deserialize_map(EntriesVisitor)
        }
    }

    fn entries(lang: Language) -> Vec<(String, String)> {
        serde_json::from_str::<Entries>(lang.source())
            .unwrap_or_else(|e| panic!("lang/{}.json: {e}", lang.code()))
            .0
    }

    /// 文言中のプレースホルダー名。`{`・`}`はプレースホルダー専用で、名前は英数字と`_`。
    /// それ以外の形で現れたら`Err`。
    fn placeholders(text: &str) -> Result<BTreeSet<&str>, String> {
        let mut names = BTreeSet::new();
        let mut rest = text;
        while let Some(open) = rest.find(['{', '}']) {
            if rest[open..].starts_with('}') {
                return Err(format!("unmatched '}}' in {text:?}"));
            }
            let after = &rest[open + 1..];
            let close = after
                .find('}')
                .ok_or_else(|| format!("unclosed '{{' in {text:?}"))?;
            let name = &after[..close];
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(format!("invalid placeholder {{{name}}} in {text:?}"));
            }
            names.insert(name);
            rest = &after[close + 1..];
        }
        Ok(names)
    }

    #[test]
    fn every_language_file_is_registered() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../lang");
        let files: BTreeSet<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        let registered: BTreeSet<String> = Language::ALL
            .iter()
            .map(|lang| format!("{}.json", lang.code()))
            .collect();
        assert_eq!(files, registered);
    }

    /// キーを探しやすく、言語ごとの差分を行で突き合わせられるよう、キー順に並べる。
    /// 重複はフラットなファイルでコピーしたときに起きやすく、読み込みでは検出できない。
    #[test]
    fn keys_are_sorted_and_unique() {
        for lang in Language::ALL {
            let keys: Vec<String> = entries(lang).into_iter().map(|(k, _)| k).collect();
            for pair in keys.windows(2) {
                assert!(
                    pair[0] < pair[1],
                    "lang/{}.json: {:?} must come before {:?} and appear once",
                    lang.code(),
                    pair[1],
                    pair[0]
                );
            }
        }
    }

    #[test]
    fn every_language_has_the_same_keys_and_placeholders() {
        let base: HashMap<String, String> = entries(Language::DEFAULT).into_iter().collect();
        for lang in Language::ALL {
            let catalog: HashMap<String, String> = entries(lang).into_iter().collect();
            let missing: BTreeSet<_> = base.keys().filter(|k| !catalog.contains_key(*k)).collect();
            let extra: BTreeSet<_> = catalog.keys().filter(|k| !base.contains_key(*k)).collect();
            assert!(
                missing.is_empty(),
                "lang/{}.json lacks {missing:?}",
                lang.code()
            );
            assert!(
                extra.is_empty(),
                "lang/{}.json has unknown {extra:?}",
                lang.code()
            );
            for (key, value) in &catalog {
                assert!(
                    !value.trim().is_empty(),
                    "lang/{}.json: {key} is empty",
                    lang.code()
                );
                assert_eq!(
                    placeholders(value),
                    placeholders(&base[key]),
                    "lang/{}.json: placeholders of {key} differ from lang/{}.json",
                    lang.code(),
                    Language::DEFAULT.code()
                );
            }
        }
    }

    #[test]
    fn placeholders_are_well_formed() {
        for lang in Language::ALL {
            for (key, value) in entries(lang) {
                if let Err(e) = placeholders(&value) {
                    panic!("lang/{}.json: {key}: {e}", lang.code());
                }
            }
        }
    }

    #[test]
    fn text_looks_up_the_language_and_falls_back_to_the_key() {
        assert_eq!(text(Language::En, "common.cancel"), "Cancel");
        assert_eq!(text(Language::Ja, "common.cancel"), "キャンセル");
        assert_eq!(text(Language::En, "no.such.key"), "no.such.key");
    }

    #[test]
    fn placeholder_parser_rejects_stray_braces() {
        assert_eq!(
            placeholders("{a} and {b_1}").unwrap(),
            BTreeSet::from(["a", "b_1"])
        );
        assert!(placeholders("{a").is_err());
        assert!(placeholders("a}").is_err());
        assert!(placeholders("{}").is_err());
        assert!(placeholders("{a b}").is_err());
    }

    /// 画面と写し合う値を、ts-rsの生成物と同じ置き場所(`frontend/src/bindings/`)へ書き出す。
    /// 画面はこれを読み、自分では書かない。CIは生成し直した結果とコミット済みの生成物が一致する
    /// ことを確かめるので、Rust側だけを変えても止まる。ts-rsは型しか書き出せないので、ここで書く。
    #[test]
    fn format_replaces_named_values_in_one_pass() {
        // 言語ファイルに無いキーは、キーそのものを文言として扱う。
        let format =
            |template: &str, values: &[(&str, &str)]| format(Language::Ja, template, values);
        assert_eq!(
            format("{a} and {b}", &[("a", "{b}"), ("b", "2")]),
            "{b} and 2"
        );
        assert_eq!(format("{missing} {", &[]), "{missing} {");
        assert_eq!(format("{a}{a}", &[("a", "x")]), "xx");
    }

    #[test]
    fn export_shared_constants() {
        let dir = std::env::var("TS_RS_EXPORT_DIR").expect("set in .cargo/config.toml");
        let quoted = |value: &str| serde_json::to_string(value).expect("a string serializes");
        let body = format!(
            "// This file was generated by a test in scitl-core (`i18n::tests::export_shared_constants`). Do not edit this file manually.\n\
             import type {{ Language }} from \"./Language\";\n\
             \n\
             /** 未設定のときの表示言語で、すべてのキーを持つ正本(`i18n::Language::DEFAULT`)。 */\n\
             export const DEFAULT_LANGUAGE: Language = {};\n\
             \n\
             /** エラー発言の文言のキーの前置き(`orchestration::turn_error::MESSAGE_KEY_PREFIX`)。 */\n\
             export const TURN_ERROR_KEY_PREFIX = {};\n\
             \n\
             /** メモリ1件の本文の上限文字数(`db::memories::MAX_MEMORY_CHARS`)。 */\n\
             export const MAX_MEMORY_CHARS = {};\n\
             \n\
             /** 持てるメモリの上限件数(`db::memories::MAX_MEMORIES`)。 */\n\
             export const MAX_MEMORIES = {};\n\
             \n\
             /** タスクのタイトルの上限文字数(`db::tasks::MAX_TITLE_CHARS`)。 */\n\
             export const MAX_TITLE_CHARS = {};\n",
            quoted(Language::DEFAULT.code()),
            quoted(crate::orchestration::turn_error::MESSAGE_KEY_PREFIX),
            crate::db::memories::MAX_MEMORY_CHARS,
            crate::db::memories::MAX_MEMORIES,
            crate::db::tasks::MAX_TITLE_CHARS,
        );
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(std::path::Path::new(&dir).join("SharedConstants.ts"), body).unwrap();
    }
}
