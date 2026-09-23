//! 設定・登録の操作(設定画面の各タブ、Issue #22・#28・#71)。GUIのコマンドもCLIも
//! ここを1つ呼ぶだけにし、登録の規則・入力検証・秘密情報の出し入れ・保存を同じ経路に通す
//! (architecture.md 1節、principles.md 5節)。
//!
//! 変更はすべて[`Draft`]を通る。書き込み同士は`writer`で直列化し、設定の複製を変更→
//! アダプタの組み立て→保存→差し替え、の順で進める。途中で失敗すれば何も差し替えないので、
//! メモリ上の設定とファイルが食い違わない。読み手(ターンの開始)が取る`current`のロックは
//! 差し替えの一瞬だけで、資格情報ストアやファイルのI/Oを待たされない。

pub mod view;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

use crate::config::{
    self, validate_mcp_server_name, ApiFormat, Config, GeneralConfig, McpEndpoint, McpServerConfig,
    ProviderConfig, SecretRef, ToolConfig,
};
use crate::db::error::{CoreError, Result};
use crate::llm::providers::{self, SharedAdapter};
use crate::llm::LlmAdapter;
use crate::mcp::{self, ToolCatalog};
use crate::orchestration::{McpAccess, SystemPrompts, ToolLimits};
use crate::secrets;

pub use view::SettingsView;

/// プロバイダー追加フォームからの入力。
pub struct NewProvider {
    pub name: String,
    pub api_format: ApiFormat,
    pub base_url: String,
    pub api_key: Option<SecretString>,
}

/// サーバー追加フォームからの入力。接続方式ごとに必要な値だけを受け取る
/// (`McpEndpoint`と同じタグ付きenumにすることで、フロントエンドが送る形と
/// Rust側の型を対応させる)。組の2つ目は秘密情報の値で、保存後は`key_ref`に置き換わる。
/// 値を含むため`Debug`は付けない(ログに出す経路を作らない)。
#[derive(Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum NewMcpEndpoint {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: Vec<(String, String)>,
    },
    StreamableHttp {
        url: String,
        #[serde(default)]
        headers: Vec<(String, String)>,
    },
}

#[derive(Clone)]
struct Current {
    config: Arc<Config>,
    /// アクティブなプロバイダーが無い(未登録・全プロバイダーを削除した等)場合は`None`。
    /// この場合、チャット送信はエラー発言(`no_provider`)として保存される。
    adapter: Option<SharedAdapter>,
    /// `adapter`を鍵無しで組み立てた(資格情報ストアから読めなかった)。
    key_unavailable: bool,
}

/// ある時点の設定と、それから作ったアダプタの組。ターンはこれを取ってからロックを離し、
/// ターンに渡す値はここから組み立てる(送信・編集・再試行、GUI・CLIで同じ組み立てを使う)。
pub struct Snapshot {
    pub config: Arc<Config>,
    adapter: Option<SharedAdapter>,
    mcp_tools: Arc<ToolCatalog>,
}

impl Snapshot {
    pub fn adapter(&self) -> Option<&dyn LlmAdapter> {
        self.adapter.as_deref().map(|a| a as &dyn LlmAdapter)
    }

    pub fn prompts(&self) -> SystemPrompts<'_> {
        SystemPrompts {
            base: self.config.general.system_prompt.as_deref(),
            task_chat: self.config.general.task_chat_system_prompt.as_deref(),
        }
    }

    pub fn mcp(&self) -> McpAccess<'_> {
        McpAccess::new(&self.config.mcp_servers, &self.mcp_tools)
    }

    pub fn tool_limits(&self) -> ToolLimits {
        ToolLimits::from_config(&self.config.tools)
    }
}

pub struct Settings {
    path: PathBuf,
    current: Mutex<Current>,
    writer: Mutex<()>,
    /// 取得済みのMCPツール一覧(Issue #104)。アプリ起動中だけ保持するメモリキャッシュで、
    /// config.tomlには書かない(ツール名・説明はユーザーの設定ではなくサーバー側の持ち物で、
    /// 永続化した写しはサーバー側の更新を検知できない)。設定画面の表示と、ターン開始時の
    /// ツール公開(`orchestration::McpAccess`)が同じここを読む。
    mcp_tools: Arc<ToolCatalog>,
    /// [`Self::fetch_mcp_tools`]の同時実行を1サーバーにつき1本に絞る。ボタンの無効化
    /// (連打防止)は画面側の責務だが、それだけでは保証にならない。
    fetching: Mutex<HashSet<String>>,
}

