mod commands;
mod dialog;
mod navigation;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use scitl_core::attachments::{AttachmentStore, Attachments, ReceivedFiles};
use scitl_core::db::messages::Chat;
use scitl_core::db::SharedConnection;
use scitl_core::in_flight::InFlightSet;
use scitl_core::paths::{self, DataDirError, DataLayout};
use scitl_core::settings::Settings;
use tauri::ipc::Channel;
use tauri::{AppHandle, DragDropEvent, Manager, RunEvent, WindowEvent};

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
    pub dropped: Mutex<Option<Channel<ReceivedFiles>>>,
}

/// データディレクトリを開けなかった理由。このときは`AppState`を置かず、画面はこれだけを表示する
/// (`commands::startup::get_startup_failure`)。
pub struct StartupFailure(pub DataDirError);

/// デスクトップでは`main.rs`から、Androidでは`gen/android`のActivityから呼ばれる入口。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    // 2つ目の起動を、DBと設定を開く`setup`より前にここで終わらせる。そのため他の
    // プラグインより先に登録する。モバイルでは使わない(`concurrency.md`「多重起動の防止」)。
    #[cfg(desktop)]
    let builder = if single_instance_available() {
        builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            focus_main_window(app)
        }))
    } else {
        builder
    };
    // クリップボードの画像を読む(`commands/attachments.rs`)。Androidでは画像を読めないので
    // 登録しない。画面に権限は与えない。
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_clipboard_manager::init());
    // 選択画面が返す`content://`のURIを開く(`commands/attachments.rs`)と、本文中のリンクをOSへ
    // 渡す(`commands/link.rs`。デスクトップは`open`クレート)。画面に権限は与えない。
    #[cfg(target_os = "android")]
    let builder = builder
        .plugin(tauri_plugin_fs::init())
        // 画面へリンクのクリックを奪うスクリプトを差し込ませない(既定は差し込む)。
        .plugin(
            tauri_plugin_opener::Builder::new()
                .open_js_links_on_click(false)
                .build(),
        );
    builder
        .plugin(navigation::guard())
        // Rust側からだけ使う(`dialog.rs`・`commands/attachments.rs`)。画面に権限は与えない。
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            show_version_in_title(app);
            match start_app_state(app.handle()) {
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
            commands::chat::send_chat_message,
            commands::chat::list_chat_messages,
            commands::chat::chat_lacks_reply,
            commands::chat::generate_chat_reply,
            commands::chat::edit_chat_message,
            commands::chat::retry_chat_message,
            commands::chat::stop_chat_response,
            commands::chat::delete_chat_message,
            commands::attachments::discard_staged_attachment,
            commands::attachments::discard_all_staged_attachments,
            commands::attachments::watch_dropped_files,
            commands::attachments::pick_attachments,
            commands::attachments::paste_clipboard_image,
            commands::attachments::stage_received_file,
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
            commands::settings::get_base_url_hint,
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
            commands::memories::list_memories,
            commands::memories::add_memory,
            commands::memories::update_memory,
            commands::memories::delete_memory,
            commands::link::open_link,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // このあとプロセスは接続を閉じずに終わるので、WALにだけある変更をここで本体へ書き戻す。
            // Androidでは裏に回ったプロセスをOSが回収するときにここを通らないので、そのときは次の起動時の
            // 書き戻しに任せる。
            if let RunEvent::Exit = event {
                if let Some(state) = app.try_state::<AppState>() {
                    scitl_core::db::checkpoint_wal(&state.db);
                }
            }
        });
}

/// データディレクトリと、添付を開くときの書き出し先を決めて開き、コマンド層の状態を作る。
fn start_app_state(app: &AppHandle) -> Result<AppState, DataDirError> {
    let data = data_dir(app)?;
    let cache = cache_dir(app)?;
    // モバイルでは利用者が置き場所を変えられないので、場所を移すよう促す失敗にしない。
    open_app_state(data, paths::revealed_attachments(&cache)).map_err(|e| {
        if cfg!(mobile) {
            e.in_app_dir()
        } else {
            e
        }
    })
}

/// データディレクトリの場所(`data-model/tables.md`「データディレクトリの場所」)。デスクトップでは
/// 実行ファイルの隣で、フォルダごと持ち運べる。モバイルではOSがアプリに与えた内部ストレージ。
#[cfg(desktop)]
fn data_dir(_app: &AppHandle) -> Result<PathBuf, DataDirError> {
    paths::data_dir_beside_executable()
}

