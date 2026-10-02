//! 設定・登録の操作(設定画面の各タブ)。GUIのコマンドもCLIもここを1つ呼ぶだけにし、登録の
//! 規則・入力検証・秘密情報の出し入れ・保存を同じ経路に通す。
//!
//! 変更はすべて[`Draft`]を通る。書き込み同士は`writer`で直列化し、設定の複製を変更→
//! アダプタの組み立て→保存→差し替え、の順で進める。途中で失敗すれば何も差し替えないので、
//! メモリ上の設定とファイルが食い違わない。読み手(ターンの開始)が取る`current`のロックは
//! 差し替えの一瞬だけで、資格情報ストアやファイルのI/Oを待たされない。
//!
//! 設定に問題があっても起動は止めない(画面から直す手段が無くなるため)。設定ファイルを
//! 読めなければ空の設定で動かし、読めなかったファイルを上書きしないよう保存を断る。
//! アクティブなプロバイダーを組み立てられなければ、そのプロバイダーを使えないものとして
//! 動かし、削除・切り替えで直せるようにする。どちらも理由を設定画面に出し、チャットでは
//! 理由に応じたエラー発言にする。

mod input;
pub mod view;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

use crate::attachments::Attachments;
use crate::config::{
    self, validate_mcp_server_name, ApiFormat, Capability, Config, McpEndpoint, McpServerConfig,
    ModelConfig, ModelOverrides, ProviderConfig, ReasoningEffort, SecretRef, ToolConfig,
};
use crate::db::messages::Chat;
use crate::error::{CoreError, Result};
use crate::i18n::Language;
use crate::in_flight::InFlightSet;
use crate::llm::providers::{self, SharedAdapter};
use crate::llm::{self, DetectedCatalog, LlmAdapter, ModelCapabilities};
use crate::mcp::{self, ToolCatalog};
use crate::orchestration::{
    self, default_opening_message, default_task_chat_prompt, stored_prompt, McpAccess,
    SystemPrompts, ToolLimits, TurnContext, TurnEvents, TurnFailure,
};
use crate::secrets;
use crate::tools::external;

pub use view::{AvailableModel, ChatModelsView, SettingsView};

/// プロバイダー追加フォームからの入力。
pub struct NewProvider {
    pub name: String,
    pub api_format: ApiFormat,
    pub base_url: String,
    pub api_key: Option<SecretString>,
}

/// 設定画面「一般」タブの入力(表示言語を除く)。プロンプトは`Option<String>`が並ぶので、
/// 位置引数にせず名前で渡す。
pub struct GeneralUpdate {
    pub system_prompt: Option<String>,
    pub task_chat_system_prompt: Option<String>,
    pub task_opening_message: Option<String>,
    pub response_timeout_secs: Option<u64>,
}

/// サーバー追加フォームからの入力。`McpEndpoint`と同じく、接続方式ごとに必要な値だけを
/// 受け取る。組の2つ目は秘密情報の値で、保存後は`key_ref`に置き換わる。値を含むため
/// `Debug`は付けない(ログに出す経路を作らない)。
#[derive(Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum NewMcpEndpoint {
    StreamableHttp {
        url: String,
        #[serde(default)]
        #[cfg_attr(test, ts(type = "Array<[string, string]>"))]
        headers: Vec<(String, SecretString)>,
    },
}

#[derive(Clone)]
struct Current {
    config: Arc<Config>,
    adapter: AdapterState,
}

/// アクティブなプロバイダーのアダプタ。
#[derive(Clone)]
enum AdapterState {
    Ready(SharedAdapter),
    /// アクティブなプロバイダーが無い(未登録・全プロバイダーを削除した等)。
    NoProvider,
    /// アクティブなプロバイダーの鍵を資格情報ストアから読めない。理由を持つ。
    /// ターンの開始と設定の変更のたびに読み直す。
    KeyUnavailable(String),
    /// アクティブなプロバイダーを組み立てられない。理由を持つ。
    Broken(String),
}

impl AdapterState {
    fn broken_reason(&self) -> Option<&str> {
        match self {
            Self::Broken(reason) => Some(reason),
            Self::Ready(_) | Self::NoProvider | Self::KeyUnavailable(_) => None,
        }
    }