impl Settings {
    /// 設定ファイルを読み、アクティブなプロバイダーのアダプタを組み立てる。ファイルが無ければ
    /// プロバイダー0件で始める。既定の通信先を補わないのは、通信先をユーザーが登録したものに
    /// 限るため(principles.md 1節)。
    pub fn load(path: PathBuf) -> Result<Self> {
        let config = config::load(&path)?;
        let built = providers::build_active_adapter(&config)?;
        Ok(Self {
            path,
            current: Mutex::new(Current {
                config: Arc::new(config),
                adapter: built.adapter,
                key_unavailable: built.key_unavailable,
            }),
            writer: Mutex::new(()),
            mcp_tools: Arc::new(ToolCatalog::new()),
            fetching: Mutex::new(HashSet::new()),
        })
    }

    pub fn snapshot(&self) -> Snapshot {
        let current = self.current();
        Snapshot {
            config: current.config,
            adapter: current.adapter,
            mcp_tools: Arc::clone(&self.mcp_tools),
        }
    }

    pub fn view(&self) -> SettingsView {
        view::build(&self.current().config, &self.mcp_tools)
    }

    fn current(&self) -> Current {
        self.current
            .lock()
            .expect("settings mutex poisoned")
            .clone()
    }

    fn edit(&self) -> Draft<'_> {
        let writer = self.writer.lock().expect("settings writer mutex poisoned");
        let current = self.current();
        Draft {
            settings: self,
            _writer: writer,
            config: (*current.config).clone(),
            before: current.config,
            key_unavailable: current.key_unavailable,
        }
    }

    /// 空文字のプロンプトは未設定として保存する。
    pub fn update_general(
        &self,
        system_prompt: Option<String>,
        task_chat_system_prompt: Option<String>,
        response_timeout_secs: Option<u64>,
    ) -> Result<SettingsView> {
        // `0`は画面側でも弾くが、UIの入力チェックはセキュリティ境界ではない。
        if response_timeout_secs == Some(0) {
            return Err(invalid("response timeout must be 1 second or greater"));
        }
        let mut draft = self.edit();
        draft.config.general = GeneralConfig {
            system_prompt: system_prompt.filter(|s| !s.is_empty()),
            task_chat_system_prompt: task_chat_system_prompt.filter(|s| !s.is_empty()),
            response_timeout_secs,
        };
        draft.commit()
    }

    /// 空欄(`None`)は「未設定」として既定値に戻す。
    pub fn update_tools(
        &self,
        max_rounds_per_turn: Option<u32>,
        total_timeout_secs: Option<u64>,
    ) -> Result<SettingsView> {
        if max_rounds_per_turn == Some(0) {
            return Err(invalid("max rounds per turn must be 1 or greater"));
        }
        if total_timeout_secs == Some(0) {
            return Err(invalid("tool timeout must be 1 second or greater"));
        }
        let mut draft = self.edit();
        draft.config.tools = ToolConfig {
            max_rounds_per_turn,
            total_timeout_secs,
        };
        draft.commit()
    }

    /// 最初に登録したプロバイダーをアクティブにする。鍵の保存に失敗したらプロバイダー自体の
    /// 登録も中断し(legacy/frontend.md 3節)、登録に失敗したら保存した鍵を消す。どちらでも
    /// `key_ref`と鍵の片方だけが残る状態を作らない。
    pub fn add_provider(&self, new: NewProvider) -> Result<SettingsView> {
        let name = new.name.trim().to_string();
        if name.is_empty() {
            return Err(invalid("provider name must not be empty"));
        }
        providers::validate_base_url(new.api_format, &new.base_url)?;

        let key_ref = match new.api_key {
            Some(key) if !key.expose_secret().is_empty() => {
                let key_ref = format!("provider:{}", ulid::Ulid::new());
                secrets::store(&key_ref, &key)?;
                Some(key_ref)
            }
            _ => None,
        };

        let mut draft = self.edit();
        let id = ulid::Ulid::new().to_string();
        if draft.config.active_provider_id.is_none() {
            draft.config.active_provider_id = Some(id.clone());
        }
        draft.config.providers.push(ProviderConfig {
            id,
            name,
            api_format: new.api_format,
            base_url: new.base_url,
            models: Vec::new(),
            active_model: None,
            key_ref: key_ref.clone(),
        });
        draft.commit().inspect_err(|_| {
            if let Some(key_ref) = &key_ref {
                delete_secret(key_ref, "provider API key");
            }
        })
    }

    /// アクティブなプロバイダーを消したら先頭をアクティブにする。保存済みAPIキーも消す
    /// (legacy/frontend.md 3節。削除確認は画面側の責務)。鍵は設定の保存が済んでから消す。
    /// 先に消すと、保存に失敗したときに設定だけが消えた鍵を指して残る。
    pub fn delete_provider(&self, provider_id: &str) -> Result<SettingsView> {
        let mut draft = self.edit();
        let config = &mut draft.config;
        let index = config
            .providers
            .iter()
            .position(|p| p.id == provider_id)
            .ok_or_else(|| provider_not_found(provider_id))?;
        let removed = config.providers.remove(index);
        if config.active_provider_id.as_deref() == Some(provider_id) {
            config.active_provider_id = config.providers.first().map(|p| p.id.clone());
        }

        let view = draft.commit()?;
        if let Some(key_ref) = &removed.key_ref {
            delete_secret(key_ref, "provider API key");
        }
        Ok(view)
    }

    pub fn set_active_provider(&self, provider_id: &str) -> Result<SettingsView> {
        let mut draft = self.edit();
        if !draft.config.providers.iter().any(|p| p.id == provider_id) {
            return Err(provider_not_found(provider_id));
        }
        draft.config.active_provider_id = Some(provider_id.to_string());
        draft.commit()
    }

    /// 最初に登録したモデルをアクティブにする。
    pub fn add_model(&self, provider_id: &str, model: &str) -> Result<SettingsView> {
        let model = model.trim();
        if model.is_empty() {
            return Err(invalid("model name must not be empty"));
        }
        let mut draft = self.edit();
        let provider = find_provider_mut(&mut draft.config, provider_id)?;
        if provider.models.iter().any(|m| m == model) {
            return Err(invalid(format!("model already registered: {model}")));
        }
        provider.models.push(model.to_string());
        if provider.active_model.is_none() {
            provider.active_model = Some(model.to_string());
        }
        draft.commit()
    }

    /// アクティブなモデルを消したら先頭をアクティブにする。
    pub fn remove_model(&self, provider_id: &str, model: &str) -> Result<SettingsView> {
        let mut draft = self.edit();
        let provider = find_provider_mut(&mut draft.config, provider_id)?;
        provider.models.retain(|m| m != model);
        if provider.active_model.as_deref() == Some(model) {
            provider.active_model = provider.models.first().cloned();
        }
        draft.commit()
    }

    pub fn set_active_model(&self, provider_id: &str, model: &str) -> Result<SettingsView> {
        let mut draft = self.edit();
        let provider = find_provider_mut(&mut draft.config, provider_id)?;
        if !provider.models.iter().any(|m| m == model) {
            return Err(invalid(format!("model not registered: {model}")));
        }
        provider.active_model = Some(model.to_string());
        draft.commit()
    }

    /// 検証→重複確認→秘密情報の保存→登録の順。重複確認から登録までを書き込みロックの中で
    /// 行うので、同名の登録が割り込んで秘密情報が孤児になることはない。登録に失敗したら
    /// 保存した秘密情報を消す。
    pub fn add_mcp_server(&self, name: &str, endpoint: NewMcpEndpoint) -> Result<SettingsView> {
        let name = name.trim().to_string();
        validate_mcp_server_name(&name)?;
        let endpoint = validate_endpoint(endpoint)?;

        let mut draft = self.edit();
        if draft.config.mcp_servers.iter().any(|s| s.name == name) {
            return Err(invalid(format!(
                "MCP server name already registered: {name}"
            )));
        }
        let endpoint = store_endpoint_secrets(endpoint)?;
        let refs = endpoint_secret_refs(&endpoint).to_vec();
        draft.config.mcp_servers.push(McpServerConfig {
            id: ulid::Ulid::new().to_string(),
            name,
            enabled: true,
            endpoint,
            enabled_tools: Default::default(),
        });
        draft.commit().inspect_err(|_| delete_secret_refs(&refs))
    }

    /// 保存済みの秘密情報も消す(legacy/frontend.md 4節)。`delete_provider`と同じく、
    /// 設定の保存が済んでから消す。取得済みツール一覧のキャッシュも捨てる(同じIDの
    /// サーバーを登録し直したときに、前のサーバーの一覧が残っていてはならない。Issue #104)。
    pub fn delete_mcp_server(&self, server_id: &str) -> Result<SettingsView> {
        let mut draft = self.edit();
        let index = draft
            .config
            .mcp_servers
            .iter()
            .position(|s| s.id == server_id)
            .ok_or_else(|| mcp_server_not_found(server_id))?;
        let removed = draft.config.mcp_servers.remove(index);

        let view = draft.commit()?;
        delete_secret_refs(endpoint_secret_refs(&removed.endpoint));
        self.mcp_tools.forget(&removed.id);
        Ok(view)
    }

    pub fn set_mcp_server_enabled(&self, server_id: &str, enabled: bool) -> Result<SettingsView> {
        let mut draft = self.edit();
        find_mcp_server_mut(&mut draft.config, server_id)?.enabled = enabled;
        draft.commit()
    }

    pub fn set_mcp_tool_enabled(
        &self,
        server_id: &str,
        tool_name: &str,
        enabled: bool,
    ) -> Result<SettingsView> {
        let mut draft = self.edit();
        let server = find_mcp_server_mut(&mut draft.config, server_id)?;
        if enabled {
            server.enabled_tools.insert(tool_name.to_string());
        } else {
            server.enabled_tools.remove(tool_name);
        }
        draft.commit()
    }

    /// サーバーに接続してツール一覧を取得し、キャッシュへ載せて設定の状態ごと返す
    /// (Issue #104)。config.tomlには書き込まない。ロックはサーバー設定を複製するまで
    /// だけ持ち、接続の`.await`をまたがせない。
    pub async fn fetch_mcp_tools(&self, server_id: &str) -> Result<SettingsView> {
        let _in_flight = InFlight::begin(&self.fetching, server_id)?;
        let server = self
            .current()
            .config
            .mcp_servers
            .iter()
            .find(|s| s.id == server_id)
            .cloned()
            .ok_or_else(|| mcp_server_not_found(server_id))?;

        let tools = mcp::list_tools(&server).await?;
        self.mcp_tools.store(server_id, tools);
        Ok(self.view())
    }
}

