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
use crate::db::{in_transaction, with_conn, SharedConnection};
use crate::error::{CoreError, Result};
use crate::in_flight::{InFlight, InFlightSet};
use crate::llm::{
    ChatMessage, InlineImage, LlmAdapter, PromptText, ResponseEvent, ToolArguments,
    ToolCallRequest, ToolSchema,
};
use crate::mcp::McpSessions;
use crate::orchestration::history::{self, HistoryOptions};
use crate::orchestration::history_trim::trim_history;
use crate::orchestration::mcp_access::McpAccess;
use crate::orchestration::state_prompt::build_system_prompt;
use crate::orchestration::tool_record::{ToolExecutionRecord, ToolExecutionView};
use crate::orchestration::turn_error::{self, TurnFailure};
use crate::orchestration::{SystemPrompts, TurnContext, TurnEvent, TurnEvents};
use crate::tools::{self, external::ExternalToolset, ToolKind, ToolOutput};

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

/// 1ターンの処理フロー(architecture.md 1節)。ユーザー発言の保存 → LLM呼び出し →
/// (ツール呼び出しがあれば実行して結果を踏まえ再度呼び出し) → 確定した応答の保存、
/// までを1つの関数に閉じる(docs/spec/principles.md 5節)。
///
/// 添付は預かりから取り出してユーザー発言と一緒に書き、書けなければ預かりに戻す。
/// 本文が空白だけでも、添付があれば送れる(添付だけの発言)。
///
/// ユーザー発言を保存したあとの失敗は、`Err`で上位に返さずエラー発言として保存し
/// `Ok`で返す([`generate_turn_response`])。`Err`になるのは、その会話が既に応答を
/// 生成中のとき、タスクが無い(削除済みを含む)とき、本文も添付も無いとき、預けていない
/// 添付を指したとき、発言やエラー発言自体を書けないときだけ。
pub async fn run_turn(
    db: SharedConnection,
    ctx: &TurnContext<'_>,
    chat: Chat,
    input: impl Into<UserInput>,
) -> Result<()> {
    let UserInput { text, attachments } = input.into();
    let _generating = begin_generating(ctx.generating, chat)?;
    let taken = ctx.attachments.take_staged(&attachments)?;
    if let Err(e) = save_user_message(db.clone(), ctx, chat, text, taken.clone()).await {
        ctx.attachments.restore_staged(taken);
        return Err(e);
    }

    generate_turn_response(db, ctx, Attempt::first(chat)).await
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
            let message_id = messages::insert_message(
                conn,
                NewMessage {
                    task_id: chat.task_id(),
                    role: Role::User,
                    content: &text,
                    kind: Kind::Normal,
                    origin: Origin::User,
                    error_kind: None,
                    error_detail: None,
                    reasoning: None,
                },
            )?;
            for row in &rows {
                db_attachments::insert(conn, message_id, row)?;
            }
            Ok(())
        })
    })
    .await
}

/// [`create_task`]の結果。
#[derive(Debug, Serialize)]
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

/// 新規タスクの作成(Issue #76)。作ったタスクでは続けて[`open_task_chat`]で聞き取りを
/// 始めるので、チャットを使えない(モデル未選択等)ならタスクを作らずに理由を返す
/// (legacy/frontend.md 1節)。作ってしまうと、聞き取りが始まらずエラー発言だけのタスクが残る。
pub async fn create_task(db: SharedConnection, ctx: &TurnContext<'_>) -> Result<TaskCreation> {
    if let Err(failure) = ready_adapter(ctx) {
        return Ok(TaskCreation::Unavailable {
            error_kind: failure.kind(),
        });
    }
    let task = with_conn(db, tasks::create_task).await?;
    Ok(TaskCreation::Created { task })
}

