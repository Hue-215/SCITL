use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde::Serialize;
use serde_json::json;
use ulid::Ulid;

use crate::attachments::{AttachmentStore, Taken};
use crate::blocking;
use crate::db::attachments as db_attachments;
use crate::db::messages::{self, Chat, Kind, Message, NewMessage, Origin, Role};
use crate::db::tasks::{self, Task};
use crate::db::transcripts::{self, NewTranscript};
use crate::db::{in_transaction, with_conn, SharedConnection};
use crate::error::{CoreError, Result};
use crate::in_flight::{InFlight, InFlightSet, StopSignal};
use crate::llm::{
    AdapterIdentity, ChatMessage, InlineImage, LlmAdapter, PromptText, ResponseEvent,
    ToolArguments, ToolCallRequest,
};
use crate::mcp::McpSessions;
use crate::orchestration::history;
use crate::orchestration::mcp_access::McpAccess;
use crate::orchestration::tool_record::{ToolExecutionRecord, ToolExecutionView};
use crate::orchestration::transcript::SavedTurn;
use crate::orchestration::turn_error::{self, TurnFailure};
use crate::orchestration::turn_request::TurnRequest;
use crate::orchestration::{TurnContext, TurnEvent, TurnEvents};
use crate::tools::{self, external::ExternalToolset, ToolOutput};

/// 送信する発言。本文と、送信前に預けた添付のトークン(`attachments::Attachments::stage`)。
#[derive(Debug, Clone, Default)]
pub struct UserInput {
    pub text: String,
    pub attachments: Vec<String>,
}

impl From<String> for UserInput {
    fn from(text: String) -> Self {
        Self {
            text,
            attachments: Vec::new(),
        }
    }
}

/// 発言を送り、応答を生成する。ユーザー発言(と添付)を保存してから
/// [`generate_turn_response`]へ進む。本文が空白だけでも、添付があれば送れる。添付は預かりから
/// 取り出してユーザー発言と一緒に書き、書けなければ預かりに戻す(画面は同じトークンで送り直す)。
///
/// ユーザー発言を保存したあとの失敗はエラー発言として保存し、`Ok`で返す。`Err`になるのは、
/// その会話が既に応答を生成中のとき、タスクが無い(削除済みを含む)とき、本文も添付も無いとき、
/// 預けていない添付を指したとき、発言やエラー発言自体を書けないときだけ。
pub async fn run_turn(
    db: SharedConnection,
    ctx: &TurnContext<'_>,
    chat: Chat,
    input: impl Into<UserInput>,
) -> Result<()> {
    let UserInput { text, attachments } = input.into();
    let generating = begin_generating(ctx.generating, chat)?;
    let taken = ctx.attachments.take_staged(&attachments)?;
    if let Err(e) = save_user_message(db.clone(), ctx, chat, text, taken.clone()).await {
        ctx.attachments.restore_staged(taken);
        return Err(e);
    }

    generate_turn_response(db, ctx, Attempt::first(chat), generating.stop_signal()).await
}

/// ユーザー発言と添付を1つのトランザクションで書く。実体は行より先に置き場所へ書く
/// (行が指す実体が無い状態を作らない)。
async fn save_user_message(
    db: SharedConnection,
    ctx: &TurnContext<'_>,
    chat: Chat,
    text: String,
    taken: Taken,
) -> Result<()> {
    require_content(&text, taken.len())?;
    let rows = ctx.attachments.store_taken(taken).await?;
    with_conn(db, move |conn| {
        in_transaction(conn, |conn| {
            require_chat(conn, chat)?;
            let message_id = insert_user_message(conn, chat, &text)?;
            for row in &rows {
                db_attachments::insert(conn, message_id, row)?;
            }
            Ok(())
        })
    })
    .await
}