/// 書き込みロックを持った設定の複製。[`Self::commit`]せずに落とせば何も変わらない。
struct Draft<'a> {
    settings: &'a Settings,
    _writer: MutexGuard<'a, ()>,
    before: Arc<Config>,
    config: Config,
    key_unavailable: bool,
}

impl Draft<'_> {
    /// アダプタを組み立ててから保存する(組み立てに失敗する設定をファイルへ残さない)。
    /// アダプタは作り直しが要る変更の時だけ組み立てる。MCPのチェック1つの切り替えで
    /// 資格情報ストアを読みに行かない。ただし前回鍵を読めなかった場合は、設定を変えるたびに
    /// 読み直す(ストアのロック解除後に、再起動せずに直るように)。
    fn commit(self) -> Result<SettingsView> {
        let rebuild =
            self.key_unavailable || adapter_inputs(&self.before) != adapter_inputs(&self.config);
        let built = if rebuild {
            Some(providers::build_active_adapter(&self.config)?)
        } else {
            None
        };
        config::save(&self.settings.path, &self.config)?;

        let config = Arc::new(self.config);
        {
            let mut current = self
                .settings
                .current
                .lock()
                .expect("settings mutex poisoned");
            current.config = Arc::clone(&config);
            if let Some(built) = built {
                current.adapter = built.adapter;
                current.key_unavailable = built.key_unavailable;
            }
        }
        Ok(view::build(&config, &self.settings.mcp_tools))
    }
}

