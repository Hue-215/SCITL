//! 設定ファイル(TOML)の読み書き。秘密情報は`key_ref`という不透明な参照だけを持ち、
//! 平文の鍵を持つフィールドは型として存在させない。鍵の出し入れは[`crate::secrets`]。

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::i18n::Language;

/// 対応するプロバイダーAPIの方言。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum ApiFormat {
    OpenAiCompat,
}

/// 1つのLLMプロバイダー設定。秘密情報を含まないため、そのままログに出しても
/// TOMLファイルとして保存してもよい。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub id: String,
    /// 画面表示用の名前(idはユーザーに見せない内部識別子)。
    pub name: String,
    pub api_format: ApiFormat,
    pub base_url: String,
    /// 登録したモデルの一覧。
    #[serde(default)]
    pub models: Vec<ModelConfig>,
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
            .filter(|m| self.model(m).is_some());
        active.or_else(|| self.models.first().map(|m| m.name.as_str()))
    }

    pub fn model(&self, name: &str) -> Option<&ModelConfig> {
        self.models.iter().find(|m| m.name == name)
    }
}

/// 登録済みの1モデル。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelConfig {
    pub name: String,
    /// チャットのモデル一覧に出すか。
    #[serde(default = "visible_by_default")]
    pub visible: bool,
    /// 能力の手動設定。値の無い項目は[`crate::llm::resolve_capabilities`]が下の層で決める。
    #[serde(default, skip_serializing_if = "ModelOverrides::is_empty")]
    pub overrides: ModelOverrides,
    /// 思考の強さ。受け付ける値がモデルごとに違うため、モデルごとに持つ。
    #[serde(default)]
    pub reasoning_effort: ReasoningEffort,
}

impl ModelConfig {
    pub fn new(name: String) -> Self {
        Self {
            name,
            visible: visible_by_default(),
            overrides: ModelOverrides::default(),
            reasoning_effort: ReasoningEffort::default(),
        }
    }
}

/// 追加したモデルは、隠すまでチャットの一覧に出す。
fn visible_by_default() -> bool {
    true
}

/// モデルの能力のうち、対応の有無で表すもの。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Image,
    Tools,
    Thinking,
}

/// 思考の強さ。リクエストでの書き方は方言ごとにアダプタが決める。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    /// 思考させない。
    Off,
    Low,
    /// まだ選んでいないモデルの値。OpenAIがAPIの既定としている強さに合わせる。
    #[default]
    Medium,
    High,
}

/// 能力の手動設定。`None`は「手動では決めていない」。
pub type ModelOverrides = crate::llm::CapabilityLayer;

/// 応答タイムアウトの既定値(秒)。未設定のときに使う値はここにだけ置く。生成の長い
/// 非ストリーミング応答も待てるよう、余裕を持たせる。
/// タイムアウト自体は常に掛ける(応答しないエンドポイント1つでターンが固まらないように)。
pub const DEFAULT_RESPONSE_TIMEOUT_SECS: u64 = 120;

/// システムプロンプト等、モデル・プロバイダーに依存しない全般設定。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GeneralConfig {
    pub system_prompt: Option<String>,
    /// タスクチャットでのみ追加するシステムプロンプト(工程ツールの使い方など)。未設定は
    /// 表示言語の既定の文面で、解釈は`orchestration::SystemPrompts::from_config`に閉じる。
    pub task_chat_system_prompt: Option<String>,
    /// 新規タスクで聞き取りを始めるとき、ユーザーの代わりに送る発言。未設定は
    /// 表示言語の既定の文面で、解釈は`orchestration::opening_message`に閉じる。
    pub task_opening_message: Option<String>,
    /// 応答タイムアウト(秒)。未設定は[`DEFAULT_RESPONSE_TIMEOUT_SECS`]。値の解釈は
    /// [`Self::response_timeout`]に閉じる。
    pub response_timeout_secs: Option<u64>,
    /// 表示言語。未設定は[`Language::DEFAULT`]。値の解釈は[`Self::language`]に閉じる。
    /// 知らない値が書かれていると、他の列挙と同じく設定ファイル全体を読めない扱いになる。
    pub language: Option<Language>,
}