/// ユーザー発言の行を書く。会話が存在するかは呼び出し側が確かめる([`require_chat`])。
pub(super) fn insert_user_message(conn: &Connection, chat: Chat, text: &str) -> Result<i64> {
    messages::insert_message(
        conn,
        NewMessage {
            chat,
            role: Role::User,
            content: text,
            kind: Kind::Normal,
            origin: Origin::User,
            error_kind: None,
            error_detail: None,
            reasoning: None,
        },
    )
}

/// [`create_task`]の結果。
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TaskCreation {
    Created {
        task: Task,
    },
    /// チャットを使えないので作らなかった。`error_kind`はエラー発言と同じ種別コードで、
    /// 画面は同じ文言を出す。
    Unavailable {
        error_kind: &'static str,
    },
}

/// 新規タスクの作成。チャットを使えない(モデル未選択等)ならタスクを作らずに理由を返す
/// (続く[`open_task_chat`]が失敗し、エラー発言だけのタスクが残るため)。
pub async fn create_task(db: SharedConnection, ctx: &TurnContext<'_>) -> Result<TaskCreation> {
    if let Err(failure) = ready_adapter(ctx) {
        return Ok(TaskCreation::Unavailable {
            error_kind: failure.kind(),
        });
    }
    let task = with_conn(db, tasks::create_task).await?;
    Ok(TaskCreation::Created { task })
}

/// 聞き取りの開始。ユーザーの発言なしに、開始の発言([`TurnContext::opening_message`])への
/// 返信として最初のターンを生成する。開始の発言は保存せず、以降のターンも
/// `history::build_history`が履歴の先頭に補う。
///
/// まだ1行も発言の無いタスクでだけ行う。
pub async fn open_task_chat(
    db: SharedConnection,
    ctx: &TurnContext<'_>,
    task_id: i64,
) -> Result<()> {
    let chat = Chat::Task(task_id);
    let generating = begin_generating(ctx.generating, chat)?;
    with_conn(db.clone(), move |conn| {
        require_chat(conn, chat)?;
        if messages::opener(conn, task_id)?.is_some() {
            return Err(CoreError::InvalidMessageOperation(
                "the conversation has already started".to_string(),
            ));
        }
        Ok(())
    })
    .await?;

    generate_turn_response(db, ctx, Attempt::first(chat), generating.stop_signal()).await
}

/// ユーザー発言の編集。対象の発言以降(自身を含む)の通常発言をすべて論理削除し、編集後の
/// 内容を新しい発言として挿入したうえで、新しいターンとして応答を生成し直す。添付は新しい
/// 発言へ引き継ぐ。
pub async fn edit_user_message(
    db: SharedConnection,
    ctx: &TurnContext<'_>,
    chat: Chat,
    message_id: i64,
    new_text: String,
) -> Result<()> {
    let generating = begin_generating(ctx.generating, chat)?;
    with_conn(db.clone(), move |conn| {
        // 挿入だけが失敗すると、会話がその位置から消えたまま置き換わらない。
        in_transaction(conn, |conn| {
            let target = find_in_chat(conn, chat, message_id)?;
            expect_normal(&target, &[Role::User])?;
            messages::soft_delete_normal_from(conn, chat, target.id)?;
            let message_id = insert_user_message(conn, chat, &new_text)?;
            let carried = db_attachments::copy_to_message(conn, target.id, message_id)?;
            // 断るとトランザクションごと戻り、元の発言は消えない。
            require_content(&new_text, carried)
        })
    })
    .await?;

    generate_turn_response(db, ctx, Attempt::first(chat), generating.stop_signal()).await
}