    fn key_error(&self) -> Option<&str> {
        match self {
            Self::KeyUnavailable(reason) => Some(reason),
            Self::Ready(_) | Self::NoProvider | Self::Broken(_) => None,
        }
    }
}

/// 組み立ての結果を状態に直す。
fn adapter_state(built: Result<providers::ActiveAdapter>) -> AdapterState {
    match built {
        Ok(providers::ActiveAdapter::Ready(adapter)) => AdapterState::Ready(adapter),
        Ok(providers::ActiveAdapter::NoProvider) => AdapterState::NoProvider,
        Ok(providers::ActiveAdapter::KeyUnavailable(reason)) => {
            AdapterState::KeyUnavailable(reason)
        }
        Err(e) => AdapterState::Broken(e.to_string()),
    }
}

/// ある時点の設定と、それから作ったアダプタの組。ターンはこれを取ってからロックを離し、
/// ターンに渡す値はここから組み立てる(送信・編集・再試行、GUI・CLIで同じ組み立てを使う)。
pub struct Snapshot {
    pub config: Arc<Config>,
    /// 使えるアダプタ、または使えない理由(ターンはこの理由のエラー発言で終わる)。
    adapter: std::result::Result<SharedAdapter, TurnFailure>,
    /// アクティブなモデルの能力(3層で解決済み)。
    capabilities: ModelCapabilities,
    mcp_tools: Arc<ToolCatalog>,
}

impl Snapshot {
    pub fn turn_context<'a>(
        &'a self,
        generating: &'a InFlightSet<Chat>,
        attachments: &'a Attachments,
        events: TurnEvents<'a>,
    ) -> TurnContext<'a> {
        TurnContext {
            adapter: match &self.adapter {
                Ok(adapter) => Ok(adapter.as_ref() as &dyn LlmAdapter),
                Err(failure) => Err(failure.clone()),
            },
            prompts: SystemPrompts::from_config(&self.config.general),
            opening_message: orchestration::opening_message(&self.config.general),
            capabilities: self.capabilities,
            reasoning_effort: self
                .config
                .active_model()
                .map(|(_, model)| model.reasoning_effort)
                .filter(|_| self.capabilities.thinking),
            mcp: McpAccess::new(&self.config.mcp_servers, &self.mcp_tools),
            limits: ToolLimits::from_config(&self.config.tools),
            generating,
            attachments,
            events,
        }
    }
}

pub struct Settings {
    path: PathBuf,
    /// 起動時に設定ファイルを読めなかった理由(パスを含む)。あれば保存を断る。直すには
    /// ファイルを直して再起動する。
    config_error: Option<String>,
    current: Mutex<Current>,
    writer: Mutex<()>,
    /// 取得済みのMCPツール一覧([`crate::mcp::ToolCatalog`])。
    mcp_tools: Arc<ToolCatalog>,
    /// [`Self::fetch_mcp_tools`]の同時実行を1サーバーにつき1本に絞る。
    fetching: InFlightSet<String>,
    /// モデル能力の自動検出の結果([`DetectedCatalog`])。
    detected: DetectedCatalog,
    /// [`Self::detect_model_capabilities`]の同時実行を1プロバイダーにつき1本に絞る。
    detecting: InFlightSet<String>,
    /// [`Self::reload_unavailable_key`]を1本ずつにする(同時に始まったターンが、ロックの
    /// 解除を求める承認を重ねて出さないように)。
    reloading_key: tokio::sync::Mutex<()>,
}

