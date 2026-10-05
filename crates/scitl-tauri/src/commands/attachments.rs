//! 添付ファイルのIPCコマンド。画面はファイルの中身を渡し、パスを受け取るコマンドは持たない。
//! WebViewを乗っ取られても、利用者が選んでいないファイルを読ませないため。窓に落としたファイルは
//! OSのドロップからパスがここへ直接届くので、画面には名前だけを知らせ、画面が受け付けたものを
//! Rust側で読む。

use std::path::PathBuf;
use std::sync::Arc;

use scitl_core::attachments::{DropNotice, PickingLimits, StageOutcome, LIMITS};
use tauri::ipc::{Channel, InvokeBody, Request};
use tauri::{AppHandle, Manager, State};

use super::{with_db, CommandResult};
use crate::AppState;

/// ファイル名を運ぶヘッダー。本文を生のバイト列で送るとJSONの引数を併せて持てないため、
/// 名前はヘッダーに載せる。ヘッダーはASCIIしか運べないので、画面が`encodeURIComponent`で
/// 符号化して入れる。
const FILE_NAME_HEADER: &str = "x-scitl-file-name";

/// 選んだファイルを預け、判定の結果を返す。本文はファイルの中身そのもの。判定は中身全体を
/// 走査し(UTF-8の検査)、画像はデコードし直す(正規化)ので、ブロッキング処理として呼ぶ。
#[tauri::command]
pub async fn stage_attachment(
    state: State<'_, AppState>,
    request: Request<'_>,
) -> CommandResult<StageOutcome> {
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err("attachment body must be raw bytes".into());
    };
    let name = request
        .headers()
        .get(FILE_NAME_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(percent_decode)
        .ok_or("attachment name header is missing or malformed")?;
    // 借りている本文を、ブロッキング処理のスレッドへ渡せるよう複製する(大きさは上限まで)。
    let bytes = bytes.clone();
    let attachments = Arc::clone(&state.attachments);
    Ok(scitl_core::blocking::run(move || attachments.stage(name, bytes)).await?)
}

/// 預けた添付を取り消す。知らないトークンは何もしない。
#[tauri::command]
pub fn discard_staged_attachment(state: State<'_, AppState>, token: String) {
    state.attachments.discard(&token);
}

/// 窓にファイルが落とされたことの知らせ先を受け取る。画面が起動時に渡し、渡し直したら
/// 置き換える。受け取るのは知らせ先だけで、パスは受け取らない。
#[tauri::command]
pub fn watch_dropped_files(state: State<'_, AppState>, on_drop: Channel<DropNotice>) {
    *state.dropped.lock().expect("dropped files mutex poisoned") = Some(on_drop);
}

/// 窓に落とされたファイルを受け取り、名前だけを画面へ知らせる。ここでは読まない。受け付けるか
/// (入力欄が出ているか・応答待ちでないか・1つの発言に付けられる数)は画面が決め、受け付けた
/// ものだけを[`stage_dropped_file`]で読ませる。画面が知らせ先を渡す前なら受け取らない。
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
    if let Some(notice) = state.attachments.receive_drop(paths) {
        let _ = channel.send(notice);
    }
}

/// 落とされたファイルのうち1つを読んで預け、判定の結果を返す。ファイルはドロップの番号と並びの
/// 位置で指し、パスは受け取らない。最後のドロップの、まだ読んでいないものだけを読める。
#[tauri::command]
pub async fn stage_dropped_file(
    state: State<'_, AppState>,
    drop_id: u64,
    index: usize,
) -> CommandResult<StageOutcome> {
    let attachments = Arc::clone(&state.attachments);
    Ok(scitl_core::blocking::run(move || attachments.stage_dropped(drop_id, index)).await?)
}

/// 受け付ける大きさの上限。画面が大きすぎるファイルを読む前に弾くのに使う。
#[tauri::command]
pub fn get_attachment_limits() -> PickingLimits {
    LIMITS.for_picking()
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

/// `encodeURIComponent`の逆。UTF-8として読めなければ`None`。`%`の後に16進2桁が続かない並びは
/// 符号化されていない文字としてそのまま残る(画面は必ず`encodeURIComponent`で符号化して送るので、
/// その形は届かない)。
fn percent_decode(encoded: &str) -> Option<String> {
    percent_encoding::percent_decode_str(encoded)
        .decode_utf8()
        .ok()
        .map(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_what_encode_uri_component_produces() {
        assert_eq!(
            percent_decode("%E8%B3%87%E6%96%99%20v2.pdf").as_deref(),
            Some("資料 v2.pdf")
        );
        assert_eq!(percent_decode("a%2Bb%25").as_deref(), Some("a+b%"));
        assert_eq!(percent_decode("%FF"), None);
        // 16進2桁の続かない`%`は、名前の一部としてそのまま残る。
        assert_eq!(percent_decode("bad%zz").as_deref(), Some("bad%zz"));
    }
}
