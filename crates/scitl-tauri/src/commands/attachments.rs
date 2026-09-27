//! 添付ファイル(Issue #21)。画面はファイルの中身を渡すだけで、パスを渡すコマンドは持たない。
//! WebViewを乗っ取られても、利用者が選んでいないファイルを読ませないため(architecture.md「添付」)。

use std::sync::Arc;

use scitl_core::attachments::{Limits, StageOutcome, LIMITS};
use tauri::ipc::{InvokeBody, Request};
use tauri::State;

use super::with_db;
use crate::AppState;

/// ファイル名を運ぶヘッダー。本文を生のバイト列で送るとJSONの引数を併せて持てないため、
/// 名前はヘッダーに載せる。ヘッダーはASCIIしか運べないので、画面が`encodeURIComponent`で
/// 符号化して入れる。
const FILE_NAME_HEADER: &str = "x-scitl-file-name";

/// 選んだファイルを預け、判定の結果を返す。本文はファイルの中身そのもの。判定は中身全体を
/// 走査する(UTF-8の検査)ので、ブロッキング処理として呼ぶ。
#[tauri::command]
pub async fn stage_attachment(
    state: State<'_, AppState>,
    request: Request<'_>,
) -> Result<StageOutcome, String> {
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err("attachment body must be raw bytes".to_string());
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
    scitl_core::blocking::run(move || attachments.stage(name, bytes))
        .await
        .map_err(|e| e.to_string())
}

/// 預けた添付を取り消す。知らないトークンは何もしない。
#[tauri::command]
pub fn discard_staged_attachment(state: State<'_, AppState>, token: String) {
    state.attachments.discard(&token);
}

/// 受け付ける大きさの上限。画面が大きすぎるファイルを読む前に弾くのに使う。
#[tauri::command]
pub fn get_attachment_limits() -> Limits {
    LIMITS
}

#[tauri::command]
pub async fn read_text_attachment(
    state: State<'_, AppState>,
    attachment_id: i64,
) -> Result<String, String> {
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
) -> Result<String, String> {
    state
        .attachments
        .image_data_url(state.db.clone(), attachment_id)
        .await
        .map_err(|e| e.to_string())
}

/// 添付を元の名前で書き出し、入っているフォルダを開く。
#[tauri::command]
pub async fn reveal_attachment(
    state: State<'_, AppState>,
    attachment_id: i64,
) -> Result<(), String> {
    state
        .attachments
        .reveal(state.db.clone(), attachment_id)
        .await
        .map_err(|e| e.to_string())
}

/// `encodeURIComponent`の逆。UTF-8として読めなければ`None`。
fn percent_decode(encoded: &str) -> Option<String> {
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
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
        assert_eq!(percent_decode("bad%2"), None);
        assert_eq!(percent_decode("bad%zz"), None);
        assert_eq!(percent_decode("%FF"), None);
    }
}
