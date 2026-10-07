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
mod mcp_settings;
mod model_settings;
mod provider_settings;
pub mod view;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::attachments::Attachments;
use secrecy::SecretString;

use crate::config::{self, Config, SecretRef, ToolConfig};
use crate::db::messages::Chat;
use crate::error::{CoreError, Result};
use crate::i18n::Language;
use crate::in_flight::InFlightSet;
use crate::llm::providers::{self, SharedAdapter};
use crate::llm::{DetectedCatalog, LlmAdapter, ModelCapabilities};
use crate::mcp::ToolCatalog;
use crate::orchestration::{
    self, default_opening_message, default_task_chat_prompt, stored_prompt, McpAccess,
    SystemPrompts, ToolLimits, TurnContext, TurnEvents, TurnFailure,
};
use crate::secrets;

pub use mcp_settings::NewMcpEndpoint;
pub use provider_settings::NewProvider;
pub use view::{AvailableModel, ChatModelsView, SettingsView};

/// 設定画面「一般」タブの入力(表示言語を除く)。プロンプトは`Option<String>`が並ぶので、
/// 位置引数にせず名前で渡す。
pub struct GeneralUpdate {
    pub system_prompt: Option<String>,
    pub task_chat_system_prompt: Option<String>,
    pub task_opening_message: Option<String>,
    pub response_timeout_secs: Option<u64>,
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
    /// ターンの開始・タスクの追加と設定の変更のたびに読み直す。
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

    /// 鍵だけを読み直す[`Self::snapshot`]。チャットを使えるかを確かめるだけで、ターンを
    /// 始めない入口(タスクの追加)に使う。推論サーバーへは問い合わせない。
    pub async fn snapshot_reloading_key(&self) -> Snapshot {
        self.reload_unavailable_key().await;
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

/// 削除の失敗は操作全体を失敗させない(設定からは既に外れており、残るのは参照されない
/// 鍵だけ)。
fn delete_secret(key_ref: &str, what: &str) {
    if let Err(e) = secrets::delete(key_ref) {
        crate::diagnostics::report(format_args!(
            "failed to delete {what} from secret store: {e}"
        ));
    }
}

/// 秘密情報の値を保存し、`(name, key_ref)`の組に変換する。`key_ref`は`{prefix}:<ULID>`。
/// 途中で失敗したらそれまでに保存した分を削除してからエラーを返す(孤児を残さない)。
/// `what`は削除に失敗したときの診断に出す名前。
fn store_secret_refs(
    pairs: Vec<(String, SecretString)>,
    prefix: &str,
    what: &str,
) -> Result<Vec<SecretRef>> {
    let mut refs = Vec::with_capacity(pairs.len());
    for (name, value) in pairs {
        let key_ref = format!("{prefix}:{}", ulid::Ulid::new());
        if let Err(e) = secrets::store(&key_ref, &value) {
            delete_secret_refs(&refs, what);
            return Err(e);
        }
        refs.push(SecretRef { name, key_ref });
    }
    Ok(refs)
}

/// 1件が失敗しても残りは試す。
fn delete_secret_refs(refs: &[SecretRef], what: &str) {
    for r in refs {
        delete_secret(&r.key_ref, &format!("{what} '{}'", r.name));
    }
}

fn invalid(message: impl Into<String>) -> CoreError {
    CoreError::InvalidSettings(message.into())
}

#[cfg(test)]
mod tests;