impl GeneralConfig {
    /// 未設定(`None`)と`0`はどちらも既定値。更新時にも`0`は弾くが、手で編集した
    /// `config.toml`もここを通るため、ここでも受け止める。
    pub fn response_timeout(&self) -> Duration {
        Duration::from_secs(
            self.response_timeout_secs
                .filter(|s| *s > 0)
                .unwrap_or(DEFAULT_RESPONSE_TIMEOUT_SECS),
        )
    }

    pub fn language(&self) -> Language {
        self.language.unwrap_or(Language::DEFAULT)
    }
}

/// ツール呼び出しの上限(設定画面「ツール/MCP」タブ)。`None`は未設定で、既定値は
/// [`crate::orchestration::ToolLimits`]が持つ。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolConfig {
    /// 1ターンでツールを実行するラウンドの上限。使い切ったら、ツールを渡さずにもう一度だけ
    /// モデルを呼んで返信させる(`orchestration::turn`)。
    pub max_rounds_per_turn: Option<u32>,
    /// 1ターン内のツール実行に使える時間の合計(秒)。LLMの応答待ちは含まない。
    pub total_timeout_secs: Option<u64>,
}

/// [`crate::secrets`]に保存した1つの値(環境変数またはHTTPヘッダーの値)を指す参照。
/// `key_ref`はULIDで払い出し、`name`からは組み立てない(`name`はユーザー入力で、`:`等を
/// 含んで衝突しうるため)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretRef {
    pub name: String,
    pub key_ref: String,
}

/// MCPサーバーへの接続方式。接続方式ごとに必要な値だけを持たせ、`Stdio`なのに`url`が
/// あるような状態を型で防ぐ。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum McpEndpoint {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env_refs: Vec<SecretRef>,
    },
    StreamableHttp {
        url: String,
        #[serde(default)]
        header_refs: Vec<SecretRef>,
    },
}

/// 1つの外部ツールサーバー(MCP)設定。秘密情報を含まない。ツール一覧(名前・説明)は
/// 永続化せず、アプリ起動中だけ[`crate::mcp::ToolCatalog`]に持つ。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub id: String,
    /// サーバー識別子(画面表示名を兼ねる)。[`validate_mcp_server_name`]の制約に従う。
    pub name: String,
    pub enabled: bool,
    pub endpoint: McpEndpoint,
    /// 有効にしたツール名の集合。ここに無い名前は無効で、サーバーが後から足したツールも
    /// 有効にするまで使われない。一覧に無い名前が残っていても、実行時に積集合を取るだけ。
    #[serde(default)]
    pub enabled_tools: BTreeSet<String>,
}

/// サーバー識別子の長さの上限。画面は入力欄の上限と案内文にこの値を使う
/// (`settings::SettingsView`)。
pub const MCP_SERVER_NAME_MAX_CHARS: usize = 16;

