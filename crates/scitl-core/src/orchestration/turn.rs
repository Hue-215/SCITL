use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde_json::{json, Value};
use ulid::Ulid;

use crate::db::error::{CoreError, Result};
use crate::db::messages::{self, Kind, NewMessage, Role};
use crate::llm::{ChatMessage, FinishReason, LlmAdapter, ResponseEvent};
use crate::orchestration::state_prompt::build_system_prompt;
use crate::orchestration::SystemPrompts;
use crate::tools;

const MAX_TOOL_ROUNDS: u32 = 4;

/// DBハンドル。`rusqlite::Connection`は`Sync`ではないため`&Connection`を非同期関数の
/// awaitをまたいで持たせられない(architecture.md 4節)。ロックは常に`spawn_blocking`の
/// クロージャ内で取得・解放し、ロックガードが await をまたがないようにする。
pub type SharedConnection = Arc<Mutex<Connection>>;

/// 1ターンの処理フロー(architecture.md 1節)。ユーザー発言の保存 → LLM呼び出し →
/// (ツール呼び出しがあれば実行して結果を踏まえ再度呼び出し) → 確定した応答の保存、
/// までを1つの関数に閉じる(docs/spec/principles.md 5節)。
pub async fn run_turn(
    db: SharedConnection,
    adapter: &dyn LlmAdapter,
    task_id: i64,
    user_text: String,
    prompts: &SystemPrompts<'_>,
) -> Result<Vec<ResponseEvent>> {
    let history = db_call(db.clone(), move |conn| {
        messages::insert_message(
            conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::User,
                content: &user_text,
                kind: Kind::Normal,
                source: None,
                turn: None,
                is_error: false,
            },
        )?;
        build_history(conn, task_id)
    })
    .await?;

    let turn_id = Ulid::new().to_string();
    // 再試行(失敗後の再送)が無い限り1のまま。ツール呼び出しの複数ラウンドはリトライでは
    // ないため、ラウンドごとに増やさない(増やすとdata-model.mdの
    // 「turn_idごとの最新attempt_noのみ表示」規則により前のラウンドの記録が
    // 隠れてしまう)。
    let attempt_no: i64 = 1;
    let mut all_events = Vec::new();
    // 状態系ツールの実行結果は会話履歴に残さず、次ラウンドのシステムプロンプトの
    // 「最新状態」再構築で完全に代替する(docs/spec/rebuild/tools.md 4節)。同一ターン内の
    // 重複操作を防ぐため、実行済みの操作だけをここに積んで毎ラウンドのプロンプトに再掲する
    // (docs/spec/legacy/backend.md 4節 手順2)。
    let mut executed_ops: Vec<Value> = Vec::new();
    // `run_turn`はawaitをまたぐため、'staticなクロージャに載せられるよう所有した文字列に
    // 変換しておく(`SystemPrompts`自体はDBスレッドとやり取りするラウンドごとに組み直す)。
    let base_owned = prompts.base.map(str::to_string);
    let task_chat_owned = prompts.task_chat.map(str::to_string);

    for _round in 1..=MAX_TOOL_ROUNDS {
        let system_prompt_text = db_call(db.clone(), {
            let executed_ops = executed_ops.clone();
            let base_owned = base_owned.clone();
            let task_chat_owned = task_chat_owned.clone();
            move |conn| {
                let prompts = SystemPrompts {
                    base: base_owned.as_deref(),
                    task_chat: task_chat_owned.as_deref(),
                };
                build_system_prompt(conn, task_id, &prompts, &executed_ops)
            }
        })
        .await?;

        let mut messages_to_send = Vec::with_capacity(history.len() + 1);
        messages_to_send.push(ChatMessage {
            role: "system",
            content: system_prompt_text,
        });
        messages_to_send.extend(history.iter().cloned());

        let events = adapter.send(&messages_to_send, &tools::task_chat_tools()).await?;

        let mut text = String::new();
        let mut tool_call = None;
        for event in &events {
            match event {
                ResponseEvent::TextDelta { text: delta } => text.push_str(delta),
                ResponseEvent::ToolCall { name, arguments, .. } => {
                    tool_call = Some((name.clone(), arguments.clone()));
                }
                ResponseEvent::Done { .. } => {}
            }
        }
        all_events.extend(events);

        match tool_call {
            Some((name, arguments)) => {
                let turn_id = turn_id.clone();
                let (name, result) = db_call(db.clone(), move |conn| {
                    let result = tools::execute_task_chat_tool(conn, task_id, &name, &arguments)?;

                    messages::insert_message(
                        conn,
                        NewMessage {
                            task_id: Some(task_id),
                            role: Role::Assistant,
                            content: &json!({
                                "tool": name.clone(),
                                "arguments": arguments,
                                "result": result.clone(),
                            })
                            .to_string(),
                            kind: Kind::ToolExecution,
                            source: None,
                            turn: Some((&turn_id, attempt_no)),
                            is_error: false,
                        },
                    )?;
                    Ok((name, result))
                })
                .await?;

                // 状態系ツールの結果は会話履歴には投入しない(docs/spec/rebuild/tools.md 4節)。
                // 次ラウンドのシステムプロンプトの最新状態JSONが結果を完全に代替し、ここでは
                // 同一ターン内の重複操作を防ぐための再掲だけを積む。
                executed_ops.push(json!({ "tool": name, "result": result }));
            }
            None => {
                let turn_id = turn_id.clone();
                db_call(db, move |conn| {
                    messages::insert_message(
                        conn,
                        NewMessage {
                            task_id: Some(task_id),
                            role: Role::Assistant,
                            content: &text,
                            kind: Kind::Normal,
                            source: None,
                            turn: Some((&turn_id, attempt_no)),
                            is_error: false,
                        },
                    )?;
                    Ok(())
                })
                .await?;
                return Ok(all_events);
            }
        }
    }

    all_events.push(ResponseEvent::Done {
        finish_reason: FinishReason::Error,
    });
    Ok(all_events)
}

/// ロックの取得からドロップまでを`spawn_blocking`のクロージャ内に閉じ込める唯一の入口。
async fn db_call<F, T>(db: SharedConnection, f: F) -> Result<T>
where
    F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let conn = db.lock().expect("db mutex poisoned");
        f(&conn)
    })
    .await
    .map_err(|e| CoreError::Llm(format!("db task panicked: {e}")))?
}

fn build_history(conn: &Connection, task_id: i64) -> Result<Vec<ChatMessage>> {
    let stored = messages::list_for_task(conn, task_id)?;
    Ok(stored
        .into_iter()
        .filter(|m| m.kind == "normal")
        .map(|m| ChatMessage {
            role: if m.role == "user" { "user" } else { "assistant" },
            content: m.content,
        })
        .collect())
}
