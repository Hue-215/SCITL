//! SCITLのCLI。GUIと同じcoreを直接呼び、GUIを開かずに同じ検証を通って操作・確認できる状態を
//! 保つ。応答生成(送信・再試行)は行わない。送信内容のプレビューは、モデルを呼ばずに次の
//! ターンのリクエストを組み立てて見せる。
//!
//! 出力はJSONに揃え、端末へは[`print_json`]・[`print_error`]・[`print_clap`]だけから書く。

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use clap::{Parser, Subcommand};
use serde::Serialize;

use scitl_core::attachments::{AttachmentStore, Attachments};
use scitl_core::db::messages::{Chat, OperationSource};
use scitl_core::db::{self, SharedConnection};
use scitl_core::in_flight::InFlightSet;
use scitl_core::orchestration::{self, discard_events, operations, PreviewOptions};
use scitl_core::paths::{self, DataLayout};
use scitl_core::settings::Settings;
use scitl_core::tools::get_current_task_detail::task_detail;
use scitl_core::{text, CoreError};

#[derive(Parser)]
#[command(
    name = "scitl-cli",
    version,
    about = "Command line interface for SCITL Task Companion"
)]
struct Cli {
    /// Data directory to open. Defaults to the one the desktop app uses.
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List, show and change tasks.
    #[command(subcommand)]
    Task(TaskCommand),
    /// Show conversations.
    #[command(subcommand)]
    Chat(ChatCommand),
}

#[derive(Subcommand)]
enum TaskCommand {
    /// List tasks that are not deleted, including archived ones.
    List,
    /// Show a task with its steps, in the form the model receives.
    Show { id: i64 },
    /// Change the title of a task.
    Rename { id: i64, title: String },
    /// Archive a task.
    Archive { id: i64 },
    /// Unarchive a task.
    Unarchive { id: i64 },
    /// Delete a task. It stays in the database and can be restored.
    Delete { id: i64 },
}

#[derive(Subcommand)]
enum ChatCommand {
    /// Show the messages of a conversation as the app displays them.
    Show {
        /// Task whose conversation to show. Without it, the general chat.
        #[arg(long, value_name = "ID")]
        task: Option<i64>,
    },
    /// Show the request body the next turn would send to the model, without sending it or
    /// saving anything. Images are shortened to their type and length. Like a turn, this reads
    /// the API key from the OS credential store and may ask the registered local inference
    /// server for the model's capabilities.
    Preview {
        /// Task whose conversation to preview. Without it, the general chat.
        #[arg(long, value_name = "ID")]
        task: Option<i64>,
        /// The next message to send. Without it, the conversation as saved.
        #[arg(long, value_name = "TEXT")]
        message: Option<String>,
        /// Connect to the enabled MCP servers to include their tools.
        #[arg(long)]
        external_tools: bool,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    // `Cli::parse`はヘルプ・引数のエラーを自分で端末へ書くので、書く前に受け取る。
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => return print_clap(&e),
    };
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            print_error(&e);
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), CliError> {
    let data = DataLayout::new(match cli.data_dir {
        Some(dir) => dir,
        None => paths::default_data_dir()?,
    });
    // 無い場所を開くと空のDBを作ってしまう。打ち間違えた`--data-dir`で黙って空の一覧を
    // 返さないよう、ディレクトリが無ければ断る。既定の場所もGUIを一度起動するまでは無いが、
    // その間はタスクも無いので断って困らない。
    if !data.root().is_dir() {
        return Err(CliError::MissingDataDir(data.root().to_path_buf()));
    }
    let db: SharedConnection = Arc::new(Mutex::new(db::open(data.database())?));
    // CLIは応答を生成しないので、この集合は常に空。別プロセス(GUI)が生成中かどうかは
    // 見えない。
    let generating = InFlightSet::new();

    match cli.command {
        Command::Task(TaskCommand::List) => {
            print_json(&db::with_conn(db, db::tasks::list_tasks).await?);
        }
        Command::Task(TaskCommand::Show { id }) => {
            print_json(&db::with_conn(db, move |conn| task_detail(conn, id)).await?);
        }
        Command::Task(TaskCommand::Rename { id, title }) => {
            operations::rename_task(db, &generating, OperationSource::Cli, id, title).await?;
        }
        Command::Task(TaskCommand::Archive { id }) => {
            operations::set_task_archived(db, &generating, OperationSource::Cli, id, true).await?;
        }
        Command::Task(TaskCommand::Unarchive { id }) => {
            operations::set_task_archived(db, &generating, OperationSource::Cli, id, false).await?;
        }
        Command::Task(TaskCommand::Delete { id }) => {
            operations::delete_task(db, &generating, OperationSource::Cli, id).await?;
        }
        Command::Chat(ChatCommand::Show { task }) => {
            let chat = task.map_or(Chat::General, Chat::Task);
            let messages =
                db::with_conn(db, move |conn| orchestration::list_chat(conn, chat)).await?;
            print_json(&messages);
        }
        Command::Chat(ChatCommand::Preview {
            task,
            message,
            external_tools,
        }) => {
            let chat = task.map_or(Chat::General, Chat::Task);
            let settings = Settings::load(data.config());
            let snapshot = settings.snapshot_for_turn().await;
            let attachments = Attachments::new(AttachmentStore::new(
                data.attachments(),
                paths::revealed_attachments(&paths::default_cache_dir()?),
            ));
            let ctx = snapshot.turn_context(&generating, &attachments, &discard_events);
            let options = PreviewOptions {
                message,
                external_tools,
            };
            let preview = orchestration::preview_request(db, &ctx, chat, options)
                .await?
                .map_err(|failure| CliError::ChatUnavailable(failure.user_message()))?;
            print_json(&preview);
        }
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error("{0}; pass --data-dir")]
    NoDataDir(#[from] paths::NoAppDir),
    #[error("data directory {} does not exist", .0.display())]
    MissingDataDir(PathBuf),
    /// ターンならエラー発言になる理由(プロバイダー・モデルの未選択等)。
    #[error("{0}")]
    ChatUnavailable(String),
    #[error(transparent)]
    Core(#[from] CoreError),
}

/// 見えない文字を端末へ書かないよう、JSONのエスケープの形にしてから書く。
fn print_json(value: &impl Serialize) {
    let json = serde_json::to_string_pretty(value).expect("views serialize to JSON");
    println!("{}", text::reveal_invisible(&json));
}

/// エラーの表示文はタスクのタイトル等の値を含みうるので、出力と同じく見えない文字を見せる形にする。
fn print_error(error: &CliError) {
    eprintln!("error: {}", text::reveal_invisible(&error.to_string()));
}

/// clapが組み立てたヘルプ・引数のエラー。エラーは受け取った引数の値をそのまま含むので、
/// 他の出力と同じく見えない文字を見せる形にする。
fn print_clap(error: &clap::Error) -> ExitCode {
    let rendered = text::reveal_invisible(&error.render().to_string());
    if error.use_stderr() {
        eprint!("{rendered}");
    } else {
        print!("{rendered}");
    }
    ExitCode::from(u8::try_from(error.exit_code()).unwrap_or(1))
}