#[cfg(mobile)]
fn data_dir(app: &AppHandle) -> Result<PathBuf, DataDirError> {
    let dir = app.path().app_data_dir().map_err(no_app_dir)?;
    Ok(paths::data_dir_within(&dir))
}

/// キャッシュの場所。デスクトップではCLIと同じ場所を使うため、CLIと同じ`paths::default_cache_dir`で
/// 決める。モバイルではOSがアプリに与えたキャッシュの場所。
#[cfg(desktop)]
fn cache_dir(_app: &AppHandle) -> Result<PathBuf, DataDirError> {
    paths::default_cache_dir().map_err(no_app_dir)
}

#[cfg(mobile)]
fn cache_dir(app: &AppHandle) -> Result<PathBuf, DataDirError> {
    app.path().app_cache_dir().map_err(no_app_dir)
}

fn no_app_dir(e: impl std::fmt::Display) -> DataDirError {
    DataDirError::NoAppDir {
        reason: e.to_string(),
    }
}

/// `data`のデータディレクトリを開き、コマンド層の状態を作る。
fn open_app_state(data: PathBuf, revealed_attachments: PathBuf) -> Result<AppState, DataDirError> {
    let data = DataLayout::new(data);
    paths::create_private_dir(data.root()).map_err(|e| DataDirError::Unusable {
        dir: data.root().display().to_string(),
        reason: e.to_string(),
    })?;
    let conn = scitl_core::db::open(data.database())
        .map_err(|e| DataDirError::from_database(data.root(), &e))?;
    let db = Arc::new(Mutex::new(conn));
    // 前回の終了時に書き戻せなかった分(強制終了など)を、ここで本体へ書き戻す。
    scitl_core::db::checkpoint_wal(&db);

    let settings = Settings::load(data.config());
    let attachments = Arc::new(Attachments::new(AttachmentStore::new(
        data.attachments(),
        revealed_attachments,
    )));

    Ok(AppState {
        db,
        settings: Arc::new(settings),
        generating: InFlightSet::new(),
        attachments,
        export_dir: data.export(),
        dropped: Mutex::new(None),
    })
}

/// `tauri.conf.json`で作るウィンドウのラベル。
pub(crate) const MAIN_WINDOW: &str = "main";

/// ウィンドウ名(`tauri.conf.json`の`title`)の後ろに版を付ける。版は`Cargo.toml`の
/// ワークスペースの`version`で、版を書く場所を増やさないためここで組み立てる。
fn show_version_in_title(app: &tauri::App) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
        return;
    };
    let Ok(title) = window.title() else {
        return;
    };
    let _ = window.set_title(&format!("{title} {}", app.package_info().version));
}

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

#[cfg(all(desktop, not(target_os = "linux")))]
fn single_instance_available() -> bool {
    true
}

/// 既に開いているウィンドウを前に出す。2つ目の起動の引数と作業ディレクトリは、同じセッションの
/// どのプロセスからも送れるので使わない。Windowsではメインスレッドで呼ばれるので、待つ処理を置かない。
#[cfg(desktop)]
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

    /// CSPの`connect-src`が、IPCの窓口(Linux・macOSは`ipc:`、Windows・Androidは
    /// `http://ipc.localhost`)を許していること。塞いでもTauriは`postMessage`へ黙って切り替えて
    /// 動き続け、目に見える症状は8KBを超える途中経過がそのターンの間止まることだけなので、ここで
    /// 止める(`architecture/webview-boundary.md`「CSP / Tauri権限設定」)。
    #[test]
    fn csp_lets_the_webview_reach_the_ipc_endpoints() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        for key in ["csp", "devCsp"] {
            let csp = conf["app"]["security"][key].as_str().unwrap();
            let connect_src: Vec<&str> = csp
                .split(';')
                .find_map(|directive| directive.trim().strip_prefix("connect-src "))
                .unwrap_or_else(|| panic!("{key} has no connect-src"))
                .split_whitespace()
                .collect();
            for endpoint in ["ipc:", "http://ipc.localhost"] {
                assert!(connect_src.contains(&endpoint), "{key}: {connect_src:?}");
            }
        }
    }
}
