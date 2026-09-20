#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;

use std::sync::{Arc, Mutex};

use scitl_core::llm::providers::openai_compat::OpenAiCompatAdapter;
use scitl_core::llm::LlmAdapter;
use scitl_core::orchestration::SharedConnection;
use tauri::Manager;

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

            // 縦切り検証段階の暫定実装。プロバイダ設定・APIキーの読み込み
            // (config.rs / secrets.rs)は別Issueで実装し、環境変数直読みを置き換える。
            let base_url = std::env::var("SCITL_LLM_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
            let api_key = std::env::var("SCITL_LLM_API_KEY").unwrap_or_default();
            let model =
                std::env::var("SCITL_LLM_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());

            let adapter = OpenAiCompatAdapter::new(base_url, api_key, model)
                .map_err(|e| format!("invalid LLM provider configuration: {e}"))?;

            app.manage(AppState {
                db: Arc::new(Mutex::new(conn)),
                adapter: Box::new(adapter),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::tasks::get_task_detail,
            commands::chat::send_task_chat_message,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