impl Settings {
    /// 設定ファイルを読み、アクティブなプロバイダーのアダプタを組み立てる。ファイルが
    /// 無ければプロバイダー0件で始める(通信先はユーザーが登録したものに限る)。読めない・
    /// 組み立てられない場合も失敗にはしない。
    pub fn load(path: PathBuf) -> Self {
        let (config, config_error) = match config::load(&path) {
            Ok(config) => (config, None),
            Err(e) => {
                let reason = format!("{}: {e}", path.display());
                crate::diagnostics::report(format_args!(
                    "failed to read the config file, starting with empty settings: {reason}"
                ));
                (Config::default(), Some(reason))
            }
        };
        let adapter = adapter_state(providers::build_active_adapter(&config));
        if let Some(reason) = adapter.broken_reason() {
            crate::diagnostics::report(format_args!(
                "the active provider cannot be used: {reason}"
            ));
        }
        Self {
            path,
            config_error,
            current: Mutex::new(Current {
                config: Arc::new(config),
                adapter,
            }),
            writer: Mutex::new(()),
            mcp_tools: Arc::new(ToolCatalog::new()),
            fetching: InFlightSet::new(),
            detected: DetectedCatalog::new(),
            detecting: InFlightSet::new(),
            reloading_key: tokio::sync::Mutex::new(()),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let current = self.current();
        let adapter = match (&self.config_error, current.adapter) {
            (Some(_), _) => Err(TurnFailure::SettingsUnreadable),
            (None, AdapterState::Ready(adapter)) => Ok(adapter),
            (None, AdapterState::NoProvider) => Err(TurnFailure::NoProvider),
            (None, AdapterState::KeyUnavailable(_)) => Err(TurnFailure::KeyUnavailable),
            (None, AdapterState::Broken(_)) => Err(TurnFailure::ProviderConfig),
        };
        let capabilities = self.active_model_capabilities(&current.config);
        Snapshot {
            config: current.config,
            adapter,
            capabilities,
            mcp_tools: Arc::clone(&self.mcp_tools),
        }
    }

    /// ターンの開始に使う[`Self::snapshot`]。鍵を読めていなければ読み直し
    /// ([`Self::reload_unavailable_key`])、アクティブなモデルの能力をまだ推論サーバーに
    /// 問い合わせていなければ、先に問い合わせる([`Self::detect_active_model_once`])。
    ///
    /// 問い合わせに失敗してもターンは止めない。サーバーに繋がらないならターン自体が
    /// 失敗して理由がエラー発言に残り、繋がるなら既定値の層で進められるため。
    pub async fn snapshot_for_turn(&self) -> Snapshot {
        self.reload_unavailable_key().await;
        self.detect_active_model_once().await;
        self.snapshot()
    }

    /// 鍵を読めずにいたら、資格情報ストアから読み直してアダプタを組み立て直す。GUIは
    /// 起動したまま使い続けるので、ここで読み直さないと、設定を変えるまで直らない。
    ///
    /// 読み直しの間は設定の書き込みロックを持たない(ロックの解除を求める承認で止まっている
    /// 間、設定画面まで止めないため)。その間に設定が変わっていれば、結果は捨てる(変えた側が
    /// 組み立て直している)。
    async fn reload_unavailable_key(&self) {
        let _reloading = self.reloading_key.lock().await;
        let before = self.current();
        if before.adapter.key_error().is_none() {
            return;
        }
        let config = Arc::clone(&before.config);
        let built = crate::blocking::run(move || providers::build_active_adapter(&config)).await;
        let mut current = self.current.lock().expect("settings mutex poisoned");
        if Arc::ptr_eq(&current.config, &before.config) {
            current.adapter = adapter_state(built);
        }
    }

    /// チャット入力欄の下のモデル選択。思考の強さを選べるかは能力で決まるので、
    /// ターンの開始と同じく、アクティブなモデルを先に問い合わせる。失敗したら自動検出より
    /// 下の層の値で出す。
    pub async fn chat_models(&self) -> ChatModelsView {
        self.detect_active_model_once().await;
        view::chat_models(&self.current().config, &self.detected)
    }

    /// アクティブなモデルの能力を、まだ推論サーバーに問い合わせていなければ問い合わせる
    /// (アプリ起動後、モデルごとに最初の1回)。失敗は覚えないので、次の機会に問い合わせ直す。
    ///
    /// 鍵を読めていない間は問い合わせない(問い合わせのたびに資格情報ストアを読みに行かない。
    /// 読み直すのはターンの開始と設定の変更だけ)。
    async fn detect_active_model_once(&self) {
        let current = self.current();
        if current.adapter.key_error().is_some() {
            return;
        }
        let config = current.config;
        let target = config.active_model().and_then(|(p, model)| {
            (providers::can_detect_capabilities(p)
                && self.detected.get(&p.id, &model.name).is_none())
            .then(|| (p.clone(), model.name.clone()))
        });
        if let Some((provider, model)) = target {
            if let Err(e) = self.detect(&provider, vec![model]).await {
                crate::diagnostics::report(format_args!(
                    "failed to detect model capabilities from '{}': {e}",
                    provider.name
                ));
            }
        }
    }

    /// プロバイダーの全モデルの能力を推論サーバーに問い合わせ直す(設定画面)。
    pub async fn detect_model_capabilities(&self, provider_id: &str) -> Result<SettingsView> {
        let _in_flight = self
            .detecting
            .try_begin(provider_id.to_string())
            .ok_or_else(|| invalid("already detecting capabilities for this provider"))?;
        let provider = self.provider(provider_id)?;
        if !providers::can_detect_capabilities(&provider) {
            return Err(invalid(
                "capabilities can be detected only from servers on this machine or the local network",
            ));
        }
        let models = provider.models.iter().map(|m| m.name.clone()).collect();
        self.detect(&provider, models).await?;
        Ok(self.view())
    }

    /// プロバイダーが提供するモデル名を問い合わせる。登録済みのものも含めて名前順に返し、
    /// 設定には書かない(登録は利用者が選んで[`Self::add_models`]で行う)。
    pub async fn list_provider_models(&self, provider_id: &str) -> Result<Vec<AvailableModel>> {
        let provider = self.provider(provider_id)?;
        Ok(view::available_models(
            providers::list_models(&provider).await?,
        ))
    }

    /// 非同期の問い合わせに使う、ある時点のプロバイダー設定の複製。ロックを`.await`に
    /// またがせないため、複製してから問い合わせる。
    fn provider(&self, provider_id: &str) -> Result<ProviderConfig> {
        self.current()
            .config
            .providers
            .iter()
            .find(|p| p.id == provider_id)
            .cloned()
            .ok_or_else(|| provider_not_found(provider_id))
    }

    /// 問い合わせた全モデルの結果を置き換える。サーバーが答えなかったモデルも「検出した
    /// 項目なし」として覚え、ターンのたびに問い合わせ直さない。問い合わせに失敗したら
    /// 何も置き換えない(一時的な失敗で、取れていた結果を空にしない)。
    async fn detect(&self, provider: &ProviderConfig, models: Vec<String>) -> Result<()> {
        let mut found = providers::detect_capabilities(provider, &models)
            .await?
            .unwrap_or_default();
        for model in models {
            let detected = found.remove(&model).unwrap_or_default();
            self.detected.store(&provider.id, &model, detected);
        }
        Ok(())
    }

    /// アクティブなモデルが無ければ既定値を返す(その場合ターンはアダプタの段階で
    /// 失敗するので、この値は使われない)。
    fn active_model_capabilities(&self, config: &Config) -> ModelCapabilities {
        config
            .active_model()
            .map(|(p, model)| {
                llm::resolve_capabilities(model, self.detected.get(&p.id, &model.name).as_ref())
            })
            .unwrap_or(llm::DEFAULT_CAPABILITIES)
    }

    /// 手動設定より下の層(自動検出 → 既定値)で決まる値。
    fn fallback_capabilities(&self, provider_id: &str, model: &str) -> ModelCapabilities {
        llm::fallback_capabilities(self.detected.get(provider_id, model).as_ref())
    }

    pub fn view(&self) -> SettingsView {
        let current = self.current();
        self.build_view(&current.config, &current.adapter)
    }

    fn build_view(&self, config: &Config, adapter: &AdapterState) -> SettingsView {
        view::build(
            config,
            &self.mcp_tools,
            &self.detected,
            view::Problems {
                config_error: self.config_error.as_deref(),
                active_provider_error: adapter.broken_reason(),
                active_provider_key_error: adapter.key_error(),
            },
        )
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
            adapter_unusable: matches!(
                current.adapter,
                AdapterState::KeyUnavailable(_) | AdapterState::Broken(_)
            ),
        }
    }