/// ターンの返信(アシスタント発言またはエラー発言)の再試行。対象の発言以降(自身を含む)の
/// 通常発言を論理削除し、同じ`turn_id`のまま`attempt_no`を増やして応答を生成し直す。対応する
/// ユーザー発言は対象より前なので残る。
///
/// ターンのユーザー発言だけが削除されていることがあるので、返信以降を消した残りが応答すべき
/// 発言で終わらなければ断る(`history::awaits_reply`)。新規送信と編集は必ずユーザー発言を
/// 用意してから生成するので、この確認は再試行にだけ要る。
pub async fn retry_reply(
    db: SharedConnection,
    ctx: &TurnContext<'_>,
    chat: Chat,
    message_id: i64,
) -> Result<()> {
    let generating = begin_generating(ctx.generating, chat)?;
    let attempt = with_conn(db.clone(), move |conn| {
        in_transaction(conn, |conn| {
            let target = find_in_chat(conn, chat, message_id)?;
            expect_normal(&target, &[Role::Assistant, Role::Error])?;
            let turn_id = target.turn_id.clone().ok_or_else(|| {
                CoreError::InvalidMessageOperation("reply has no turn_id to retry".to_string())
            })?;
            let attempt_no = messages::next_attempt_no(conn, &turn_id)?;
            messages::soft_delete_normal_from(conn, chat, message_id)?;
            // 断るとトランザクションごと戻り、返信は消えない。
            if !history::awaits_reply(conn, chat)? {
                return Err(CoreError::InvalidMessageOperation(
                    "nothing to reply to: the message this reply answered was deleted".to_string(),
                ));
            }
            Ok(Attempt {
                chat,
                turn_id,
                attempt_no,
            })
        })
    })
    .await?;

    generate_turn_response(db, ctx, attempt, generating.stop_signal()).await
}

/// 発言と、それより後ろの通常発言をまとめて論理削除する(編集・再試行と同じく、その地点から
/// 後ろを消す。`docs/spec/rebuild/data-model.md`「ターン境界」)。対象はユーザー発言とターンの
/// 返信(アシスタント発言・エラー発言)。生成中の会話では断る(生成中のターンが読んだ履歴と、
/// DBの発言が食い違うため)。
pub async fn delete_message(
    db: SharedConnection,
    generating: &InFlightSet<Chat>,
    chat: Chat,
    message_id: i64,
) -> Result<()> {
    let _generating = begin_generating(generating, chat)?;
    with_conn(db, move |conn| {
        in_transaction(conn, |conn| {
            let target = find_in_chat(conn, chat, message_id)?;
            expect_normal(&target, &[Role::User, Role::Assistant, Role::Error])?;
            messages::soft_delete_normal_from(conn, chat, target.id)
        })
    })
    .await
}

/// 会話で生成中の応答を止める。止める指示を出すだけで、止まるのは生成の側が次に指示を
/// 見たとき。LLMの応答を待っている間ならすぐに、ツールの実行中ならその呼び出しが済んでから
/// 止まる(実行中の呼び出しは打ち切らない。[`run_tool_rounds`])。止めたターンは、それまでに
/// 実行したツールの記録と、止めたことを表すエラー発言(`TurnFailure::Stopped`)を残して終わり、
/// その保存は生成を始めた呼び出し([`run_turn`]等)が返るまでに済む。
///
/// 同じプロセスの中で生成している会話にしか効かない。生成中でなければ何もせず`false`を返す
/// (止める指示と生成の終わりが行き違った場合を含む)。発言の削除・タスクの操作も同じ集合で
/// 生成中として扱うが、それらは指示を見ないので止まらない。
pub fn stop_response(generating: &InFlightSet<Chat>, chat: Chat) -> bool {
    generating.request_stop(&chat)
}

/// ユーザー発言には本文か添付のどちらかが要る。本文が空白だけでも、添付があれば送れる。
pub(super) fn require_content(text: &str, attachment_count: usize) -> Result<()> {
    if text.trim().is_empty() && attachment_count == 0 {
        return Err(CoreError::InvalidMessageOperation(
            "a message needs text or attachments".to_string(),
        ));
    }
    Ok(())
}

/// 会話の行を1行も書かないうちに、同じ会話の応答生成が走っていないかを確かめる
/// ([`TurnContext::generating`])。返ったガードを持っている間、その会話は生成中になる。
pub(super) fn begin_generating(
    generating: &InFlightSet<Chat>,
    chat: Chat,
) -> Result<InFlight<'_, Chat>> {
    generating.try_begin(chat).ok_or(CoreError::ChatBusy(chat))
}

