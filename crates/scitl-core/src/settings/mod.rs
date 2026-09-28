//! 設定・登録の操作(設定画面の各タブ、Issue #22・#28・#71)。GUIのコマンドもCLIも
//! ここを1つ呼ぶだけにし、登録の規則・入力検証・秘密情報の出し入れ・保存を同じ経路に通す
//! (architecture.md 1節、principles.md 5節)。
//!
//! 変更はすべて[`Draft`]を通る。書き込み同士は`writer`で直列化し、設定の複製を変更→
//! アダプタの組み立て→保存→差し替え、の順で進める。途中で失敗すれば何も差し替えないので、
//! メモリ上の設定とファイルが食い違わない。読み手(ターンの開始)が取る`current`のロックは
//! 差し替えの一瞬だけで、資格情報ストアやファイルのI/Oを待たされない。
//!
//! 設定に問題があっても起動は止めない(Issue #155。画面から直す手段が無くなるため)。
//! 設定ファイルを読めなければ空の設定で動かし、読めなかったファイルを上書きしないよう
//! 保存を断る。アクティブなプロバイダーを組み立てられなければ、そのプロバイダーを使えない
//! ものとして動かし、削除・切り替えで直せるようにする。どちらも理由を設定画面に出し、
//! チャットでは理由に応じたエラー発言にする。

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
    adapter: AdapterState,
    /// `adapter`を鍵無しで組み立てた(資格情報ストアから読めなかった)。
    key_unavailable: bool,
}

/// アクティブなプロバイダーのアダプタ。
#[derive(Clone)]
enum AdapterState {
    Ready(SharedAdapter),
    /// アクティブなプロバイダーが無い(未登録・全プロバイダーを削除した等)。
    NoProvider,
    /// アクティブなプロバイダーを組み立てられない。理由を持つ。
    Broken(String),
}

impl AdapterState {
    fn broken_reason(&self) -> Option<&str> {
        match self {
            Self::Broken(reason) => Some(reason),
            Self::Ready(_) | Self::NoProvider => None,
        }
    }
}

