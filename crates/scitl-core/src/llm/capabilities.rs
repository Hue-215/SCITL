//! モデル能力の解決(principles.md 3節「モデル能力は3層で解決する」、architecture.md 3節)。
//! 画面の表示も、能力に応じた送信の切り替えも、ここが返す値だけを見る。
//!
//! 3層は上から、手動設定(`config::ModelOverrides`)→ 自動検出([`DetectedCapabilities`]。
//! 推論サーバーへの問い合わせは`providers`が行う)→ 既定値([`default_capabilities`])。
//! 項目ごとに、値を持つ一番上の層が決める。

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;

use crate::config::{Capability, ModelConfig};

/// 解決済みのモデル能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ModelCapabilities {
    pub image: bool,
    pub tools: bool,
    pub thinking: bool,
    /// 1回の呼び出しに入るトークン数。どの層でも分からなければ保守的な値
    /// ([`FALLBACK_CONTEXT_LENGTH`])になり、未定のまま返ることはない。
    pub context_length: u32,
}

impl ModelCapabilities {
    pub fn flag(&self, capability: Capability) -> bool {
        match capability {
            Capability::Image => self.image,
            Capability::Tools => self.tools,
            Capability::Thinking => self.thinking,
        }
    }
}

/// 推論サーバーから分かった能力。サーバーが教えない項目は`None`で、下の層(既定値)に任せる。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DetectedCapabilities {
    pub image: Option<bool>,
    pub tools: Option<bool>,
    pub thinking: Option<bool>,
    pub context_length: Option<u32>,
}

/// 既定値の表にも当たらないモデルのコンテキスト長。ローカル推論サーバーが既定で確保する
/// 長さの小さい側に合わせる。大きく見積もると履歴の間引きが足りずに超過で止まるが、
/// 小さく見積もっても古い発言が早めに落ちるだけで会話は続くため。
pub const FALLBACK_CONTEXT_LENGTH: u32 = 4096;

/// 既定値の表の1行。
struct DefaultRow {
    pattern: NamePattern,
    image: bool,
    tools: bool,
    thinking: bool,
    /// クラウド専用のモデルにだけ書く。手元で動かせるモデルは、学習時の長さではなく
    /// 推論サーバーの起動時の設定で決まるため、名前からは分からない。
    context_length: Option<u32>,
}