/// 会話に行を書く前に、タスクが存在し削除されていないかを確かめる。削除と操作が
/// 行き違うと、外部キーは通るので削除済みのタスクに行が書かれてしまう。
pub(super) fn require_chat(conn: &Connection, chat: Chat) -> Result<()> {
    if let Chat::Task(task_id) = chat {
        tasks::get_task(conn, task_id)?;
    }
    Ok(())
}

/// 編集・再試行・削除の対象を引き、その会話の発言であることを1箇所で確認する。
fn find_in_chat(conn: &Connection, chat: Chat, message_id: i64) -> Result<Message> {
    require_chat(conn, chat)?;
    let target =
        messages::find_message(conn, message_id)?.ok_or(CoreError::MessageNotFound(message_id))?;
    if target.task_id != chat.task_id() {
        return Err(CoreError::InvalidMessageOperation(format!(
            "message does not belong to {chat}"
        )));
    }
    Ok(target)
}

/// 編集・再試行・削除の対象の役割・種別の確認。
fn expect_normal(target: &Message, expected_roles: &[Role]) -> Result<()> {
    if target.kind != Kind::Normal || !expected_roles.contains(&target.role) {
        let roles: Vec<&str> = expected_roles.iter().map(|r| r.as_str()).collect();
        return Err(CoreError::InvalidMessageOperation(format!(
            "target must be a normal {} message",
            roles.join("/")
        )));
    }
    Ok(())
}

/// 応答生成の本体。LLM呼び出し → (ツール呼び出しがあれば実行して再度呼び出し) → 応答の保存。
/// 送信・編集・再試行はどれも、ユーザー発言をDBに用意してからこれを呼ぶ。
///
/// 外部(MCP)サーバーへの接続はこのターンの間だけ生かし、結果によらずここで閉じる。途中で
/// 抜ける経路が複数あるため、往復の本体は[`run_tool_rounds`]に分けてある。
///
/// 失敗はどれもこの試行のエラー発言として保存して`Ok`で返す(何も書かずに抜けると、返信を
/// 消した再試行ではターンごと会話から消える)。`Err`が返るのはエラー発言自体を書けないときだけ。
/// `stop`で止めた場合も同じく、止めたことを表すエラー発言を書いて`Ok`で返す([`stop_response`])。
async fn generate_turn_response(
    db: SharedConnection,
    ctx: &TurnContext<'_>,
    attempt: Attempt,
    stop: &StopSignal,
) -> Result<()> {
    let adapter = match ready_adapter(ctx) {
        Ok(adapter) => adapter,
        Err(failure) => return fail_turn(db, &attempt, failure).await,
    };

    let mut sessions = McpSessions::new();
    let external = prepare_external_tools(&ctx.mcp, attempt.chat, &mut sessions).await;
    let result = run_tool_rounds(
        db.clone(),
        adapter,
        ctx,
        &attempt,
        &external,
        &mut sessions,
        stop,
    )
    .await;
    sessions.close().await;
    match result {
        Err(e) => fail_turn(db, &attempt, turn_error::classify(&e)).await,
        done => done,
    }
}

/// 呼び出しに使えるアダプタ。使えなければ、ターンを終えるエラー発言の分類。
pub(super) fn ready_adapter<'a>(
    ctx: &TurnContext<'a>,
) -> std::result::Result<&'a dyn LlmAdapter, TurnFailure> {
    let adapter = ctx.adapter.clone()?;
    match turn_error::from_readiness(adapter.readiness()) {
        Some(failure) => Err(failure),
        None => Ok(adapter),
    }
}

/// 応答生成の1試行。この試行で書く行は、すべてこの組をそのまま持つ。
#[derive(Clone)]
struct Attempt {
    chat: Chat,
    turn_id: String,
    attempt_no: i64,
}

impl Attempt {
    /// 新しいターンの最初の試行。以降の再試行は`retry_reply`が`next_attempt_no`で採番する。
    fn first(chat: Chat) -> Self {
        Self {
            chat,
            turn_id: Ulid::new().to_string(),
            attempt_no: 1,
        }
    }

