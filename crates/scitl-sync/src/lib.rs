//! 端末間の同期(`docs/sync/index.md`)。接続・やり取り・結合・取り込みを持ち、DBへは`scitl-core`を
//! 通して触る。待ち受けとそのための依存は`scitl-core`に入れず、ここに閉じる(「構成」)。

mod address;
mod code;
mod conflict;
mod convert;
mod device;
mod error;
mod files;
mod frame;
mod invitation;
mod merge;
mod pake;
mod run;
mod session;
mod tls;
mod transfer;
mod transport;
mod validate;
mod write_plan;

pub use address::{local_addresses, parse_peer_address};
pub use code::PairingCode;
pub use conflict::{
    ChosenSide, ConflictChoice, ConflictKind, ConflictMessage, ConflictSide, ConflictStep,
    ConflictView,
};
pub use device::{confirm_device_id, renew_device_id, DeviceRegistry, SecretStore};
pub use error::{InvalidRow, SyncError, MESSAGE_KEY_PREFIX};
pub use invitation::{parse_invitation, Invitation, QrModules};
pub use run::{
    answer, approve_write, cancel, connect_scanned, connect_to, listen, ConflictAnswers,
    DialogAnswer, Request, SyncDialog, SyncEvent, SyncProgress, SyncReport, WriteAnswers,
};
pub use session::{connect, Limits, Listener, LocalVersions, Session};
pub use transfer::{sync, Confirmer, Context, Outcome, Resolver, Side, Stage, WriteAnswer};
pub use transport::{AbortReason, Counts};
pub use write_plan::{WriteCounts, WritePlan};

/// プロトコルの版。運ぶ形・IDの宣言・取り込む行の検証・照合の値のどれかを変えたら上げる
/// (`docs/sync/index.md`「接続」)。
pub const PROTOCOL_VERSION: u32 = 3;
