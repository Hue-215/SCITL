//! Markdownエクスポートのコマンド。どれもパスを受け取らない。書き出し先は、起動時に決めた
//! `AppState::export_dir`か、利用者がOSの保存画面で選んだファイル(`ExportTarget::CURRENT`)。

use std::sync::Arc;

use scitl_core::export::{ExportOutcome, ExportTarget, OpenChosen};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::{DialogExt, FilePath};

use super::CommandResult;
use crate::AppState;

/// このOSで書き出す先。画面は、押せる操作と結果の文言を出し分けるために使う。
#[tauri::command]
pub fn get_export_target() -> ExportTarget {
    ExportTarget::CURRENT
}

/// 書き出す。選んだファイルへ書くOSでは、Rust側から保存画面を出し、閉じるまで待つ。何を書くかは
/// 利用者が選ぶので、乗っ取られた画面から呼ばれても、選ばれない限りキャッシュの外には何も書かない。
#[tauri::command]
pub async fn export_markdown(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CommandResult<ExportOutcome> {
    let attachments = Arc::clone(&state.attachments);
    let outcome = match ExportTarget::CURRENT {
        ExportTarget::Folder => ExportOutcome::Written {
            summary: scitl_core::export::export_markdown(
                state.db.clone(),
                &attachments,
                state.export_dir.clone(),
            )
            .await?,
        },
        ExportTarget::ChosenFile => {
            scitl_core::export::export_to_chosen_file(
                state.db.clone(),
                &attachments,
                state.export_staging.clone(),
                move |name| choose_file(&app, name),
            )
            .await?
        }
    };
    Ok(outcome)
}

/// 書き出し先のフォルダを開く。外部プロセスの起動を伴うため`blocking::run`で呼ぶ。
#[tauri::command]
pub async fn open_export_folder(state: State<'_, AppState>) -> CommandResult<()> {
    let root = state.export_dir.clone();
    Ok(scitl_core::blocking::run(move || scitl_core::export::open_folder(&root)).await?)
}

/// OSの保存画面でzipの保存先を選ばせ、書き込み用の開き方を返す(取りやめたら`None`)。
fn choose_file(app: &AppHandle, name: &str) -> Option<OpenChosen> {
    let dialog = app
        .dialog()
        .file()
        .set_file_name(name)
        .add_filter("ZIP", &["zip"]);
    // 窓の前に出し、閉じるまで窓を操作させない(モバイルの保存画面は元から画面の前に出る)。
    #[cfg(desktop)]
    let dialog = {
        use tauri::Manager;
        match app.get_webview_window(crate::MAIN_WINDOW) {
            Some(window) => dialog.set_parent(&window),
            None => dialog,
        }
    };
    dialog
        .blocking_save_file()
        .map(|path| open_for_writing(app, path))
}

/// 保存画面が返したものを、中身を置き換えて書く形で開く。デスクトップはパスで、Androidは
/// `content://`のURIで届く(URIは`tauri-plugin-fs`で開く。画面に権限は与えない)。
fn open_for_writing(app: &AppHandle, path: FilePath) -> OpenChosen {
    match path {
        FilePath::Path(path) => Box::new(move || std::fs::File::create(path)),
        #[cfg(target_os = "android")]
        FilePath::Url(url) => {
            use tauri_plugin_fs::{FsExt, OpenOptions};
            let app = app.clone();
            Box::new(move || {
                let mut options = OpenOptions::new();
                options.write(true).truncate(true);
                app.fs().open(url, options)
            })
        }
        #[cfg(not(target_os = "android"))]
        FilePath::Url(url) => {
            let _ = app;
            Box::new(move || match url.to_file_path() {
                Ok(path) => std::fs::File::create(path),
                Err(()) => Err(std::io::Error::other("the chosen location is not a file")),
            })
        }
    }
}