/// アダプタの組み立てに使う設定値。`providers::build_active_adapter`が読むものと揃える。
fn adapter_inputs(config: &Config) -> (Option<&ProviderConfig>, Option<std::time::Duration>) {
    (config.active_provider(), config.general.response_timeout())
}

struct InFlight<'a> {
    set: &'a Mutex<HashSet<String>>,
    key: String,
}

impl<'a> InFlight<'a> {
    fn begin(set: &'a Mutex<HashSet<String>>, key: &str) -> Result<Self> {
        if !set.lock().expect("mutex poisoned").insert(key.to_string()) {
            return Err(invalid("already fetching tools for this server"));
        }
        Ok(Self {
            set,
            key: key.to_string(),
        })
    }
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.set.lock().expect("mutex poisoned").remove(&self.key);
    }
}

/// 秘密情報に触れる前に済ませられる検証をすべて行う。
fn validate_endpoint(endpoint: NewMcpEndpoint) -> Result<NewMcpEndpoint> {
    match endpoint {
        NewMcpEndpoint::Stdio { command, args, env } => {
            let command = command.trim().to_string();
            if command.is_empty() {
                return Err(invalid("command must not be empty"));
            }
            Ok(NewMcpEndpoint::Stdio { command, args, env })
        }
        NewMcpEndpoint::StreamableHttp { url, headers } => {
            mcp::validate_streamable_http_url(&url)?;
            for (name, value) in &headers {
                mcp::validate_header_name(name)?;
                mcp::validate_header_value(value)?;
            }
            Ok(NewMcpEndpoint::StreamableHttp { url, headers })
        }
    }
}

