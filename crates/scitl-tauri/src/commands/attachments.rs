//! 添付ファイルのIPCコマンド。画面はファイルのパスも中身も渡さない(WebViewを乗っ取られても、
//! 利用者が選んでいないファイルを読ませないため)。窓に落とした・選択画面で選んだ・クリップボードの
//! 画像は、どれもRust側がOSから受け取り、画面には名前だけを知らせ、画面が受け付けたものをRust側で
//! 読んで預かる(`docs/spec/architecture/attachments.md`「受け取り方」)。

use std::path::PathBuf;
use std::sync::Arc;

use scitl_core::attachments::{ReceivedFile, ReceivedFiles, StageOutcome};
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::{DialogExt, FilePath};

use super::{with_db, CommandResult};
use crate::AppState;

/// 預けた添付を取り消す。知らないトークンは何もしない。
#[tauri::command]
pub fn discard_staged_attachment(state: State<'_, AppState>, token: String) {
    state.attachments.discard(&token);
}

/// 送っていない添付をすべて捨てる。画面が起動のたびに呼ぶ(読み込み直された画面は入力欄の添付を
/// 持たないので、残った預かりが1つの発言に付けられる数を埋めたままにならないように)。
#[tauri::command]
pub fn discard_all_staged_attachments(state: State<'_, AppState>) {
    state.attachments.discard_all();
}

/// 窓にファイルが落とされたことの知らせ先を受け取る。画面が起動時に渡し、渡し直したら
/// 置き換える。受け取るのは知らせ先だけで、パスは受け取らない。
#[tauri::command]
pub fn watch_dropped_files(state: State<'_, AppState>, on_drop: Channel<ReceivedFiles>) {
    *state.dropped.lock().expect("dropped files mutex poisoned") = Some(on_drop);
}

/// 窓に落とされたファイルを受け取り、名前だけを画面へ知らせる。ここでは読まない。受け付けるか
/// (入力欄が出ているか・応答待ちでないか)は画面が決め、受け付けたものだけを
/// [`stage_received_file`]で読ませる。画面が知らせ先を渡す前なら受け取らない。
pub fn receive_drop(app: &AppHandle, paths: Vec<PathBuf>) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let Some(channel) = state
        .dropped
        .lock()
        .expect("dropped files mutex poisoned")
        .clone()
    else {
        return;
    };
    let files = paths.into_iter().map(ReceivedFile::Path).collect();
    if let Some(notice) = state.attachments.receive(files) {
        let _ = channel.send(notice);
    }
}

