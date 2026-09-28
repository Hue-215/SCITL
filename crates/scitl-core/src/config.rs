//! 秘密情報を含まないプロバイダー設定の永続化(TOML)。architecture.md 6節が定める通り、
//! ここが持つのは`key_ref`という不透明な参照文字列だけで、平文の鍵を持つフィールドは
//! 型として存在させない。実際の鍵の出し入れは[`crate::secrets`]の責務。

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::i18n::Language;

/// 対応するプロバイダーAPIの方言。現状はOpenAI互換チャットコンプリーションAPIのみ
/// (architecture.md 2節)。将来プロバイダーを追加する際はここにバリアントを足す。
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
    /// 登録したモデルの一覧(設定画面「APIプロバイダー」タブ)。名前を打って1件ずつ、または
    /// プロバイダーから取得した一覧から選んで登録する(一括で登録しない理由はarchitecture.md 3節)。
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

/// 登録済みの1モデル(Issue #65)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelConfig {
    pub name: String,
    /// チャットのモデル一覧に出すか。
    #[serde(default = "visible_by_default")]
    pub visible: bool,
    /// 能力の手動設定。能力を3層で解決するうちの一番上の層で、値の無い項目は下の層で決まる
    /// (principles.md 3節、[`crate::llm::resolve_capabilities`])。
    #[serde(default, skip_serializing_if = "ModelOverrides::is_empty")]
    pub overrides: ModelOverrides,
    /// 思考の強さ(チャット入力欄の下で選ぶ。Issue #64)。思考に対応するモデルには常に
    /// 明示して送り、サーバーの既定には任せない。モデルごとに持つのは、受け付ける値が
    /// モデルごとに違うため(あるモデルに合わせた値を、切り替えた先のモデルへ持ち込まない)。
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelOverrides {
    pub image: Option<bool>,
    pub tools: Option<bool>,
    pub thinking: Option<bool>,
    pub context_length: Option<u32>,
}

impl ModelOverrides {
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
}

/// 応答タイムアウトの既定値(秒)。未設定のときに使う実体はここ1箇所だけ。
/// タイムアウト自体は常に掛ける(HTTPクライアントの既定は無制限で、応答しない
/// エンドポイント1つでターンが永久に固まるため)。生成の長い非ストリーミング応答も
/// 待てるよう、余裕を持たせる。
pub const DEFAULT_RESPONSE_TIMEOUT_SECS: u64 = 120;

/// システムプロンプト等、モデル・プロバイダーに依存しない全般設定
/// (legacy/frontend.md 2節)。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GeneralConfig {
    pub system_prompt: Option<String>,
    /// タスクチャットでのみ追加するシステムプロンプト。総合チャット(#43)と
    /// タスクチャットでは公開ツールが異なるため(docs/spec/rebuild/tools.md 5節)、
    /// 工程ツールの使い分けのようなタスクチャット固有の指示は`system_prompt`とは
    /// 別に持つ(docs/spec/legacy/data-model.md 3節「システムプロンプト3種」)。
    /// 未設定は表示言語の既定の文面で、解釈は`orchestration::SystemPrompts::from_config`に
    /// 閉じる。
    pub task_chat_system_prompt: Option<String>,
    /// 新規タスクで聞き取りを始めるとき、ユーザーの代わりに送る発言(Issue #76)。未設定は
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
    /// 未設定(`None`)と、保存済みの設定に紛れ込んだ`0`はどちらも既定値。`0`は
    /// 「即タイムアウト」ではなく設定の不備として扱う。更新時にも弾くが、手で編集した
    /// `config.toml`が同じ経路を通るため、ここでも受け止める
    /// (`orchestration::ToolLimits::from_config`と同じ扱い)。
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

