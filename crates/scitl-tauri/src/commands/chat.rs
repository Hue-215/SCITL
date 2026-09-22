use tauri::State;

use scitl_core::config::McpServerConfig;
use scitl_core::llm::{LlmAdapter, ResponseEvent};
use scitl_core::orchestration::{
    delete_message, edit_user_message, retry_assistant_message, run_turn, McpAccess, SystemPrompts,
    ToolLimits,
};

use crate::AppState;

/// ターンの実行に要る設定の複製。送信・編集・再試行いずれも同じ組み立てを使う
/// (`docs/spec/principles.md` 5節、判断を1箇所に閉じる)。ロックはこの複製を取るまでだけ
/// 持つ(main.rsの`AppState::runtime`のドキュメント参照)。
struct TurnInputs {
    adapter: Option<std::sync::Arc<dyn LlmAdapter + Send + Sync>>,
    system_prompt: Option<String>,
    task_chat_system_prompt: Option<String>,
    /// ターン中に使う外部ツールサーバー(Issue #44)。設定の複製を持ち、ロックを
    /// `.await`へ持ち込まない。
    mcp_servers: Vec<McpServerConfig>,
    /// ツール呼び出しの上限(Issue #71)。`Option`の解釈はここで済ませ、ターンには
    /// 解決済みの値だけを渡す。
    tool_limits: ToolLimits,
}

fn load_turn_inputs(state: &State<'_, AppState>) -> TurnInputs {
    let runtime = state.runtime.lock().expect("runtime mutex poisoned");
    TurnInputs {
        adapter: runtime.adapter.clone(),
        system_prompt: runtime.config.general.system_prompt.clone(),
        task_chat_system_prompt: runtime.config.general.task_chat_system_prompt.clone(),
        mcp_servers: runtime.config.mcp_servers.clone(),
        tool_limits: ToolLimits::from_config(&runtime.config.tools),
    }
}

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
    let inputs = load_turn_inputs(&state);
    let adapter_ref: Option<&dyn LlmAdapter> =
        inputs.adapter.as_deref().map(|a| a as &dyn LlmAdapter);

    let prompts = SystemPrompts {
        base: inputs.system_prompt.as_deref(),
        task_chat: inputs.task_chat_system_prompt.as_deref(),
    };
    let mcp = McpAccess::new(&inputs.mcp_servers, &state.mcp_tools);

    run_turn(
        state.db.clone(),
        adapter_ref,
        task_id,
        text,
        &prompts,
        &mcp,
        inputs.tool_limits,
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
    let inputs = load_turn_inputs(&state);
    let adapter_ref: Option<&dyn LlmAdapter> =
        inputs.adapter.as_deref().map(|a| a as &dyn LlmAdapter);

    let prompts = SystemPrompts {
        base: inputs.system_prompt.as_deref(),
        task_chat: inputs.task_chat_system_prompt.as_deref(),
    };
    let mcp = McpAccess::new(&inputs.mcp_servers, &state.mcp_tools);

    edit_user_message(
        state.db.clone(),
        adapter_ref,
        task_id,
        message_id,
        text,
        &prompts,
        &mcp,
        inputs.tool_limits,
    )
    .await
    .map_err(|e| e.to_string())
}

/// 発言の再試行(Issue #41)。アシスタント発言のみが対象で、同じターンのまま
/// `attempt_no`を増やして応答を作り直す。
#[tauri::command]
pub async fn retry_task_chat_message(
    state: State<'_, AppState>,
    task_id: i64,
    message_id: i64,
) -> Result<Vec<ResponseEvent>, String> {
    let inputs = load_turn_inputs(&state);
    let adapter_ref: Option<&dyn LlmAdapter> =
        inputs.adapter.as_deref().map(|a| a as &dyn LlmAdapter);

    let prompts = SystemPrompts {
        base: inputs.system_prompt.as_deref(),
        task_chat: inputs.task_chat_system_prompt.as_deref(),
    };
    let mcp = McpAccess::new(&inputs.mcp_servers, &state.mcp_tools);

    retry_assistant_message(
        state.db.clone(),
        adapter_ref,
        task_id,
        message_id,
        &prompts,
        &mcp,
        inputs.tool_limits,
    )
    .await
    .map_err(|e| e.to_string())
}

/// 発言の削除(Issue #41)。ユーザー/アシスタント発言が対象で、確認ダイアログ無しの
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

/// タスクチャンネルの発言履歴取得(#37)。`commands::tasks`と同じ
/// `spawn_blocking` + ロックの型を踏襲する。
#[tauri::command]
pub async fn list_task_messages(
    state: State<'_, AppState>,
    task_id: i64,
) -> Result<Vec<scitl_core::db::messages::Message>, String> {
    let db = state.db.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let conn = db.lock().expect("db mutex poisoned");
        scitl_core::db::messages::list_for_task(&conn, task_id)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}
