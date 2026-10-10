mod commands;
mod dialog;
mod navigation;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use scitl_core::attachments::{AttachmentStore, Attachments, ReceivedFiles};
use scitl_core::db::messages::Chat;
use scitl_core::db::SharedConnection;
use scitl_core::in_flight::InFlightSet;
use scitl_core::orchestration::{FinishedTurn, TurnContext, TurnEvents};
use scitl_core::paths::{self, DataDirError, DataLayout};
use scitl_core::settings::{Settings, Snapshot};
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
    /// 利用者が選んだファイルへ書き写す前に、エクスポートのzipを作る場所(キャッシュの中)。
    pub export_staging: PathBuf,
    /// 窓にファイルが落とされたことの知らせ先(`commands::attachments::watch_dropped_files`)。
    pub dropped: Mutex<Option<Channel<ReceivedFiles>>>,
    /// 応答生成が終わったことの受け口(`orchestration::TurnContext::finished`)。
    pub finished: Box<dyn Fn(FinishedTurn) + Send + Sync>,
    /// 通知を押して開くことになった会話の知らせ先(`commands::requested_chat::watch_requested_chats`)。
    pub requested_chat: Mutex<Option<Channel<Chat>>>,
}

impl AppState {
    /// ターンに渡す文脈(`settings::Snapshot::turn_context`)。アプリの起動中ずっと同じものを渡す分を
    /// ここで足す。
    pub fn turn_context<'a>(
        &'a self,
        snapshot: &'a Snapshot,
        events: TurnEvents<'a>,
    ) -> TurnContext<'a> {
        snapshot.turn_context(
            &self.generating,
            &self.attachments,
            events,
            self.finished.as_ref(),
        )
    }
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
            #[cfg(target_os = "android")]
            match app.get_webview_window(MAIN_WINDOW) {
                Some(window) => init_jni_users(window.as_ref()),
                None => scitl_core::diagnostics::report(
                    "could not initialize the certificate verifier and the secret store: no window",
                ),
            }
            match start_app_state(app.handle()) {
                Ok(state) => app.manage(state),
                Err(failure) => {
                    scitl_core::diagnostics::report(format_args!("could not start: {failure}"));
                    app.manage(StartupFailure(failure))
                }
            };
            #[cfg(target_os = "android")]
            reload_key_after_secret_store_init(app.handle());
            Ok(())
        })
        // `setup`で証明書の検証・秘密情報の保存先を初期化できなかったときに頼み直す(`init_jni_users`)。
        .on_page_load(|_webview, _payload| {
            #[cfg(target_os = "android")]
            init_jni_users(_webview);
        })
        // 窓に落としたファイルのパスは、OSのドロップからここへ直接届く(WebViewを通らない)。
        .on_window_event(|window, event| match event {
            WindowEvent::DragDrop(DragDropEvent::Drop { paths, .. }) => {
                commands::attachments::receive_drop(window.app_handle(), paths.clone());
            }
            // 通知を押してアプリが前に出たら、開く会話の頼みを画面へ届ける。
            #[cfg(mobile)]
            WindowEvent::Resumed | WindowEvent::Focused(true) => {
                commands::requested_chat::deliver(window.app_handle());
            }
            _ => {}
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
            commands::export::get_export_target,
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
            commands::requested_chat::watch_requested_chats,
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
    open_app_state(data, &cache).map_err(|e| if cfg!(mobile) { e.in_app_dir() } else { e })
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

/// `data`のデータディレクトリを開き、コマンド層の状態を作る。`cache`はキャッシュの場所。
fn open_app_state(data: PathBuf, cache: &Path) -> Result<AppState, DataDirError> {
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
        paths::revealed_attachments(cache),
    )));

    let settings = Arc::new(settings);
    Ok(AppState {
        db,
        generating: generating_set(&settings),
        finished: finished_sink(&settings),
        settings,
        attachments,
        export_dir: data.export(),
        export_staging: paths::export_staging(cache),
        dropped: Mutex::new(None),
        requested_chat: Mutex::new(None),
    })
}

