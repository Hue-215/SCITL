use std::fmt;
use std::io;

use serde::Serialize;

use crate::transport::AbortReason;

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// 宛先・待ち受けのアドレスとして受け付けない(プライベートIP・ループバックのリテラル以外)。
    #[error("address not allowed: {0}")]
    AddressNotAllowed(String),
    #[error("no private address to listen on")]
    NoLocalAddress,
    #[error("failed to listen: {0}")]
    Bind(io::Error),
    /// どの候補にもつながらなかった。最後の候補の失敗を持つ。
    #[error("failed to connect: {0}")]
    Connect(io::Error),
    #[error("connection failed: {0}")]
    Io(#[from] io::Error),
    #[error("TLS failed: {0}")]
    Tls(#[from] rustls::Error),
    #[error("timed out")]
    TimedOut,
    /// 待ち受けの時間が来て、相手が来ないまま閉じた。
    #[error("no peer connected in time")]
    WindowExpired,
    /// 数字で鍵が合わなかった(数字の打ち間違いか、間に入った相手)。
    #[error("pairing code did not match")]
    CodeMismatch,
    #[error("peer closed the connection")]
    PeerClosed,
    #[error("message too large: {len} bytes (max {max})")]
    FrameTooLarge { len: usize, max: usize },
    #[error("malformed message: {0}")]
    Malformed(String),
    #[error("protocol version mismatch: ours {ours}, theirs {theirs}")]
    ProtocolMismatch { ours: u32, theirs: u32 },
    #[error("database version mismatch: ours {ours}, theirs {theirs}")]
    DbVersionMismatch { ours: u32, theirs: u32 },
    /// 資格情報ストアで端末IDを確かめられない(読めない)。確かめずには始めない。
    #[error("could not check this device's ID in the secret store: {0}")]
    DeviceCheck(scitl_core::CoreError),
    /// 同じ端末IDの2台(写したDBどうし)。
    #[error("the peer has the same device ID (one database is a copy of the other)")]
    SameDevice,
    #[error("both devices have data; copying needs one of them to be empty")]
    BothHaveData,
    #[error("neither device has data to copy")]
    NothingToCopy,
    /// 運ぶ行が、本体が書く行の形に合わない。
    #[error("{}", InvalidRows(.0))]
    InvalidRows(Vec<InvalidRow>),
    /// 受け取る側に、移送の途中でデータができた。
    #[error("this device is no longer empty")]
    NoLongerEmpty,
    #[error("the peer stopped: {0}")]
    PeerRefused(AbortReason),
    #[error(transparent)]
    Core(#[from] scitl_core::CoreError),
    /// 数字の形が違う(6桁でない)。
    #[error("the pairing code must be 6 digits")]
    InvalidCode,
    /// 応答を生成中の会話があるか、別の同期の最中。
    #[error("a response is being generated or another sync is running")]
    Busy,
    /// 止める指示で止めた。
    #[error("stopped")]
    Cancelled,
}

impl SyncError {
    /// 画面が文言を引く種類(`settings.sync.error.{種類}`)。
    pub fn kind(&self) -> &'static str {
        match self {
            Self::AddressNotAllowed(_) => "invalid_address",
            Self::InvalidCode => "invalid_code",
            Self::NoLocalAddress => "no_local_address",
            Self::Bind(_) => "listen_failed",
            Self::Connect(_) => "connect_failed",
            Self::Io(_)
            | Self::Tls(_)
            | Self::PeerClosed
            | Self::Malformed(_)
            | Self::FrameTooLarge { .. } => "connection_failed",
            Self::TimedOut => "timed_out",
            Self::WindowExpired => "no_peer",
            Self::CodeMismatch => "code_mismatch",
            Self::ProtocolMismatch { .. } | Self::DbVersionMismatch { .. } => "version_mismatch",
            Self::DeviceCheck(_) => "device_check",
            Self::SameDevice => "same_device",
            Self::BothHaveData => "both_have_data",
            Self::NothingToCopy => "nothing_to_copy",
            Self::InvalidRows(_) => "invalid_rows",
            Self::NoLongerEmpty => "no_longer_empty",
            Self::PeerRefused(AbortReason::InvalidRows) => "peer_invalid_rows",
            Self::PeerRefused(AbortReason::NotEmpty) => "peer_not_empty",
            Self::PeerRefused(AbortReason::Failed) => "peer_failed",
            Self::Busy => "busy",
            Self::Cancelled => "cancelled",
            Self::Core(_) => "internal",
        }
    }
}

/// 同期できない行。`id`は送る側の端末の中の連番(受け取る側では分からない)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InvalidRow {
    pub table: &'static str,
    pub id: Option<i64>,
    pub uid: String,
    pub reason: String,
}

/// 示す行の数の上限。残りは数だけ出す。
const SHOWN_ROWS: usize = 20;

struct InvalidRows<'a>(&'a [InvalidRow]);

impl fmt::Display for InvalidRows<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} rows cannot be synced:", self.0.len())?;
        for row in self.0.iter().take(SHOWN_ROWS) {
            match row.id {
                Some(id) => write!(f, " {}#{id}", row.table)?,
                None => write!(f, " {} {}", row.table, row.uid)?,
            }
            write!(f, " ({});", row.reason)?;
        }
        if self.0.len() > SHOWN_ROWS {
            write!(f, " and {} more", self.0.len() - SHOWN_ROWS)?;
        }
        Ok(())
    }
}
