use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde_json::json;
use ulid::Ulid;

use crate::db::error::{CoreError, Result};
use crate::db::messages::{self, Kind, Message, NewMessage, Role};
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
    db_call(db.clone(), move |conn| {
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
                reasoning: None,
            },
        )?;
        Ok(())
    })
    .await?;

    let turn_id = Ulid::new().to_string();
    // 新規ターンなので1から始まる。以降の再試行は`retry_assistant_message`が
    // `next_attempt_no`で採番する。
    let attempt_no: i64 = 1;
    generate_turn_response(db, adapter, task_id, turn_id, attempt_no, prompts).await
}

/// 編集(ユーザー発言のみ、Issue #41)。対象の発言以降(自身を含む)の通常発言をすべて
/// 論理削除し、編集後の内容を新しい発言として挿入したうえで、新しいターンとして
/// 応答を生成し直す。ツール実行記録は対象外(`db::messages::soft_delete_normal_from`
/// 参照)。添付ファイルは現時点で未実装(Issue #21)のため引き継ぎ処理自体が無いが、
/// 実装され次第ここに「新しい発言へコピーする」処理を追加する必要がある。
pub async fn edit_user_message(
    db: SharedConnection,
    adapter: Option<&dyn LlmAdapter>,
    task_id: i64,
    message_id: i64,
    new_text: String,
    prompts: &SystemPrompts<'_>,
) -> Result<Vec<ResponseEvent>> {
    db_call(db.clone(), move |conn| {
        let target = messages::find_message(conn, message_id)?
            .ok_or(CoreError::MessageNotFound(message_id))?;
        validate_target(&target, task_id, "user")?;

        messages::soft_delete_normal_from(conn, task_id, message_id)?;
        messages::insert_message(
            conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::User,
                content: &new_text,
                kind: Kind::Normal,
                source: None,
                turn: None,
                error_kind: None,
                reasoning: None,
            },
        )?;
        Ok(())
    })
    .await?;

    let turn_id = Ulid::new().to_string();
    generate_turn_response(db, adapter, task_id, turn_id, 1, prompts).await
}

/// 再試行(アシスタント発言のみ、Issue #41)。対象の発言以降(自身を含む)の通常発言を
/// 論理削除し、同じ`turn_id`のまま`attempt_no`を増やして応答を生成し直す
/// (`docs/spec/rebuild/data-model.md`「ターン境界」)。対応するユーザー発言は
/// `id < message_id`のためカスケードの対象外で、そのまま履歴に残る。
pub async fn retry_assistant_message(
    db: SharedConnection,
    adapter: Option<&dyn LlmAdapter>,
    task_id: i64,
    message_id: i64,
    prompts: &SystemPrompts<'_>,
) -> Result<Vec<ResponseEvent>> {
    let (turn_id, attempt_no) = db_call(db.clone(), move |conn| {
        let target = messages::find_message(conn, message_id)?
            .ok_or(CoreError::MessageNotFound(message_id))?;
        validate_target(&target, task_id, "assistant")?;
        let turn_id = target.turn_id.clone().ok_or_else(|| {
            CoreError::InvalidMessageOperation(
                "assistant message has no turn_id to retry".to_string(),
            )
        })?;

        messages::soft_delete_normal_from(conn, task_id, message_id)?;
        let attempt_no = messages::next_attempt_no(conn, &turn_id)?;
        Ok((turn_id, attempt_no))
    })
    .await?;

    generate_turn_response(db, adapter, task_id, turn_id, attempt_no, prompts).await
}

/// 削除(共通、Issue #41)。確認ダイアログ無しの即座に取り消し可能な論理削除で、
/// カスケードはしない(対象の1件だけを消す。編集・再試行のカスケード削除とは別の操作)。
/// 対象はユーザー/アシスタントの通常発言のみ(`db::messages::soft_delete_message`が検証する)。
pub async fn delete_message(db: SharedConnection, task_id: i64, message_id: i64) -> Result<()> {
    db_call(db, move |conn| {
        let target = messages::find_message(conn, message_id)?
            .ok_or(CoreError::MessageNotFound(message_id))?;
        if target.task_id != Some(task_id) {
            return Err(CoreError::InvalidMessageOperation(
                "message does not belong to this task".to_string(),
            ));
        }
        messages::soft_delete_message(conn, message_id)
    })
    .await
}