    /// この試行で送った形を保存する。返信の行と同じトランザクションの中で呼ぶ。
    fn save_transcript(
        &self,
        conn: &Connection,
        identity: &AdapterIdentity,
        saved: &SavedTurn,
    ) -> Result<()> {
        let api_format =
            serde_json::to_value(identity.api_format).expect("an API format serializes");
        transcripts::insert(
            conn,
            &NewTranscript {
                chat: self.chat,
                turn_id: &self.turn_id,
                attempt_no: self.attempt_no,
                api_format: api_format
                    .as_str()
                    .expect("an API format serializes to a string"),
                model: &identity.model,
                server: &identity.server,
                system: &saved.system,
                settings_system: &saved.settings_system,
                tools: &saved.tools,
                prefix_digest: &saved.prefix_digest,
                history_start: saved.history_start,
                input: &saved.input,
                rounds: &saved.rounds,
            },
        )
    }

    /// この試行に属する行を書き、行のidを返す。
    fn insert(
        &self,
        conn: &Connection,
        role: Role,
        content: &str,
        kind: Kind,
        error: Option<(&str, Option<&str>)>,
        reasoning: Option<&str>,
    ) -> Result<i64> {
        messages::insert_message(
            conn,
            NewMessage {
                chat: self.chat,
                role,
                content,
                kind,
                origin: Origin::Turn {
                    turn_id: &self.turn_id,
                    attempt_no: self.attempt_no,
                },
                error_kind: error.map(|(kind, _)| kind),
                error_detail: error.and_then(|(_, detail)| detail),
                reasoning,
            },
        )
    }
}

/// このターンでモデルへ公開する外部ツールを決める。ツールを1つも有効化していないサーバーには
/// 接続しない。総合チャットにも公開する(総合チャットで絞るのはSCITL自身のタスクへの書き込み
/// だけ)。
///
/// 一覧はキャッシュを優先し、無ければ取得してキャッシュに載せる。接続・取得に失敗した
/// サーバーはこのターンでは公開しない。ここでターン全体を失敗させると、外部サーバーが1つ
/// 落ちているだけでチャットが使えなくなるため。前に固定したツール定義には残し、呼ばれたら
/// 今は使えないという失敗を返す(`ExternalToolset::with_unavailable`)。
pub(super) async fn prepare_external_tools(
    mcp: &McpAccess<'_>,
    chat: Chat,
    sessions: &mut McpSessions,
) -> ExternalToolset {
    let mut fetched = Vec::new();
    let mut unavailable = Vec::new();
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
                unavailable.push(server);
            }
        }
    }
    let reserved = tools::names(chat);
    ExternalToolset::build(fetched, &reserved).with_unavailable(unavailable, &reserved)
}