/// ツール呼び出しの上限(legacy/frontend.md 4節「共通設定」)。設定画面「ツール/MCP」
/// タブの末尾で編集する。プロバイダーではなくツールの使い方に関する設定なので、
/// `GeneralConfig`ではなく独立した節として持つ。
///
/// どちらも`None`は「未設定」で、既定値の実体は[`crate::orchestration::ToolLimits`]が
/// 1箇所だけ持つ(設定ファイル側に既定値を書き写すと、2箇所を揃える必要が生まれる)。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolConfig {
    /// 1ターンでツールを実行するラウンドの上限。使い切ったら、ツールを渡さずにもう一度だけ
    /// モデルを呼んで返信させる(`orchestration::turn`)。
    pub max_rounds_per_turn: Option<u32>,
    /// 1ターン内のツール実行に使える時間の合計(秒)。LLMの応答待ちは含まない
    /// (そちらは`GeneralConfig::response_timeout_secs`が見る)。
    pub total_timeout_secs: Option<u64>,
}

/// [`crate::secrets`]に保存した1つの値(環境変数またはHTTPヘッダーの値)を指す参照。
/// `name`(環境変数名/ヘッダー名)と`key_ref`(秘密情報ストア上の不透明な参照)は別物であり、
/// `key_ref`は`name`から機械的に導出しない(`name`はユーザー入力で
/// `:`等を含みうるため、そこから`key_ref`を組み立てると衝突・曖昧さの元になる。
/// `provider:{ULID}`と同様、`key_ref`はULIDで払い出す)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretRef {
    pub name: String,
    pub key_ref: String,
}

/// MCPサーバーへの接続方式。フィールドの組み合わせを型で保証するため、
/// (transport種別, command, url)のような別々のフィールドに分けず、
/// タグ付きenumとして接続方式ごとに必要な値だけを持たせる
/// (`ProviderConfig`のような平坦な構造だと、`Stdio`なのに`url`が入っている
/// といった不正な状態を型で防げない)。
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

/// 1つの外部ツールサーバー(MCP)設定。秘密情報を含まない(architecture.md 6節)。
/// ツール一覧そのもの(名前・説明)はここに永続化せず、アプリ起動中だけ
/// [`crate::mcp::ToolCatalog`]に持つ。永続化すると、起動のたびに古い一覧と実サーバーの
/// 食い違いを気にする必要が生まれるため
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub id: String,
    /// サーバー識別子(画面表示名を兼ねる)。[`validate_mcp_server_name`]の制約に従う。
    pub name: String,
    pub enabled: bool,
    pub endpoint: McpEndpoint,
    /// 有効なツール名の集合(opt-in)。ここに無い名前は無効として扱う。取得したツール
    /// 一覧に無い名前が残っていても実害はない(実行時に積集合を取るだけ)。逆に、
    /// サーバー側が後からツールを追加しても、ユーザーが明示的に有効化するまで
    /// 使われない(`HashMap<String, bool>`による「既定で有効」の読み方を型で排除する)。
    #[serde(default)]
    pub enabled_tools: BTreeSet<String>,
}

/// サーバー識別子の長さの上限(legacy/frontend.md 4節)。画面は入力欄の上限と案内文にこの値を
/// 使う(`settings::SettingsView`)。
pub const MCP_SERVER_NAME_MAX_CHARS: usize = 16;

/// サーバー識別子の制約(legacy/frontend.md 4節: [`MCP_SERVER_NAME_MAX_CHARS`]字以内、
/// 英数字とアンダースコアのみ)。UIでの入力チェックはセキュリティ境界ではないため、
/// Rust側でも検証する。英数字だけなので、バイト数と文字数は同じ。
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
/// 起動時の読み込みエラーは設定画面から直せない。置き換えまで同期するのは、呼び出し元が
/// 保存の直後に古い設定だけが参照していた秘密情報を消すため(置き換えが電源断で巻き戻ると、
/// 設定が消えた鍵を指して残る)。
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

    /// モデル名だけを並べた形(Issue #65より前)は読まない。起動時の読み込みエラー
    /// (`settings`モジュール冒頭)として扱われる。
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

    /// `task_chat_system_prompt`追加前のTOML(このキーを含まない)が引き続き読めることを
    /// 保証する。`Option<T>`フィールドは`#[serde(default)]`が無くても欠損時`None`になる
    /// serde_deriveの挙動に頼っているため、将来型を変える際の回帰検知として残す。
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