/// `edit_user_message`/`retry_assistant_message`共通の対象検証。役割・種別・所属タスクを
/// 1箇所で確認する(`docs/spec/principles.md` 5節)。
fn validate_target(target: &Message, task_id: i64, expected_role: &str) -> Result<()> {
    if target.task_id != Some(task_id) {
        return Err(CoreError::InvalidMessageOperation(
            "message does not belong to this task".to_string(),
        ));
    }
    if target.kind != "normal" || target.role != expected_role {
        return Err(CoreError::InvalidMessageOperation(format!(
            "target must be a normal {expected_role} message"
        )));
    }
    Ok(())
}

/// 応答生成の本体(architecture.md 1節)。LLM呼び出し →
/// (ツール呼び出しがあれば実行して結果を踏まえ再度呼び出し) → 確定した応答の保存、
/// までを1つの関数に閉じる(docs/spec/principles.md 5節)。`run_turn`(新規発言)・
/// `edit_user_message`(編集)・`retry_assistant_message`(再試行)はいずれも、対象となる
/// ユーザー発言をDBに用意した上でこれを呼ぶ共通の末尾処理。
///
/// `adapter`が`None`(プロバイダー未選択)・モデル未選択・APIキー未設定・空応答・
/// コンテキスト超過・ツール呼び出し回数の上限到達は、`Err`で上位に返さずエラー発言として
/// 保存し`Ok`で返す(Issue #40)。DB自体への書き込みが失敗する場合のみ`Err`のまま返る。
async fn generate_turn_response(
    db: SharedConnection,
    adapter: Option<&dyn LlmAdapter>,
    task_id: i64,
    turn_id: String,
    attempt_no: i64,
    prompts: &SystemPrompts<'_>,
) -> Result<Vec<ResponseEvent>> {
    let Some(adapter) = adapter else {
        return fail_turn(db, task_id, &turn_id, attempt_no, TurnFailure::NoProvider).await;
    };
    if let Some(failure) = turn_error::from_readiness(adapter.readiness()) {
        return fail_turn(db, task_id, &turn_id, attempt_no, failure).await;
    }

    // 呼び出し元(`run_turn`/`edit_user_message`/`retry_assistant_message`)が対象の
    // ユーザー発言の挿入・カスケード削除を済ませたあとの状態を読む。
    let history = db_call(db.clone(), move |conn| build_history(conn, task_id)).await?;

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
        // このラウンドで生じた思考の断片。表示・保存専用で`round_trip`(モデルへの
        // 再送信用)には載せない(principles.md 3節「思考は履歴に送り返さない」)。
        let mut reasoning = String::new();
        let mut tool_calls: Vec<ToolCallRequest> = Vec::new();
        for event in &events {
            match event {
                ResponseEvent::TextDelta { text: delta } => text.push_str(delta),
                ResponseEvent::ReasoningDelta { text: delta } => reasoning.push_str(delta),
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
        let reasoning_for_db = (!reasoning.is_empty()).then_some(reasoning);

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
                        reasoning: reasoning_for_db.as_deref(),
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
                for (i, call) in pending.into_iter().enumerate() {
                    let result =
                        tools::execute_task_chat_tool(conn, task_id, &call.name, &call.arguments)?;

                    // このラウンドの思考は、ラウンド内最初のツール実行記録の`reasoning`列に
                    // 1回だけ紐付ける(発生順に混在させて表示するため。同一ラウンドの
                    // 全呼び出しに複製すると「思考・ツール」折りたたみの件数が水増しされる)。
                    let reasoning_for_row = if i == 0 { reasoning_for_db.as_deref() } else { None };

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
                            reasoning: reasoning_for_row,
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
                reasoning: None,
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
