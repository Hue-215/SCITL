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
}