/// LLM呼び出しとツール呼び出しの往復。切断の都合で[`generate_turn_response`]から
/// 分けてあるだけで、1ターンの流れとしては地続き。
///
/// 止める指示(`stop`)は、LLMの応答待ちの間と、ツール呼び出しの区切りで見る。
async fn run_tool_rounds(
    db: SharedConnection,
    adapter: &dyn LlmAdapter,
    ctx: &TurnContext<'_>,
    attempt: &Attempt,
    external: &ExternalToolset,
    sessions: &mut McpSessions,
    stop: &StopSignal,
) -> Result<()> {
    let chat = attempt.chat;
    let stored = with_conn(db.clone(), move |conn| history::load(conn, chat)).await?;
    let request = TurnRequest::prepare(ctx, adapter, chat, stored, external).await?;
    // 同一ターン内のツール呼び出しの往復。そのままモデルに返し、通常発言の行としては書かない
    // (実行記録が同じ結果を持っており、次ターン以降はそこから組み立てる)。
    let mut round_trip: Vec<ChatMessage> = Vec::new();
    // ツール実行に使った時間の合計。LLMの応答待ちは数えない(アダプタのタイムアウトが見る)。
    let mut tool_time_used = Duration::ZERO;
    // ツールを呼んだラウンドにモデルが添えた本文も、このターンの返信の一部として最終行に
    // まとめて保存する。
    let mut reply_parts: Vec<String> = Vec::new();

    let tool_rounds = request.tool_rounds(ctx);
    for round in 1..=tool_rounds + 1 {
        let final_call = round > tool_rounds;
        let (messages_to_send, offered) = request.round(&round_trip, final_call);

        // 受け取った順に画面へ流しつつ、解釈はラウンドを受け取り終えてから行う。
        let mut events = Vec::new();
        let notify = ctx.events;
        let sent = stop
            .unless_requested(adapter.send(
                &messages_to_send,
                offered,
                ctx.reasoning_effort,
                &mut |event| {
                    notify(TurnEvent::Response {
                        event: event.clone(),
                    });
                    events.push(event);
                },
            ))
            .await;
        let replay = match sent {
            Some(Ok(replay)) => replay,
            Some(Err(e)) => return fail_turn(db, attempt, turn_error::classify(&e)).await,
            None => return fail_turn(db, attempt, TurnFailure::Stopped).await,
        };

        let mut text = String::new();
        // このラウンドで生じた思考の断片。表示・保存専用で`round_trip`(モデルへの
        // 再送信用)には載せない。送り返しが要る方言の分は、アダプタが返す`replay`に
        // 入っている。
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
        let reasoning_for_db = (!reasoning.is_empty()).then_some(reasoning);

        if tool_calls.is_empty() {
            // 送った形の保存には、最後の応答も思考の生ブロックごと並べる(次のターンで送り返す)。
            let mut rounds = request.appended(&messages_to_send).to_vec();
            rounds.push(ChatMessage::Assistant {
                content: (!text.is_empty()).then(|| text.clone()),
                tool_calls: Vec::new(),
                replay,
            });
            if !text.is_empty() {
                reply_parts.push(text);
            }
            let reply = reply_parts.join("\n\n");
            if reply.is_empty() {
                return fail_turn(db, attempt, TurnFailure::EmptyResponse).await;
            }

            let transcript = adapter.identity().and_then(|identity| {
                let saved = request.transcript(&rounds);
                if saved.is_none() {
                    eprintln!(
                        "cannot save what was sent: an image was not read from an attachment"
                    );
                }
                Some((identity, saved?))
            });
            let attempt = attempt.clone();
            with_conn(db, move |conn| {
                in_transaction(conn, |conn| {
                    attempt.insert(
                        conn,
                        Role::Assistant,
                        &reply,
                        Kind::Normal,
                        None,
                        reasoning_for_db.as_deref(),
                    )?;
                    // 保存は会話ログの補助なので、失敗しても返信は書く(保存の無いターンは
                    // 実行記録から組み立てる)。
                    if let Some((identity, saved)) = &transcript {
                        if let Err(e) = attempt.save_transcript(conn, identity, saved) {
                            eprintln!("failed to save what was sent: {e}");
                        }
                    }
                    Ok(())
                })
            })
            .await?;
            return Ok(());
        }
        // 上限に達して呼べないようにしたのに呼んできた。実行はせずにエラーで終える。
        if final_call {
            return fail_turn(db, attempt, TurnFailure::ToolRoundLimit).await;
        }

        // 1応答に複数のtool_callsが載る場合、すべて実行する(取りこぼさない)。
        let mut executed: Vec<(ToolCallRequest, CallOutcome)> =
            Vec::with_capacity(tool_calls.len());
        for (i, call) in tool_calls.into_iter().enumerate() {
            // 止める指示と合計時間は呼び出しの区切りで判定し、ターンを打ち切る(ほかの失敗と
            // 違い、モデルに返して続けても意味が無い)。実行中の呼び出しを外から打ち切らないのは、
            // 内部ツールのDB書き込みは待つのをやめても完走し、書き込みだけが済んで
            // 実行記録が残らない状態を作るため(1回の呼び出しは`mcp`のタイムアウトで有界)。
            // 外部ツールも、相手の側で済んだ操作の記録を残すため同じく待つ。
            if stop.is_requested() {
                return fail_turn(db, attempt, TurnFailure::Stopped).await;
            }
            if tool_time_used >= ctx.limits.total_timeout {
                return fail_turn(db, attempt, TurnFailure::ToolTimeout).await;
            }
            let started = Instant::now();
            let outcome = execute_call(db.clone(), chat, ctx, external, sessions, &call).await?;
            tool_time_used = tool_time_used.saturating_add(started.elapsed());

            // このラウンドの思考は、ラウンド内最初のツール実行記録にだけ紐付ける(全呼び出しに
            // 複製すると、画面の「思考・ツール」の件数が水増しされる)。
            let reasoning_for_row = if i == 0 {
                reasoning_for_db.clone()
            } else {
                None
            };
            let record = ToolExecutionRecord {
                tool: call.name.clone(),
                arguments: match &call.arguments {
                    ToolArguments::Valid { value } => value.clone(),
                    ToolArguments::Malformed { raw, .. } => serde_json::Value::String(raw.clone()),
                },
                result: outcome.result.clone(),
                call_id: call.id.clone(),
            };
            save_tool_execution(db.clone(), attempt, record, reasoning_for_row, ctx.events).await?;
            executed.push((call, outcome));
        }

        // モデルへの往復: assistant(tool_calls) 1件 + tool(結果) を呼び出し数ぶん。
        // OpenAI互換プロトコルの標準的な表現に合わせる。
        if !text.is_empty() {
            reply_parts.push(text.clone());
        }
        round_trip.push(ChatMessage::Assistant {
            content: if text.is_empty() { None } else { Some(text) },
            tool_calls: executed.iter().map(|(call, _)| call.clone()).collect(),
            replay,
        });
        // 結果には自由入力が載る。保存する実行記録(上)は受け取ったまま残し、モデルへ
        // 送る側でだけ無害化する。
        for (call, outcome) in executed {
            round_trip.push(ChatMessage::Tool {
                tool_call_id: call.id,
                content: PromptText::json(outcome.turn_result()),
                images: outcome.images,
            });
        }
    }

    unreachable!("the final call always returns")
}

