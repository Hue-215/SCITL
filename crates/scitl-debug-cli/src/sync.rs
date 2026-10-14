//! 端末間の同期(`docs/sync/index.md`)の両方の役。1台の上の2つのデータの間でも往復を確かめ
//! られる。利用者自身が打つ端末なので、始める前の確認のダイアログは出さない(11節)。

use std::future::Future;
use std::io::BufRead;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;

use clap::{Subcommand, ValueEnum};
use serde::Serialize;

use scitl_cli::terminal::{self, print_json, print_json_line};
use scitl_cli::Session;
use scitl_core::attachments::AttachmentStore;
use scitl_core::in_flight::InFlightSet;
use scitl_core::{db, paths};
use scitl_sync::{
    ChosenSide, ConflictChoice, ConflictView, Context, DeviceRegistry, Limits, Listener,
    LocalVersions, PairingCode, Resolver, SecretStore, Side, SyncError,
};

use crate::DebugError;

#[derive(Subcommand)]
pub enum SyncCommand {
    /// Wait for the other device to connect, then sync the two. Prints the addresses and the code
    /// to give the other device first. Conflicts are settled on this side: each one is printed as
    /// a JSON line and answered on standard input with `ours` or `theirs`, unless --prefer is given.
    Listen {
        /// IP address to listen on (a private or loopback address). Repeatable. Defaults to every
        /// private address of this machine.
        #[arg(long = "bind", value_name = "IP")]
        bind: Vec<IpAddr>,
        /// Settle every conflict with this side without asking.
        #[arg(long, value_enum)]
        prefer: Option<Prefer>,
    },
    /// Connect to a device that is listening, then sync the two. Reads the code from standard
    /// input.
    Connect {
        /// Address the other device printed (`IP:PORT`). Repeatable; tried in order.
        #[arg(required = true, value_name = "ADDRESS")]
        addresses: Vec<String>,
    },
    /// Give this data directory a new device ID. Use it after copying a data directory and using
    /// both copies on this machine, which syncing cannot tell apart.
    RenewDeviceId,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Prefer {
    /// This device.
    Ours,
    /// The other device.
    Theirs,
}

impl From<Prefer> for ChosenSide {
    fn from(prefer: Prefer) -> Self {
        match prefer {
            Prefer::Ours => Self::Ours,
            Prefer::Theirs => Self::Theirs,
        }
    }
}

/// 競合を、`prefer`があればその側に、無ければ1つずつ標準入力で聞いて決める。
struct Ask {
    prefer: Option<Prefer>,
}

impl Resolver for Ask {
    fn choose(
        &self,
        conflicts: Vec<ConflictView>,
    ) -> Pin<Box<dyn Future<Output = Option<Vec<ConflictChoice>>> + Send + '_>> {
        let prefer = self.prefer;
        Box::pin(async move {
            let mut choices = Vec::with_capacity(conflicts.len());
            for conflict in conflicts {
                print_json_line(&conflict);
                let side = match prefer {
                    Some(prefer) => prefer.into(),
                    None => {
                        let id = conflict.id.clone();
                        tokio::task::spawn_blocking(move || ask_side(&id))
                            .await
                            .ok()??
                    }
                };
                choices.push(ConflictChoice {
                    id: conflict.id,
                    side,
                });
            }
            Some(choices)
        })
    }
}

/// `ours`か`theirs`を読む。入力が終われば取りやめ(`None`)。
fn ask_side(id: &str) -> Option<ChosenSide> {
    loop {
        terminal::print_prompt(&format!("{id} (ours/theirs): "));
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line).ok()? == 0 {
            return None;
        }
        match line.trim() {
            "ours" => return Some(ChosenSide::Ours),
            "theirs" => return Some(ChosenSide::Theirs),
            _ => {}
        }
    }
}

#[derive(Serialize)]
struct Listening {
    addresses: Vec<String>,
    code: String,
}

#[derive(Serialize)]
struct DeviceId {
    device_id: String,
}

pub async fn run(session: &Session, command: SyncCommand) -> Result<(), DebugError> {
    let registry: Arc<dyn DeviceRegistry> = Arc::new(SecretStore);
    let ctx = Context {
        db: session.db.clone(),
        attachments: attachment_store(session)?,
        registry: registry.clone(),
    };
    let local = LocalVersions {
        db_version: db::schema_version(),
    };
    // 端末で打つので止める指示は来ない。同期に渡すためだけに押さえる。
    let running = InFlightSet::<()>::new();
    let held = running
        .try_begin_all()
        .expect("nothing else runs in this process");
    match command {
        SyncCommand::Listen { bind, prefer } => {
            let device_id = scitl_sync::confirm_device_id(ctx.db.clone(), registry).await?;
            let ips = if bind.is_empty() {
                scitl_sync::local_addresses()?
            } else {
                bind
            };
            let listener = Listener::bind(&ips, Limits::default()).await?;
            print_json_line(&Listening {
                addresses: listener
                    .addresses()
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                code: listener.code().as_str().to_string(),
            });
            let mut sync = listener.accept(local).await?;
            let outcome = scitl_sync::sync(
                &mut sync,
                &ctx,
                Side::Listener,
                &device_id,
                held.stop_signal(),
                |_| {},
                &Ask { prefer },
            )
            .await;
            sync.close().await;
            print_json(&outcome?);
        }
        SyncCommand::Connect { addresses } => {
            let addresses = addresses
                .iter()
                .map(|a| scitl_sync::parse_peer_address(a))
                .collect::<Result<Vec<_>, _>>()?;
            let code = PairingCode::parse(&read_code()?)?;
            let device_id = scitl_sync::confirm_device_id(ctx.db.clone(), registry).await?;
            let mut sync = scitl_sync::connect(&addresses, &code, local, Limits::default()).await?;
            // 競合は待ち受ける側で決まるので、こちらでは聞かない。
            let outcome = scitl_sync::sync(
                &mut sync,
                &ctx,
                Side::Connector,
                &device_id,
                held.stop_signal(),
                |_| {},
                &Ask {
                    prefer: Some(Prefer::Ours),
                },
            )
            .await;
            sync.close().await;
            print_json(&outcome?);
        }
        SyncCommand::RenewDeviceId => {
            let device_id = scitl_sync::renew_device_id(ctx.db.clone(), registry).await?;
            print_json(&DeviceId { device_id });
        }
    }
    Ok(())
}

/// 数字は引数で受け取らない(プロセスの一覧に残り、待ち受けの間に同じマシンのほかの利用者が
/// 先につなげる)。
fn read_code() -> Result<String, DebugError> {
    terminal::print_prompt("code shown on the other device: ");
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| DebugError::Sync(SyncError::Io(e)))?;
    Ok(line)
}

/// 添付の実体の置き場所。GUIと同じ場所を使う。
fn attachment_store(session: &Session) -> Result<AttachmentStore, DebugError> {
    let cache = paths::default_cache_dir().map_err(DebugError::NoCacheDir)?;
    Ok(AttachmentStore::new(
        session.data.attachments(),
        paths::revealed_attachments(&cache),
    ))
}
