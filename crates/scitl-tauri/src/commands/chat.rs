use tauri::ipc::Channel;
use tauri::State;

use scitl_core::db::messages::Chat;
use scitl_core::orchestration::{
    self, delete_message, edit_user_message, retry_reply, run_turn, stop_response, MessageView,
    TurnEvent, UserInput,
};

use super::{with_db, CommandResult};
use crate::AppState;

/// ターンの途中経過を`channel`へ送る受け口。送れなくても(画面が閉じた等)ターンは最後まで
/// 走らせて保存するので、送信の失敗は捨てる。
pub(super) fn forward(channel: &Channel<TurnEvent>) -> impl Fn(TurnEvent) + Send + Sync + '_ {
    move |event| {
        let _ = channel.send(event);
    }
}

/// 発言を送り、応答を生成する([`run_turn`])。`chat`は表示中の会話で、ターンの途中経過は
/// `on_event`へ送る。`attachments`は`stage_attachment`が返したトークン。
#[tauri::command]
pub async fn send_chat_message(
    state: State<'_, AppState>,
    chat: Chat,
    text: String,
    attachments: Vec<String>,
    on_event: Channel<TurnEvent>,
) -> CommandResult<()> {
    // ロックは設定の複製を取るまでだけ持ち、ターンの`.await`へ持ち込まない。
    let snapshot = state.settings.snapshot_for_turn().await;
    let events = forward(&on_event);
    Ok(run_turn(
        state.db.clone(),
        &snapshot.turn_context(&state.generating, &state.attachments, &events),
        chat,
        UserInput { text, attachments },
    )
    .await?)
}

/// ユーザー発言を編集し、そこから応答を生成し直す([`edit_user_message`])。
#[tauri::command]
pub async fn edit_chat_message(
    state: State<'_, AppState>,
    chat: Chat,
    message_id: i64,
    text: String,
    on_event: Channel<TurnEvent>,
) -> CommandResult<()> {
    let snapshot = state.settings.snapshot_for_turn().await;
    let events = forward(&on_event);
    Ok(edit_user_message(
        state.db.clone(),
        &snapshot.turn_context(&state.generating, &state.attachments, &events),
        chat,
        message_id,
        text,
    )
    .await?)
}

/// ターンの返信を作り直す([`retry_reply`])。
#[tauri::command]
pub async fn retry_chat_message(
    state: State<'_, AppState>,
    chat: Chat,
    message_id: i64,
    on_event: Channel<TurnEvent>,
) -> CommandResult<()> {
    let snapshot = state.settings.snapshot_for_turn().await;
    let events = forward(&on_event);
    Ok(retry_reply(
        state.db.clone(),
        &snapshot.turn_context(&state.generating, &state.attachments, &events),
        chat,
        message_id,
    )
    .await?)
}

/// 返信の無いまま終わった会話に、応答を生成する([`orchestration::generate_reply`])。
#[tauri::command]
pub async fn generate_chat_reply(
    state: State<'_, AppState>,
    chat: Chat,
    on_event: Channel<TurnEvent>,
) -> CommandResult<()> {
    let snapshot = state.settings.snapshot_for_turn().await;
    let events = forward(&on_event);
    Ok(orchestration::generate_reply(
        state.db.clone(),
        &snapshot.turn_context(&state.generating, &state.attachments, &events),
        chat,
    )
    .await?)
}

/// 会話で生成中の応答を止める([`stop_response`])。
#[tauri::command]
pub fn stop_chat_response(state: State<'_, AppState>, chat: Chat) -> bool {
    stop_response(&state.generating, chat)
}

/// 発言と、それより後ろの発言をまとめて削除する([`delete_message`])。
#[tauri::command]
pub async fn delete_chat_message(
    state: State<'_, AppState>,
    chat: Chat,
    message_id: i64,
) -> CommandResult<()> {
    Ok(delete_message(state.db.clone(), &state.generating, chat, message_id).await?)
}

/// 会話が返信の無いまま終わっているか([`orchestration::lacks_reply`])。
#[tauri::command]
pub async fn chat_lacks_reply(state: State<'_, AppState>, chat: Chat) -> CommandResult<bool> {
    with_db(&state, move |conn| orchestration::lacks_reply(conn, chat)).await
}

/// 会話の発言を表示用に取得する。
#[tauri::command]
pub async fn list_chat_messages(
    state: State<'_, AppState>,
    chat: Chat,
) -> CommandResult<Vec<MessageView>> {
    with_db(&state, move |conn| orchestration::list_chat(conn, chat)).await
}
