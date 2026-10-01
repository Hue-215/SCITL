//! SCITLのデバッグ用CLI。GUIを開かずに、応答生成・設定・登録まで含めて動作を確かめる。
//! scitl-cliのコマンドをそのまま持ち、その上に足す。
//!
//! 端末へは`scitl_cli::terminal`だけから書く。秘密情報(APIキー、MCPサーバーの環境変数・
//! ヘッダーの値)は引数では受け取らず、このプロセスの環境変数から読んでcoreへ渡す
//! ([`settings::secret_from_env`])。

mod chat;
mod settings;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};

use scitl_cli::terminal::{self, print_json};
use scitl_cli::{CliError, DataDirArg, Session};
use scitl_core::attachments::{AttachmentStore, Attachments};
use scitl_core::settings::Settings;
use scitl_core::{blocking, db, paths, CoreError};

#[derive(Parser)]
#[command(
    name = "scitl-debug-cli",
    version,
    about = "Debugging command line interface for SCITL Task Companion"
)]
struct Cli {
    #[command(flatten)]
    data_dir: DataDirArg,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List, show, change and create tasks.
    #[command(subcommand)]
    Task(TaskCommand),
    /// Show conversations, preview requests and generate responses.
    #[command(subcommand)]
    Chat(chat::ChatCommand),
    /// Inspect attachments and the files they point at.
    #[command(subcommand)]
    Attachment(AttachmentCommand),
    /// Write every task and the general chat as Markdown into a new folder under the data
    /// directory.
    Export,
    /// Show and change the general settings.
    #[command(subcommand)]
    Settings(settings::SettingsCommand),
    /// Register and remove LLM providers.
    #[command(subcommand)]
    Provider(settings::ProviderCommand),
    /// Register, remove and select models of a provider.
    #[command(subcommand)]
    Model(settings::ModelCommand),
    /// Register, change and remove external tool (MCP) servers.
    #[command(subcommand)]
    Mcp(settings::McpCommand),
}

#[derive(Subcommand)]
enum TaskCommand {
    #[command(flatten)]
    Base(scitl_cli::TaskCommand),
    /// Create a task and generate the reply that opens its conversation. Not created while
    /// the chat is unavailable (no provider or model selected).
    Create,
}

#[derive(Subcommand)]
enum AttachmentCommand {
    /// List every attachment, including those of deleted messages.
    List,
    /// List the stored files no attachment points at.
    Orphans {
        /// Delete them. Run this only while no other process is sending attachments: a file
        /// being sent looks orphaned until its message is saved.
        #[arg(long)]
        delete: bool,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = match terminal::parse::<Cli>() {
        Ok(cli) => cli,
        Err(code) => return code,
    };
    terminal::finish(run(cli).await)
}

async fn run(cli: Cli) -> Result<(), DebugError> {
    let session = Session::open(cli.data_dir)?;
    match cli.command {
        Command::Task(TaskCommand::Base(command)) => {
            scitl_cli::run_task(&session, command).await?;
        }
        Command::Task(TaskCommand::Create) => chat::create_task(&session).await?,
        Command::Chat(command) => chat::run(&session, command).await?,
        Command::Attachment(AttachmentCommand::List) => {
            let records = db::with_conn(session.db.clone(), db::attachments::list_all).await?;
            print_json(&records);
        }
        Command::Attachment(AttachmentCommand::Orphans { delete }) => {
            let attachments = open_attachments(&session)?;
            print_json(
                &attachments
                    .orphaned_blobs(session.db.clone(), delete)
                    .await?,
            );
        }
        Command::Export => {
            let attachments = open_attachments(&session)?;
            let summary = scitl_core::export::export_markdown(
                session.db.clone(),
                &attachments,
                session.data.export(),
            )
            .await?;
            print_json(&summary);
        }
        Command::Settings(command) => settings::run_settings(&session, command).await?,
        Command::Provider(command) => settings::run_provider(&session, command).await?,
        Command::Model(command) => settings::run_model(&session, command).await?,
        Command::Mcp(command) => settings::run_mcp(&session, command).await?,
    }
    Ok(())
}

/// 設定ファイルを読む。アクティブなプロバイダーの鍵を資格情報ストアから読むので、設定に
/// 触れるコマンドだけが呼ぶ。
async fn load_settings(session: &Session) -> Result<Arc<Settings>, DebugError> {
    let path = session.data.config();
    Ok(Arc::new(
        blocking::run(move || Ok(Settings::load(path))).await?,
    ))
}

/// 添付の置き場所。GUIと同じ場所を使う。
fn open_attachments(session: &Session) -> Result<Attachments, DebugError> {
    let cache = paths::default_cache_dir().map_err(DebugError::NoCacheDir)?;
    Ok(Attachments::new(AttachmentStore::new(
        session.data.attachments(),
        paths::revealed_attachments(&cache),
    )))
}

#[derive(Debug, thiserror::Error)]
enum DebugError {
    #[error(transparent)]
    Cli(#[from] CliError),
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error("{0}")]
    NoCacheDir(paths::NoAppDir),
    /// ターンならエラー発言になる理由(プロバイダー・モデルの未選択等)。
    #[error("chat is unavailable: {0}")]
    ChatUnavailable(String),
    #[error("environment variable {0} is not set or is not valid Unicode")]
    MissingEnv(String),
    #[error("failed to read {}: {kind:?}", .path.display())]
    ReadFile {
        path: PathBuf,
        kind: std::io::ErrorKind,
    },
    #[error("{} was not accepted as an attachment: {reason}", .path.display())]
    AttachmentRejected { path: PathBuf, reason: String },
}
