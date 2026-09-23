use tauri::State;

use scitl_core::llm::ResponseEvent;
use scitl_core::orchestration::{delete_message, edit_user_message, retry_reply, run_turn};

use super::with_db;
use crate::AppState;

/// タスクチャットへの発言送信。`task_id`は文脈(表示中のタスク)から決まる引数であり、
/// モデルへのツール引数には出てこない(update_taskのタスクチャット版と同じ区別。
/// docs/spec/rebuild/tools.md 1節)。
///
/// プロバイダー未選択・モデル未選択・APIキー未設定・空応答等は`run_turn`内でエラー発言
/// として保存され`Ok`で返る(Issue #40)。ここで`Err`になるのはDB自体への書き込み失敗など、
/// 発言として保存すらできない場合のみ。
#[tauri::command]
pub async fn send_task_chat_message(
    state: State<'_, AppState>,
    task_id: i64,
    text: String,
) -> Result<Vec<ResponseEvent>, String> {
    // ロックは設定の複製を取るまでだけ持ち、ターンの`.await`へ持ち込まない。
    let turn = state.settings.snapshot();
    let mcp = turn.mcp();

    run_turn(
        state.db.clone(),
        turn.adapter(),
        task_id,
        text,
        &turn.prompts(),
        &mcp,
        turn.tool_limits(),
    )
    .await
    .map_err(|e| e.to_string())
}

/// 発言の編集(Issue #41)。ユーザー発言のみが対象で、対象以降の発言をすべて論理削除して
/// 編集後の内容から会話を再生成する。応答待ち中はフロントエンド側で操作自体を出さない
/// (本コマンドは全操作を停止させる専用のロックは持たず、既存のsend_task_chat_messageと
/// 同様にUI側の`sending`状態で直列化する設計を踏襲する)。
#[tauri::command]
pub async fn edit_task_chat_message(
    state: State<'_, AppState>,
    task_id: i64,
    message_id: i64,
    text: String,
) -> Result<Vec<ResponseEvent>, String> {
    // ロックは設定の複製を取るまでだけ持ち、ターンの`.await`へ持ち込まない。
    let turn = state.settings.snapshot();
    let mcp = turn.mcp();

    edit_user_message(
        state.db.clone(),
        turn.adapter(),
        task_id,
        message_id,
        text,
        &turn.prompts(),
        &mcp,
        turn.tool_limits(),
    )
    .await
    .map_err(|e| e.to_string())
}

/// 発言の再試行(Issue #41・#130)。ターンの返信(アシスタント発言・エラー発言)が対象で、
/// 同じターンのまま`attempt_no`を増やして応答を作り直す。
#[tauri::command]
pub async fn retry_task_chat_message(
    state: State<'_, AppState>,
    task_id: i64,
    message_id: i64,
) -> Result<Vec<ResponseEvent>, String> {
    // ロックは設定の複製を取るまでだけ持ち、ターンの`.await`へ持ち込まない。
    let turn = state.settings.snapshot();
    let mcp = turn.mcp();

    retry_reply(
        state.db.clone(),
        turn.adapter(),
        task_id,
        message_id,
        &turn.prompts(),
        &mcp,
        turn.tool_limits(),
    )
    .await
    .map_err(|e| e.to_string())
}

/// 発言の削除(Issue #41)。ユーザー発言とターンの返信が対象で、確認ダイアログ無しの
/// 即座に取り消し可能な論理削除。カスケードはしない(対象の1件だけを消す)。
#[tauri::command]
pub async fn delete_task_chat_message(
    state: State<'_, AppState>,
    task_id: i64,
    message_id: i64,
) -> Result<(), String> {
    delete_message(state.db.clone(), task_id, message_id)
        .await
        .map_err(|e| e.to_string())
}

/// タスクチャンネルの発言履歴取得(#37)。
#[tauri::command]
pub async fn list_task_messages(
    state: State<'_, AppState>,
    task_id: i64,
) -> Result<Vec<scitl_core::db::messages::Message>, String> {
    with_db(&state, move |conn| {
        scitl_core::db::messages::list_for_task(conn, task_id)
    })
    .await
}