/// モデル名との照合。名前は[`normalize`]した形で比べる。
enum NamePattern {
    /// 先頭が一致する。短い名前(`o3`等)が他のモデル名の途中に当たらないように使う。
    Prefix(&'static str),
    Contains(&'static str),
    /// すべてを含む。語順が揃わない名前(`llama3.2-vision`と`Llama-3.2-11B-Vision`)に使う。
    ContainsAll(&'static [&'static str]),
}

impl NamePattern {
    fn matches(&self, normalized: &str) -> bool {
        match self {
            Self::Prefix(p) => normalized.starts_with(p),
            Self::Contains(p) => normalized.contains(p),
            Self::ContainsAll(parts) => parts.iter().all(|p| normalized.contains(p)),
        }
    }
}

const fn row(
    pattern: NamePattern,
    image: bool,
    tools: bool,
    thinking: bool,
    context_length: Option<u32>,
) -> DefaultRow {
    DefaultRow {
        pattern,
        image,
        tools,
        thinking,
        context_length,
    }
}

use NamePattern::{Contains, ContainsAll, Prefix};

/// モデル名ごとの既定値(architecture.md 2節「モデル能力の既定値」)。上から順に照合し、
/// 最初に当たった行を使う。同じ系列の中で能力が違うものは、細かい名前を先に置く。
/// ここに無いモデルは[`UNKNOWN_MODEL`]になる。表の値が実物と違っても、利用者は手動設定で
/// 直せる(設定画面のモデル表)。
const DEFAULT_TABLE: &[DefaultRow] = &[
    // OpenAI
    row(Contains("gpt4o"), true, true, false, Some(128_000)),
    row(Contains("gpt4.1"), true, true, false, Some(1_047_576)),
    row(Contains("gpt4turbo"), true, true, false, Some(128_000)),
    row(Contains("gpt5"), true, true, true, Some(400_000)),
    row(Contains("gpt3.5"), false, true, false, Some(16_385)),
    row(Prefix("o1mini"), false, false, true, Some(128_000)),
    row(Prefix("o3mini"), false, true, true, Some(200_000)),
    row(Prefix("o1"), true, true, true, Some(200_000)),
    row(Prefix("o3"), true, true, true, Some(200_000)),
    row(Prefix("o4"), true, true, true, Some(200_000)),
    // 手元でも動かせるので長さは書かない。
    row(Contains("gptoss"), false, true, true, None),
    // Anthropic
    row(Contains("claude37"), true, true, true, Some(200_000)),
    row(Contains("claude3"), true, true, false, Some(200_000)),
    row(Contains("claude"), true, true, true, Some(200_000)),
    // Google
    row(Contains("gemini1"), true, true, false, Some(1_048_576)),
    row(Contains("gemini2.0"), true, true, false, Some(1_048_576)),
    row(Contains("gemini"), true, true, true, Some(1_048_576)),
    row(Contains("gemma3"), true, false, false, None),
    row(Contains("gemma"), false, false, false, None),
    // DeepSeek。`deepseek-chat`・`deepseek-reasoner`はAPI専用の名前。
    row(
        Contains("deepseekreasoner"),
        false,
        true,
        true,
        Some(128_000),
    ),
    row(Contains("deepseekchat"), false, true, false, Some(128_000)),
    row(Contains("deepseekr1"), false, false, true, None),
    // Qwen
    row(Contains("qwen3vl"), true, true, false, None),
    row(Contains("qwen2.5vl"), true, false, false, None),
    row(Contains("qwen2vl"), true, false, false, None),
    row(Contains("qwq"), false, true, true, None),
    row(Contains("qwen3"), false, true, true, None),
    row(Contains("qwen2.5"), false, true, false, None),
    // Meta
    row(
        ContainsAll(&["llama3.2", "vision"]),
        true,
        false,
        false,
        None,
    ),
    row(Contains("llama4"), true, true, false, None),
    row(Contains("llama3"), false, true, false, None),
    row(Contains("llava"), true, false, false, None),
    // その他
    row(Contains("mistral"), false, true, false, None),
    row(Contains("phi4reasoning"), false, false, true, None),
    row(Contains("phi"), false, false, false, None),
];

/// 表に無いモデル。ツールは対応ありとする。タスクの更新はツール経由でしか行えず
/// (tools.md)、対応なしを既定にすると、登録しただけのモデルではアプリの中心の操作が
/// できなくなるため。
const UNKNOWN_MODEL: DefaultRow = row(Contains(""), false, true, false, None);

/// 照合用に名前を揃える。提供元の前置き(`openai/`、`Qwen/`等)を落とし、大文字小文字と
/// 区切り(`-`・`_`・空白)の違いを無視する(`Llama-3.2-Vision`と`llama3.2-vision`を
/// 同じ系列として扱うため)。`.`は版の区切り(`qwen2.5`)なので残す。
fn normalize(model_name: &str) -> String {
    let base = model_name.rsplit('/').next().unwrap_or(model_name);
    base.chars()
        .filter(|c| !matches!(c, '-' | '_') && !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

/// 一番下の層。モデル名だけから決める。
pub fn default_capabilities(model_name: &str) -> ModelCapabilities {
    let normalized = normalize(model_name);
    let row = DEFAULT_TABLE
        .iter()
        .find(|r| r.pattern.matches(&normalized))
        .unwrap_or(&UNKNOWN_MODEL);
    ModelCapabilities {
        image: row.image,
        tools: row.tools,
        thinking: row.thinking,
        context_length: row.context_length.unwrap_or(FALLBACK_CONTEXT_LENGTH),
    }
}

/// 手動設定より下の層(自動検出 → 既定値)で決まる値。手動設定を「下の層と同じなら外す」
/// 判定(`settings`)と、設定画面のプレースホルダはこれを見る。
pub fn fallback_capabilities(
    model_name: &str,
    detected: Option<&DetectedCapabilities>,
) -> ModelCapabilities {
    let default = default_capabilities(model_name);
    let Some(detected) = detected else {
        return default;
    };
    ModelCapabilities {
        image: detected.image.unwrap_or(default.image),
        tools: detected.tools.unwrap_or(default.tools),
        thinking: detected.thinking.unwrap_or(default.thinking),
        context_length: detected
            .context_length
            .filter(|n| *n > 0)
            .unwrap_or(default.context_length),
    }
}

/// 手動設定 → 自動検出 → 既定値の順に解決する。
///
/// コンテキスト長の`0`は未設定として扱う。設定画面からは入らないが、手で編集した
/// `config.toml`が同じ経路を通るため(`GeneralConfig::response_timeout`と同じ扱い)。
pub fn resolve_capabilities(
    model: &ModelConfig,
    detected: Option<&DetectedCapabilities>,
) -> ModelCapabilities {
    let fallback = fallback_capabilities(&model.name, detected);
    let manual = &model.overrides;
    ModelCapabilities {
        image: manual.image.unwrap_or(fallback.image),
        tools: manual.tools.unwrap_or(fallback.tools),
        thinking: manual.thinking.unwrap_or(fallback.thinking),
        context_length: manual
            .context_length
            .filter(|n| *n > 0)
            .unwrap_or(fallback.context_length),
    }
}

/// 自動検出の結果。アプリ起動中だけ保持するメモリキャッシュで、config.tomlには書かない
/// (能力はユーザーの設定ではなくサーバー側の持ち物で、永続化した写しはサーバー側の
/// 変更(読み込み直したモデル、起動オプション)を検知できない。MCPのツール一覧
/// (`mcp::ToolCatalog`)と同じ扱い)。
#[derive(Default)]
pub struct DetectedCatalog {
    by_model: Mutex<HashMap<(String, String), DetectedCapabilities>>,
}

impl DetectedCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, provider_id: &str, model: &str) -> Option<DetectedCapabilities> {
        self.lock()
            .get(&(provider_id.to_string(), model.to_string()))
            .cloned()
    }

    pub fn store(&self, provider_id: &str, model: &str, detected: DetectedCapabilities) {
        self.lock()
            .insert((provider_id.to_string(), model.to_string()), detected);
    }

    /// プロバイダーの削除で、そのプロバイダーの結果をまとめて捨てる。
    pub fn forget_provider(&self, provider_id: &str) {
        self.lock().retain(|(p, _), _| p != provider_id);
    }

    pub fn forget(&self, provider_id: &str, model: &str) {
        self.lock()
            .remove(&(provider_id.to_string(), model.to_string()));
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(String, String), DetectedCapabilities>> {
        self.by_model
            .lock()
            .expect("detected capabilities mutex poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_settings_take_precedence_over_lower_layers() {
        let mut model = ModelConfig::new("m".to_string());
        assert_eq!(
            resolve_capabilities(&model, None),
            default_capabilities("m")
        );

        model.overrides.image = Some(true);
        model.overrides.tools = Some(false);
        model.overrides.context_length = Some(4096);
        let resolved = resolve_capabilities(&model, None);
        assert!(resolved.image);
        assert!(!resolved.tools);
        assert_eq!(resolved.thinking, default_capabilities("m").thinking);
        assert_eq!(resolved.context_length, 4096);

        model.overrides.context_length = Some(0);
        assert_eq!(
            resolve_capabilities(&model, None).context_length,
            default_capabilities("m").context_length
        );
    }

    #[test]
    fn detected_values_sit_between_manual_settings_and_defaults() {
        let mut model = ModelConfig::new("unknown-model".to_string());
        let detected = DetectedCapabilities {
            image: Some(true),
            tools: None,
            thinking: Some(true),
            context_length: Some(32_768),
        };
        let resolved = resolve_capabilities(&model, Some(&detected));
        assert!(resolved.image);
        assert!(resolved.thinking);
        assert_eq!(resolved.context_length, 32_768);
        // サーバーが教えない項目は既定値。
        assert_eq!(resolved.tools, default_capabilities("unknown-model").tools);

        model.overrides.thinking = Some(false);
        model.overrides.context_length = Some(8192);
        let resolved = resolve_capabilities(&model, Some(&detected));
        assert!(!resolved.thinking);
        assert_eq!(resolved.context_length, 8192);
    }

    #[test]
    fn detected_zero_context_length_falls_through_to_defaults() {
        let detected = DetectedCapabilities {
            context_length: Some(0),
            ..Default::default()
        };
        assert_eq!(
            fallback_capabilities("m", Some(&detected)).context_length,
            FALLBACK_CONTEXT_LENGTH
        );
    }

    #[test]
    fn default_table_ignores_case_separators_and_vendor_prefix() {
        for name in [
            "llama3.2-vision:11b",
            "Llama-3.2-11B-Vision-Instruct",
            "meta-llama/Llama-3.2-11B-Vision",
        ] {
            let caps = default_capabilities(name);
            assert!(caps.image, "{name}");
            assert!(!caps.tools, "{name}");
        }
        assert_eq!(
            default_capabilities("openai/gpt-4o"),
            default_capabilities("gpt-4o-2024-08-06")
        );
    }

    #[test]
    fn more_specific_rows_win_within_a_family() {
        assert!(!default_capabilities("qwen2.5-vl:7b").tools);
        assert!(default_capabilities("qwen2.5-vl:7b").image);
        assert!(default_capabilities("qwen2.5:7b").tools);
        assert!(!default_capabilities("qwen2.5:7b").image);
        assert!(!default_capabilities("claude-3-5-sonnet").thinking);
        assert!(default_capabilities("claude-sonnet-4-5").thinking);
    }

    #[test]
    fn short_openai_names_match_only_at_the_start() {
        assert!(default_capabilities("o3").thinking);
        assert!(default_capabilities("o4-mini").thinking);
        // 途中に`o3`を含むだけのモデルは当たらない。
        assert_eq!(
            default_capabilities("foo3"),
            default_capabilities("unknown-model")
        );
    }

    #[test]
    fn open_weight_models_leave_context_length_to_the_fallback() {
        assert_eq!(
            default_capabilities("qwen3:8b").context_length,
            FALLBACK_CONTEXT_LENGTH
        );
        assert_eq!(default_capabilities("gpt-4o").context_length, 128_000);
    }

    #[test]
    fn unknown_models_assume_tool_support() {
        let caps = default_capabilities("some-new-model");
        assert!(caps.tools);
        assert!(!caps.image);
        assert!(!caps.thinking);
        assert_eq!(caps.context_length, FALLBACK_CONTEXT_LENGTH);
    }

    #[test]
    fn catalog_forgets_by_provider() {
        let catalog = DetectedCatalog::new();
        catalog.store("p1", "a", DetectedCapabilities::default());
        catalog.store("p1", "b", DetectedCapabilities::default());
        catalog.store("p2", "a", DetectedCapabilities::default());
        catalog.forget_provider("p1");
        assert!(catalog.get("p1", "a").is_none());
        assert!(catalog.get("p1", "b").is_none());
        assert!(catalog.get("p2", "a").is_some());
    }
}