/// ツール1件の実行。名前が外部ツールとして公開したものなら対応するサーバーへ、
/// そうでなければ内部ツールへ振り分ける(振り分けの判断はここ1箇所)。
///
/// 実行の失敗は`Err`で上に返さず、`{"error": ...}`の結果JSONにしてモデルへ伝え、ターンを
/// 続ける(引数の誤りや対象の取り違えはモデルが自分で直せ、外部サーバーの不達は日常的に
/// 起こるため)。返る`Err`はDBスレッドか、添付の実体を読むブロッキング処理自体が落ちた場合だけ。
///
/// 引数がJSONとして読めなかった呼び出し(`ToolArguments::Malformed`)は、どのツールも
/// 実行せずに失敗を返し、出し直させる。
async fn execute_call(
    db: SharedConnection,
    chat: Chat,
    ctx: &TurnContext<'_>,
    external: &ExternalToolset,
    sessions: &mut McpSessions,
    call: &ToolCallRequest,
) -> Result<CallOutcome> {
    let arguments = match &call.arguments {
        ToolArguments::Valid { value } => value,
        ToolArguments::Malformed { error, .. } => {
            let result = json!({
                "error": format!(
                    "the arguments were not valid JSON ({error}); \
                     the tool was not run. Call it again with valid JSON arguments."
                )
            });
            return Ok(CallOutcome::plain(result));
        }
    };
    if external.is_unavailable(&call.name) {
        let result = json!({
            "error": "the server providing this tool cannot be reached right now; \
                      the tool was not run."
        });
        return Ok(CallOutcome::plain(result));
    }
    let Some((server_id, tool_name)) = external.route(&call.name) else {
        let name = call.name.clone();
        let arguments = arguments.clone();
        let image_input = ctx.capabilities.image;
        let output = with_conn(db, move |conn| {
            Ok(tools::execute(conn, chat, image_input, &name, &arguments)
                .unwrap_or_else(|e| json!({ "error": e.to_string() }).into()))
        })
        .await?;
        return read_tool_images(output, ctx.attachments.store()).await;
    };

    let Some(server) = ctx.mcp.servers.iter().find(|s| s.id == server_id) else {
        let result = json!({ "error": format!("MCP server not found: {server_id}") });
        return Ok(CallOutcome::plain(result));
    };
    let result = sessions
        .call_tool(server, tool_name, arguments)
        .await
        .unwrap_or_else(|e| json!({ "error": e.to_string() }));
    Ok(CallOutcome::plain(result))
}

