#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;

use std::sync::{Arc, Mutex};

use scitl_core::config::{self, ApiFormat, ProviderConfig};
use scitl_core::llm::providers::openai_compat::OpenAiCompatAdapter;
use scitl_core::llm::LlmAdapter;
use scitl_core::orchestration::SharedConnection;
use scitl_core::secrets;
use secrecy::SecretString;
use tauri::Manager;

const DEFAULT_PROVIDER_ID: &str = "default";

/// コマンド層(`commands/*.rs`)が触れる唯一の状態。DB接続はロック内でのみ触り、
/// asyncのawaitをまたいで保持しない(architecture.md 4節、orchestration::turn参照)。
pub struct AppState {
    db: SharedConnection,
    adapter: Box<dyn LlmAdapter + Send + Sync>,
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&app_data_dir)?;
            let conn = scitl_core::db::open(app_data_dir.join("scitl.sqlite3"))?;

            let config_path = app_data_dir.join("config.toml");
            let provider = load_or_seed_provider(&config_path)?;
            // 資格情報ストアが利用できない(OSユーザーが変わった、キーチェーンをクリアした等)
            // 場合でもアプリ自体は起動させる。設定画面(#22)が無い現状、ここで起動を止めると
            // 復旧手段が「config.tomlを手で消す」しかなくなるため、鍵無し扱いに落とす。
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

            let adapter =
                OpenAiCompatAdapter::new(provider.base_url.clone(), api_key, provider.model.clone())
                    .map_err(|e| format!("invalid LLM provider configuration: {e}"))?;

            app.manage(AppState {
                db: Arc::new(Mutex::new(conn)),
                adapter: Box::new(adapter),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::tasks::get_task_detail,
            commands::tasks::list_tasks,
            commands::tasks::create_task,
            commands::chat::send_task_chat_message,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// `config.toml`から有効なプロバイダー設定を読み込む。設定画面(Issue #22)がまだ無く
/// 初回起動時にファイルが存在しないため、その場合は環境変数から1回だけ設定を作り、
/// APIキーがあれば`secrets.rs`(keyring)へ保存してから`config.toml`に書き出す。
/// 以降の起動はこの`config.toml`とkeyringだけを読む、正式な経路を通る
/// (Issue #19。環境変数直読みは初回移行のためだけに残す)。
fn load_or_seed_provider(
    config_path: &std::path::Path,
) -> Result<ProviderConfig, Box<dyn std::error::Error>> {
    let mut cfg = config::load(config_path)?;

    // 「初回起動か」の判定は`active_provider_id`ではなく`providers`が空かどうかで行う。
    // 前者だと、有効なプロバイダーが既にあるのに`active_provider_id`だけが欠けている
    // (将来のバグ・手動編集ミス等)場合に、同じidのプロバイダーを2重に作り、
    // 固定`key_ref`で既存の秘密情報を上書きしてしまう。
    if !cfg.providers.is_empty() {
        return cfg
            .active_provider()
            .cloned()
            .ok_or_else(|| "config.toml has providers but no valid active_provider_id".into());
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
        api_format: ApiFormat::OpenAiCompat,
        base_url,
        model,
        key_ref,
    };

    cfg.providers.push(provider.clone());
    cfg.active_provider_id = Some(DEFAULT_PROVIDER_ID.to_string());
    config::save(config_path, &cfg)?;

    Ok(provider)
}