    /// 空白だけのプロンプトと、既定の文面と同じ値は未設定として保存する
    /// (`orchestration::stored_prompt`)。表示言語は[`Self::update_language`]が別に持つので、
    /// ここでは変えない。
    pub fn update_general(&self, update: GeneralUpdate) -> Result<SettingsView> {
        let response_timeout_secs = input::bounded(
            update.response_timeout_secs,
            "response timeout (seconds)",
            input::MAX_TIMEOUT_SECS,
        )?;
        let mut draft = self.edit();
        let general = &mut draft.config.general;
        general.system_prompt = stored_prompt(update.system_prompt, None);
        let language = general.language();
        general.task_chat_system_prompt = stored_prompt(
            update.task_chat_system_prompt,
            Some(default_task_chat_prompt(language)),
        );
        general.task_opening_message = stored_prompt(
            update.task_opening_message,
            Some(default_opening_message(language)),
        );
        general.response_timeout_secs = response_timeout_secs;
        draft.commit()
    }

    /// 保存した表示言語。画面は起動時に1度だけ読み、切り替えは再起動で反映する。
    pub fn display_language(&self) -> Language {
        self.current().config.general.language()
    }

    pub fn update_language(&self, language: Language) -> Result<SettingsView> {
        let mut draft = self.edit();
        draft.config.general.language = Some(language.code().to_string());
        draft.commit()
    }

