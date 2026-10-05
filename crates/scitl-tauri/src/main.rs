#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod navigation;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use scitl_core::attachments::{AttachmentStore, Attachments, DropNotice};
use scitl_core::db::messages::Chat;
use scitl_core::db::SharedConnection;
use scitl_core::in_flight::InFlightSet;
use scitl_core::paths::{self, DataDirError, DataLayout};
use scitl_core::settings::Settings;
use tauri::ipc::Channel;
use tauri::{DragDropEvent, Manager, WindowEvent};

/// コマンド層(`commands/*.rs`)が触れる唯一の状態。ロックの扱いはどれもcore側に閉じる
/// (DBは`db::with_conn`、設定は`settings::Settings`、生成中の会話は`in_flight`)。
pub struct AppState {
    pub db: SharedConnection,
    pub settings: Arc<Settings>,
    /// 応答を生成中の会話(`orchestration::TurnContext::generating`)。
    pub generating: InFlightSet<Chat>,
    /// 送信前の添付と実体の置き場所(`orchestration::TurnContext::attachments`)。
    pub attachments: Arc<Attachments>,
    /// Markdownエクスポートの書き出し先。画面からは変えられない。
    pub export_dir: PathBuf,
    /// 窓にファイルが落とされたことの知らせ先(`commands::attachments::watch_dropped_files`)。
    pub dropped: Mutex<Option<Channel<DropNotice>>>,
}

/// データディレクトリを開けなかった理由。このときは`AppState`を置かず、画面はこれだけを表示する
/// (`commands::startup::get_startup_failure`)。
pub struct StartupFailure(pub DataDirError);

fn main() {
    let builder = tauri::Builder::default();
    // 2つ目の起動を、DBと設定を開く`setup`より前にここで終わらせる。そのため他の
    // プラグインより先に登録する。
    let builder = if single_instance_available() {
        builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            focus_main_window(app)
        }))
    } else {
        builder
    };
    builder
        .plugin(navigation::guard())
        .setup(|app| {
            let revealed = paths::revealed_attachments(&paths::default_cache_dir()?);
            match open_app_state(revealed) {
                Ok(state) => app.manage(state),
                Err(failure) => {
                    scitl_core::diagnostics::report(format_args!("could not start: {failure}"));
                    app.manage(StartupFailure(failure))
                }
            };
            Ok(())
        })
        // 窓に落としたファイルのパスは、OSのドロップからここへ直接届く(WebViewを通らない)。
        .on_window_event(|window, event| {
            if let WindowEvent::DragDrop(DragDropEvent::Drop { paths, .. }) = event {
                commands::attachments::receive_drop(window.app_handle(), paths.clone());
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::startup::get_startup_failure,
            commands::tasks::get_task_detail,
            commands::tasks::list_tasks,
            commands::tasks::create_task,
            commands::tasks::rename_task,
            commands::tasks::set_task_archived,
            commands::tasks::delete_task,
            commands::chat::open_task_chat,
            commands::chat::send_chat_message,
            commands::chat::list_chat_messages,
            commands::chat::chat_lacks_reply,
            commands::chat::generate_chat_reply,
            commands::chat::edit_chat_message,
            commands::chat::retry_chat_message,
            commands::chat::stop_chat_response,
            commands::chat::delete_chat_message,
            commands::attachments::stage_attachment,
            commands::attachments::discard_staged_attachment,
            commands::attachments::get_attachment_limits,
            commands::attachments::watch_dropped_files,
            commands::attachments::stage_dropped_file,
            commands::attachments::read_text_attachment,
            commands::attachments::read_image_attachment,
            commands::attachments::reveal_attachment,
            commands::export::export_markdown,
            commands::export::open_export_folder,
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

/// 実行ファイルの隣のデータディレクトリを開き、コマンド層の状態を作る。
fn open_app_state(revealed_attachments: PathBuf) -> Result<AppState, DataDirError> {
    let data = DataLayout::new(paths::data_dir_beside_executable()?);
    paths::create_private_dir(data.root()).map_err(|e| DataDirError::Unusable {
        dir: data.root().display().to_string(),
        reason: e.to_string(),
    })?;
    let conn = scitl_core::db::open(data.database())
        .map_err(|e| DataDirError::from_database(data.root(), &e))?;

    let settings = Settings::load(data.config());
    let attachments = Arc::new(Attachments::new(AttachmentStore::new(
        data.attachments(),
        revealed_attachments,
    )));

    Ok(AppState {
        db: Arc::new(Mutex::new(conn)),
        settings: Arc::new(settings),
        generating: InFlightSet::new(),
        attachments,
        export_dir: data.export(),
        dropped: Mutex::new(None),
    })
}

/// `tauri.conf.json`で作るウィンドウのラベル。
const MAIN_WINDOW: &str = "main";

/// 多重起動の防止を使えるか。Linuxのプラグインはセッションバスのアドレスを解釈できないと
/// 起動ごとpanicするので、同じ解釈で先に確かめ、使えなければ防止なしで起動する。
#[cfg(target_os = "linux")]
fn single_instance_available() -> bool {
    match zbus::Address::session() {
        Ok(_) => true,
        Err(e) => {
            scitl_core::diagnostics::report(format_args!(
                "starting without preventing a second instance: {e}"
            ));
            false
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn single_instance_available() -> bool {
    true
}

/// 既に開いているウィンドウを前に出す。2つ目の起動の引数と作業ディレクトリは、同じセッションの
/// どのプロセスからも送れるので使わない。Windowsではメインスレッドで呼ばれるので、待つ処理を置かない。
fn focus_main_window(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
        return;
    };
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
}

#[cfg(test)]
mod tests {
    /// coreの識別子をTauriの設定と照合する(`scitl_core::APP_IDENTIFIER`)。
    #[test]
    fn core_identifier_matches_tauri_config() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(conf["identifier"], scitl_core::APP_IDENTIFIER);
    }

    /// 外部ツールサーバーへ名乗る名前を、配布物の名前と照合する(`scitl_core::PRODUCT_NAME`)。
    #[test]
    fn core_product_name_matches_tauri_config() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(conf["productName"], scitl_core::PRODUCT_NAME);
    }

    #[test]
    fn main_window_label_matches_tauri_config() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let windows = conf["app"]["windows"].as_array().unwrap();
        assert!(windows
            .iter()
            .any(|window| window["label"] == super::MAIN_WINDOW));
    }
}
