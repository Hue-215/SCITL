//! 1ターンでのツール呼び出しに掛ける上限(Issue #71)。
//!
//! 設定([`crate::config::ToolConfig`])は「未設定」を`None`で持ち、既定値は持たない。
//! 既定値の実体はここ1箇所だけにあり、上限の解釈も[`crate::orchestration::turn`]と
//! ここに閉じる(`docs/spec/principles.md` 5節)。

use std::time::Duration;

use crate::config::ToolConfig;

/// ラウンド数の既定値。旧来の定数`MAX_TOOL_ROUNDS`をそのまま引き継ぐ
/// (設定可能にしただけで、何も設定していないユーザーの挙動は変えない)。
pub const DEFAULT_MAX_ROUNDS_PER_TURN: u32 = 4;

/// ツール実行に使える合計時間の既定値。応答タイムアウトの既定値(300秒、
/// `docs/spec/legacy/frontend.md` 3節)と揃える。普段は発動せず、応答しない
/// 外部サーバーでターンが延々と返らなくなるのを防ぐための天井として置く。
pub const DEFAULT_TOTAL_TIMEOUT_SECS: u64 = 300;

/// 解決済みの上限。ターンはこの型だけを見て、`Option`の解釈はしない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolLimits {
    /// 1ターンあたりのツール呼び出しラウンド数の上限。
    pub max_rounds_per_turn: u32,
    /// 1ターン内のツール実行に使える時間の合計。LLMの応答待ちは含まない
    /// (そちらはアダプタ側のタイムアウトが見る)。判定はツール呼び出しの区切りで
    /// 行うため、実際の打ち切りは最後の1回分ぶん超えうる(理由は`turn`側のコメント)。
    pub total_timeout: Duration,
}

impl Default for ToolLimits {
    fn default() -> Self {
        Self {
            max_rounds_per_turn: DEFAULT_MAX_ROUNDS_PER_TURN,
            total_timeout: Duration::from_secs(DEFAULT_TOTAL_TIMEOUT_SECS),
        }
    }
}

impl ToolLimits {
    /// 未設定(`None`)と、保存済みの設定に紛れ込んだ`0`を既定値へ落とす。`0`は
    /// 「ツールを一切呼ばせない」ではなく設定の不備として扱う(コマンド側でも弾くが、
    /// 手で編集した`config.toml`が同じ経路を通るため、ここでも受け止める)。
    pub fn from_config(config: &ToolConfig) -> Self {
        let default = Self::default();
        Self {
            max_rounds_per_turn: config
                .max_rounds_per_turn
                .filter(|n| *n > 0)
                .unwrap_or(default.max_rounds_per_turn),
            total_timeout: config
                .total_timeout_secs
                .filter(|s| *s > 0)
                .map(Duration::from_secs)
                .unwrap_or(default.total_timeout),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_config_falls_back_to_defaults() {
        assert_eq!(ToolLimits::from_config(&ToolConfig::default()), ToolLimits::default());
    }

    #[test]
    fn zero_is_treated_as_unset() {
        let limits = ToolLimits::from_config(&ToolConfig {
            max_rounds_per_turn: Some(0),
            total_timeout_secs: Some(0),
        });
        assert_eq!(limits, ToolLimits::default());
    }

    #[test]
    fn configured_values_are_used() {
        let limits = ToolLimits::from_config(&ToolConfig {
            max_rounds_per_turn: Some(12),
            total_timeout_secs: Some(30),
        });
        assert_eq!(limits.max_rounds_per_turn, 12);
        assert_eq!(limits.total_timeout, Duration::from_secs(30));
    }
}