fn store_endpoint_secrets(endpoint: NewMcpEndpoint) -> Result<McpEndpoint> {
    Ok(match endpoint {
        NewMcpEndpoint::Stdio { command, args, env } => McpEndpoint::Stdio {
            command,
            args,
            env_refs: store_secret_refs(env)?,
        },
        NewMcpEndpoint::StreamableHttp { url, headers } => McpEndpoint::StreamableHttp {
            url,
            header_refs: store_secret_refs(headers)?,
        },
    })
}

/// 秘密情報の値を保存し、`(name, key_ref)`の組に変換する。途中で失敗したら
/// それまでに保存した分を削除してからエラーを返す(孤児を残さない)。
fn store_secret_refs(pairs: Vec<(String, String)>) -> Result<Vec<SecretRef>> {
    let mut refs = Vec::with_capacity(pairs.len());
    for (name, value) in pairs {
        let key_ref = format!("mcp:{}", ulid::Ulid::new());
        if let Err(e) = secrets::store(&key_ref, &SecretString::from(value)) {
            delete_secret_refs(&refs);
            return Err(e);
        }
        refs.push(SecretRef { name, key_ref });
    }
    Ok(refs)
}

/// 1件が失敗しても残りは試す。
fn delete_secret_refs(refs: &[SecretRef]) {
    for r in refs {
        delete_secret(&r.key_ref, &format!("MCP secret '{}'", r.name));
    }
}

/// 削除の失敗は操作全体を失敗させない(設定からは既に外れており、残るのは参照されない
/// 鍵だけ)。
fn delete_secret(key_ref: &str, what: &str) {
    if let Err(e) = secrets::delete(key_ref) {
        eprintln!("failed to delete {what} from secret store: {e}");
    }
}

fn endpoint_secret_refs(endpoint: &McpEndpoint) -> &[SecretRef] {
    match endpoint {
        McpEndpoint::Stdio { env_refs, .. } => env_refs,
        McpEndpoint::StreamableHttp { header_refs, .. } => header_refs,
    }
}

fn find_provider_mut<'a>(
    config: &'a mut Config,
    provider_id: &str,
) -> Result<&'a mut ProviderConfig> {
    config
        .providers
        .iter_mut()
        .find(|p| p.id == provider_id)
        .ok_or_else(|| provider_not_found(provider_id))
}

fn find_mcp_server_mut<'a>(
    config: &'a mut Config,
    server_id: &str,
) -> Result<&'a mut McpServerConfig> {
    config
        .mcp_servers
        .iter_mut()
        .find(|s| s.id == server_id)
        .ok_or_else(|| mcp_server_not_found(server_id))
}

fn provider_not_found(provider_id: &str) -> CoreError {
    invalid(format!("provider not found: {provider_id}"))
}

fn mcp_server_not_found(server_id: &str) -> CoreError {
    invalid(format!("MCP server not found: {server_id}"))
}