/// 聞き取りの開始(Issue #76)。ユーザーの発言なしに、開始の発言
/// ([`TurnContext::opening_message`])への返信として最初のターンを生成する。開始の発言は
/// 保存せず、以降のターンも`history::build_history`が履歴の先頭に補う
/// (architecture.md 3節「聞き取りの開始」)。
///
/// まだ1行も発言の無いタスクでだけ行う。
pub async fn open_task_chat(
    db: SharedConnection,
    ctx: &TurnContext<'_>,
    task_id: i64,
) -> Result<()> {
    let chat = Chat::Task(task_id);
    let _generating = begin_generating(ctx.generating, chat)?;
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

    generate_turn_response(db, ctx, Attempt::first(chat)).await
}

/// 編集(ユーザー発言のみ、Issue #41)。対象の発言以降(自身を含む)の通常発言をすべて
/// 論理削除し、編集後の内容を新しい発言として挿入したうえで、新しいターンとして
/// 応答を生成し直す。ツール実行記録は対象外(`db::messages::soft_delete_normal_from`
/// 参照)。添付は新しい発言へ引き継ぐ(legacy/frontend.md 1節)。
pub async fn edit_user_message(
    db: SharedConnection,
    ctx: &TurnContext<'_>,
    chat: Chat,
    message_id: i64,
    new_text: String,
) -> Result<()> {
    let _generating = begin_generating(ctx.generating, chat)?;
    with_conn(db.clone(), move |conn| {
        // 挿入だけが失敗すると、会話がその位置から消えたまま置き換わらない。
        in_transaction(conn, |conn| {
            let target = find_in_chat(conn, chat, message_id)?;
            expect_normal(&target, &[Role::User])?;
            messages::soft_delete_normal_from(conn, chat, target.id)?;
            let message_id = messages::insert_message(
                conn,
                NewMessage {
                    task_id: chat.task_id(),
                    role: Role::User,
                    content: &new_text,
                    kind: Kind::Normal,
                    origin: Origin::User,
                    error_kind: None,
                    error_detail: None,
                    reasoning: None,
                },
            )?;
            let carried = db_attachments::copy_to_message(conn, target.id, message_id)?;
            // 断るとトランザクションごと戻り、元の発言は消えない。
            require_content(&new_text, carried)
        })
    })
    .await?;

    generate_turn_response(db, ctx, Attempt::first(chat)).await
}

/// 再試行(ターンの返信のみ、Issue #41・#130)。対象の発言以降(自身を含む)の通常発言を
/// 論理削除し、同じ`turn_id`のまま`attempt_no`を増やして応答を生成し直す
/// (`docs/spec/rebuild/data-model.md`「ターン境界」)。対応するユーザー発言は
/// `id < message_id`のためカスケードの対象外で、そのまま履歴に残る。
///
/// 返信は成功時のアシスタント発言と失敗時のエラー発言のどちらでもよい。どちらも1試行に
/// 1行だけの通常発言で(data-model.md「1ターン内の往復で保存するもの」)、作り直し方は
/// 変わらない。
///
/// 削除はカスケードしないので、ターンのユーザー発言だけが消されていることがある。返信以降を
/// 消した残りが応答すべき発言で終わらなければ断る(`history::awaits_reply`)。新規送信と編集は
/// 必ずユーザー発言を用意してから生成するので、この確認は再試行にだけ要る。
pub async fn retry_reply(
    db: SharedConnection,
    ctx: &TurnContext<'_>,
    chat: Chat,
    message_id: i64,
) -> Result<()> {
    let _generating = begin_generating(ctx.generating, chat)?;
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

    generate_turn_response(db, ctx, attempt).await
}

/// 削除(共通、Issue #41)。確認ダイアログ無しの即座に取り消し可能な論理削除で、
/// カスケードはしない(対象の1件だけを消す。編集・再試行のカスケード削除とは別の操作)。
/// 対象はユーザー発言とターンの返信(`db::messages::soft_delete_message`が検証する)。
/// 生成中の会話では断る。生成中のターンが読んだ履歴と、DBの発言が食い違うため。
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
            messages::soft_delete_message(conn, target.id)
        })
    })
    .await
}

