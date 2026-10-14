//! 端末間の同期(`docs/sync/index.md`)の両方の役。1台の上の2つのデータの間でも往復を確かめ
//! られる。利用者自身が打つ端末なので、始める前の確認のダイアログは出さない(11節)。

use std::io::BufRead;
use std::net::IpAddr;
use std::sync::Arc;

use clap::Subcommand;
use serde::Serialize;

use scitl_cli::terminal::{self, print_json, print_json_line};
use scitl_cli::Session;
use scitl_core::attachments::AttachmentStore;
use scitl_core::in_flight::InFlightSet;
use scitl_core::{db, paths};
use scitl_sync::{
    Context, DeviceRegistry, Limits, Listener, LocalVersions, PairingCode, SecretStore, Side,
    SyncError,
};

use crate::DebugError;

#[derive(Subcommand)]
pub enum SyncCommand {
    /// Wait for the other device to connect, then copy all data between the two (one of them
    /// must be empty). Prints the addresses and the code to give the other device first.
    Listen {
        /// IP address to listen on (a private or loopback address). Repeatable. Defaults to every
        /// private address of this machine.
        #[arg(long = "bind", value_name = "IP")]
        bind: Vec<IpAddr>,
    },
    /// Connect to a device that is listening, then copy all data between the two (one of them
    /// must be empty). Reads the code from standard input.
    Connect {
        /// Address the other device printed (`IP:PORT`). Repeatable; tried in order.
        #[arg(required = true, value_name = "ADDRESS")]
        addresses: Vec<String>,
    },
    /// Give this data directory a new device ID. Use it after copying a data directory and using
    /// both copies on this machine, which syncing cannot tell apart.
    RenewDeviceId,
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
    // 端末で打つので止める指示は来ない。移送に渡すためだけに押さえる。
    let running = InFlightSet::<()>::new();
    let held = running
        .try_begin_all()
        .expect("nothing else runs in this process");
    match command {
        SyncCommand::Listen { bind } => {
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
            let outcome = scitl_sync::copy(
                &mut sync,
                &ctx,
                Side::Listener,
                &device_id,
                held.stop_signal(),
                || {},
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
            let outcome = scitl_sync::copy(
                &mut sync,
                &ctx,
                Side::Connector,
                &device_id,
                held.stop_signal(),
                || {},
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
