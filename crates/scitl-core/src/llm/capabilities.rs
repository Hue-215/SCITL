//! モデル能力の解決。画面の表示も、能力に応じた送信の切り替えも、ここが返す値だけを見る。
//!
//! 3層は上から、手動設定(`config::ModelOverrides`)→ 自動検出([`DetectedCapabilities`]。
//! 推論サーバーへの問い合わせは`providers`が行う)→ 既定値([`DEFAULT_CAPABILITIES`])。
//! 項目ごとに、値を持つ一番上の層が決める。上の2層は同じ形([`CapabilityLayer`])で、
//! 重ね方も[`CapabilityLayer::over`]の1つだけにする。

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::config::{Capability, ModelConfig};

/// 解決済みのモデル能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
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

/// 能力の層のうち、項目ごとに値を持たないことがあるもの(手動設定・自動検出)。値を持たない
/// 項目(`None`)は下の層に任せる。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityLayer {
    pub image: Option<bool>,
    pub tools: Option<bool>,
    pub thinking: Option<bool>,
    pub context_length: Option<u32>,
}

impl CapabilityLayer {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn flag_mut(&mut self, capability: Capability) -> &mut Option<bool> {
        match capability {
            Capability::Image => &mut self.image,
            Capability::Tools => &mut self.tools,
            Capability::Thinking => &mut self.thinking,
        }
    }

    /// この層を`below`の上に重ねる。
    ///
    /// コンテキスト長の`0`は未設定として扱う。設定画面からは入らないが、手で編集した
    /// `config.toml`も同じ経路を通るため。
    pub fn over(&self, below: ModelCapabilities) -> ModelCapabilities {
        ModelCapabilities {
            image: self.image.unwrap_or(below.image),
            tools: self.tools.unwrap_or(below.tools),
            thinking: self.thinking.unwrap_or(below.thinking),
            context_length: self
                .context_length
                .filter(|n| *n > 0)
                .unwrap_or(below.context_length),
        }
    }
}

/// 推論サーバーから分かった能力。サーバーが教えない項目は`None`で、下の層(既定値)に任せる。
pub type DetectedCapabilities = CapabilityLayer;

/// どの層でもコンテキスト長が分からないときの値。ローカル推論サーバーが既定で確保する
/// 長さの小さい側に合わせる。大きく見積もると履歴の間引きが足りずに超過で止まるが、
/// 小さく見積もっても古い発言が早めに落ちるだけで会話は続くため。
pub const FALLBACK_CONTEXT_LENGTH: u32 = 4096;

/// 一番下の層。モデル名によらず一律の値にする。名前から能力を引く表は、出典を確かめられず
/// 新しいモデルにも追従できないため持たない。実物との違いは手動設定(設定画面のモデル表)と
/// 自動検出で埋める。
///
/// - ツールはありとする。タスクの更新はツール経由でしか行えず、なしにすると
///   登録しただけのモデルではアプリの中心の操作ができなくなるため
/// - 思考はありとする。なしにすると、手動設定しない限り思考の強さを選べなくなるため。
///   思考の強さの指定を拒むAPIでは、その旨のエラー発言からモデル表での変更へ誘導する
/// - 画像はなしとする。非対応のモデルに画像を送ると呼び出しごと失敗するが、送らない側に
///   倒しても画像を添えられないだけで会話は続くため
pub const DEFAULT_CAPABILITIES: ModelCapabilities = ModelCapabilities {
    image: false,
    tools: true,
    thinking: true,
    context_length: FALLBACK_CONTEXT_LENGTH,
};

/// 手動設定より下の層(自動検出 → 既定値)で決まる値。手動設定を「下の層と同じなら外す」
/// 判定(`settings`)と、設定画面のプレースホルダはこれを見る。
pub fn fallback_capabilities(detected: Option<&DetectedCapabilities>) -> ModelCapabilities {
    match detected {
        Some(detected) => detected.over(DEFAULT_CAPABILITIES),
        None => DEFAULT_CAPABILITIES,
    }
}

/// 手動設定 → 自動検出 → 既定値の順に解決する。
pub fn resolve_capabilities(
    model: &ModelConfig,
    detected: Option<&DetectedCapabilities>,
) -> ModelCapabilities {
    model.overrides.over(fallback_capabilities(detected))
}

/// 自動検出の結果。アプリ起動中だけ保持するメモリキャッシュで、config.tomlには書かない
/// (永続化すると、サーバー側でモデルや起動オプションが変わっても追従できないため)。
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
        assert_eq!(resolve_capabilities(&model, None), DEFAULT_CAPABILITIES);

        model.overrides.image = Some(true);
        model.overrides.tools = Some(false);
        model.overrides.context_length = Some(4096);
        let resolved = resolve_capabilities(&model, None);
        assert!(resolved.image);
        assert!(!resolved.tools);
        assert_eq!(resolved.thinking, DEFAULT_CAPABILITIES.thinking);
        assert_eq!(resolved.context_length, 4096);

        model.overrides.context_length = Some(0);
        assert_eq!(
            resolve_capabilities(&model, None).context_length,
            DEFAULT_CAPABILITIES.context_length
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
        assert_eq!(resolved.tools, DEFAULT_CAPABILITIES.tools);

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
            fallback_capabilities(Some(&detected)).context_length,
            FALLBACK_CONTEXT_LENGTH
        );
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
