//! 会話のコマンド。scitl-cliの表示に、送信内容のプレビューと応答生成を足す。
//!
//! 応答生成は、別のプロセス(GUI等)が同じ会話で生成中でも断らない
//! (`docs/spec/rebuild/data-model.md`「複数プロセスからの書き込みの排他」)。

use std::path::PathBuf;

use clap::Subcommand;
use serde::Serialize;

use scitl_cli::terminal::{print_json, print_json_line};
use scitl_cli::{ChatArg, Session};
use scitl_core::attachments::{Attachments, StageOutcome};
use scitl_core::db;
use scitl_core::db::messages::Chat;
use scitl_core::db::tasks::Task;
use scitl_core::orchestration::{
    self, discard_events, MessageView, PreviewOptions, TaskCreation, TurnContext, TurnEvent,
    TurnEvents, UserInput,
};
use scitl_core::settings::Snapshot;

use crate::{load_settings, open_attachments, DebugError};

#[derive(Subcommand)]
pub enum ChatCommand {
    #[command(flatten)]
    Base(scitl_cli::ChatCommand),
    /// Show the request body the next turn would send to the model, without sending it or
    /// saving anything. Images are shortened to their type and length. Like a turn, this reads
    /// the API key from the OS credential store and may ask the registered local inference
    /// server for the model's capabilities.
    Preview {
        #[command(flatten)]
        chat: ChatArg,
        /// The next message to send. Without it, the conversation as saved.
        #[arg(long, value_name = "TEXT")]
        message: Option<String>,
        /// Connect to the enabled MCP servers to include their tools.
        #[arg(long)]
        external_tools: bool,
    },
    /// Send a message and generate the response. Progress is written as one JSON value per
    /// line, ending with the last message of the conversation. A failed turn is saved as an
    /// error message and still exits successfully. Do not run this while another process is
    /// generating in the same conversation: that is not refused.
    Send {
        #[command(flatten)]
        chat: ChatArg,
        /// File to attach. Can be repeated.
        #[arg(long = "attach", value_name = "FILE")]
        attachments: Vec<PathBuf>,
        /// The message. Can be omitted when files are attached.
        text: Option<String>,
    },
    /// Generate the reply of a turn again. Output and caveats are those of `send`.
    Retry {
        #[command(flatten)]
        chat: ChatArg,
        /// The reply (an assistant or error message) to replace.
        message_id: i64,
    },
    /// Generate a reply in a conversation that ends without one (for example, the process ended
    /// while generating). Nothing is deleted. Refused when the conversation ends with a reply or
    /// an error message; use `retry` for those. Output and caveats are those of `send`.
    Reply {
        #[command(flatten)]
        chat: ChatArg,
    },
}

/// 応答生成の出力の1行。途中経過([`TurnEvent`])と同じく`type`で見分ける。
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Outcome<'a> {
    TaskCreated { task: &'a Task },
    LastMessage { message: &'a MessageView },
}

fn print_event(event: TurnEvent) {
    print_json_line(&event);
}

/// ターンの文脈を作る材料。GUIの送信と同じく、設定の複製を取ってから生成へ進む。
struct Turns {
    snapshot: Snapshot,
    attachments: Attachments,
}

impl Turns {
    async fn open(session: &Session) -> Result<Self, DebugError> {
        let settings = load_settings(session).await?;
        Ok(Self {
            snapshot: settings.snapshot_for_turn().await,
            attachments: open_attachments(session)?,
        })
    }

    fn context<'a>(&'a self, session: &'a Session, events: TurnEvents<'a>) -> TurnContext<'a> {
        self.snapshot
            .turn_context(&session.generating, &self.attachments, events)
    }
}

pub async fn run(session: &Session, command: ChatCommand) -> Result<(), DebugError> {
    let db = session.db.clone();
    match command {
        ChatCommand::Base(command) => scitl_cli::run_chat(session, command).await?,
        ChatCommand::Preview {
            chat,
            message,
            external_tools,
        } => {
            let turns = Turns::open(session).await?;
            let ctx = turns.context(session, &discard_events);
            let options = PreviewOptions {
                message,
                external_tools,
            };
            let preview = orchestration::preview_request(db, &ctx, chat.chat(), options)
                .await?
                .map_err(|failure| DebugError::ChatUnavailable(failure.user_message()))?;
            print_json(&preview);
        }
        ChatCommand::Send {
            chat,
            attachments,
            text,
        } => {
            let chat = chat.chat();
            let turns = Turns::open(session).await?;
            let input = UserInput {
                text: text.unwrap_or_default(),
                attachments: stage(&turns.attachments, attachments)?,
            };
            let ctx = turns.context(session, &print_event);
            orchestration::run_turn(db, &ctx, chat, input).await?;
            print_last_message(session, chat).await?;
        }
        ChatCommand::Retry { chat, message_id } => {
            let chat = chat.chat();
            let turns = Turns::open(session).await?;
            let ctx = turns.context(session, &print_event);
            orchestration::retry_reply(db, &ctx, chat, message_id).await?;
            print_last_message(session, chat).await?;
        }
        ChatCommand::Reply { chat } => {
            let chat = chat.chat();
            let turns = Turns::open(session).await?;
            let ctx = turns.context(session, &print_event);
            orchestration::generate_reply(db, &ctx, chat).await?;
            print_last_message(session, chat).await?;
        }
    }
    Ok(())
}

/// タスクを作り、GUIの新規タスク追加と同じく続けて聞き取りを始める。
pub async fn create_task(session: &Session) -> Result<(), DebugError> {
    let turns = Turns::open(session).await?;
    let ctx = turns.context(session, &print_event);
    let task = match orchestration::create_task(session.db.clone(), &ctx).await? {
        TaskCreation::Created { task } => task,
        TaskCreation::Unavailable { error_kind } => {
            return Err(DebugError::ChatUnavailable(error_kind.to_string()));
        }
    };
    print_json_line(&Outcome::TaskCreated { task: &task });
    orchestration::open_task_chat(session.db.clone(), &ctx, task.id).await?;
    print_last_message(session, Chat::Task(task.id)).await
}

/// ファイルを読んで預け、送信に渡すトークンにする。1つでも受け付けられなければ送らない。
fn stage(attachments: &Attachments, files: Vec<PathBuf>) -> Result<Vec<String>, DebugError> {
    files
        .into_iter()
        .map(|path| {
            let bytes = std::fs::read(&path).map_err(|source| DebugError::ReadFile {
                path: path.clone(),
                source,
            })?;
            let name = path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into_owned();
            match attachments.stage(name, bytes)? {
                StageOutcome::Staged { token, .. } => Ok(token),
                StageOutcome::Rejected { reason } => Err(DebugError::AttachmentRejected {
                    path,
                    reason: serde_json::to_string(&reason).expect("rejections serialize to JSON"),
                }),
            }
        })
        .collect()
}

/// 生成を終えた会話の最後の発言(返信か、失敗を表すエラー発言)を出す。
async fn print_last_message(session: &Session, chat: Chat) -> Result<(), DebugError> {
    let messages = db::with_conn(session.db.clone(), move |conn| {
        orchestration::list_chat(conn, chat)
    })
    .await?;
    if let Some(message) = messages.last() {
        print_json_line(&Outcome::LastMessage { message });
    }
    Ok(())
}