/// ユーザー発言には本文か添付のどちらかが要る。本文が空白だけでも、添付があれば送れる。
fn require_content(text: &str, attachment_count: usize) -> Result<()> {
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
fn require_chat(conn: &Connection, chat: Chat) -> Result<()> {
    if let Chat::Task(task_id) = chat {
        tasks::get_task(conn, task_id)?;
    }
    Ok(())
}

/// 編集・再試行・削除の対象を引き、その会話の発言であることを1箇所で確認する
/// (`docs/spec/principles.md` 5節)。
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

/// `edit_user_message`/`retry_reply`共通の役割・種別の確認。
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

/// 応答生成の本体(architecture.md 1節)。LLM呼び出し →
/// (ツール呼び出しがあれば実行して結果を踏まえ再度呼び出し) → 確定した応答の保存、
/// までを1つの関数に閉じる(docs/spec/principles.md 5節)。`run_turn`(新規発言)・
/// `edit_user_message`(編集)・`retry_reply`(再試行)はいずれも、対象となる
/// ユーザー発言をDBに用意した上でこれを呼ぶ共通の末尾処理。
///
/// 外部(MCP)サーバーへの接続はこのターンの間だけ生かし、結果によらずここで閉じる
/// (legacy/backend.md 9節。ラウンドの途中で抜ける経路が複数あるため、往復の本体は
/// [`run_tool_rounds`]に分け、切断をこの1箇所に集める)。
///
/// 失敗はどれも`Err`で上位に返さず、この試行のエラー発言として保存して`Ok`で返す
/// (Issue #40)。プロバイダー未選択・空応答・上限到達のような想定内の失敗に加え、途中の
/// `Err`も`turn_error::classify`で種別を決めて残す。再試行の失敗も1回目と同じ形で残り、
/// もう一度再試行できるようにするため(再試行は返信を消してから作り直すので、何も
/// 書かずに抜けるとターンごと会話から消える)。`Err`が返るのはエラー発言自体を書けない
/// ときだけ。
async fn generate_turn_response(
    db: SharedConnection,
    ctx: &TurnContext<'_>,
    attempt: Attempt,
) -> Result<()> {
    let adapter = match ready_adapter(ctx) {
        Ok(adapter) => adapter,
        Err(failure) => return fail_turn(db, &attempt, failure).await,
    };

    let mut sessions = McpSessions::new();
    // ツールに対応しないモデルには外部ツールも渡さないので、外部サーバーにも繋がない。
    let external = if ctx.capabilities.tools {
        prepare_external_tools(&ctx.mcp, attempt.chat, &mut sessions).await
    } else {
        ExternalToolset::default()
    };
    let result =
        run_tool_rounds(db.clone(), adapter, ctx, &attempt, &external, &mut sessions).await;
    sessions.close().await;
    match result {
        Err(e) => fail_turn(db, &attempt, turn_error::classify(&e)).await,
        done => done,
    }
}

/// 呼び出しに使えるアダプタ。使えなければ、ターンを終えるエラー発言の分類。
fn ready_adapter<'a>(
    ctx: &TurnContext<'a>,
) -> std::result::Result<&'a dyn LlmAdapter, TurnFailure> {
    let adapter = ctx.adapter.clone()?;
    match turn_error::from_readiness(adapter.readiness()) {
        Some(failure) => Err(failure),
        None => Ok(adapter),
    }
}

/// 応答生成の1試行(data-model.md「ターン境界」)。この試行で書く行は、すべてこの組を
/// そのまま持つ。
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
                task_id: self.chat.task_id(),
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

/// このターンでモデルへ公開する外部ツールを決める。ツールを1つも有効化していない
/// サーバーには接続しない(ユーザーが有効化していない以上、繋ぐ理由が無い)。
/// 総合チャットにも公開する(tools.md 5節。権限の分離はSCITL自身のタスクへの書き込みの話で、
/// 外部ツールの安全性の境界はユーザーが信頼して登録したこと)。
///
/// 一覧はキャッシュ(Issue #104)を優先し、無ければ取得してキャッシュに載せる。
/// 接続・取得に失敗したサーバーはこのターンでは公開しない。ここでターン全体を失敗させると、
/// 外部サーバーが1つ落ちているだけでチャットが使えなくなるため(principles.md 3節)。
async fn prepare_external_tools(
    mcp: &McpAccess<'_>,
    chat: Chat,
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
    ExternalToolset::build(fetched, &tools::names(chat))
}

