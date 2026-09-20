use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde_json::json;
use ulid::Ulid;

use crate::db::error::{CoreError, Result};
use crate::db::messages::{self, Kind, NewMessage, Role};
use crate::llm::{ChatMessage, FinishReason, LlmAdapter, ResponseEvent};
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
    system_prompt: Option<&str>,
) -> Result<Vec<ResponseEvent>> {
    let mut history = db_call(db.clone(), move |conn| {
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

    // システムプロンプトは会話履歴として保存せず、送信のたびに現在の設定値を先頭に足す
    // (設定画面(Issue #22)で変更したら次のターンから即座に反映されるべきであり、
    // 発言として`messages`に残す対象ではないため)。
    if let Some(prompt) = system_prompt.filter(|p| !p.is_empty()) {
        history.insert(
            0,
            ChatMessage {
                role: "system",
                content: prompt.to_string(),
            },
        );
    }

    let turn_id = Ulid::new().to_string();
    // 再試行(失敗後の再送)が無い限り1のまま。ツール呼び出しの複数ラウンドはリトライでは
    // ないため、ラウンドごとに増やさない(増やすとdata-model.mdの
    // 「turn_idごとの最新attempt_noのみ表示」規則により前のラウンドの記録が
    // 隠れてしまう)。
    let attempt_no: i64 = 1;
    let mut all_events = Vec::new();

    for _round in 1..=MAX_TOOL_ROUNDS {
        let events = adapter.send(&history, &tools::task_chat_tools()).await?;

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
                let result = db_call(db.clone(), move |conn| {
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
                // 「現在の状態」を今回のリクエスト限りでモデルに返し、確定した応答を得る。
                history.push(ChatMessage {
                    role: "user",
                    content: format!("[tool result: {}] {}", result.0, result.1),
                });
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