/// サーバー識別子を検証する([`MCP_SERVER_NAME_MAX_CHARS`]字以内、英数字とアンダースコア
/// のみ)。英数字だけなので、バイト数と文字数は同じ。
pub fn validate_mcp_server_name(name: &str) -> Result<(), CoreError> {
    if name.is_empty() || name.len() > MCP_SERVER_NAME_MAX_CHARS {
        return Err(CoreError::InvalidSettings(format!(
            "MCP server name must be 1-{MCP_SERVER_NAME_MAX_CHARS} characters"
        )));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(CoreError::InvalidSettings(
            "MCP server name must be alphanumeric or underscore".to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    pub active_provider_id: Option<String>,
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub tools: ToolConfig,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
}

impl Config {
    pub fn active_provider(&self) -> Option<&ProviderConfig> {
        let active_id = self.active_provider_id.as_deref()?;
        self.providers.iter().find(|p| p.id == active_id)
    }

    /// チャットで使うモデルと、その属するプロバイダー。
    pub fn active_model(&self) -> Option<(&ProviderConfig, &ModelConfig)> {
        let provider = self.active_provider()?;
        Some((provider, provider.model(provider.resolved_model()?)?))
    }
}

/// 設定ファイルを読み込む。ファイルが存在しない場合は初回起動として空の設定を返す
/// (プロバイダー未登録のまま起動し、設定画面で登録する)。
pub fn load(path: &Path) -> Result<Config, CoreError> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(|e| CoreError::Config(e.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(CoreError::Config(e.to_string())),
    }
}

/// 設定ファイルを保存する。呼び出し元が親ディレクトリの存在を保証する。
///
/// 書き込み途中で落ちても`config.toml`が壊れないように書く([`crate::files::write_durably`])。
/// 置き換えまで同期するのは、呼び出し元が保存の直後に古い鍵を消すため(置き換えが電源断で
/// 巻き戻ると、設定が消えた鍵を指して残る)。
pub fn save(path: &Path, config: &Config) -> Result<(), CoreError> {
    let text = toml::to_string_pretty(config).map_err(|e| CoreError::Config(e.to_string()))?;
    crate::files::write_durably(path, text.as_bytes()).map_err(|e| CoreError::Config(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_missing_file_returns_default_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = load(&dir.path().join("config.toml")).unwrap();
        assert!(config.providers.is_empty());
        assert!(config.active_provider_id.is_none());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let config = Config {
            providers: vec![ProviderConfig {
                id: "default".to_string(),
                name: "OpenAI".to_string(),
                api_format: ApiFormat::OpenAiCompat,
                base_url: "https://api.openai.com/v1".to_string(),
                models: vec![ModelConfig::new("gpt-4o-mini".to_string())],
                active_model: Some("gpt-4o-mini".to_string()),
                key_ref: Some("provider:default".to_string()),
            }],
            active_provider_id: Some("default".to_string()),
            general: GeneralConfig::default(),
            tools: ToolConfig {
                max_rounds_per_turn: Some(8),
                total_timeout_secs: Some(90),
            },
            mcp_servers: vec![McpServerConfig {
                id: "srv".to_string(),
                name: "my_tools".to_string(),
                enabled: true,
                endpoint: McpEndpoint::Stdio {
                    command: "npx".to_string(),
                    args: vec!["-y".to_string(), "some-server".to_string()],
                    env_refs: vec![SecretRef {
                        name: "API_TOKEN".to_string(),
                        key_ref: "mcp:01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
                    }],
                },
                enabled_tools: BTreeSet::from(["list_things".to_string()]),
            }],
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

        assert_eq!(loaded.tools.max_rounds_per_turn, Some(8));
        assert_eq!(loaded.tools.total_timeout_secs, Some(90));

        assert_eq!(loaded.mcp_servers.len(), 1);
        let server = &loaded.mcp_servers[0];
        assert!(server.enabled_tools.contains("list_things"));
        match &server.endpoint {
            McpEndpoint::Stdio {
                command, env_refs, ..
            } => {
                assert_eq!(command, "npx");
                assert_eq!(env_refs[0].name, "API_TOKEN");
            }
            McpEndpoint::StreamableHttp { .. } => panic!("expected stdio endpoint"),
        }
    }

    #[test]
    fn save_overwrites_existing_file_without_leaving_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        save(&path, &Config::default()).unwrap();
        let config = Config {
            active_provider_id: Some("p".to_string()),
            ..Config::default()
        };
        save(&path, &config).unwrap();

        assert_eq!(
            load(&path).unwrap().active_provider_id.as_deref(),
            Some("p")
        );
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("config.toml")]);
    }

    #[test]
    fn unset_or_zero_response_timeout_falls_back_to_the_default() {
        let general = GeneralConfig {
            response_timeout_secs: Some(0),
            ..GeneralConfig::default()
        };
        assert_eq!(
            general.response_timeout(),
            Duration::from_secs(DEFAULT_RESPONSE_TIMEOUT_SECS)
        );
        assert_eq!(
            GeneralConfig::default().response_timeout(),
            Duration::from_secs(DEFAULT_RESPONSE_TIMEOUT_SECS)
        );
        let general = GeneralConfig {
            response_timeout_secs: Some(30),
            ..GeneralConfig::default()
        };
        assert_eq!(general.response_timeout(), Duration::from_secs(30));
    }

    #[test]
    fn mcp_server_name_validation() {
        assert!(validate_mcp_server_name("my_tools").is_ok());
        assert!(validate_mcp_server_name("").is_err());
        assert!(validate_mcp_server_name("this_name_is_way_too_long").is_err());
        assert!(validate_mcp_server_name("has space").is_err());
        assert!(validate_mcp_server_name("has-dash").is_err());
    }

    #[test]
    fn resolved_model_falls_back_to_first_when_active_is_invalid() {
        let provider = ProviderConfig {
            id: "p".to_string(),
            name: "Local".to_string(),
            api_format: ApiFormat::OpenAiCompat,
            base_url: "http://localhost:1234/v1".to_string(),
            models: vec![
                ModelConfig::new("a".to_string()),
                ModelConfig::new("b".to_string()),
            ],
            active_model: Some("not-in-list".to_string()),
            key_ref: None,
        };
        assert_eq!(provider.resolved_model(), Some("a"));
    }

    #[test]
    fn model_table_roundtrips_and_omitted_fields_take_defaults() {
        let text = r#"
[[providers]]
id = "p"
name = "Local"
api_format = "open_ai_compat"
base_url = "http://localhost:1234/v1"

[[providers.models]]
name = "plain"

[[providers.models]]
name = "tuned"
visible = false
reasoning_effort = "high"

[providers.models.overrides]
tools = false
context_length = 8192
"#;
        let config: Config = toml::from_str(text).unwrap();
        let models = &config.providers[0].models;
        assert_eq!(models[0], ModelConfig::new("plain".to_string()));
        assert!(!models[1].visible);
        assert_eq!(models[1].reasoning_effort, ReasoningEffort::High);
        assert_eq!(models[1].overrides.tools, Some(false));
        assert_eq!(models[1].overrides.image, None);
        assert_eq!(models[1].overrides.context_length, Some(8192));

        let saved = toml::to_string_pretty(&config).unwrap();
        let reloaded: Config = toml::from_str(&saved).unwrap();
        assert_eq!(reloaded.providers[0].models, *models);
    }

    /// モデル名だけを並べた古い形は読まない(起動時の読み込みエラーになる)。
    #[test]
    fn model_names_without_table_are_rejected() {
        let text = r#"
[[providers]]
id = "p"
name = "Local"
api_format = "open_ai_compat"
base_url = "http://localhost:1234/v1"
models = ["a"]
"#;
        assert!(toml::from_str::<Config>(text).is_err());
    }

    /// `task_chat_system_prompt`を含まないTOMLも読める。`#[serde(default)]`の無い`Option`が
    /// 欠損時に`None`になるserde_deriveの挙動に頼っているため、その確認。
    #[test]
    fn load_reads_config_without_task_chat_system_prompt_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
active_provider_id = "default"

[general]
system_prompt = "base"

[[providers]]
id = "default"
name = "OpenAI"
api_format = "open_ai_compat"
base_url = "https://api.openai.com/v1"
active_model = "gpt-4o-mini"

[[providers.models]]
name = "gpt-4o-mini"
"#,
        )
        .unwrap();

        let config = load(&path).unwrap();
        assert_eq!(config.general.system_prompt.as_deref(), Some("base"));
        assert!(config.general.task_chat_system_prompt.is_none());
        assert_eq!(config.general.language(), Language::DEFAULT);
        // `[tools]`節ごと無いTOMLも読める(`#[serde(default)]`)。
        assert!(config.tools.max_rounds_per_turn.is_none());
        assert!(config.tools.total_timeout_secs.is_none());
    }
}
