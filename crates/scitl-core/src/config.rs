//! 秘密情報を含まないプロバイダー設定の永続化(TOML)。architecture.md 6節が定める通り、
//! ここが持つのは`key_ref`という不透明な参照文字列だけで、平文の鍵を持つフィールドは
//! 型として存在させない。実際の鍵の出し入れは[`crate::secrets`]の責務。

use std::collections::BTreeSet;
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
    /// タスクチャットでのみ追加するシステムプロンプト。総合チャット(#43)と
    /// タスクチャットでは公開ツールが異なるため(docs/spec/rebuild/tools.md 5節)、
    /// 工程ツールの使い分けのようなタスクチャット固有の指示は`system_prompt`とは
    /// 別に持つ(docs/spec/legacy/data-model.md 3節「システムプロンプト3種」)。
    pub task_chat_system_prompt: Option<String>,
    /// 応答タイムアウト(秒)。未設定はアダプタ側の既定値を使う。
    pub response_timeout_secs: Option<u64>,
}

/// ツール呼び出しの上限(legacy/frontend.md 4節「共通設定」)。設定画面「ツール/MCP」
/// タブの末尾で編集する。プロバイダーではなくツールの使い方に関する設定なので、
/// `GeneralConfig`ではなく独立した節として持つ。
///
/// どちらも`None`は「未設定」で、既定値の実体は[`crate::orchestration::ToolLimits`]が
/// 1箇所だけ持つ(設定ファイル側に既定値を書き写すと、2箇所を揃える必要が生まれる)。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolConfig {
    /// 1ターンあたりのツール呼び出しラウンド数の上限。
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
/// ツール一覧そのもの(名前・説明)はここに永続化しない。旧実装と同じく画面側で
/// 都度取得する(legacy/frontend.md 4節「未取得時は案内文を表示する」)。永続化すると、
/// 起動のたびに古い一覧と実サーバーの食い違いを気にする必要が生まれるため
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

/// サーバー識別子の制約(legacy/frontend.md 4節: 16字以内、英数字とアンダースコアのみ)。
/// UIでの入力チェックはセキュリティ境界ではないため、Rust側でも検証する。
pub fn validate_mcp_server_name(name: &str) -> Result<(), CoreError> {
    if name.is_empty() || name.len() > 16 {
        return Err(CoreError::Config(
            "MCP server name must be 1-16 characters".to_string(),
        ));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(CoreError::Config(
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

    /// `task_chat_system_prompt`追加前のTOML(このキーを含まない)が引き続き読めることを
    /// 保証する。`Option<T>`フィールドは`#[serde(default)]`が無くても欠損時`None`になる
    /// serde_deriveの挙動に頼っているため、将来型を変える際の回帰検知として残す。
    #[test]
    fn load_reads_config_without_task_chat_system_prompt_key() {
        let dir = tempdir();
        let path = dir.join("config.toml");
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
models = ["gpt-4o-mini"]
active_model = "gpt-4o-mini"
"#,
        )
        .unwrap();

        let config = load(&path).unwrap();
        assert_eq!(config.general.system_prompt.as_deref(), Some("base"));
        assert!(config.general.task_chat_system_prompt.is_none());
        // `[tools]`節ごと無いTOMLも読める(`#[serde(default)]`)。
        assert!(config.tools.max_rounds_per_turn.is_none());
        assert!(config.tools.total_timeout_secs.is_none());
    }
}
