//! 秘密情報を含まないプロバイダー設定の永続化(TOML)。architecture.md 6節が定める通り、
//! ここが持つのは`key_ref`という不透明な参照文字列だけで、平文の鍵を持つフィールドは
//! 型として存在させない。実際の鍵の出し入れは[`crate::secrets`]の責務。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::db::error::CoreError;

/// 対応するプロバイダーAPIの方言。現状はOpenAI互換チャットコンプリーションAPIのみ
/// (architecture.md 2節)。将来プロバイダーを追加する際はここにバリアントを足す。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiFormat {
    OpenAiCompat,
}

/// 1つのLLMプロバイダー設定。秘密情報を含まないため、そのままログに出しても
/// TOMLファイルとして保存してもよい。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub id: String,
    /// 画面表示用の名前(idはユーザーに見せない内部識別子)。
    pub name: String,
    pub api_format: ApiFormat,
    pub base_url: String,
    /// このプロバイダーで使えるモデル名の一覧。手動追加・削除する(設定画面「APIプロバイダー」
    /// タブ)。API問い合わせによる一括取得は別Issueで扱う。
    #[serde(default)]
    pub models: Vec<String>,
    /// `models`のうちチャットで実際に使うモデル。`models`に無い値は無効。
    pub active_model: Option<String>,
    /// [`crate::secrets`]に保存した秘密情報を指す不透明な参照。未設定(鍵が要らない
    /// ローカル推論サーバー等)の場合は`None`。
    pub key_ref: Option<String>,
}

impl ProviderConfig {
    /// チャットに使うモデル名。`active_model`が未設定または`models`に存在しない場合は
    /// `models`の先頭にフォールバックする。
    pub fn resolved_model(&self) -> Option<&str> {
        let active = self
            .active_model
            .as_deref()
            .filter(|m| self.models.iter().any(|x| x == m));
        active.or_else(|| self.models.first().map(String::as_str))
    }
}

/// システムプロンプト等、モデル・プロバイダーに依存しない全般設定
/// (legacy/frontend.md 2節)。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GeneralConfig {
    pub system_prompt: Option<String>,
    /// 応答タイムアウト(秒)。未設定はアダプタ側の既定値を使う。
    pub response_timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    pub active_provider_id: Option<String>,
    #[serde(default)]
    pub general: GeneralConfig,
}

impl Config {
    pub fn active_provider(&self) -> Option<&ProviderConfig> {
        let active_id = self.active_provider_id.as_deref()?;
        self.providers.iter().find(|p| p.id == active_id)
    }
}

/// 設定ファイルを読み込む。ファイルが存在しない場合は初回起動として空の設定を返す
/// (設定画面(Issue #22)がまだ無いため、これが有効な初期状態)。
pub fn load(path: &Path) -> Result<Config, CoreError> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(|e| CoreError::Config(e.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(CoreError::Config(e.to_string())),
    }
}

/// 設定ファイルを保存する。呼び出し元(Tauriコマンド層)が親ディレクトリの存在を保証する。
pub fn save(path: &Path, config: &Config) -> Result<(), CoreError> {
    let text = toml::to_string_pretty(config).map_err(|e| CoreError::Config(e.to_string()))?;
    std::fs::write(path, text).map_err(|e| CoreError::Config(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_missing_file_returns_default_config() {
        let dir = tempdir();
        let config = load(&dir.join("config.toml")).unwrap();
        assert!(config.providers.is_empty());
        assert!(config.active_provider_id.is_none());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = tempdir();
        let path = dir.join("config.toml");
        let config = Config {
            providers: vec![ProviderConfig {
                id: "default".to_string(),
                name: "OpenAI".to_string(),
                api_format: ApiFormat::OpenAiCompat,
                base_url: "https://api.openai.com/v1".to_string(),
                models: vec!["gpt-4o-mini".to_string()],
                active_model: Some("gpt-4o-mini".to_string()),
                key_ref: Some("provider:default".to_string()),
            }],
            active_provider_id: Some("default".to_string()),
            general: GeneralConfig::default(),
        };

        save(&path, &config).unwrap();
        let loaded = load(&path).unwrap();

        assert_eq!(loaded.providers.len(), 1);
        assert_eq!(loaded.active_provider().unwrap().id, "default");
        assert_eq!(
            loaded.active_provider().unwrap().key_ref.as_deref(),
            Some("provider:default")
        );
        assert_eq!(
            loaded.active_provider().unwrap().resolved_model(),
            Some("gpt-4o-mini")
        );
    }

    #[test]
    fn resolved_model_falls_back_to_first_when_active_is_invalid() {
        let provider = ProviderConfig {
            id: "p".to_string(),
            name: "Local".to_string(),
            api_format: ApiFormat::OpenAiCompat,
            base_url: "http://localhost:1234/v1".to_string(),
            models: vec!["a".to_string(), "b".to_string()],
            active_model: Some("not-in-list".to_string()),
            key_ref: None,
        };
        assert_eq!(provider.resolved_model(), Some("a"));
    }

    fn tempdir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("scitl-config-test-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
