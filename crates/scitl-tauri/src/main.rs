#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod navigation;

use std::sync::{Arc, Mutex};

use scitl_core::db::messages::Chat;
use scitl_core::db::SharedConnection;
use scitl_core::in_flight::InFlightSet;
use scitl_core::settings::Settings;
use tauri::Manager;

/// コマンド層(`commands/*.rs`)が触れる唯一の状態。ロックの扱いはどれもcore側に閉じる
/// (DBは`db::with_conn`、設定は`settings::Settings`、生成中の会話は`in_flight`)。
pub struct AppState {
    pub db: SharedConnection,
    pub settings: Arc<Settings>,
    /// 応答を生成中の会話(`orchestration::TurnContext::generating`)。
    pub generating: InFlightSet<Chat>,
}

fn main() {
    tauri::Builder::default()
        .plugin(navigation::guard())
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&app_data_dir)?;
            let conn = scitl_core::db::open(app_data_dir.join("scitl.sqlite3"))?;

            let settings = Settings::load(app_data_dir.join("config.toml"));

            app.manage(AppState {
                db: Arc::new(Mutex::new(conn)),
                settings: Arc::new(settings),
                generating: InFlightSet::new(),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::tasks::get_task_detail,
            commands::tasks::list_tasks,
            commands::tasks::create_task,
            commands::tasks::rename_task,
            commands::tasks::set_task_archived,
            commands::tasks::delete_task,
            commands::chat::open_task_chat,
            commands::chat::send_chat_message,
            commands::chat::list_chat_messages,
            commands::chat::edit_chat_message,
            commands::chat::retry_chat_message,
            commands::chat::delete_chat_message,
            commands::settings::get_settings,
            commands::settings::update_general_settings,
            commands::settings::get_display_language,
            commands::settings::update_language,
            commands::settings::update_tool_settings,
            commands::settings::add_provider,
            commands::settings::delete_provider,
            commands::settings::add_models,
            commands::settings::remove_model,
            commands::settings::set_model_visible,
            commands::settings::set_model_capability,
            commands::settings::set_model_context_length,
            commands::settings::reset_model_capabilities,
            commands::settings::detect_model_capabilities,
            commands::settings::list_provider_models,
            commands::settings::get_chat_models,
            commands::settings::select_chat_model,
            commands::settings::set_reasoning_effort,
            commands::mcp::add_mcp_server,
            commands::mcp::delete_mcp_server,
            commands::mcp::set_mcp_server_enabled,
            commands::mcp::set_mcp_tool_enabled,
            commands::mcp::fetch_mcp_tools,
            commands::link::inspect_link,
            commands::link::open_confirmed_link,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
