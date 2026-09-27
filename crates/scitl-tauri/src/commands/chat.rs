use tauri::ipc::Channel;
use tauri::State;

use scitl_core::db::messages::{self, Chat, Message};
use scitl_core::orchestration::{
    self, delete_message, edit_user_message, retry_reply, run_turn, TurnEvent, UserInput,
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

/// 発言送信。`chat`は文脈(表示中の会話)から決まる引数であり、モデルへのツール引数には
/// 出てこない(update_taskのタスクチャット版と同じ区別。docs/spec/rebuild/tools.md 1節)。
/// ターンの途中経過は`on_event`へ送る(編集・再試行も同じ。architecture.md 3節)。
/// `attachments`は`stage_attachment`が返したトークン。
///
/// プロバイダー未選択・モデル未選択・APIキー未設定・空応答等は`run_turn`内でエラー発言
/// として保存され`Ok`で返る(Issue #40)。ここで`Err`になるのはDB自体への書き込み失敗など、
/// 発言として保存すらできない場合のみ。
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

/// 聞き取りの開始(Issue #76)。作ったばかりのタスクで、ユーザーの発言なしにモデルの返信から
/// 会話を始める。
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

/// 発言の編集(Issue #41)。ユーザー発言のみが対象で、対象以降の発言をすべて論理削除して
/// 編集後の内容から会話を再生成する。
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

/// 発言の再試行(Issue #41・#130)。ターンの返信(アシスタント発言・エラー発言)が対象で、
/// 同じターンのまま`attempt_no`を増やして応答を作り直す。
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

/// 発言の削除(Issue #41)。ユーザー発言とターンの返信が対象で、確認ダイアログ無しの
/// 即座に取り消し可能な論理削除。カスケードはしない(対象の1件だけを消す)。
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

/// 会話の発言履歴取得(#37)。
#[tauri::command]
pub async fn list_chat_messages(
    state: State<'_, AppState>,
    chat: Chat,
) -> Result<Vec<Message>, String> {
    with_db(&state, move |conn| messages::list_for_chat(conn, chat)).await
}