/// OSの選択画面でファイルを選ばせ、名前だけを返す(取りやめたら`None`)。読むのは、画面が受け付けた
/// ものを[`stage_received_file`]で指したとき。何を読むかは利用者が選ぶので、呼べる時機は絞らない
/// (乗っ取られた画面から呼ばれても、選ばれない限り何も読まない)。選択画面を閉じるまで待つので、
/// ブロッキング処理として呼ぶ。
#[tauri::command]
pub async fn pick_attachments(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CommandResult<Option<ReceivedFiles>> {
    let attachments = Arc::clone(&state.attachments);
    Ok(scitl_core::blocking::run(move || {
        let dialog = app.dialog().file();
        // 窓の前に出し、閉じるまで窓を操作させない(モバイルの選択画面は元から画面の前に出る)。
        #[cfg(desktop)]
        let dialog = match app.get_webview_window(crate::MAIN_WINDOW) {
            Some(window) => dialog.set_parent(&window),
            None => dialog,
        };
        let Some(picked) = dialog.blocking_pick_files() else {
            return Ok(None);
        };
        let files = picked
            .into_iter()
            .filter_map(|path| received_file(&app, path))
            .collect();
        Ok(attachments.receive(files))
    })
    .await?)
}

/// 選択画面が返したものを、読み方とともに受け取る形にする。デスクトップはパスで、Androidは
/// `content://`のURIで届く(URIは`tauri-plugin-fs`で開く。画面に権限は与えない)。
fn received_file(app: &AppHandle, path: FilePath) -> Option<ReceivedFile> {
    match path {
        FilePath::Path(path) => Some(ReceivedFile::Path(path)),
        #[cfg(target_os = "android")]
        FilePath::Url(url) => {
            use tauri_plugin_fs::{FsExt, OpenOptions};
            let name = scitl_core::attachments::name_from_uri(url.as_str());
            let app = app.clone();
            Some(ReceivedFile::Opened {
                name,
                open: Box::new(move || {
                    let mut options = OpenOptions::new();
                    options.read(true);
                    app.fs().open(url, options)
                }),
            })
        }
        #[cfg(not(target_os = "android"))]
        FilePath::Url(url) => {
            let _ = app;
            url.to_file_path().ok().map(ReceivedFile::Path)
        }
    }
}

/// 文字の無い貼り付けが起きたときに呼ばれ、クリップボードの画像を受け取って名前だけを返す
/// (画像が無ければ`None`)。読むのは画像だけで、文字やファイルの一覧(ファイル管理ソフトで
/// コピーしたファイル)は読まない。乗っ取られた画面からもいつでも呼べるが、読んだ画像は預かりに
/// 入るだけで、外へ出すには送信が要り、送り先は利用者が確かめて登録したものに限られる。
/// Androidではクリップボードの画像を読めないので、いつも`None`(`attachments.md`「受け取り方」)。
#[tauri::command]
pub async fn paste_clipboard_image(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CommandResult<Option<ReceivedFiles>> {
    let attachments = Arc::clone(&state.attachments);
    // Linuxではメインスレッドで読むと止まりうる(clipboard-managerの注意書き)ので、ブロッキング
    // 処理として呼ぶ。
    Ok(scitl_core::blocking::run(move || {
        let Some(file) = clipboard_image(&app)? else {
            return Ok(None);
        };
        Ok(attachments.receive(vec![file]))
    })
    .await?)
}

#[cfg(desktop)]
fn clipboard_image(app: &AppHandle) -> scitl_core::error::Result<Option<ReceivedFile>> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    // 画像が載っていない(文字だけ・空)ときも失敗として返るので、無いものとして扱う。
    let Ok(image) = app.clipboard().read_image() else {
        return Ok(None);
    };
    let (width, height) = (image.width(), image.height());
    scitl_core::attachments::clipboard_image(width, height, image.rgba().to_vec()).map(Some)
}

#[cfg(mobile)]
fn clipboard_image(_app: &AppHandle) -> scitl_core::error::Result<Option<ReceivedFile>> {
    Ok(None)
}

/// 受け取ったファイル(落とした・選んだ・貼り付けた)のうち1つを読んで預け、判定の結果を返す。
/// ファイルは受け取りの番号と並びの位置で指し、パスは受け取らない。最後に受け取った分の、まだ
/// 読んでいないものだけを読める。
#[tauri::command]
pub async fn stage_received_file(
    state: State<'_, AppState>,
    batch_id: u64,
    index: usize,
) -> CommandResult<StageOutcome> {
    let attachments = Arc::clone(&state.attachments);
    Ok(scitl_core::blocking::run(move || attachments.stage_received(batch_id, index)).await?)
}

#[tauri::command]
pub async fn read_text_attachment(
    state: State<'_, AppState>,
    attachment_id: i64,
) -> CommandResult<String> {
    let attachments = Arc::clone(&state.attachments);
    with_db(&state, move |conn| {
        attachments.read_text(conn, attachment_id)
    })
    .await
}

/// 画像の添付をdata URLで返す(サムネイルと拡大表示)。
#[tauri::command]
pub async fn read_image_attachment(
    state: State<'_, AppState>,
    attachment_id: i64,
) -> CommandResult<String> {
    Ok(state
        .attachments
        .image_data_url(state.db.clone(), attachment_id)
        .await?)
}

/// 添付を元の名前で書き出し、入っているフォルダを開く。
#[tauri::command]
pub async fn reveal_attachment(
    state: State<'_, AppState>,
    attachment_id: i64,
) -> CommandResult<()> {
    Ok(state
        .attachments
        .reveal(state.db.clone(), attachment_id)
        .await?)
}