/// 組み立ての結果を状態に直す。2つ目は「鍵を読めずに鍵無しで組み立てた」。
fn adapter_state(built: Result<providers::ActiveAdapter>) -> (AdapterState, bool) {
    match built {
        Ok(providers::ActiveAdapter {
            adapter: Some(adapter),
            key_unavailable,
        }) => (AdapterState::Ready(adapter), key_unavailable),
        Ok(providers::ActiveAdapter { adapter: None, .. }) => (AdapterState::NoProvider, false),
        Err(e) => (AdapterState::Broken(e.to_string()), false),
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
    /// 取得済みのMCPツール一覧(Issue #104)。アプリ起動中だけ保持するメモリキャッシュで、
    /// config.tomlには書かない(ツール名・説明はユーザーの設定ではなくサーバー側の持ち物で、
    /// 永続化した写しはサーバー側の更新を検知できない)。設定画面の表示と、ターン開始時の
    /// ツール公開(`orchestration::McpAccess`)が同じここを読む。
    mcp_tools: Arc<ToolCatalog>,
    /// [`Self::fetch_mcp_tools`]の同時実行を1サーバーにつき1本に絞る。
    fetching: InFlightSet<String>,
    /// モデル能力の自動検出の結果(能力解決の3層の真ん中)。`mcp_tools`と同じく
    /// アプリ起動中だけ保持する(理由は[`DetectedCatalog`])。
    detected: DetectedCatalog,
    /// [`Self::detect_model_capabilities`]の同時実行を1プロバイダーにつき1本に絞る。
    detecting: InFlightSet<String>,
}

impl Settings {
    /// 設定ファイルを読み、アクティブなプロバイダーのアダプタを組み立てる。ファイルが無ければ
    /// プロバイダー0件で始める。既定の通信先を補わないのは、通信先をユーザーが登録したものに
    /// 限るため(principles.md 1節)。読めない・組み立てられない場合も失敗にはしない
    /// (モジュール冒頭)。
    pub fn load(path: PathBuf) -> Self {
        let (config, config_error) = match config::load(&path) {
            Ok(config) => (config, None),
            Err(e) => {
                let reason = format!("{}: {e}", path.display());
                eprintln!("failed to read the config file, starting with empty settings: {reason}");
                (Config::default(), Some(reason))
            }
        };
        let (adapter, key_unavailable) = adapter_state(providers::build_active_adapter(&config));
        if let Some(reason) = adapter.broken_reason() {
            eprintln!("the active provider cannot be used: {reason}");
        }
        Self {
            path,
            config_error,
            current: Mutex::new(Current {
                config: Arc::new(config),
                adapter,
                key_unavailable,
            }),
            writer: Mutex::new(()),
            mcp_tools: Arc::new(ToolCatalog::new()),
            fetching: InFlightSet::new(),
            detected: DetectedCatalog::new(),
            detecting: InFlightSet::new(),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let current = self.current();
        let adapter = match (&self.config_error, current.adapter) {
            (Some(_), _) => Err(TurnFailure::SettingsUnreadable),
            (None, AdapterState::Ready(adapter)) => Ok(adapter),
            (None, AdapterState::NoProvider) => Err(TurnFailure::NoProvider),
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

    /// ターンの開始に使う[`Self::snapshot`]。アクティブなモデルの能力をまだ推論サーバーに
    /// 問い合わせていなければ、先に問い合わせる([`Self::detect_active_model_once`])。
    ///
    /// 問い合わせに失敗してもターンは止めない。サーバーに繋がらないならターン自体が
    /// 失敗して理由がエラー発言に残り、繋がるなら既定値の層で進められるため。
    pub async fn snapshot_for_turn(&self) -> Snapshot {
        self.detect_active_model_once().await;
        self.snapshot()
    }

    /// チャット入力欄の下のモデル選択(Issue #64)。思考の強さを選べるかは能力で決まるので、
    /// ターンの開始と同じく、アクティブなモデルを先に問い合わせる。失敗したら自動検出より
    /// 下の層の値で出す。
    pub async fn chat_models(&self) -> ChatModelsView {
        self.detect_active_model_once().await;
        view::chat_models(&self.current().config, &self.detected)
    }

    /// アクティブなモデルの能力を、まだ推論サーバーに問い合わせていなければ問い合わせる
    /// (アプリ起動後、モデルごとに最初の1回)。失敗は覚えないので、次の機会に問い合わせ直す。
    async fn detect_active_model_once(&self) {
        let config = self.current().config;
        let target = config.active_model().and_then(|(p, model)| {
            (providers::can_detect_capabilities(p)
                && self.detected.get(&p.id, &model.name).is_none())
            .then(|| (p.clone(), model.name.clone()))
        });
        if let Some((provider, model)) = target {
            if let Err(e) = self.detect(&provider, vec![model]).await {
                eprintln!(
                    "failed to detect model capabilities from '{}': {e}",
                    provider.name
                );
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

    /// プロバイダーが提供するモデル名を問い合わせる(Issue #33)。登録済みのものも含めて
    /// 名前順に返し、設定には書かない。どれを登録するかは利用者が選び、[`Self::add_models`]で
    /// 登録する(一括で登録しない理由はarchitecture.md 3節)。
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
            key_unavailable: current.key_unavailable,
            adapter_broken: current.adapter.broken_reason().is_some(),
        }
    }

    /// 空白だけのプロンプトは未設定として保存する。既定の文面を持つもの(タスクチャット用・
    /// 開始の発言)は、既定の文面と同じ値も未設定にする(architecture.md 3節「聞き取りの開始」。
    /// 判定は`orchestration::stored_prompt`)。
    /// 表示言語は[`Self::update_language`]が別に持つので、ここでは変えない。
    pub fn update_general(&self, update: GeneralUpdate) -> Result<SettingsView> {
        // `0`は画面側でも弾くが、UIの入力チェックはセキュリティ境界ではない。
        if update.response_timeout_secs == Some(0) {
            return Err(invalid("response timeout must be 1 second or greater"));
        }
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
        general.response_timeout_secs = update.response_timeout_secs;
        draft.commit()
    }

    /// 保存した表示言語。画面は起動時に1度だけ読み、切り替えは再起動で反映する
    /// (legacy/frontend.md 2節)。
    pub fn display_language(&self) -> Language {
        self.current().config.general.language()
    }

    pub fn update_language(&self, language: Language) -> Result<SettingsView> {
        let mut draft = self.edit();
        draft.config.general.language = Some(language);
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
        self.detected.forget_provider(provider_id);
        if let Some(key_ref) = &removed.key_ref {
            delete_secret(key_ref, "provider API key");
        }
        Ok(view)
    }

    /// 手動追加(1件)と、取得した一覧から選んだ分(複数件)の両方が通る。1件でも登録できない
    /// 名前があれば何も登録しない。モデルが無かったプロバイダーでは、最初の1件を
    /// アクティブにする。
    pub fn add_models<S: AsRef<str>>(
        &self,
        provider_id: &str,
        models: &[S],
    ) -> Result<SettingsView> {
        let mut draft = self.edit();
        let provider = find_provider_mut(&mut draft.config, provider_id)?;
        for model in models {
            let model = model.as_ref().trim();
            if model.is_empty() {
                return Err(invalid("model name must not be empty"));
            }
            if provider.model(model).is_some() {
                return Err(invalid(format!("model already registered: {model}")));
            }
            provider.models.push(ModelConfig::new(model.to_string()));
        }
        if provider.active_model.is_none() {
            provider.active_model = provider.models.first().map(|m| m.name.clone());
        }
        draft.commit()
    }

    /// アクティブなモデルを消したら先頭をアクティブにする。
    pub fn remove_model(&self, provider_id: &str, model: &str) -> Result<SettingsView> {
        let mut draft = self.edit();
        let provider = find_provider_mut(&mut draft.config, provider_id)?;
        provider.models.retain(|m| m.name != model);
        if provider.active_model.as_deref() == Some(model) {
            provider.active_model = provider.models.first().map(|m| m.name.clone());
        }
        let view = draft.commit()?;
        self.detected.forget(provider_id, model);
        Ok(view)
    }

    /// チャット入力欄の下で選んだモデルに切り替える(Issue #64)。一覧はプロバイダーを跨ぐので、
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

    /// 手動設定より下の層と同じ値にしたら、手動設定を外す(architecture.md 3節)。
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
            // 画面でも有効にできないようにしているが、判定を画面に任せない。
            if external::exposed_name(&server.name, tool_name).is_none() {
                return Err(invalid(
                    "this tool cannot be enabled because its name cannot be exposed to the model",
                ));
            }
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
    key_unavailable: bool,
    adapter_broken: bool,
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
        let rebuilt = if inputs_changed || self.key_unavailable || self.adapter_broken {
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
            if let Some((adapter, key_unavailable)) = rebuilt {
                current.adapter = adapter;
                current.key_unavailable = key_unavailable;
            }
            current.adapter.clone()
        };
        Ok(self.settings.build_view(&config, &adapter))
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
mod tests {
    use super::*;
    use crate::attachments::{AttachmentStore, Delivery};
    use crate::orchestration::discard_events;

    // 鍵を渡さない操作だけを試す(資格情報ストアに触れない)。

    fn temp_settings() -> (Settings, PathBuf) {
        let dir = std::env::temp_dir().join(format!("scitl-settings-test-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        (Settings::load(path.clone()), path)
    }

    fn ready_adapter(settings: &Settings) -> SharedAdapter {
        match settings.current().adapter {
            AdapterState::Ready(adapter) => adapter,
            AdapterState::NoProvider | AdapterState::Broken(_) => panic!("adapter is not ready"),
        }
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

    /// 思考に対応するモデルには強さを必ず送り、対応しないモデルには送らない。
    #[test]
    fn reasoning_effort_is_sent_only_to_models_that_think() {
        let (_, path) = temp_settings();
        let config_with = |model_lines: &str| {
            format!(
                r#"
active_provider_id = "p"

[[providers]]
id = "p"
name = "Local"
api_format = "open_ai_compat"
base_url = "http://localhost:1234/v1"

[[providers.models]]
name = "m"
{model_lines}
"#
            )
        };
        let effort_for = |model_lines: &str| {
            std::fs::write(&path, config_with(model_lines)).unwrap();
            let settings = Settings::load(path.clone());
            let generating = InFlightSet::new();
            // このテストは添付を使わないので、置き場所は作られない。
            let unused = path.with_file_name("attachments");
            let attachments = Attachments::new(AttachmentStore::new(
                unused.join("blobs"),
                unused.join("revealed"),
            ));
            settings
                .snapshot()
                .turn_context(&generating, &attachments, &discard_events)
                .reasoning_effort
        };

        let low = r#"reasoning_effort = "low""#;
        assert_eq!(effort_for(low), Some(ReasoningEffort::Low));
        let without_thinking = format!("{low}\n[providers.models.overrides]\nthinking = false");
        assert_eq!(effort_for(&without_thinking), None);
        // まだ選んでいないモデルでも、サーバーの既定には任せない。
        assert_eq!(effort_for(""), Some(ReasoningEffort::default()));
    }

    #[test]
    fn chat_model_selection_switches_provider_and_model_together() {
        let (settings, path) = temp_settings();
        let a = add_local_provider(&settings, "A").providers[0].id.clone();
        let b = add_local_provider(&settings, "B").providers[1].id.clone();
        settings.add_models(&a, &["a1"]).unwrap();
        settings.add_models(&b, &["b1"]).unwrap();
        settings.add_models(&b, &["b2"]).unwrap();

        settings.select_chat_model(&b, "b2").unwrap();
        let reloaded = config::load(&path).unwrap();
        let (provider, model) = reloaded.active_model().unwrap();
        assert_eq!(
            (provider.id.as_str(), model.name.as_str()),
            (b.as_str(), "b2")
        );

        assert!(settings.select_chat_model(&a, "b1").is_err());
        assert_eq!(
            settings.current().config.active_provider_id.as_deref(),
            Some(b.as_str()),
            "失敗した選択は何も変えない"
        );
    }

    #[test]
    fn chat_models_list_visible_models_and_the_selected_one_even_if_hidden() {
        let (settings, _) = temp_settings();
        let a = add_local_provider(&settings, "A").providers[0].id.clone();
        let b = add_local_provider(&settings, "B").providers[1].id.clone();
        settings.add_models(&a, &["qwen3:8b"]).unwrap();
        settings.add_models(&a, &["hidden"]).unwrap();
        settings.add_models(&b, &["qwen2.5:7b"]).unwrap();
        settings
            .set_model_capability(&b, "qwen2.5:7b", Capability::Thinking, false)
            .unwrap();
        settings.set_model_visible(&a, "hidden", false).unwrap();
        settings
            .set_reasoning_effort(&a, "qwen3:8b", ReasoningEffort::High)
            .unwrap();

        let chat_models =
            |settings: &Settings| view::chat_models(&settings.current().config, &settings.detected);
        let listed = chat_models(&settings);
        let names: Vec<_> = listed.choices.iter().map(|c| c.model.as_str()).collect();
        assert_eq!(names, ["qwen3:8b", "qwen2.5:7b"]);
        let selected = listed.selected.unwrap();
        assert_eq!(selected.choice.model, "qwen3:8b");
        assert!(selected.thinking);
        assert_eq!(selected.reasoning_effort, ReasoningEffort::High);

        settings.select_chat_model(&a, "hidden").unwrap();
        assert_eq!(
            chat_models(&settings).selected.unwrap().choice.model,
            "hidden"
        );

        settings.select_chat_model(&b, "qwen2.5:7b").unwrap();
        let selected = chat_models(&settings).selected.unwrap();
        assert_eq!(selected.choice.provider_name, "B");
        assert!(!selected.thinking);
        // 既定では画像に対応しないので、画像の添付は名前だけになる。
        assert_eq!(selected.attachments.image, Delivery::NameOnly);
        assert_eq!(selected.attachments.text, Delivery::Content);
        assert_eq!(selected.attachments.other, Delivery::NameOnly);

        settings
            .set_model_capability(&b, "qwen2.5:7b", Capability::Image, true)
            .unwrap();
        let selected = chat_models(&settings).selected.unwrap();
        assert_eq!(selected.attachments.image, Delivery::Image);
        assert_eq!(selected.attachments.other, Delivery::NameOnly);
    }

    #[test]
    fn first_provider_and_model_become_active_and_are_saved() {
        let (settings, path) = temp_settings();
        let view = add_local_provider(&settings, "Local");
        let id = view.providers[0].id.clone();
        assert_eq!(view.active_provider_id.as_deref(), Some(id.as_str()));
        assert!(settings.snapshot().adapter.is_ok());

        let view = settings.add_models(&id, &[" m1 "]).unwrap();
        assert_eq!(view.providers[0].active_model.as_deref(), Some("m1"));
        let view = settings.add_models(&id, &["m2"]).unwrap();
        assert_eq!(
            view.providers[0].active_model.as_deref(),
            Some("m1"),
            "2つ目のモデルでアクティブは変わらない"
        );

        let reloaded = config::load(&path).unwrap();
        let names: Vec<_> = reloaded.providers[0]
            .models
            .iter()
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(names, ["m1", "m2"]);
    }

    #[test]
    fn adding_several_models_registers_all_or_none() {
        let (settings, _) = temp_settings();
        let id = add_local_provider(&settings, "Local").providers[0]
            .id
            .clone();

        let view = settings.add_models(&id, &["b", "a"]).unwrap();
        let names: Vec<_> = view.providers[0]
            .models
            .iter()
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(names, ["b", "a"]);
        assert_eq!(
            view.providers[0].active_model.as_deref(),
            Some("b"),
            "モデルが無かったプロバイダーでは最初の1件がアクティブになる"
        );

        for rejected in [&["c", "a"][..], &["c", "c"], &["c", " "]] {
            assert!(settings.add_models(&id, rejected).is_err());
            let config = settings.current().config;
            assert!(
                config.providers[0].model("c").is_none(),
                "{rejected:?}: 登録できない名前が混ざれば1件も登録しない"
            );
        }
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
        assert!(matches!(
            settings.snapshot().adapter,
            Err(TurnFailure::NoProvider)
        ));
    }

    #[test]
    fn removing_active_model_falls_back_to_first() {
        let (settings, _) = temp_settings();
        let id = add_local_provider(&settings, "A").providers[0].id.clone();
        settings.add_models(&id, &["m1"]).unwrap();
        settings.add_models(&id, &["m2"]).unwrap();
        settings.select_chat_model(&id, "m2").unwrap();

        let view = settings.remove_model(&id, "m2").unwrap();
        assert_eq!(view.providers[0].active_model.as_deref(), Some("m1"));
    }

    #[test]
    fn rejects_zero_limits_and_timeout() {
        let (settings, _) = temp_settings();
        assert!(settings
            .update_general(general_update(None, Some(0)))
            .is_err());
        assert!(settings.update_tools(Some(0), None).is_err());
        assert!(settings.update_tools(None, Some(0)).is_err());
    }

    fn general_update(
        system_prompt: Option<&str>,
        response_timeout_secs: Option<u64>,
    ) -> GeneralUpdate {
        GeneralUpdate {
            system_prompt: system_prompt.map(str::to_string),
            task_chat_system_prompt: None,
            task_opening_message: None,
            response_timeout_secs,
        }
    }

    #[test]
    fn blank_prompts_and_prompts_equal_to_their_defaults_are_saved_as_unset() {
        let (settings, _) = temp_settings();
        let view = settings
            .update_general(GeneralUpdate {
                task_chat_system_prompt: Some(
                    default_task_chat_prompt(Language::DEFAULT).to_string(),
                ),
                task_opening_message: Some(default_opening_message(Language::DEFAULT).to_string()),
                ..general_update(None, None)
            })
            .unwrap();
        assert!(view.general.task_chat_system_prompt.is_none());
        assert!(view.general.task_opening_message.is_none());

        let view = settings
            .update_general(GeneralUpdate {
                task_chat_system_prompt: Some("custom".to_string()),
                task_opening_message: Some(" \n".to_string()),
                ..general_update(Some("  "), None)
            })
            .unwrap();
        assert_eq!(
            view.general.task_chat_system_prompt.as_deref(),
            Some("custom")
        );
        assert!(view.general.task_opening_message.is_none());
        assert!(view.general.system_prompt.is_none());
    }

    #[test]
    fn language_is_saved_apart_from_the_other_general_settings() {
        let (settings, path) = temp_settings();
        assert_eq!(settings.display_language(), Language::DEFAULT);

        settings.update_language(Language::En).unwrap();
        // プロンプト欄を保存しても、表示言語は変えない。
        let view = settings
            .update_general(general_update(Some("prompt"), None))
            .unwrap();
        assert_eq!(view.general.language, Language::En);
        assert_eq!(Settings::load(path).display_language(), Language::En);
    }

    #[test]
    fn failed_save_leaves_current_settings_unchanged() {
        let (settings, path) = temp_settings();
        // 保存先をディレクトリにして書き込みを失敗させる。
        std::fs::create_dir_all(&path).unwrap();
        assert!(settings
            .update_general(general_update(Some("prompt"), None))
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
    fn tools_whose_names_cannot_be_exposed_cannot_be_enabled() {
        let (settings, _) = temp_settings();
        let view = settings
            .add_mcp_server(
                "tools",
                NewMcpEndpoint::Stdio {
                    command: "npx".to_string(),
                    args: Vec::new(),
                    env: Vec::new(),
                },
            )
            .unwrap();
        let id = view.mcp_servers[0].id.clone();

        let err = settings
            .set_mcp_tool_enabled(&id, "read\u{202E}file", true)
            .unwrap_err();
        assert!(matches!(err, CoreError::InvalidSettings(_)));

        let view = settings
            .set_mcp_tool_enabled(&id, "read_file", true)
            .unwrap();
        assert_eq!(view.mcp_servers[0].enabled_tools, vec!["read_file"]);
        // 一覧が未取得でも、有効化済みのツールは説明なしで一覧に出る。
        let server = &view.mcp_servers[0];
        assert!(!server.tools_fetched);
        assert_eq!(server.tools.len(), 1);
        assert_eq!(server.tools[0].label, "read_file");
        assert!(server.tools[0].description.is_none());
    }

    /// 設定ファイルを読めなくても起動し、理由を画面とターンへ渡す。読めなかったファイルは
    /// 上書きしない(Issue #155)。
    #[test]
    fn unreadable_config_file_starts_empty_and_refuses_to_save() {
        let dir = std::env::temp_dir().join(format!("scitl-settings-test-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "providers = [").unwrap();

        let settings = Settings::load(path.clone());

        assert!(settings.view().config_error.is_some());
        assert!(matches!(
            settings.snapshot().adapter,
            Err(TurnFailure::SettingsUnreadable)
        ));
        let err = settings
            .update_general(general_update(Some("prompt"), None))
            .unwrap_err();
        assert!(matches!(err, CoreError::InvalidSettings(_)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "providers = [");
    }

    /// アクティブなプロバイダーを組み立てられなくても起動し、そのプロバイダーを使えない
    /// ものとして扱う。無関係な変更は通し、削除すれば直る(Issue #155)。
    #[test]
    fn broken_active_provider_does_not_block_startup_or_unrelated_changes() {
        let dir = std::env::temp_dir().join(format!("scitl-settings-test-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        // ホスト名宛のhttpは検証で弾かれる(net::validate_external_url)。
        std::fs::write(
            &path,
            r#"
active_provider_id = "broken"

[[providers]]
id = "broken"
name = "Broken"
api_format = "open_ai_compat"
base_url = "http://example.com/v1"

[[providers.models]]
name = "m"
"#,
        )
        .unwrap();

        let settings = Settings::load(path);
        let view = settings.view();
        assert!(view.config_error.is_none());
        assert!(view.providers[0].error.is_some());
        assert!(matches!(
            settings.snapshot().adapter,
            Err(TurnFailure::ProviderConfig)
        ));

        // アダプタの入力を変えない変更は通る。
        settings.update_tools(Some(3), None).unwrap();
        // 使えるプロバイダーを足しても、アクティブは壊れたままなので状態は変わらない。
        add_local_provider(&settings, "Local");
        assert!(settings.view().providers[0].error.is_some());

        let view = settings.delete_provider("broken").unwrap();
        assert!(view.providers.iter().all(|p| p.error.is_none()));
        assert!(settings.snapshot().adapter.is_ok());
    }

    #[test]
    fn adapter_is_rebuilt_only_when_its_inputs_change() {
        let (settings, _) = temp_settings();
        let id = add_local_provider(&settings, "A").providers[0].id.clone();
        let before = ready_adapter(&settings);

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
        assert!(Arc::ptr_eq(&before, &ready_adapter(&settings)));

        settings.add_models(&id, &["m1"]).unwrap();
        let after_model = ready_adapter(&settings);
        assert!(!Arc::ptr_eq(&before, &after_model));

        // 表示と能力はアダプタの入力ではない。
        settings.set_model_visible(&id, "m1", false).unwrap();
        settings
            .set_model_capability(&id, "m1", Capability::Image, true)
            .unwrap();
        settings
            .set_model_context_length(&id, "m1", Some(4096))
            .unwrap();
        assert!(Arc::ptr_eq(&after_model, &ready_adapter(&settings)));
    }

    #[test]
    fn capability_overrides_are_kept_only_while_they_differ_from_the_default() {
        let (settings, path) = temp_settings();
        let id = add_local_provider(&settings, "Local").providers[0]
            .id
            .clone();
        settings.add_models(&id, &["m"]).unwrap();
        let fallback = llm::DEFAULT_CAPABILITIES;

        let view = settings
            .set_model_capability(&id, "m", Capability::Tools, !fallback.tools)
            .unwrap();
        let model = &view.providers[0].models[0];
        assert_eq!(model.capabilities.tools, !fallback.tools);
        assert!(model.overridden);

        // 初期値と同じ値に戻したら、手動設定は残らない。
        let view = settings
            .set_model_capability(&id, "m", Capability::Tools, fallback.tools)
            .unwrap();
        assert!(!view.providers[0].models[0].overridden);

        settings
            .set_model_capability(&id, "m", Capability::Thinking, !fallback.thinking)
            .unwrap();
        let view = settings
            .set_model_context_length(&id, "m", Some(8192))
            .unwrap();
        let model = &view.providers[0].models[0];
        assert_eq!(model.capabilities.context_length, 8192);
        assert!(settings
            .set_model_context_length(&id, "m", Some(0))
            .is_err());

        let saved = &config::load(&path).unwrap().providers[0].models[0];
        assert_eq!(saved.overrides.thinking, Some(!fallback.thinking));
        assert_eq!(saved.overrides.context_length, Some(8192));

        settings.set_model_visible(&id, "m", false).unwrap();
        let view = settings.reset_model_capabilities(&id, "m").unwrap();
        let model = &view.providers[0].models[0];
        assert!(!model.overridden);
        assert_eq!(model.capabilities, fallback);
        assert!(!model.visible, "表示/非表示は能力ではないので戻さない");

        assert!(settings.set_model_visible(&id, "missing", true).is_err());
    }

    /// 自動検出の結果は、手動設定を外す基準と、ターンに渡す能力の両方に効く。
    #[test]
    fn detected_capabilities_are_the_layer_below_manual_settings() {
        let (settings, _path) = temp_settings();
        let id = add_local_provider(&settings, "Local").providers[0]
            .id
            .clone();
        settings.add_models(&id, &["m"]).unwrap();
        settings.detected.store(
            &id,
            "m",
            llm::DetectedCapabilities {
                tools: Some(false),
                context_length: Some(16_384),
                ..Default::default()
            },
        );

        let view = settings.view();
        assert!(view.providers[0].can_detect_capabilities);
        let model = &view.providers[0].models[0];
        assert!(!model.capabilities.tools);
        assert_eq!(model.default_context_length, 16_384);
        assert!(!model.overridden);
        assert!(!settings.snapshot().capabilities.tools);

        // 検出した値と同じにしたら手動設定は残らず、既定値と同じでも違えば残る。
        let view = settings
            .set_model_context_length(&id, "m", Some(16_384))
            .unwrap();
        assert!(!view.providers[0].models[0].overridden);
        let view = settings
            .set_model_capability(&id, "m", Capability::Tools, true)
            .unwrap();
        assert!(view.providers[0].models[0].overridden);
        assert!(settings.snapshot().capabilities.tools);

        // モデルを消したら結果も捨てる。
        settings.remove_model(&id, "m").unwrap();
        assert!(settings.detected.get(&id, "m").is_none());
    }
}
