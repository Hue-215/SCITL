use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde_json::json;
use ulid::Ulid;

use crate::db::error::{CoreError, Result};
use crate::db::messages::{self, Kind, Message, NewMessage, Role};
use crate::db::{with_conn, SharedConnection};
use crate::llm::{
    ChatMessage, FinishReason, LlmAdapter, ResponseEvent, ToolArguments, ToolCallRequest,
};
use crate::mcp::McpSessions;
use crate::orchestration::mcp_access::McpAccess;
use crate::orchestration::state_prompt::build_system_prompt;
use crate::orchestration::turn_error::{self, TurnFailure};
use crate::orchestration::{SystemPrompts, ToolLimits};
use crate::tools::{self, external::ExternalToolset};

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
    mcp: &McpAccess<'_>,
    limits: ToolLimits,
) -> Result<Vec<ResponseEvent>> {
    with_conn(db.clone(), move |conn| {
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
    generate_turn_response(
        db, adapter, task_id, turn_id, attempt_no, prompts, mcp, limits,
    )
    .await
}

/// 編集(ユーザー発言のみ、Issue #41)。対象の発言以降(自身を含む)の通常発言をすべて
/// 論理削除し、編集後の内容を新しい発言として挿入したうえで、新しいターンとして
/// 応答を生成し直す。ツール実行記録は対象外(`db::messages::soft_delete_normal_from`
/// 参照)。添付ファイルは現時点で未実装(Issue #21)のため引き継ぎ処理自体が無いが、
/// 実装され次第ここに「新しい発言へコピーする」処理を追加する必要がある。
// turn層の入口はどれも「db・adapter・task_id・prompts・mcp・limits」という同じ文脈を
// 受け取る。まとめ方はIssue #119(引数の定型の共通化)で決める。
#[allow(clippy::too_many_arguments)]
pub async fn edit_user_message(
    db: SharedConnection,
    adapter: Option<&dyn LlmAdapter>,
    task_id: i64,
    message_id: i64,
    new_text: String,
    prompts: &SystemPrompts<'_>,
    mcp: &McpAccess<'_>,
    limits: ToolLimits,
) -> Result<Vec<ResponseEvent>> {
    with_conn(db.clone(), move |conn| {
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
    generate_turn_response(db, adapter, task_id, turn_id, 1, prompts, mcp, limits).await
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
    mcp: &McpAccess<'_>,
    limits: ToolLimits,
) -> Result<Vec<ResponseEvent>> {
    let (turn_id, attempt_no) = with_conn(db.clone(), move |conn| {
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

    generate_turn_response(
        db, adapter, task_id, turn_id, attempt_no, prompts, mcp, limits,
    )
    .await
}

/// 削除(共通、Issue #41)。確認ダイアログ無しの即座に取り消し可能な論理削除で、
/// カスケードはしない(対象の1件だけを消す。編集・再試行のカスケード削除とは別の操作)。
/// 対象はユーザー/アシスタントの通常発言のみ(`db::messages::soft_delete_message`が検証する)。
pub async fn delete_message(db: SharedConnection, task_id: i64, message_id: i64) -> Result<()> {
    with_conn(db, move |conn| {
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
/// 外部(MCP)サーバーへの接続はこのターンの間だけ生かし、結果によらずここで閉じる
/// (legacy/backend.md 9節。ラウンドの途中で抜ける経路が複数あるため、往復の本体は
/// [`run_tool_rounds`]に分け、切断をこの1箇所に集める)。
///
/// `adapter`が`None`(プロバイダー未選択)・モデル未選択・APIキー未設定・空応答・
/// コンテキスト超過・ツール呼び出し回数の上限到達は、`Err`で上位に返さずエラー発言として
/// 保存し`Ok`で返す(Issue #40)。DB自体への書き込みが失敗する場合のみ`Err`のまま返る。
#[allow(clippy::too_many_arguments)]
async fn generate_turn_response(
    db: SharedConnection,
    adapter: Option<&dyn LlmAdapter>,
    task_id: i64,
    turn_id: String,
    attempt_no: i64,
    prompts: &SystemPrompts<'_>,
    mcp: &McpAccess<'_>,
    limits: ToolLimits,
) -> Result<Vec<ResponseEvent>> {
    let Some(adapter) = adapter else {
        return fail_turn(db, task_id, &turn_id, attempt_no, TurnFailure::NoProvider).await;
    };
    if let Some(failure) = turn_error::from_readiness(adapter.readiness()) {
        return fail_turn(db, task_id, &turn_id, attempt_no, failure).await;
    }

    let mut sessions = McpSessions::new();
    let external = prepare_external_tools(mcp, &mut sessions).await;
    let result = run_tool_rounds(
        db,
        adapter,
        task_id,
        &turn_id,
        attempt_no,
        prompts,
        mcp,
        &external,
        &mut sessions,
        limits,
    )
    .await;
    sessions.close().await;
    result
}

/// このターンでモデルへ公開する外部ツールを決める。ツールを1つも有効化していない
/// サーバーには接続しない(ユーザーが有効化していない以上、繋ぐ理由が無い)。
///
/// 一覧はキャッシュ(Issue #104)を優先し、無ければ取得してキャッシュに載せる。
/// 接続・取得に失敗したサーバーはこのターンでは公開しない。ここでターン全体を失敗させると、
/// 外部サーバーが1つ落ちているだけでチャットが使えなくなるため(principles.md 3節)。
async fn prepare_external_tools(
    mcp: &McpAccess<'_>,
    sessions: &mut McpSessions,
) -> ExternalToolset {
    let mut fetched = Vec::new();
    for server in mcp
        .servers
        .iter()
        .filter(|s| s.enabled && !s.enabled_tools.is_empty())
    {
        if let Some(cached) = mcp.catalog.and_then(|c| c.get(&server.id)) {
            fetched.push((server, cached));
            continue;
        }
        match sessions.list_tools(server).await {
            Ok(tools) => {
                if let Some(catalog) = mcp.catalog {
                    catalog.store(&server.id, tools.clone());
                }
                fetched.push((server, tools));
            }
            Err(e) => {
                eprintln!(
                    "failed to list tools from MCP server '{}': {e}",
                    server.name
                );
            }
        }
    }
    ExternalToolset::build(fetched, &tools::task_chat_tool_names())
}

/// LLM呼び出しとツール呼び出しの往復。切断の都合で[`generate_turn_response`]から
/// 分けてあるだけで、1ターンの流れとしては地続き。
#[allow(clippy::too_many_arguments)]
async fn run_tool_rounds(
    db: SharedConnection,
    adapter: &dyn LlmAdapter,
    task_id: i64,
    turn_id: &str,
    attempt_no: i64,
    prompts: &SystemPrompts<'_>,
    mcp: &McpAccess<'_>,
    external: &ExternalToolset,
    sessions: &mut McpSessions,
    limits: ToolLimits,
) -> Result<Vec<ResponseEvent>> {
    // 呼び出し元(`run_turn`/`edit_user_message`/`retry_assistant_message`)が対象の
    // ユーザー発言の挿入・カスケード削除を済ませたあとの状態を読む。
    let history = with_conn(db.clone(), move |conn| build_history(conn, task_id)).await?;

    // 内部ツールと外部ツールを1つの一覧にして公開する(Issue #44)。名前空間化と
    // 衝突の排除は`ExternalToolset`が済ませてある。
    let mut exposed_tools = tools::task_chat_tools();
    exposed_tools.extend(external.schemas());

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
    // ツール実行に使った時間の合計(Issue #71)。LLMの応答待ちは数えない。そちらは
    // アダプタ側のタイムアウト(`GeneralConfig::response_timeout_secs`)が見るもので、
    // ここで合算すると「モデルが遅いのでツールが打ち切られた」という筋の通らない
    // 打ち切り方になる。
    let mut tool_time_used = Duration::ZERO;

    for _round in 1..=limits.max_rounds_per_turn {
        let system_prompt_text = with_conn(db.clone(), {
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

        let mut messages_to_send = Vec::with_capacity(1 + history.len() + round_trip.len());
        messages_to_send.push(ChatMessage::System(system_prompt_text));
        messages_to_send.extend(history.iter().cloned());
        messages_to_send.extend(round_trip.iter().cloned());

        let events = match adapter.send(&messages_to_send, &exposed_tools).await {
            Ok(events) => events,
            Err(e) => {
                let failure = turn_error::classify(&e);
                return fail_turn(db, task_id, turn_id, attempt_no, failure).await;
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
                ResponseEvent::ToolCall {
                    id,
                    name,
                    arguments,
                } => {
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
                return fail_turn(db, task_id, turn_id, attempt_no, TurnFailure::EmptyResponse)
                    .await;
            }

            let turn_id = turn_id.to_string();
            with_conn(db, move |conn| {
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
        let mut executed: Vec<(ToolCallRequest, serde_json::Value)> =
            Vec::with_capacity(tool_calls.len());
        for (i, call) in tool_calls.into_iter().enumerate() {
            // 合計時間は呼び出しの区切りで判定する。`execute_call`自身は内部・外部
            // それぞれの失敗を結果JSONに落としてターンを続けるが(同関数のドキュメント
            // 参照)、合計時間の超過だけはモデルに返して続けても意味が無いため、
            // ここでターンを打ち切る。
            //
            // 実行中の呼び出しを外から打ち切らないのは、内部ツールのDB書き込みが
            // `spawn_blocking`の上で走っており、待つのをやめてもタスク自体は完走する
            // ため。打ち切ると、書き込みだけが済んで実行記録が
            // 残らない状態を作る。1回の呼び出しは内部ツールならDB操作、外部ツールなら
            // `mcp`のper-callタイムアウトで有界なので、超過はその1回分に収まる。
            if tool_time_used >= limits.total_timeout {
                return fail_turn(db, task_id, turn_id, attempt_no, TurnFailure::ToolTimeout).await;
            }
            let started = Instant::now();
            let result = execute_call(db.clone(), task_id, mcp, external, sessions, &call).await?;
            tool_time_used = tool_time_used.saturating_add(started.elapsed());

            // このラウンドの思考は、ラウンド内最初のツール実行記録の`reasoning`列に
            // 1回だけ紐付ける(発生順に混在させて表示するため。同一ラウンドの
            // 全呼び出しに複製すると「思考・ツール」折りたたみの件数が水増しされる)。
            let reasoning_for_row = if i == 0 {
                reasoning_for_db.clone()
            } else {
                None
            };
            // 読めなかった引数は、モデルが実際に何を出したかが分かるよう生の文字列で残す。
            let recorded_arguments = match &call.arguments {
                ToolArguments::Valid { value } => value.clone(),
                ToolArguments::Malformed { raw, .. } => serde_json::Value::String(raw.clone()),
            };
            let content = json!({
                "tool": call.name.clone(),
                "arguments": recorded_arguments,
                "result": result.clone(),
            })
            .to_string();
            let turn_id_for_db = turn_id.to_string();
            with_conn(db.clone(), move |conn| {
                messages::insert_message(
                    conn,
                    NewMessage {
                        task_id: Some(task_id),
                        role: Role::Tool,
                        content: &content,
                        kind: Kind::ToolExecution,
                        // 外部サーバーのツールを呼んだ記録もこのターンに属する。
                        // `source`は逆向き(外部のLLMがMCP経由でSCITLを操作した)専用の
                        // 印であり、ここでは付けない(data-model.md「ターン境界」の3分類)。
                        source: None,
                        turn: Some((&turn_id_for_db, attempt_no)),
                        error_kind: None,
                        reasoning: reasoning_for_row.as_deref(),
                    },
                )?;
                Ok(())
            })
            .await?;
            executed.push((call, result));
        }

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

    fail_turn(
        db,
        task_id,
        turn_id,
        attempt_no,
        TurnFailure::ToolRoundLimit,
    )
    .await
}

/// ツール1件の実行。名前が外部ツールとして公開したものなら対応するサーバーへ、
/// そうでなければ内部ツールへ振り分ける(振り分けの判断はここ1箇所)。
///
/// 内部・外部のどちらも、実行の失敗は`Err`で上に返さず`{"error": ...}`の結果JSONに
/// 落としてターンを続ける(docs/spec/principles.md 3節「失敗しても会話を止めない」)。
/// 引数の型違いや対象の取り違えはモデルが自分で直せる失敗であり、外部サーバーの
/// 不達に至っては日常的に起こるため、モデルに失敗を伝えて続けさせる方が会話として
/// 自然になる。返る`Err`はDBスレッド自体が落ちた場合だけで、それは呼び出し元が
/// 実行記録を保存できないのと同じ状況にあたる。
///
/// 引数がJSONとして読めなかった呼び出し(`ToolArguments::Malformed`)は、どのツールも
/// 実行せずに失敗を返し、出し直させる。
async fn execute_call(
    db: SharedConnection,
    task_id: i64,
    mcp: &McpAccess<'_>,
    external: &ExternalToolset,
    sessions: &mut McpSessions,
    call: &ToolCallRequest,
) -> Result<serde_json::Value> {
    let arguments = match &call.arguments {
        ToolArguments::Valid { value } => value,
        ToolArguments::Malformed { error, .. } => {
            return Ok(json!({
                "error": format!(
                    "the arguments were not valid JSON ({error}); \
                     the tool was not run. Call it again with valid JSON arguments."
                )
            }));
        }
    };
    let Some((server_id, tool_name)) = external.route(&call.name) else {
        let name = call.name.clone();
        let arguments = arguments.clone();
        return with_conn(db, move |conn| {
            Ok(
                tools::execute_task_chat_tool(conn, task_id, &name, &arguments)
                    .unwrap_or_else(|e| json!({ "error": e.to_string() })),
            )
        })
        .await;
    };

    let Some(server) = mcp.servers.iter().find(|s| s.id == server_id) else {
        return Ok(json!({ "error": format!("MCP server not found: {server_id}") }));
    };
    Ok(sessions
        .call_tool(server, tool_name, arguments)
        .await
        .unwrap_or_else(|e| json!({ "error": e.to_string() })))
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
    with_conn(db, move |conn| {
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

/// API送信用の履歴。送信日時は`ChatMessage::User`の`sent_at`として本文と分けて運ぶ
/// (Issue #68。組み立ては`llm::render_user_content`)。アシスタント発言に日時を付けないのは、
/// モデルが自分の過去の発言の形を真似て、応答の地の文に日時やタグを書き出すのを避けるため。
///
/// エラー発言(`role='error'`)は除外する
/// (`legacy/backend.md` 4節手順2「エラー発言・ツール実行記録はこのAPI送信用の履歴からは
/// 除外する」)。表示・エクスポートには`list_for_task`経由で引き続き残る。
fn build_history(conn: &Connection, task_id: i64) -> Result<Vec<ChatMessage>> {
    let stored = messages::list_for_task(conn, task_id)?;
    Ok(stored
        .into_iter()
        .filter(|m| m.kind == "normal" && m.role != "error")
        .map(|m| {
            if m.role == "user" {
                ChatMessage::User {
                    text: m.content,
                    sent_at: Some(m.created_at),
                }
            } else {
                ChatMessage::Assistant {
                    content: Some(m.content),
                    tool_calls: Vec::new(),
                }
            }
        })
        .collect())
}