    /// 空欄(`None`)は「未設定」として既定値に戻す。
    pub fn update_tools(
        &self,
        max_rounds_per_turn: Option<u32>,
        total_timeout_secs: Option<u64>,
    ) -> Result<SettingsView> {
        let max_rounds_per_turn = input::bounded(
            max_rounds_per_turn,
            "max rounds per turn",
            input::MAX_ROUNDS_PER_TURN,
        )?;
        let total_timeout_secs = input::bounded(
            total_timeout_secs,
            "tool timeout (seconds)",
            input::MAX_TIMEOUT_SECS,
        )?;
        let mut draft = self.edit();
        draft.config.tools = ToolConfig {
            max_rounds_per_turn,
            total_timeout_secs,
        };
        draft.commit()
    }

    /// 最初に登録したプロバイダーをアクティブにする
    /// ([`Config::reselect_active_provider`])。同じ名前のプロバイダーは登録できない
    /// (チャットのモデル選択で見分けられなくなる)。鍵の保存に失敗したらプロバイダー自体の
    /// 登録も中断し、登録に失敗したら保存した鍵を消す。どちらでも`key_ref`と鍵の片方だけが
    /// 残る状態を作らない。
    pub fn add_provider(&self, new: NewProvider) -> Result<SettingsView> {
        let name = input::name(&new.name, "provider name", input::PROVIDER_NAME_MAX_CHARS)?;
        let base_url = new.base_url.trim().to_string();
        providers::validate_base_url(&base_url)?;
        // 鍵を保存する前にも確かめ、登録できないと分かっている名前のために資格情報ストアへ
        // 書かない。
        refuse_registered_provider_name(&self.current().config, &name)?;

        let key_ref = match new.api_key {
            Some(key) if !key.expose_secret().is_empty() => {
                providers::validate_api_key(&key)?;
                let key_ref = format!("provider:{}", ulid::Ulid::new());
                secrets::store(&key_ref, &key)?;
                Some(key_ref)
            }
            _ => None,
        };

        self.register_provider(ProviderConfig {
            id: ulid::Ulid::new().to_string(),
            name,
            api_format: new.api_format,
            base_url,
            models: Vec::new(),
            active_model: None,
            key_ref: key_ref.clone(),
        })
        .inspect_err(|_| {
            if let Some(key_ref) = &key_ref {
                delete_secret(key_ref, "provider API key");
            }
        })
    }

    /// 名前の重複確認から登録までを書き込みロックの中で行う。
    fn register_provider(&self, provider: ProviderConfig) -> Result<SettingsView> {
        let mut draft = self.edit();
        refuse_registered_provider_name(&draft.config, &provider.name)?;
        draft.config.providers.push(provider);
        draft.config.reselect_active_provider();
        draft.commit()
    }

    /// アクティブなプロバイダーを消したら、選択を移す([`Config::reselect_active_provider`])。
    /// 保存済みAPIキーも消す。鍵は設定の保存が済んでから消す(先に消すと、保存に失敗したときに設定が消えた鍵を
    /// 指して残る)。
    pub fn delete_provider(&self, provider_id: &str) -> Result<SettingsView> {
        let mut draft = self.edit();
        let config = &mut draft.config;
        let index = config
            .providers
            .iter()
            .position(|p| p.id == provider_id)
            .ok_or_else(|| provider_not_found(provider_id))?;
        let removed = config.providers.remove(index);
        config.reselect_active_provider();

        let view = draft.commit()?;
        self.detected.forget_provider(provider_id);
        if let Some(key_ref) = &removed.key_ref {
            delete_secret(key_ref, "provider API key");
        }
        Ok(view)
    }

