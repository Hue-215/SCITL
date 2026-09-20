#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use scitl_core::config::{self, ApiFormat, Config, ProviderConfig};
use scitl_core::db::error::CoreError;
use scitl_core::llm::providers::openai_compat::OpenAiCompatAdapter;
use scitl_core::llm::LlmAdapter;
use scitl_core::orchestration::SharedConnection;
use scitl_core::secrets;
use secrecy::SecretString;
use tauri::Manager;

const DEFAULT_PROVIDER_ID: &str = "default";

/// 現在有効なプロバイダーから作った実行時アダプタと、それを作った元の設定。
/// 設定画面(Issue #22)でプロバイダーを切り替えたら[`build_active_adapter`]で丸ごと作り直す
/// (差分更新はせず、常に「設定→アダプタ」を1方向に保つ。呼び出し元は
/// `commands/settings.rs`の`persist_and_rebuild`)。
pub struct Runtime {
    pub config: Config,
    /// アクティブなプロバイダーが無い(全プロバイダーを削除した等)場合は`None`。
    /// この場合チャット送信コマンドがエラーを返す(main.rs下部`send_task_chat_message`)。
    pub adapter: Option<Arc<dyn LlmAdapter + Send + Sync>>,
}

/// コマンド層(`commands/*.rs`)が触れる唯一の状態。DB接続はロック内でのみ触り、
/// asyncのawaitをまたいで保持しない(architecture.md 4節、orchestration::turn参照)。
///
/// `runtime`のロックはアダプタの読み書きだけに使い、`.send().await`のような非同期呼び出しを
/// ロックを持ったまままたがせない(`Arc`を複製してからロックを外す。architecture.md 4節と
/// 同じ「ロックはawaitをまたがない」規律をここにも適用する)。
pub struct AppState {
    pub db: SharedConnection,
    pub config_path: PathBuf,
    pub runtime: Mutex<Runtime>,
    /// `fetch_mcp_tools`の同時実行を1サーバーにつき1本に絞るためのガード
    /// (`commands::mcp`)。ボタンの無効化(連打防止)はフロントエンド側の責務だが、
    /// それだけでは保証にならないため、Rust側にも同時実行を防ぐ手段を持つ
    /// (Opusレビュー指摘)。
    pub mcp_fetch_in_flight: Mutex<HashSet<String>>,
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&app_data_dir)?;
            let conn = scitl_core::db::open(app_data_dir.join("scitl.sqlite3"))?;

            let config_path = app_data_dir.join("config.toml");
            let config = load_or_seed_config(&config_path)?;
            let adapter = build_active_adapter(&config)
                .map_err(|e| format!("invalid LLM provider configuration: {e}"))?;

            app.manage(AppState {
                db: Arc::new(Mutex::new(conn)),
                config_path,
                runtime: Mutex::new(Runtime { config, adapter }),
                mcp_fetch_in_flight: Mutex::new(HashSet::new()),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::tasks::get_task_detail,
            commands::tasks::list_tasks,
            commands::tasks::create_task,
            commands::chat::send_task_chat_message,
            commands::settings::get_settings,
            commands::settings::update_general_settings,
            commands::settings::add_provider,
            commands::settings::delete_provider,
            commands::settings::set_active_provider,
            commands::settings::add_model,
            commands::settings::remove_model,
            commands::settings::set_active_model,
            commands::mcp::add_mcp_server,
            commands::mcp::delete_mcp_server,
            commands::mcp::set_mcp_server_enabled,
            commands::mcp::set_mcp_tool_enabled,
            commands::mcp::fetch_mcp_tools,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// 現在の`active_provider_id`からアダプタを組み立てる。設定画面の各コマンドが
/// 設定を変更するたびにこれを呼び直し、`Runtime::adapter`を丸ごと差し替える。
/// アクティブなプロバイダーが無い場合はエラーではなく`None`を返す(全プロバイダー削除は
/// 有効な状態であり、チャット送信時に初めてエラーとして表面化させる)。
///
/// 資格情報ストアが利用できない(OSユーザーが変わった、キーチェーンをクリアした等)
/// 場合でもアプリ自体は起動させる。鍵無し扱いに落とし、実際のAPI呼び出し時に
/// プロバイダー側の認証エラーとして表面化させる。
pub fn build_active_adapter(
    config: &Config,
) -> Result<Option<Arc<dyn LlmAdapter + Send + Sync>>, CoreError> {
    match config.active_provider() {
        Some(provider) => build_adapter_for(config, provider).map(Some),
        None => Ok(None),
    }
}

fn build_adapter_for(
    config: &Config,
    provider: &ProviderConfig,
) -> Result<Arc<dyn LlmAdapter + Send + Sync>, CoreError> {
    let api_key = match &provider.key_ref {
        Some(key_ref) => match secrets::load(key_ref) {
            Ok(secret) => secret,
            Err(e) => {
                eprintln!("failed to read API key from secret store, continuing without it: {e}");
                SecretString::from(String::new())
            }
        },
        None => SecretString::from(String::new()),
    };
    let model = provider.resolved_model().unwrap_or_default();
    let timeout = config.general.response_timeout_secs.map(Duration::from_secs);

    let adapter = OpenAiCompatAdapter::new(provider.base_url.clone(), api_key, model, timeout)?;
    Ok(Arc::new(adapter))
}

/// `config.toml`を読み込む。初回起動でファイルが無い場合は環境変数から1回だけ設定を作り、
/// APIキーがあれば`secrets.rs`(keyring)へ保存してから`config.toml`に書き出す。設定画面
/// (Issue #22)が入った以降は、この移行経路を通るのは初回起動時だけになる(Issue #19)。
fn load_or_seed_config(config_path: &Path) -> Result<Config, Box<dyn std::error::Error>> {
    let mut cfg = config::load(config_path)?;

    // 「初回起動か」の判定は`active_provider_id`ではなく`providers`が空かどうかで行う。
    // 前者だと、有効なプロバイダーが既にあるのに`active_provider_id`だけが欠けている
    // (将来のバグ・手動編集ミス等)場合に、同じidのプロバイダーを2重に作り、
    // 固定`key_ref`で既存の秘密情報を上書きしてしまう。
    if !cfg.providers.is_empty() {
        if cfg.active_provider().is_none() {
            return Err("config.toml has providers but no valid active_provider_id".into());
        }
        return Ok(cfg);
    }

    let base_url = std::env::var("SCITL_LLM_BASE_URL")
        .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
    let model = std::env::var("SCITL_LLM_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
    let api_key = std::env::var("SCITL_LLM_API_KEY").unwrap_or_default();

    let key_ref = if api_key.is_empty() {
        None
    } else {
        let key_ref = format!("provider:{}", ulid::Ulid::new());
        secrets::store(&key_ref, &SecretString::from(api_key))?;
        Some(key_ref)
    };

    let provider = ProviderConfig {
        id: DEFAULT_PROVIDER_ID.to_string(),
        name: "Default".to_string(),
        api_format: ApiFormat::OpenAiCompat,
        base_url,
        models: vec![model.clone()],
        active_model: Some(model),
        key_ref,
    };

    cfg.providers.push(provider);
    cfg.active_provider_id = Some(DEFAULT_PROVIDER_ID.to_string());
    config::save(config_path, &cfg)?;

    Ok(cfg)
}
