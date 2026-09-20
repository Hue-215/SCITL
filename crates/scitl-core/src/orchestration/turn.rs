use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde_json::json;
use ulid::Ulid;

use crate::db::error::{CoreError, Result};
use crate::db::messages::{self, Kind, NewMessage, Role};
use crate::llm::{ChatMessage, FinishReason, LlmAdapter, ResponseEvent, ToolCallRequest};
use crate::orchestration::state_prompt::build_system_prompt;
use crate::orchestration::turn_error::{self, TurnFailure};
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
///
/// `adapter`が`None`(プロバイダー未選択)・モデル未選択・APIキー未設定・空応答・
/// コンテキスト超過・ツール呼び出し回数の上限到達は、`Err`で上位に返さずエラー発言として
/// 保存し`Ok`で返す(Issue #40)。DB自体への書き込みが失敗する場合のみ`Err`のまま返る。
pub async fn run_turn(
    db: SharedConnection,
    adapter: Option<&dyn LlmAdapter>,
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
                error_kind: None,
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

    let Some(adapter) = adapter else {
        return fail_turn(db, task_id, &turn_id, attempt_no, TurnFailure::NoProvider).await;
    };
    if let Some(failure) = turn_error::from_readiness(adapter.readiness()) {
        return fail_turn(db, task_id, &turn_id, attempt_no, failure).await;
    }

    let mut all_events = Vec::new();
    // `run_turn`はawaitをまたぐため、'staticなクロージャに載せられるよう所有した文字列に
    // 変換しておく(`SystemPrompts`自体はDBスレッドとやり取りするラウンドごとに組み直す)。
    let base_owned = prompts.base.map(str::to_string);
    let task_chat_owned = prompts.task_chat.map(str::to_string);
    // 同一ターン内のツール呼び出し往復。分類(状態系/事実系)によらずモデルに返す
    // (docs/spec/rebuild/tools.md 4節「同一ターン内では分類によらず結果を返す」)。
    // このターンのリクエスト組み立てにのみ使い、DBの`messages`テーブルには書かない
    // (書くと次ターン以降の履歴に残ってしまう)。
    let mut round_trip: Vec<ChatMessage> = Vec::new();

    for _round in 1..=MAX_TOOL_ROUNDS {
        let system_prompt_text = db_call(db.clone(), {
            let base_owned = base_owned.clone();
            let task_chat_owned = task_chat_owned.clone();
            move |conn| {
                let prompts = SystemPrompts {
                    base: base_owned.as_deref(),
                    task_chat: task_chat_owned.as_deref(),
                };
                build_system_prompt(conn, task_id, &prompts)
            }
        })
        .await?;

        let mut messages_to_send =
            Vec::with_capacity(1 + history.len() + round_trip.len());
        messages_to_send.push(ChatMessage::System(system_prompt_text));
        messages_to_send.extend(history.iter().cloned());
        messages_to_send.extend(round_trip.iter().cloned());

        let events = match adapter.send(&messages_to_send, &tools::task_chat_tools()).await {
            Ok(events) => events,
            Err(e) => {
                let failure = turn_error::classify(&e);
                return fail_turn(db, task_id, &turn_id, attempt_no, failure).await;
            }
        };

        let mut text = String::new();
        let mut tool_calls: Vec<ToolCallRequest> = Vec::new();
        for event in &events {
            match event {
                ResponseEvent::TextDelta { text: delta } => text.push_str(delta),
                ResponseEvent::ToolCall { id, name, arguments } => {
                    tool_calls.push(ToolCallRequest {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: arguments.clone(),
                    });
                }
                ResponseEvent::Done { .. } => {}
            }
        }
        all_events.extend(events);

        if tool_calls.is_empty() {
            if text.is_empty() {
                return fail_turn(db, task_id, &turn_id, attempt_no, TurnFailure::EmptyResponse)
                    .await;
            }

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
                        error_kind: None,
                    },
                )?;
                Ok(())
            })
            .await?;
            return Ok(all_events);
        }

        // 1応答に複数のtool_callsが載る場合、すべて実行する(取りこぼさない)。
        let turn_id_for_db = turn_id.clone();
        let pending = tool_calls.clone();
        let executed: Vec<(ToolCallRequest, serde_json::Value)> =
            db_call(db.clone(), move |conn| {
                let mut out = Vec::with_capacity(pending.len());
                for call in pending {
                    let result =
                        tools::execute_task_chat_tool(conn, task_id, &call.name, &call.arguments)?;

                    messages::insert_message(
                        conn,
                        NewMessage {
                            task_id: Some(task_id),
                            role: Role::Assistant,
                            content: &json!({
                                "tool": call.name.clone(),
                                "arguments": call.arguments.clone(),
                                "result": result.clone(),
                            })
                            .to_string(),
                            kind: Kind::ToolExecution,
                            source: None,
                            turn: Some((&turn_id_for_db, attempt_no)),
                            error_kind: None,
                        },
                    )?;
                    out.push((call, result));
                }
                Ok(out)
            })
            .await?;

        // モデルへの往復: assistant(tool_calls) 1件 + tool(結果) を呼び出し数ぶん。
        // OpenAI互換プロトコルの標準的な表現に合わせる(architecture.md 3節)。
        round_trip.push(ChatMessage::Assistant {
            content: if text.is_empty() { None } else { Some(text) },
            tool_calls: executed.iter().map(|(call, _)| call.clone()).collect(),
        });
        for (call, result) in executed {
            round_trip.push(ChatMessage::Tool {
                tool_call_id: call.id,
                content: result.to_string(),
            });
        }
    }

    fail_turn(db, task_id, &turn_id, attempt_no, TurnFailure::ToolRoundLimit).await
}

/// エラー発言(`role='error'`)を保存する唯一の入口。`content`は`failure.user_message()`
/// (定型文言、`Unexpected`の場合のみsanitize済みの詳細を含む)。
async fn fail_turn(
    db: SharedConnection,
    task_id: i64,
    turn_id: &str,
    attempt_no: i64,
    failure: TurnFailure,
) -> Result<Vec<ResponseEvent>> {
    let turn_id = turn_id.to_string();
    let content = failure.user_message();
    let error_kind = failure.kind();
    db_call(db, move |conn| {
        messages::insert_message(
            conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::Error,
                content: &content,
                kind: Kind::Normal,
                source: None,
                turn: Some((&turn_id, attempt_no)),
                error_kind: Some(error_kind),
            },
        )?;
        Ok(())
    })
    .await?;
    Ok(vec![ResponseEvent::Done {
        finish_reason: FinishReason::Error,
    }])
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

/// API送信用の履歴。エラー発言(`role='error'`)は除外する
/// (`legacy/backend.md` 4節手順2「エラー発言・ツール実行記録はこのAPI送信用の履歴からは
/// 除外する」)。表示・エクスポートには`list_for_task`経由で引き続き残る。
fn build_history(conn: &Connection, task_id: i64) -> Result<Vec<ChatMessage>> {
    let stored = messages::list_for_task(conn, task_id)?;
    Ok(stored
        .into_iter()
        .filter(|m| m.kind == "normal" && m.role != "error")
        .map(|m| {
            if m.role == "user" {
                ChatMessage::User(m.content)
            } else {
                ChatMessage::Assistant {
                    content: Some(m.content),
                    tool_calls: Vec::new(),
                }
            }
        })
        .collect())
}
