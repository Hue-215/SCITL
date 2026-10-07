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
    Anthropic,
    Gemini,
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
    /// リクエストに添えるカスタムHTTPヘッダー。値は[`crate::secrets`]に置く(MCPサーバーの
    /// ヘッダーと同じ扱い)。値の中の`{session_id}`は送るときに置き換わる
    /// (`llm::providers::SESSION_ID_PLACEHOLDER`)。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub header_refs: Vec<SecretRef>,
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
    /// 思考の強さ。思考に対応するモデルには常に明示して送り、サーバーの既定には任せない。
    /// 受け付ける値がモデルごとに違うため、モデルごとに持つ。
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

/// 応答タイムアウトの既定値(秒)。未設定のときに使う値はここにだけ置く。生成の
/// 長い非ストリーミング応答も待てるよう、余裕を持たせる。ストリーミングで読む方言では、
/// 全体ではなくデータの届かない時間の上限になる(`net::RequestTimeout`)。タイムアウト自体は常に掛ける
/// (応答しないエンドポイント1つでターンが固まらないように)。
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
    /// 表示言語のコード([`Language::code`])。未設定と知らない値は[`Language::DEFAULT`]で、
    /// 値の解釈は[`Self::language`]に閉じる。知らない値(打ち間違い、別の版が書いた言語)も
    /// 書かれたまま持ち、言語を選び直すまでファイルから消さない。
    pub language: Option<String>,
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
        self.language
            .as_deref()
            .and_then(Language::from_code)
            .unwrap_or(Language::DEFAULT)
    }

    /// 設定に書かれているが使えない表示言語の値。
    pub fn unknown_language(&self) -> Option<&str> {
        self.language
            .as_deref()
            .filter(|code| Language::from_code(code).is_none())
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

/// [`crate::secrets`]に保存した1つの値(HTTPヘッダーの値)を指す参照。
/// `key_ref`はULIDで払い出し、`name`からは組み立てない(`name`はユーザー入力で、`:`等を
/// 含んで衝突しうるため)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRef {
    pub name: String,
    pub key_ref: String,
}

/// MCPサーバーへの接続方式。設定ファイルでは`transport`の値で見分ける。接続方式ごとに
/// 必要な値だけを持たせる。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum McpEndpoint {
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
/// のみ)。画面の入力チェックはセキュリティ境界ではないので、ここでも検証する。
pub fn validate_mcp_server_name(name: &str) -> Result<(), CoreError> {
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(CoreError::InvalidSettings(
            "MCP server name must be alphanumeric or underscore".to_string(),
        ));
    }
    // 文字種を先に確かめてあるので、バイト数と文字数は同じ。
    if name.is_empty() || name.len() > MCP_SERVER_NAME_MAX_CHARS {
        return Err(CoreError::InvalidSettings(format!(
            "MCP server name must be 1-{MCP_SERVER_NAME_MAX_CHARS} characters"
        )));
    }
    // ツールの公開名は`<サーバー名>__<ツール名>`(`tools::external::exposed_name`)。サーバー名に
    // `__`や末尾の`_`があると、別々のツールが同じ公開名になる(サーバー`a`のツール`b__c`と、
    // サーバー`a__b`のツール`c`)。先頭の`_`は衝突しないが、案内を簡単にするため揃えて断る。
    if name.contains("__") || name.starts_with('_') || name.ends_with('_') {
        return Err(CoreError::InvalidSettings(
            "MCP server name must not contain \"__\" or start or end with an underscore"
                .to_string(),
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

    /// アクティブなプロバイダーで使えるモデルが無ければ、モデルのある先頭のプロバイダーを
    /// アクティブにする(登録・削除のたびに呼ぶ)。どのプロバイダーにもモデルが無ければ、
    /// 今のプロバイダーのままにし、それも無ければ先頭のプロバイダーにする。
    pub fn reselect_active_provider(&mut self) {
        if self.active_model().is_some() {
            return;
        }
        let next = self
            .providers
            .iter()
            .find(|p| p.resolved_model().is_some())
            .or(self.active_provider())
            .or(self.providers.first())
            .map(|p| p.id.clone());
        self.active_provider_id = next;
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
                header_refs: Vec::new(),
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
                endpoint: McpEndpoint::StreamableHttp {
                    url: "http://127.0.0.1:8000/mcp".to_string(),
                    header_refs: vec![SecretRef {
                        name: "Authorization".to_string(),
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
        let McpEndpoint::StreamableHttp { url, header_refs } = &server.endpoint;
        assert_eq!(url, "http://127.0.0.1:8000/mcp");
        assert_eq!(header_refs[0].name, "Authorization");
    }

    /// 外部ツールサーバーを子プロセスとして起動する方式(stdio)は持たない。書かれた設定ファイルは
    /// 読めない設定として扱う(`docs/spec/tools.md`「外部(MCP)ツールの公開」)。
    #[test]
    fn stdio_servers_are_rejected() {
        let text = r#"
[[mcp_servers]]
id = "srv"
name = "files"
enabled = true

[mcp_servers.endpoint]
transport = "stdio"
command = "npx"
"#;
        assert!(toml::from_str::<Config>(text).is_err());
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
        assert!(validate_mcp_server_name("a__b").is_err());
        assert!(validate_mcp_server_name("_a").is_err());
        assert!(validate_mcp_server_name("a_").is_err());

        // 使えない文字は、バイト数が上限を超えていても文字種の理由で断る。
        let err = validate_mcp_server_name("あいうえおか").unwrap_err();
        assert!(err.to_string().contains("alphanumeric"), "{err}");
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
            header_refs: Vec::new(),
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
        assert_eq!(models[1].overrides.image, None);
        assert_eq!(models[1].overrides.context_length, Some(8192));

        // 手動設定に書かれた`tools`は読み飛ばし、次の保存で消える。
        let saved = toml::to_string_pretty(&config).unwrap();
        let saved_overrides = &toml::from_str::<toml::Value>(&saved).unwrap()["providers"][0]
            ["models"][1]["overrides"];
        assert!(saved_overrides.get("tools").is_none());
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

    /// 知らない表示言語は既定の言語として読み、ほかの設定を変えて保存しても書かれた値を残す。
    #[test]
    fn unknown_language_falls_back_to_the_default_and_survives_a_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[general]\nlanguage = \"jp\"\n").unwrap();

        let config = load(&path).unwrap();
        assert_eq!(config.general.language(), Language::DEFAULT);
        assert_eq!(config.general.unknown_language(), Some("jp"));

        save(&path, &config).unwrap();
        assert_eq!(load(&path).unwrap().general.language.as_deref(), Some("jp"));

        let known = GeneralConfig {
            language: Some("en".to_string()),
            ..GeneralConfig::default()
        };
        assert_eq!(known.language(), Language::En);
        assert_eq!(known.unknown_language(), None);
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