    /// 手動追加(1件)と、取得した一覧から選んだ分(複数件)の両方が通る。1件でも登録できない
    /// 名前があれば何も登録しない。モデルが無かったプロバイダーでは、最初の1件を
    /// アクティブにする。アクティブなプロバイダーにモデルが無ければ、選択をこのプロバイダーへ
    /// 移す([`Config::reselect_active_provider`])。
    pub fn add_models<S: AsRef<str>>(
        &self,
        provider_id: &str,
        models: &[S],
    ) -> Result<SettingsView> {
        let mut draft = self.edit();
        let provider = find_provider_mut(&mut draft.config, provider_id)?;
        for model in models {
            let model = input::name(model.as_ref(), "model name", input::MODEL_NAME_MAX_CHARS)?;
            if provider.model(&model).is_some() {
                return Err(invalid(format!("model already registered: {model}")));
            }
            provider.models.push(ModelConfig::new(model));
        }
        if provider.active_model.is_none() {
            provider.active_model = provider.models.first().map(|m| m.name.clone());
        }
        draft.config.reselect_active_provider();
        draft.commit()
    }

    /// アクティブなモデルを消したら先頭をアクティブにする。プロバイダーの最後のモデルを
    /// 消したら、選択を移す([`Config::reselect_active_provider`])。
    pub fn remove_model(&self, provider_id: &str, model: &str) -> Result<SettingsView> {
        let mut draft = self.edit();
        let provider = find_provider_mut(&mut draft.config, provider_id)?;
        provider.models.retain(|m| m.name != model);
        if provider.active_model.as_deref() == Some(model) {
            provider.active_model = provider.models.first().map(|m| m.name.clone());
        }
        draft.config.reselect_active_provider();
        let view = draft.commit()?;
        self.detected.forget(provider_id, model);
        Ok(view)
    }

    /// チャット入力欄の下で選んだモデルに切り替える。一覧はプロバイダーを跨ぐので、
    /// アクティブなプロバイダーとそのモデルを1回の保存で切り替える。2回に分けると、間で
    /// 落ちたときに意図しない組が残る。
    pub fn select_chat_model(&self, provider_id: &str, model: &str) -> Result<()> {
        let mut draft = self.edit();
        let provider = find_provider_mut(&mut draft.config, provider_id)?;
        if provider.model(model).is_none() {
            return Err(model_not_found(model));
        }
        provider.active_model = Some(model.to_string());
        draft.config.active_provider_id = Some(provider_id.to_string());
        draft.commit().map(drop)
    }

    /// 思考に対応しないモデルにも保存はできる(送るときに外す。[`Snapshot::turn_context`])。
    /// 能力は手動設定で後から変わりうるため、選んだ値は捨てずに残す。
    pub fn set_reasoning_effort(
        &self,
        provider_id: &str,
        model: &str,
        effort: ReasoningEffort,
    ) -> Result<()> {
        let mut draft = self.edit();
        find_model_mut(&mut draft.config, provider_id, model)?.reasoning_effort = effort;
        draft.commit().map(drop)
    }

    /// チャットのモデル一覧に出すかどうか。
    pub fn set_model_visible(
        &self,
        provider_id: &str,
        model: &str,
        visible: bool,
    ) -> Result<SettingsView> {
        let mut draft = self.edit();
        find_model_mut(&mut draft.config, provider_id, model)?.visible = visible;
        draft.commit()
    }

    /// 手動設定より下の層と同じ値にしたら、手動設定を外す。
    pub fn set_model_capability(
        &self,
        provider_id: &str,
        model: &str,
        capability: Capability,
        supported: bool,
    ) -> Result<SettingsView> {
        let fallback = self
            .fallback_capabilities(provider_id, model)
            .flag(capability);
        let mut draft = self.edit();
        let entry = find_model_mut(&mut draft.config, provider_id, model)?;
        *entry.overrides.flag_mut(capability) = (supported != fallback).then_some(supported);
        draft.commit()
    }

