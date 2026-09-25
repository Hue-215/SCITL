//! モデル能力の解決(principles.md 3節「モデル能力は3層で解決する」、architecture.md 3節)。
//! 画面の表示も、能力に応じた送信の切り替えも、ここが返す値だけを見る。

use serde::Serialize;

use crate::config::{Capability, ModelConfig};

/// 解決済みのモデル能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ModelCapabilities {
    pub image: bool,
    pub tools: bool,
    pub thinking: bool,
    /// 分からなければ`None`。
    pub context_length: Option<u32>,
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

/// 手動設定が無い項目に使う値。3層のうち手動設定より下の層(自動検出・既定値)にあたる。
///
/// 今はモデルによらない一律の値で、自動検出と、モデル名ごとの既定値の表はIssue #69で足す。
/// ツールは対応ありとする。タスクの更新はツール経由でしか行えず(tools.md)、対応なしを
/// 既定にすると、登録しただけのモデルではアプリの中心の操作ができなくなるため。
pub fn default_capabilities(_model_name: &str) -> ModelCapabilities {
    ModelCapabilities {
        image: false,
        tools: true,
        thinking: false,
        context_length: None,
    }
}

/// 手動設定 → 手動設定より下の層([`default_capabilities`])の順に解決する。
pub fn resolve_capabilities(model: &ModelConfig) -> ModelCapabilities {
    let fallback = default_capabilities(&model.name);
    let manual = &model.overrides;
    ModelCapabilities {
        image: manual.image.unwrap_or(fallback.image),
        tools: manual.tools.unwrap_or(fallback.tools),
        thinking: manual.thinking.unwrap_or(fallback.thinking),
        context_length: manual.context_length.or(fallback.context_length),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_settings_take_precedence_over_defaults() {
        let mut model = ModelConfig::new("m".to_string());
        assert_eq!(resolve_capabilities(&model), default_capabilities("m"));

        model.overrides.image = Some(true);
        model.overrides.tools = Some(false);
        model.overrides.context_length = Some(4096);
        let resolved = resolve_capabilities(&model);
        assert!(resolved.image);
        assert!(!resolved.tools);
        assert_eq!(resolved.thinking, default_capabilities("m").thinking);
        assert_eq!(resolved.context_length, Some(4096));
    }
}