/// ツールの上限に達したあとの最後の呼び出しで、システムプロンプトの末尾に足す一節。
/// ツールを渡さない理由を伝えないと、モデルがツールを呼ぶつもりの文を返しがちになる。
const ROUND_LIMIT_NOTE: &str = "The tool call limit for this turn has been reached, so no tools \
     are available now. Reply to the user based on the tool results so far.";

/// LLM呼び出しとツール呼び出しの往復。切断の都合で[`generate_turn_response`]から
/// 分けてあるだけで、1ターンの流れとしては地続き。
async fn run_tool_rounds(
    db: SharedConnection,
    adapter: &dyn LlmAdapter,
    ctx: &TurnContext<'_>,
    attempt: &Attempt,
    external: &ExternalToolset,
    sessions: &mut McpSessions,
) -> Result<()> {
    let chat = attempt.chat;
    // 内部ツールと外部ツールを1つの一覧にして公開する(Issue #44)。名前空間化と
    // 衝突の排除は`ExternalToolset`が済ませてある。ツールに対応しないモデルには何も渡さない
    // (対応しないモデルにツールを渡すと、リクエストごと拒否するサーバーがある)。
    let tools_available = ctx.capabilities.tools;
    // 呼び出し元(`run_turn`/`edit_user_message`/`retry_reply`)が対象の
    // ユーザー発言の挿入・カスケード削除を済ませたあとの状態を読む。
    let stored = with_conn(db.clone(), move |conn| history::load(conn, chat)).await?;
    let options = HistoryOptions {
        tools_available,
        image_input: ctx.capabilities.image,
        opening: ctx.opening_message.to_string(),
    };
    // 添付画像の読み出しはファイルI/Oなので、DBのロックの外でブロッキング処理として行う。
    let store = ctx.attachments.store();
    let history =
        blocking::run(move || Ok(history::build_history(stored, &options, &store))).await?;
    let mut exposed_tools = Vec::new();
    if tools_available {
        exposed_tools.extend(tools::schemas(chat));
        exposed_tools.extend(external.schemas());
    }

    // `run_turn`はawaitをまたぐため、'staticなクロージャに載せられるよう所有した文字列に
    // 変換しておく(`SystemPrompts`自体はDBスレッドとやり取りするラウンドごとに組み直す)。
    let base_owned = ctx.prompts.base.map(str::to_string);
    let task_chat_owned = ctx.prompts.task_chat.map(str::to_string);
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
    // ツールを呼んだラウンドにモデルが添えた本文も、このターンの返信の一部として最終行に
    // まとめて保存する(docs/spec/rebuild/data-model.md「1ターン内の往復で保存するもの」)。
    let mut reply_parts: Vec<String> = Vec::new();

    // 上限のラウンドまでツールを実行したら、ツールを渡さずにもう一度だけ呼ぶ
    // (docs/spec/rebuild/tools.md 4節)。`u64`で数えるのは、上限が`u32::MAX`でも
    // 最後の1回を数えられるようにするため。ツールに対応しないモデルは、最初の呼び出しが
    // その最後の1回になる。
    let tool_rounds = if tools_available {
        u64::from(ctx.limits.max_rounds_per_turn)
    } else {
        0
    };
    for round in 1..=tool_rounds + 1 {
        let final_call = round > tool_rounds;
        let mut system_prompt_text = with_conn(db.clone(), {
            let base_owned = base_owned.clone();
            let task_chat_owned = task_chat_owned.clone();
            move |conn| {
                let prompts = SystemPrompts {
                    base: base_owned.as_deref(),
                    task_chat: task_chat_owned.as_deref(),
                };
                build_system_prompt(conn, chat, &prompts, tools_available)
            }
        })
        .await?;
        if final_call && tools_available {
            system_prompt_text.push_str("\n\n");
            system_prompt_text.push_str(ROUND_LIMIT_NOTE);
        }

        let offered: &[ToolSchema] = if final_call { &[] } else { &exposed_tools };
        let system = ChatMessage::System(system_prompt_text);
        // システムプロンプトとこのラウンドまでの往復はラウンドごとに伸びるので、間引きも
        // ラウンドごとにやり直す。
        let kept = trim_history(
            &history,
            ctx.capabilities.context_length,
            std::iter::once(&system).chain(&round_trip),
            offered,
        );

        let mut messages_to_send = Vec::with_capacity(1 + kept.len() + round_trip.len());
        messages_to_send.push(system);
        messages_to_send.extend(kept.iter().cloned());
        messages_to_send.extend(round_trip.iter().cloned());

        // 受け取った順に画面へ流しつつ、解釈はラウンドを受け取り終えてから行う。
        let mut events = Vec::new();
        let notify = ctx.events;
        let sent = adapter
            .send(
                &messages_to_send,
                offered,
                ctx.reasoning_effort,
                &mut |event| {
                    notify(TurnEvent::Response {
                        event: event.clone(),
                    });
                    events.push(event);
                },
            )
            .await;
        if let Err(e) = sent {
            return fail_turn(db, attempt, turn_error::classify(&e)).await;
        }

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
        let reasoning_for_db = (!reasoning.is_empty()).then_some(reasoning);

        if tool_calls.is_empty() {
            if !text.is_empty() {
                reply_parts.push(text);
            }
            let reply = reply_parts.join("\n\n");
            if reply.is_empty() {
                return fail_turn(db, attempt, TurnFailure::EmptyResponse).await;
            }

            let attempt = attempt.clone();
            with_conn(db, move |conn| {
                attempt.insert(
                    conn,
                    Role::Assistant,
                    &reply,
                    Kind::Normal,
                    None,
                    reasoning_for_db.as_deref(),
                )
            })
            .await?;
            return Ok(());
        }
        // ツールを渡していないのに呼んできた。実行はせず、ツールを渡さなかった理由
        // (上限に達した・ツールに対応しないモデル)のエラーで終える。
        if final_call {
            let failure = if tools_available {
                TurnFailure::ToolRoundLimit
            } else {
                TurnFailure::ToolsDisabled
            };
            return fail_turn(db, attempt, failure).await;
        }

        // 1応答に複数のtool_callsが載る場合、すべて実行する(取りこぼさない)。
        let mut executed: Vec<(ToolCallRequest, CallOutcome)> =
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
            if tool_time_used >= ctx.limits.total_timeout {
                return fail_turn(db, attempt, TurnFailure::ToolTimeout).await;
            }
            let started = Instant::now();
            let outcome = execute_call(db.clone(), chat, ctx, external, sessions, &call).await?;
            tool_time_used = tool_time_used.saturating_add(started.elapsed());

            // このラウンドの思考は、ラウンド内最初のツール実行記録の`reasoning`列に
            // 1回だけ紐付ける(発生順に混在させて表示するため。同一ラウンドの
            // 全呼び出しに複製すると「思考・ツール」折りたたみの件数が水増しされる)。
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
                tool_kind: outcome.tool_kind,
                call_id: call.id.clone(),
            };
            save_tool_execution(db.clone(), attempt, record, reasoning_for_row, ctx.events).await?;
            executed.push((call, outcome));
        }

        // モデルへの往復: assistant(tool_calls) 1件 + tool(結果) を呼び出し数ぶん。
        // OpenAI互換プロトコルの標準的な表現に合わせる(architecture.md 3節)。
        if !text.is_empty() {
            reply_parts.push(text.clone());
        }
        round_trip.push(ChatMessage::Assistant {
            content: if text.is_empty() { None } else { Some(text) },
            tool_calls: executed.iter().map(|(call, _)| call.clone()).collect(),
        });
        // 結果には自由入力が載る。保存する実行記録(上)は受け取ったまま残し、モデルへ
        // 送る側でだけ無害化する(docs/spec/rebuild/architecture.md 10節)。
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
/// 内部・外部のどちらも、実行の失敗は`Err`で上に返さず`{"error": ...}`の結果JSONに
/// 落としてターンを続ける(docs/spec/principles.md 3節「失敗しても会話を止めない」)。
/// 引数の型違いや対象の取り違えはモデルが自分で直せる失敗であり、外部サーバーの
/// 不達に至っては日常的に起こるため、モデルに失敗を伝えて続けさせる方が会話として
/// 自然になる。返る`Err`はDBスレッドか、添付の実体を読むブロッキング処理自体が落ちた
/// 場合だけで、それは呼び出し元が実行記録を保存できないのと同じ状況にあたる。
///
/// 引数がJSONとして読めなかった呼び出し(`ToolArguments::Malformed`)は、どのツールも
/// 実行せずに失敗を返し、出し直させる。
///
/// 結果と一緒に、実行したツールの分類(tools.md 4節)を返す。分類は振り分け先の定義から
/// 引き、ここでは決めない。実行しなかった呼び出し(引数が読めない・公開していない名前・
/// 接続先が無い)は`None`で、次ターン以降の履歴に載らない。
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
            return Ok(CallOutcome::plain(result, None));
        }
    };
    let Some((server_id, tool_name, kind)) = external.route(&call.name) else {
        let name = call.name.clone();
        let arguments = arguments.clone();
        let image_input = ctx.capabilities.image;
        let (output, kind) = with_conn(db, move |conn| {
            let output = tools::execute(conn, chat, image_input, &name, &arguments)
                .unwrap_or_else(|e| json!({ "error": e.to_string() }).into());
            Ok((output, tools::kind(chat, &name)))
        })
        .await?;
        return read_tool_images(output, kind, ctx.attachments.store()).await;
    };

    let Some(server) = ctx.mcp.servers.iter().find(|s| s.id == server_id) else {
        let result = json!({ "error": format!("MCP server not found: {server_id}") });
        return Ok(CallOutcome::plain(result, None));
    };
    let result = sessions
        .call_tool(server, tool_name, arguments)
        .await
        .unwrap_or_else(|e| json!({ "error": e.to_string() }));
    Ok(CallOutcome::plain(result, Some(kind)))
}