    /// `None`(空欄)は手動設定を外す。下の層と同じ値の扱いは[`Self::set_model_capability`]と同じ。
    pub fn set_model_context_length(
        &self,
        provider_id: &str,
        model: &str,
        context_length: Option<u32>,
    ) -> Result<SettingsView> {
        if context_length == Some(0) {
            return Err(invalid("context length must be 1 or greater"));
        }
        let fallback = self
            .fallback_capabilities(provider_id, model)
            .context_length;
        let mut draft = self.edit();
        let entry = find_model_mut(&mut draft.config, provider_id, model)?;
        entry.overrides.context_length = context_length.filter(|n| *n != fallback);
        draft.commit()
    }

    /// 能力の手動設定(コンテキスト長を含む)をすべて外す。表示/非表示は能力ではないので残す。
    pub fn reset_model_capabilities(&self, provider_id: &str, model: &str) -> Result<SettingsView> {
        let mut draft = self.edit();
        find_model_mut(&mut draft.config, provider_id, model)?.overrides =
            ModelOverrides::default();
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

    /// 保存済みの秘密情報も消す。`delete_provider`と同じく、設定の保存が済んでから消す。
    /// 削除したサーバーのツール一覧がキャッシュに残らないよう、ここで捨てる。
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
        let enabled_count: usize = draft
            .config
            .mcp_servers
            .iter()
            .map(|s| s.enabled_tools.len())
            .sum();
        let server = find_mcp_server_mut(&mut draft.config, server_id)?;
        if enabled && !server.enabled_tools.contains(tool_name) {
            // 画面でも有効にできないようにしているが、判定を画面に任せない。一覧が未取得なら
            // 名前だけで判定する(引数スキーマはターンで公開するときにも見る)。
            let listed = self
                .mcp_tools
                .get(server_id)
                .and_then(|tools| tools.into_iter().find(|t| t.name == tool_name));
            let exposable = match listed {
                Some(tool) => external::is_exposable(&server.name, &tool),
                None => external::exposed_name(&server.name, tool_name).is_some(),
            };
            if !exposable {
                return Err(invalid(
                    "this tool cannot be enabled because its name or argument schema cannot be exposed to the model",
                ));
            }
            // 無効なサーバーのツールも数える。サーバーを有効に戻したときに上限を超えないため。
            if enabled_count >= external::MAX_EXTERNAL_TOOLS {
                return Err(invalid(format!(
                    "at most {} external tools can be enabled",
                    external::MAX_EXTERNAL_TOOLS
                )));
            }
            server.enabled_tools.insert(tool_name.to_string());
        } else {
            server.enabled_tools.remove(tool_name);
        }
        draft.commit()
    }

