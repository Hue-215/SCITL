use std::io;

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
}
