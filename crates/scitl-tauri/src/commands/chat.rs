use tauri::ipc::Channel;
use tauri::State;

use scitl_core::db::messages::Chat;
use scitl_core::orchestration::{
    self, delete_message, edit_user_message, retry_reply, run_turn, stop_response, MessageView,
    TurnEvent, UserInput,
};

use super::with_db;
use crate::AppState;

/// ターンの途中経過を`channel`へ送る受け口。送れなくても(画面が閉じた等)ターンは最後まで
/// 走らせて保存するので、送信の失敗は捨てる。
fn forward(channel: &Channel<TurnEvent>) -> impl Fn(TurnEvent) + Send + Sync + '_ {
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
) -> Result<(), String> {
    // ロックは設定の複製を取るまでだけ持ち、ターンの`.await`へ持ち込まない。
    let snapshot = state.settings.snapshot_for_turn().await;
    let events = forward(&on_event);
    run_turn(
        state.db.clone(),
        &snapshot.turn_context(&state.generating, &state.attachments, &events),
        chat,
        UserInput { text, attachments },
    )
    .await
    .map_err(|e| e.to_string())
}

/// 作ったばかりのタスクで、ユーザーの発言なしにモデルの返信から聞き取りを始める。
#[tauri::command]
pub async fn open_task_chat(
    state: State<'_, AppState>,
    task_id: i64,
    on_event: Channel<TurnEvent>,
) -> Result<(), String> {
    let snapshot = state.settings.snapshot_for_turn().await;
    let events = forward(&on_event);
    orchestration::open_task_chat(
        state.db.clone(),
        &snapshot.turn_context(&state.generating, &state.attachments, &events),
        task_id,
    )
    .await
    .map_err(|e| e.to_string())
}

/// ユーザー発言を編集し、そこから応答を生成し直す([`edit_user_message`])。
#[tauri::command]
pub async fn edit_chat_message(
    state: State<'_, AppState>,
    chat: Chat,
    message_id: i64,
    text: String,
    on_event: Channel<TurnEvent>,
) -> Result<(), String> {
    let snapshot = state.settings.snapshot_for_turn().await;
    let events = forward(&on_event);
    edit_user_message(
        state.db.clone(),
        &snapshot.turn_context(&state.generating, &state.attachments, &events),
        chat,
        message_id,
        text,
    )
    .await
    .map_err(|e| e.to_string())
}

/// ターンの返信を作り直す([`retry_reply`])。
#[tauri::command]
pub async fn retry_chat_message(
    state: State<'_, AppState>,
    chat: Chat,
    message_id: i64,
    on_event: Channel<TurnEvent>,
) -> Result<(), String> {
    let snapshot = state.settings.snapshot_for_turn().await;
    let events = forward(&on_event);
    retry_reply(
        state.db.clone(),
        &snapshot.turn_context(&state.generating, &state.attachments, &events),
        chat,
        message_id,
    )
    .await
    .map_err(|e| e.to_string())
}

/// 会話で生成中の応答を止める([`stop_response`])。
#[tauri::command]
pub fn stop_chat_response(state: State<'_, AppState>, chat: Chat) {
    stop_response(&state.generating, chat);
}

/// 発言と、それより後ろの発言をまとめて削除する([`delete_message`])。
#[tauri::command]
pub async fn delete_chat_message(
    state: State<'_, AppState>,
    chat: Chat,
    message_id: i64,
) -> Result<(), String> {
    delete_message(state.db.clone(), &state.generating, chat, message_id)
        .await
        .map_err(|e| e.to_string())
}

/// 会話の発言を表示用に取得する。
#[tauri::command]
pub async fn list_chat_messages(
    state: State<'_, AppState>,
    chat: Chat,
) -> Result<Vec<MessageView>, String> {
    with_db(&state, move |conn| orchestration::list_chat(conn, chat)).await
}