    /// サーバーに接続してツール一覧を取得し、キャッシュへ載せて設定の状態ごと返す。
    /// config.tomlには書き込まない。ロックはサーバー設定を複製するまでだけ持ち、接続の
    /// `.await`をまたがせない。
    pub async fn fetch_mcp_tools(&self, server_id: &str) -> Result<SettingsView> {
        let _in_flight = self
            .fetching
            .try_begin(server_id.to_string())
            .ok_or_else(|| invalid("already fetching tools for this server"))?;
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
    /// 鍵を読めていない・組み立てられていない。
    adapter_unusable: bool,
}

impl Draft<'_> {
    /// アダプタを組み立ててから保存する。アダプタは作り直しが要る変更の時だけ組み立てる。
    /// MCPのチェック1つの切り替えで資格情報ストアを読みに行かない。ただし前回鍵を読めなかった・
    /// 組み立てられなかった場合は、設定を変えるたびに組み立て直す(ストアのロック解除後などに、
    /// 再起動せずに直るように)。
    ///
    /// アダプタの入力を変える変更で組み立てに失敗したら、保存しない(組み立てられない設定を
    /// 新たにファイルへ残さない)。入力を変えない変更は、組み立てられないままの状態で通す
    /// (起動時から壊れているプロバイダーがあっても、無関係な設定は変えられるように)。
    fn commit(self) -> Result<SettingsView> {
        // 理由(パスと読めなかった箇所)は設定画面の上部に出ているので、ここでは繰り返さない。
        if self.settings.config_error.is_some() {
            return Err(invalid(
                "settings are not saved because the config file could not be read at startup; \
                 fix the file and restart the app",
            ));
        }
        let inputs_changed = providers::AdapterInputs::of(&self.before)
            != providers::AdapterInputs::of(&self.config);
        let rebuilt = if inputs_changed || self.adapter_unusable {
            let built = match providers::build_active_adapter(&self.config) {
                Err(e) if inputs_changed => return Err(e),
                built => built,
            };
            Some(adapter_state(built))
        } else {
            None
        };
        config::save(&self.settings.path, &self.config)?;

        let config = Arc::new(self.config);
        let adapter = {
            let mut current = self
                .settings
                .current
                .lock()
                .expect("settings mutex poisoned");
            current.config = Arc::clone(&config);
            // 入力を変えない変更で組み立て直した結果は、その間に読み直し
            // ([`Settings::reload_unavailable_key`])が使える状態にしていれば当てはめない
            // (読み直しの成功を、こちらの失敗で潰さないため)。
            if let Some(adapter) = rebuilt {
                if inputs_changed || !matches!(current.adapter, AdapterState::Ready(_)) {
                    current.adapter = adapter;
                }
            }
            current.adapter.clone()
        };
        Ok(self.settings.build_view(&config, &adapter))
    }
}

/// 秘密情報に触れる前に済ませられる検証をすべて行う。
fn validate_endpoint(endpoint: NewMcpEndpoint) -> Result<NewMcpEndpoint> {
    let NewMcpEndpoint::StreamableHttp { url, headers } = endpoint;
    let url = url.trim().to_string();
    mcp::validate_streamable_http_url(&url)?;
    for (name, value) in &headers {
        mcp::validate_header_name(name)?;
        mcp::validate_header_value(value.expose_secret())?;
    }
    input::unique_names(&headers, "header", str::to_ascii_lowercase)?;
    Ok(NewMcpEndpoint::StreamableHttp { url, headers })
}

fn store_endpoint_secrets(endpoint: NewMcpEndpoint) -> Result<McpEndpoint> {
    let NewMcpEndpoint::StreamableHttp { url, headers } = endpoint;
    Ok(McpEndpoint::StreamableHttp {
        url,
        header_refs: store_secret_refs(headers)?,
    })
}

/// 秘密情報の値を保存し、`(name, key_ref)`の組に変換する。途中で失敗したら
/// それまでに保存した分を削除してからエラーを返す(孤児を残さない)。
fn store_secret_refs(pairs: Vec<(String, SecretString)>) -> Result<Vec<SecretRef>> {
    let mut refs = Vec::with_capacity(pairs.len());
    for (name, value) in pairs {
        let key_ref = format!("mcp:{}", ulid::Ulid::new());
        if let Err(e) = secrets::store(&key_ref, &value) {
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
        crate::diagnostics::report(format_args!(
            "failed to delete {what} from secret store: {e}"
        ));
    }
}

fn endpoint_secret_refs(endpoint: &McpEndpoint) -> &[SecretRef] {
    let McpEndpoint::StreamableHttp { header_refs, .. } = endpoint;
    header_refs
}

fn refuse_registered_provider_name(config: &Config, name: &str) -> Result<()> {
    if config.providers.iter().any(|p| p.name == name) {
        return Err(invalid(format!("provider name already registered: {name}")));
    }
    Ok(())
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

fn find_model_mut<'a>(
    config: &'a mut Config,
    provider_id: &str,
    model: &str,
) -> Result<&'a mut ModelConfig> {
    find_provider_mut(config, provider_id)?
        .models
        .iter_mut()
        .find(|m| m.name == model)
        .ok_or_else(|| model_not_found(model))
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

fn model_not_found(model: &str) -> CoreError {
    invalid(format!("model not registered: {model}"))
}

fn mcp_server_not_found(server_id: &str) -> CoreError {
    invalid(format!("MCP server not found: {server_id}"))
}

fn invalid(message: impl Into<String>) -> CoreError {
    CoreError::InvalidSettings(message.into())
}

#[cfg(test)]
mod tests;