/// ツール1件の実行の結果。`turn_result`と`images`はモデルへの往復(と送った形の保存)にだけ
/// 載せ、実行記録には残さない([`ToolOutput`])。
struct CallOutcome {
    result: serde_json::Value,
    turn_result: Option<serde_json::Value>,
    images: Vec<InlineImage>,
}

impl CallOutcome {
    /// 実行記録と往復で同じ結果を返し、画像を伴わない。
    fn plain(result: serde_json::Value) -> Self {
        Self {
            result,
            turn_result: None,
            images: Vec::new(),
        }
    }

    fn turn_result(&self) -> &serde_json::Value {
        self.turn_result.as_ref().unwrap_or(&self.result)
    }
}

/// 内部ツールが添えた画像の実体を、DBのロックの外で読む。読めなければ結果を失敗に
/// 差し替える(会話は止めない)。
async fn read_tool_images(output: ToolOutput, store: AttachmentStore) -> Result<CallOutcome> {
    if output.image_hashes.is_empty() {
        return Ok(CallOutcome {
            turn_result: output.turn_result,
            ..CallOutcome::plain(output.result)
        });
    }
    let hashes = output.image_hashes;
    let read = blocking::run(move || {
        Ok(hashes
            .iter()
            .map(|hash| store.read_image(hash))
            .collect::<Result<Vec<_>>>())
    })
    .await?;
    Ok(match read {
        Ok(images) => CallOutcome {
            result: output.result,
            turn_result: output.turn_result,
            images,
        },
        Err(e) => CallOutcome::plain(json!({ "error": e.to_string() })),
    })
}

/// ツール実行記録を保存する唯一の入口。保存した値をそのまま画面へ知らせる
/// ([`TurnEvent::ToolExecuted`])。画面への出力の規則は保存値を前提にしているので、保存する
/// 値と知らせる値をここ1箇所で作る。
async fn save_tool_execution(
    db: SharedConnection,
    attempt: &Attempt,
    record: ToolExecutionRecord,
    reasoning: Option<String>,
    events: TurnEvents<'_>,
) -> Result<()> {
    let content = serde_json::to_string(&record).expect("a record of JSON values serializes");
    let attempt = attempt.clone();
    let id = with_conn(db, move |conn| {
        attempt.insert(
            conn,
            Role::Tool,
            &content,
            Kind::ToolExecution,
            None,
            reasoning.as_deref(),
        )
    })
    .await?;
    // DBのロックを離してから知らせる。
    events(TurnEvent::ToolExecuted {
        id,
        execution: ToolExecutionView::of_record(&record),
    });
    Ok(())
}

/// エラー発言(`role='error'`)を保存する唯一の入口。`content`は`failure.user_message()`
/// の定型文言、`error_detail`は`failure.detail()`。
async fn fail_turn(db: SharedConnection, attempt: &Attempt, failure: TurnFailure) -> Result<()> {
    let attempt = attempt.clone();
    let content = failure.user_message();
    with_conn(db, move |conn| {
        attempt.insert(
            conn,
            Role::Error,
            &content,
            Kind::Normal,
            Some((failure.kind(), failure.detail())),
            None,
        )
    })
    .await?;
    Ok(())
}