fn invalid(message: impl Into<String>) -> CoreError {
    CoreError::InvalidSettings(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    // 鍵を渡さない操作だけを試す(資格情報ストアに触れない)。

    fn temp_settings() -> (Settings, PathBuf) {
        let dir = std::env::temp_dir().join(format!("scitl-settings-test-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        (Settings::load(path.clone()).unwrap(), path)
    }

    fn add_local_provider(settings: &Settings, name: &str) -> SettingsView {
        settings
            .add_provider(NewProvider {
                name: name.to_string(),
                api_format: ApiFormat::OpenAiCompat,
                base_url: "http://localhost:1234/v1".to_string(),
                api_key: None,
            })
            .unwrap()
    }

    #[test]
    fn first_provider_and_model_become_active_and_are_saved() {
        let (settings, path) = temp_settings();
        let view = add_local_provider(&settings, "Local");
        let id = view.providers[0].id.clone();
        assert_eq!(view.active_provider_id.as_deref(), Some(id.as_str()));
        assert!(settings.snapshot().adapter().is_some());

        let view = settings.add_model(&id, " m1 ").unwrap();
        assert_eq!(view.providers[0].active_model.as_deref(), Some("m1"));
        let view = settings.add_model(&id, "m2").unwrap();
        assert_eq!(
            view.providers[0].active_model.as_deref(),
            Some("m1"),
            "2つ目のモデルでアクティブは変わらない"
        );

        let reloaded = config::load(&path).unwrap();
        assert_eq!(reloaded.providers[0].models, vec!["m1", "m2"]);
    }

    #[test]
    fn deleting_active_provider_activates_first_remaining() {
        let (settings, _) = temp_settings();
        let first = add_local_provider(&settings, "A").providers[0].id.clone();
        let second = add_local_provider(&settings, "B").providers[1].id.clone();

        let view = settings.delete_provider(&first).unwrap();
        assert_eq!(view.active_provider_id.as_deref(), Some(second.as_str()));
        let view = settings.delete_provider(&second).unwrap();
        assert_eq!(view.active_provider_id, None);
        assert!(settings.snapshot().adapter().is_none());
    }

    #[test]
    fn removing_active_model_falls_back_to_first() {
        let (settings, _) = temp_settings();
        let id = add_local_provider(&settings, "A").providers[0].id.clone();
        settings.add_model(&id, "m1").unwrap();
        settings.add_model(&id, "m2").unwrap();
        settings.set_active_model(&id, "m2").unwrap();

        let view = settings.remove_model(&id, "m2").unwrap();
        assert_eq!(view.providers[0].active_model.as_deref(), Some("m1"));
    }

    #[test]
    fn rejects_zero_limits_and_timeout() {
        let (settings, _) = temp_settings();
        assert!(settings.update_general(None, None, Some(0)).is_err());
        assert!(settings.update_tools(Some(0), None).is_err());
        assert!(settings.update_tools(None, Some(0)).is_err());
    }

    #[test]
    fn failed_save_leaves_current_settings_unchanged() {
        let (settings, path) = temp_settings();
        // 保存先をディレクトリにして書き込みを失敗させる。
        std::fs::create_dir_all(&path).unwrap();
        assert!(settings
            .update_general(Some("prompt".to_string()), None, None)
            .is_err());
        assert!(settings.current().config.general.system_prompt.is_none());
    }

    #[test]
    fn mcp_server_name_must_be_unique() {
        let (settings, _) = temp_settings();
        let endpoint = || NewMcpEndpoint::Stdio {
            command: "npx".to_string(),
            args: Vec::new(),
            env: Vec::new(),
        };
        settings.add_mcp_server("tools", endpoint()).unwrap();
        let err = settings.add_mcp_server("tools", endpoint()).unwrap_err();
        assert!(matches!(err, CoreError::InvalidSettings(_)));
    }

    #[test]
    fn adapter_is_rebuilt_only_when_its_inputs_change() {
        let (settings, _) = temp_settings();
        let id = add_local_provider(&settings, "A").providers[0].id.clone();
        let before = settings.current().adapter.unwrap();

        settings
            .add_mcp_server(
                "tools",
                NewMcpEndpoint::Stdio {
                    command: "npx".to_string(),
                    args: Vec::new(),
                    env: Vec::new(),
                },
            )
            .unwrap();
        assert!(Arc::ptr_eq(&before, &settings.current().adapter.unwrap()));

        settings.add_model(&id, "m1").unwrap();
        assert!(!Arc::ptr_eq(&before, &settings.current().adapter.unwrap()));
    }
}