/// ツール1件の実行の結果。`turn_result`と`images`はこのターンのモデルへの往復にだけ載せ、
/// 実行記録には残さない([`ToolOutput`])。
struct CallOutcome {
    result: serde_json::Value,
    tool_kind: Option<ToolKind>,
    turn_result: Option<serde_json::Value>,
    images: Vec<InlineImage>,
}

impl CallOutcome {
    /// 実行記録と往復で同じ結果を返し、画像を伴わない。
    fn plain(result: serde_json::Value, tool_kind: Option<ToolKind>) -> Self {
        Self {
            result,
            tool_kind,
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
async fn read_tool_images(
    output: ToolOutput,
    tool_kind: Option<ToolKind>,
    store: AttachmentStore,
) -> Result<CallOutcome> {
    if output.image_hashes.is_empty() {
        return Ok(CallOutcome {
            turn_result: output.turn_result,
            ..CallOutcome::plain(output.result, tool_kind)
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
            tool_kind,
            turn_result: output.turn_result,
            images,
        },
        Err(e) => CallOutcome::plain(json!({ "error": e.to_string() }), tool_kind),
    })
}

/// ツール実行記録を保存する唯一の入口。保存した値をそのまま画面へ知らせる
/// ([`TurnEvent::ToolExecuted`])。画面への出力の規則(architecture.md 10節)は保存値を
/// 前提にしているので、保存する値と知らせる値をここ1箇所で作る。
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
/// の定型文言、`error_detail`は`failure.detail()`(Issue #159)。
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