/// 応答を生成中の会話の集合。Androidでは、応答を生成している間だけフォアグラウンドサービスにする
/// (`architecture/concurrency.md`「Androidで裏へ回ったとき」)。通知の文面は表示言語で出す。
#[cfg(target_os = "android")]
fn generating_set(settings: &Arc<Settings>) -> InFlightSet<Chat> {
    let settings = Arc::clone(settings);
    InFlightSet::watching_long_running(move |generating| {
        scitl_core::foreground_service::set_running(generating, settings.display_language())
    })
}

#[cfg(not(target_os = "android"))]
fn generating_set(_settings: &Arc<Settings>) -> InFlightSet<Chat> {
    InFlightSet::new()
}

/// 応答生成が終わったことの受け口。Androidでは、利用者がアプリを見ていなければ通知で知らせる
/// (`architecture/concurrency.md`「Androidで裏へ回ったとき」)。通知の文面は表示言語で出す。
#[cfg(target_os = "android")]
fn finished_sink(settings: &Arc<Settings>) -> Box<dyn Fn(FinishedTurn) + Send + Sync> {
    let settings = Arc::clone(settings);
    Box::new(move |finished| {
        scitl_core::reply_notification::post(settings.display_language(), &finished)
    })
}

#[cfg(not(target_os = "android"))]
fn finished_sink(_settings: &Arc<Settings>) -> Box<dyn Fn(FinishedTurn) + Send + Sync> {
    Box::new(scitl_core::orchestration::discard_finished)
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

/// JNIの参照を要る部品へ渡す。HTTPSの証明書の検証(`scitl_core::net::android`)と秘密情報の保存先
/// (`scitl_core::secrets::android`)。JavaVMとActivityはwryがWebViewのスレッドで呼ぶコールバックから
/// しか得られないので、WebViewに頼み、渡すのは後になる。
///
/// HTTPSを使う経路はどれも画面からのIPCで始まり、起動時に裏で通信しないので、画面が読み込まれる
/// までに済む。間に合わなかったときや渡せなかったときは、`net::hardened_client`がHTTPSを断る。
/// 秘密情報は起動時の設定の読み込みで読むので、渡すより先に読んで「鍵を読めない」になりうる。
/// 渡せたら読み直させる(`reload_key_after_secret_store_init`)。
///
/// 渡せなかったまま使い続けないよう、`setup`のほかにページを読み込むたびにも、済んでいなければ頼み直す。
#[cfg(target_os = "android")]
fn init_jni_users(webview: &tauri::Webview) {
    if scitl_core::net::android::initialized() && scitl_core::secrets::android::initialized() {
        return;
    }
    let app = webview.app_handle().clone();
    let requested = webview.with_webview(move |webview| {
        webview.jni_handle().exec(move |env, activity, _webview| {
            let java_vm = match env.get_java_vm() {
                Ok(vm) => vm.get_java_vm_pointer(),
                Err(e) => {
                    scitl_core::diagnostics::report(format_args!(
                        "could not initialize the certificate verifier and the secret store: {e}"
                    ));
                    return;
                }
            };
            // SAFETY: `java_vm`はこのプロセスのJavaVM。`activity`はwryが持つActivityのグローバル参照
            // (Activityが無ければnull)で、このコールバックの間は有効。
            let verifier =
                unsafe { scitl_core::net::android::init(java_vm.cast(), activity.as_raw().cast()) };
            if let Err(e) = verifier {
                scitl_core::diagnostics::report(e);
            }
            // SAFETY: 上と同じ。
            let secrets = unsafe {
                scitl_core::secrets::android::init(java_vm.cast(), activity.as_raw().cast())
            };
            match secrets {
                Ok(()) => reload_key_after_secret_store_init(&app),
                Err(e) => scitl_core::diagnostics::report(e),
            }
        })
    });
    if let Err(e) = requested {
        scitl_core::diagnostics::report(format_args!(
            "could not initialize the certificate verifier and the secret store: {e}"
        ));
    }
}

/// 秘密情報の保存先を初期化できていれば、起動時に読み損ねた鍵を読み直させる。初期化と`AppState`の
/// 登録のどちらが先に済むかは決まらないので、両方の後で呼ぶ(後に済んだ側の呼び出しで、初期化済みかつ
/// 登録済みになる)。読めていれば何もしない。
#[cfg(target_os = "android")]
fn reload_key_after_secret_store_init(app: &AppHandle) {
    if !scitl_core::secrets::android::initialized() {
        return;
    }
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let settings = Arc::clone(&state.settings);
    tauri::async_runtime::spawn(async move { settings.reload_unavailable_key().await });
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

    /// 応答の生成中のサービスを、coreが名前で指せ、通知の文面を渡せること
    /// (`scitl_core::foreground_service`)。
    #[test]
    fn core_service_class_matches_android_manifest() {
        let manifest = include_str!("../gen/android/app/src/main/AndroidManifest.xml");
        assert!(manifest.contains(&format!(
            "android:name=\".{}\"",
            scitl_core::foreground_service::SERVICE_CLASS
        )));
        let kotlin =
            include_str!("../gen/android/app/src/main/java/net/niigo/scitl/GeneratingService.kt");
        assert!(kotlin.contains(&format!(
            "class {} :",
            scitl_core::foreground_service::SERVICE_CLASS
        )));
        for (name, value) in [
            ("EXTRA_TITLE", scitl_core::foreground_service::EXTRA_TITLE),
            (
                "EXTRA_CHANNEL",
                scitl_core::foreground_service::EXTRA_CHANNEL,
            ),
        ] {
            assert!(kotlin.contains(&format!("const val {name} = \"{value}\"")));
        }
    }

    /// 応答が終わったことの通知と、通知を押して開く会話の引き取りを、coreが名前と値で指せること
    /// (`scitl_core::reply_notification`)。
    #[test]
    fn core_reply_notification_names_match_kotlin() {
        use scitl_core::reply_notification::{
            ACTIVITY_CLASS, CHAT_GENERAL, CHAT_NONE, NOTIFIER_CLASS,
        };
        let notifier =
            include_str!("../gen/android/app/src/main/java/net/niigo/scitl/ReplyNotifier.kt");
        assert!(notifier.contains(&format!("object {NOTIFIER_CLASS} {{")));
        assert!(notifier.contains(
            "fun post(context: Context, chat: Long, title: String, body: String, channelName: String)"
        ));
        let activity =
            include_str!("../gen/android/app/src/main/java/net/niigo/scitl/MainActivity.kt");
        assert!(activity.contains(&format!("class {ACTIVITY_CLASS} :")));
        assert!(activity.contains("fun takeRequestedChat(): Long"));
        assert!(activity.contains(&format!("const val CHAT_GENERAL = {CHAT_GENERAL}L")));
        assert!(activity.contains(&format!("const val CHAT_NONE = {CHAT_NONE}L")));
        // 名前で引くクラスとメソッドを、配布用のビルドの難読化から外してあること。
        let proguard = include_str!("../gen/android/app/proguard-rules.pro");
        for class in [NOTIFIER_CLASS, ACTIVITY_CLASS] {
            assert!(proguard.contains(&format!("-keep class net.niigo.scitl.{class} {{")));
        }
    }

    /// 応答の生成中のサービスを、ほかのアプリから始められないこと。
    #[test]
    fn the_generating_service_is_not_exported() {
        let manifest = include_str!("../gen/android/app/src/main/AndroidManifest.xml");
        let (_, after) = manifest
            .split_once("<service")
            .expect("the manifest declares a service");
        let service = after.split("/>").next().unwrap();
        assert!(service.contains("android:exported=\"false\""));
        assert!(service.contains("android:foregroundServiceType=\"dataSync\""));
        assert!(!after.contains("<intent-filter"));
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
