//! SCITLのCLI。GUIと同じcoreを直接呼び、GUIを開かずにタスクの確認・操作と会話の表示が
//! できる状態を保つ。外部のLLMツールにもシェル経由で使わせる入口なので、範囲はここまでに
//! 留める。応答生成・設定・秘密情報に触れるコマンドはscitl-debug-cliが持つ。

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use scitl_cli::{terminal, ChatCommand, CliError, DataDirArg, Session, TaskCommand};

#[derive(Parser)]
#[command(
    name = "scitl-cli",
    version,
    about = "Command line interface for SCITL Task Companion"
)]
struct Cli {
    #[command(flatten)]
    data_dir: DataDirArg,
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

#[tokio::main]
async fn main() -> ExitCode {
    let cli = match terminal::parse::<Cli>() {
        Ok(cli) => cli,
        Err(code) => return code,
    };
    terminal::finish(run(cli).await)
}

async fn run(cli: Cli) -> Result<(), CliError> {
    let session = Session::open(cli.data_dir)?;
    match cli.command {
        Command::Task(command) => scitl_cli::run_task(&session, command).await,
        Command::Chat(command) => scitl_cli::run_chat(&session, command).await,
    }
}
