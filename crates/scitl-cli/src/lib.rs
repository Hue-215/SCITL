//! scitl-cliのコマンドと、端末・データディレクトリの扱い。scitl-debug-cliは同じコマンドを
//! そのまま持つので、バイナリ(`main.rs`)から分けてここに置く。
//!
//! 端末へは[`terminal`]だけから書く。

pub mod terminal;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clap::{Args, Subcommand};

use scitl_core::db::messages::{Chat, OperationSource};
use scitl_core::db::{self, SharedConnection};
use scitl_core::in_flight::InFlightSet;
use scitl_core::orchestration::{self, operations};
use scitl_core::paths::{self, DataLayout};
use scitl_core::tools::get_current_task_detail::task_detail;
use scitl_core::CoreError;

use terminal::print_json;

#[derive(Args)]
pub struct DataDirArg {
    /// Data directory to open. Defaults to the one the desktop app uses.
    #[arg(long, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,
}

/// 対象の会話を選ぶ引数。
#[derive(Args)]
pub struct ChatArg {
    /// Task whose conversation to use. Without it, the general chat.
    #[arg(long, value_name = "ID")]
    pub task: Option<i64>,
}

impl ChatArg {
    pub fn chat(&self) -> Chat {
        self.task.map_or(Chat::General, Chat::Task)
    }
}

#[derive(Subcommand)]
pub enum TaskCommand {
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
pub enum ChatCommand {
    /// Show the messages of a conversation as the app displays them.
    Show {
        #[command(flatten)]
        chat: ChatArg,
    },
}

/// 開いたデータディレクトリ。1回の起動の間ずっと同じものを使う。
pub struct Session {
    pub data: DataLayout,
    pub db: SharedConnection,
    /// このプロセスが応答を生成中の会話。別プロセス(GUI等)が生成中かどうかは見えない。
    pub generating: InFlightSet<Chat>,
}

impl Session {
    /// `data_dir`が無ければ、GUIと同じ既定の場所を開く。
    ///
    /// 無い場所を開くと空のDBを作ってしまう。打ち間違えた`--data-dir`で黙って空の一覧を
    /// 返さないよう、ディレクトリが無ければ断る。既定の場所もGUIを一度起動するまでは無いが、
    /// その間はタスクも無いので断って困らない。
    ///
    /// 書き込めないディレクトリも断る。読むだけのコマンドでも、DBを開くときにログ先行書き込みの
    /// ファイルを作る。
    pub fn open(arg: DataDirArg) -> Result<Self, CliError> {
        let data = DataLayout::new(match arg.data_dir {
            Some(dir) => dir,
            None => paths::default_data_dir()?,
        });
        let root = data.root();
        if !root.is_dir() {
            return Err(if root.exists() {
                CliError::DataDirNotADirectory(root.to_path_buf())
            } else {
                CliError::MissingDataDir(root.to_path_buf())
            });
        }
        let conn = db::open(data.database()).map_err(|e| {
            if db::is_read_only_error(&e) {
                CliError::ReadOnlyDataDir(root.to_path_buf())
            } else {
                CliError::Core(e)
            }
        })?;
        let db = Arc::new(Mutex::new(conn));
        Ok(Self {
            data,
            db,
            generating: InFlightSet::new(),
        })
    }
}

/// タスクの確認と操作。書き込み系は成功しても何も出さない。
pub async fn run_task(session: &Session, command: TaskCommand) -> Result<(), CliError> {
    let db = session.db.clone();
    let generating = &session.generating;
    let source = OperationSource::Cli;
    match command {
        TaskCommand::List => print_json(&db::with_conn(db, db::tasks::list_tasks).await?),
        TaskCommand::Show { id } => {
            print_json(&db::with_conn(db, move |conn| task_detail(conn, id)).await?);
        }
        TaskCommand::Rename { id, title } => {
            operations::rename_task(db, generating, source, id, title).await?;
        }
        TaskCommand::Archive { id } => {
            operations::set_task_archived(db, generating, source, id, true).await?;
        }
        TaskCommand::Unarchive { id } => {
            operations::set_task_archived(db, generating, source, id, false).await?;
        }
        TaskCommand::Delete { id } => {
            operations::delete_task(db, generating, source, id).await?;
        }
    }
    Ok(())
}

pub async fn run_chat(session: &Session, command: ChatCommand) -> Result<(), CliError> {
    match command {
        ChatCommand::Show { chat } => {
            let chat = chat.chat();
            let messages = db::with_conn(session.db.clone(), move |conn| {
                orchestration::list_chat(conn, chat)
            })
            .await?;
            print_json(&messages);
        }
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("{0}; pass --data-dir")]
    NoDataDir(#[from] paths::NoAppDir),
    #[error("data directory {} does not exist", .0.display())]
    MissingDataDir(PathBuf),
    #[error("data directory {} is not a directory", .0.display())]
    DataDirNotADirectory(PathBuf),
    #[error(
        "data directory {} is not writable; even commands that only read need to write there",
        .0.display()
    )]
    ReadOnlyDataDir(PathBuf),
    #[error(transparent)]
    Core(#[from] CoreError),
}
