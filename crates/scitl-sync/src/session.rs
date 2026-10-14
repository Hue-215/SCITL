//! 待ち受けと接続(`docs/sync/index.md` 3節)。TLSを張り、数字で互いを確かめ、版を伝え合うまで。
//! 確かめたあとのやり取りは[`Session`]の上で行う。

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use futures::future::select_all;
use rustls::pki_types::ServerName;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tokio::time::{timeout, timeout_at, Instant};
use tokio_rustls::{TlsAcceptor, TlsConnector, TlsStream};

use crate::address::is_allowed_ip;
use crate::pake::{self, Role, CONFIRM_LEN, MESSAGE_LEN};
use crate::{frame, tls, PairingCode, SyncError, PROTOCOL_VERSION};

/// 相手を確かめ終えるまでに読む1メッセージの上限。PAKEのメッセージと確認しか来ない。
const HANDSHAKE_MAX_MESSAGE: usize = 64;

/// 待ち受ける側が、相手を確かめる前の接続を同時に抱える上限。超えた分はすぐ閉じる。
const MAX_PENDING: usize = 8;

/// 読む量と待つ時間の上限(両側)。
#[derive(Debug, Clone)]
pub struct Limits {
    /// 待ち受けを始めてから、相手が来ないまま閉じるまで。
    pub listen_window: Duration,
    /// 相手を確かめ終えるまでの段ごとの上限。つなぐ側は、TCPの接続から確かめ終えるまでを
    /// これに収める。待ち受ける側は、受け付けてから最初の値を受け取るまでと、そこから
    /// 確かめ終えるまでに、それぞれ掛ける。
    pub handshake: Duration,
    /// つなぐ側が、1つの候補へのTCPの接続を待つ時間。
    pub connect: Duration,
    /// 相手を確かめたあと、1メッセージを読み終える(書き終える)までの上限。待つ間に相手が
    /// 何も送らない時間を含むので、段階3で、利用者が競合を選ぶ時間を見込む。
    pub idle: Duration,
    /// 相手を確かめたあとの1メッセージの上限。
    pub max_message: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            listen_window: Duration::from_secs(5 * 60),
            handshake: Duration::from_secs(10),
            connect: Duration::from_secs(5),
            idle: Duration::from_secs(10 * 60),
            max_message: 16 * 1024 * 1024,
        }
    }
}

/// 自分の側の版。プロトコルの版は[`PROTOCOL_VERSION`]。
#[derive(Debug, Clone, Copy)]
pub struct LocalVersions {
    /// DBの版(`user_version`)。
    pub db_version: u32,
}

/// 相手を確かめた直後に伝え合う版。版が違えば、後から足す欄は読めないことがあるので、
/// 知らない欄は捨てて版だけを読む。
#[derive(Serialize, Deserialize)]
struct Hello {
    protocol: u32,
    db_version: u32,
}

/// 待ち受け。使い捨ての鍵と数字は、作ったこの値と一緒に捨てる。
pub struct Listener {
    listeners: Vec<TcpListener>,
    addresses: Vec<SocketAddr>,
    code: PairingCode,
    acceptor: TlsAcceptor,
    limits: Limits,
}

impl Listener {
    /// 渡したアドレスにだけバインドする(全インターフェースでは待たない)。ポートは揃えられれば
    /// 揃える(利用者が打つのは1つだけでも、出す候補が読みやすい)。
    pub async fn bind(ips: &[IpAddr], limits: Limits) -> Result<Self, SyncError> {
        if ips.is_empty() {
            return Err(SyncError::NoLocalAddress);
        }
        if let Some(ip) = ips.iter().find(|ip| !is_allowed_ip(**ip)) {
            return Err(SyncError::AddressNotAllowed(ip.to_string()));
        }
        let mut listeners = Vec::with_capacity(ips.len());
        let mut port = 0;
        for &ip in ips {
            let listener = match TcpListener::bind((ip, port)).await {
                Ok(l) => l,
                Err(_) if port != 0 => TcpListener::bind((ip, 0)).await.map_err(SyncError::Bind)?,
                Err(e) => return Err(SyncError::Bind(e)),
            };
            if port == 0 {
                port = listener.local_addr().map_err(SyncError::Bind)?.port();
            }
            listeners.push(listener);
        }
        let addresses = listeners
            .iter()
            .map(|l| l.local_addr())
            .collect::<Result<_, _>>()
            .map_err(SyncError::Bind)?;
        Ok(Self {
            listeners,
            addresses,
            code: PairingCode::generate(),
            acceptor: TlsAcceptor::from(tls::server_config()?),
            limits,
        })
    }

    pub fn addresses(&self) -> &[SocketAddr] {
        &self.addresses
    }

    pub fn code(&self) -> &PairingCode {
        &self.code
    }

    /// 相手を1台だけ受け入れる。長さの正しいPAKEの値を送ってくる前の接続(TLSに失敗した、
    /// 何も送らない等)は並べて扱い、最初に値を送ってきた1本を相手にする。その1本を選んだ時点で
    /// この待ち受けの1回を使ったものとし、待ち受けを閉じる(2台目は接続を断られる)。そのあとは
    /// 成功以外すべて待ち受けを終える。
    pub async fn accept(self, local: LocalVersions) -> Result<Session, SyncError> {
        let window = Instant::now() + self.limits.listen_window;
        let mut listeners = self.listeners;
        let mut pending = JoinSet::new();
        let mut failed = None;
        let (stream, peer_msg, peer) = 'accepting: loop {
            if let Some(index) = failed.take() {
                // 記述子の不足などで受けられなかった。空回りしないよう少し待ち、失敗したものを
                // 後ろへ回して、ほかの受け付けを先に見る。
                listeners[index..].rotate_left(1);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let accepts = listeners.iter().map(|l| Box::pin(l.accept()));
            tokio::select! {
                (accepted, index, _) = select_all(accepts) => {
                    let Ok((tcp, peer)) = accepted else {
                        failed = Some(index);
                        continue;
                    };
                    // 終わったが回収していない分を、上限に数えない。
                    while let Some(joined) = pending.try_join_next() {
                        if let Some(chosen) = first_message(joined) {
                            break 'accepting chosen;
                        }
                    }
                    if !is_allowed_ip(peer.ip()) || pending.len() >= MAX_PENDING {
                        continue;
                    }
                    let acceptor = self.acceptor.clone();
                    let limit = self.limits.handshake;
                    pending.spawn(async move {
                        let received = timeout(limit, receive_first_message(acceptor, tcp, limit)).await;
                        (received, peer)
                    });
                }
                Some(joined) = pending.join_next(), if !pending.is_empty() => {
                    if let Some(chosen) = first_message(joined) {
                        break chosen;
                    }
                }
                () = tokio::time::sleep_until(window) => return Err(SyncError::WindowExpired),
            }
        };
        drop(pending);
        drop(listeners);
        let mut stream = TlsStream::from(stream);
        let deadline = Instant::now() + self.limits.handshake;
        timeout_at(
            deadline,
            confirm_as_listener(&mut stream, &peer_msg, &self.code, &self.limits),
        )
        .await
        .map_err(|_| SyncError::TimedOut)??;
        let mut session = Session {
            stream,
            limits: self.limits,
            peer,
        };
        session.exchange_versions(local, Role::Listener).await?;
        Ok(session)
    }
}

type ServerStream = tokio_rustls::server::TlsStream<TcpStream>;
type Received = Result<Result<(ServerStream, Vec<u8>), SyncError>, tokio::time::error::Elapsed>;

fn first_message(
    joined: Result<(Received, SocketAddr), tokio::task::JoinError>,
) -> Option<(ServerStream, Vec<u8>, SocketAddr)> {
    match joined {
        Ok((Ok(Ok((stream, msg))), peer)) => Some((stream, msg, peer)),
        _ => None,
    }
}

async fn receive_first_message(
    acceptor: TlsAcceptor,
    tcp: TcpStream,
    limit: Duration,
) -> Result<(ServerStream, Vec<u8>), SyncError> {
    let _ = tcp.set_nodelay(true);
    let mut stream = acceptor.accept(tcp).await?;
    if stream.get_ref().1.alpn_protocol() != Some(tls::ALPN) {
        return Err(SyncError::Malformed("ALPN".to_string()));
    }
    let msg = frame::read(&mut stream, HANDSHAKE_MAX_MESSAGE, limit).await?;
    if msg.len() != MESSAGE_LEN {
        return Err(SyncError::Malformed("PAKE message length".to_string()));
    }
    Ok((stream, msg))
}

async fn confirm_as_listener(
    stream: &mut TlsStream<TcpStream>,
    peer_msg: &[u8],
    code: &PairingCode,
    limits: &Limits,
) -> Result<(), SyncError> {
    let exporter = exporter(stream)?;
    let (started, msg) = pake::start(Role::Listener, code);
    frame::write(stream, &msg, limits.handshake).await?;
    let confirmation = pake::finish(started, peer_msg, &exporter)?;
    let tag = frame::read(stream, HANDSHAKE_MAX_MESSAGE, limits.handshake).await?;
    confirmation.check_theirs(&tag)?;
    frame::write(stream, &confirmation.ours(), limits.handshake).await
}

/// 候補を順に試してつなぐ。次の候補へ進むのは、PAKEの自分の値を送る前に失敗したときだけ
/// (候補の数だけ当て推量をさせない)。
pub async fn connect(
    candidates: &[SocketAddr],
    code: &PairingCode,
    local: LocalVersions,
    limits: Limits,
) -> Result<Session, SyncError> {
    if let Some(addr) = candidates.iter().find(|a| !is_allowed_ip(a.ip())) {
        return Err(SyncError::AddressNotAllowed(addr.to_string()));
    }
    let connector = TlsConnector::from(tls::client_config()?);
    let mut last_error = std::io::Error::other("no address to connect to");
    for &addr in candidates {
        let deadline = Instant::now() + limits.handshake;
        let stream = match open_tls(&connector, addr, &limits, deadline).await {
            Ok(stream) => stream,
            Err(e) => {
                last_error = e;
                continue;
            }
        };
        let mut stream = TlsStream::from(stream);
        timeout_at(deadline, confirm_as_connector(&mut stream, code, &limits))
            .await
            .map_err(|_| SyncError::TimedOut)??;
        let mut session = Session {
            stream,
            limits,
            peer: addr,
        };
        session.exchange_versions(local, Role::Connector).await?;
        return Ok(session);
    }
    Err(SyncError::Connect(last_error))
}

async fn open_tls(
    connector: &TlsConnector,
    addr: SocketAddr,
    limits: &Limits,
    deadline: Instant,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, std::io::Error> {
    let tcp = timeout(limits.connect, TcpStream::connect(addr))
        .await
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))??;
    let _ = tcp.set_nodelay(true);
    let stream = timeout_at(
        deadline,
        connector.connect(ServerName::IpAddress(addr.ip().into()), tcp),
    )
    .await
    .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))??;
    if stream.get_ref().1.alpn_protocol() != Some(tls::ALPN) {
        return Err(std::io::Error::other("peer did not agree on ALPN"));
    }
    Ok(stream)
}

async fn confirm_as_connector(
    stream: &mut TlsStream<TcpStream>,
    code: &PairingCode,
    limits: &Limits,
) -> Result<(), SyncError> {
    let exporter = exporter(stream)?;
    let (started, msg) = pake::start(Role::Connector, code);
    frame::write(stream, &msg, limits.handshake).await?;
    let peer_msg = frame::read(stream, HANDSHAKE_MAX_MESSAGE, limits.handshake).await?;
    if peer_msg.len() != MESSAGE_LEN {
        return Err(SyncError::Malformed("PAKE message length".to_string()));
    }
    let confirmation = pake::finish(started, &peer_msg, &exporter)?;
    frame::write(stream, &confirmation.ours(), limits.handshake).await?;
    let tag = match frame::read(stream, HANDSHAKE_MAX_MESSAGE, limits.handshake).await {
        Ok(tag) => tag,
        // 待ち受ける側は、確認が合わなければ何も返さずに切る。
        Err(SyncError::PeerClosed) => return Err(SyncError::CodeMismatch),
        Err(e) => return Err(e),
    };
    if tag.len() != CONFIRM_LEN {
        return Err(SyncError::Malformed("confirmation length".to_string()));
    }
    confirmation.check_theirs(&tag)
}

fn exporter(stream: &TlsStream<TcpStream>) -> Result<Vec<u8>, SyncError> {
    let value = match stream {
        TlsStream::Client(s) => {
            s.get_ref()
                .1
                .export_keying_material(vec![0u8; 32], tls::EXPORTER_LABEL, None)
        }
        TlsStream::Server(s) => {
            s.get_ref()
                .1
                .export_keying_material(vec![0u8; 32], tls::EXPORTER_LABEL, None)
        }
    };
    Ok(value?)
}

/// 相手を確かめたあとの接続。中身はJSONで、1つずつ読む量と待つ時間に上限がある。
pub struct Session {
    stream: TlsStream<TcpStream>,
    limits: Limits,
    peer: SocketAddr,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("peer", &self.peer)
            .finish_non_exhaustive()
    }
}

impl Session {
    pub fn peer_address(&self) -> SocketAddr {
        self.peer
    }

    pub async fn send<T: Serialize>(&mut self, msg: &T) -> Result<(), SyncError> {
        let body = serde_json::to_vec(msg).map_err(|e| SyncError::Malformed(e.to_string()))?;
        if body.len() > self.limits.max_message {
            return Err(SyncError::FrameTooLarge {
                len: body.len(),
                max: self.limits.max_message,
            });
        }
        frame::write(&mut self.stream, &body, self.limits.idle).await
    }

    pub async fn recv<T: DeserializeOwned>(&mut self) -> Result<T, SyncError> {
        Ok(self.recv_with_len().await?.0)
    }

    /// 受け取ったメッセージと、その中身の大きさ。受け取る量の合計に上限を置くのに使う。
    pub async fn recv_with_len<T: DeserializeOwned>(&mut self) -> Result<(T, usize), SyncError> {
        let body = frame::read(&mut self.stream, self.limits.max_message, self.limits.idle).await?;
        let message =
            serde_json::from_slice(&body).map_err(|e| SyncError::Malformed(e.to_string()))?;
        Ok((message, body.len()))
    }

    /// TLSの終わりを送って閉じる。相手が先に閉じていても失敗にしない。
    pub async fn close(mut self) {
        let _ = timeout(Duration::from_secs(5), self.stream.shutdown()).await;
    }

    /// プロトコルとDBの版を伝え合い、違えばほかに何も送らずに断る。つなぐ側が先に送る。
    async fn exchange_versions(
        &mut self,
        local: LocalVersions,
        role: Role,
    ) -> Result<(), SyncError> {
        let ours = Hello {
            protocol: PROTOCOL_VERSION,
            db_version: local.db_version,
        };
        let theirs: Hello = match role {
            Role::Connector => {
                self.send(&ours).await?;
                self.recv().await?
            }
            Role::Listener => {
                let theirs = self.recv().await?;
                self.send(&ours).await?;
                theirs
            }
        };
        if theirs.protocol != ours.protocol {
            return Err(SyncError::ProtocolMismatch {
                ours: ours.protocol,
                theirs: theirs.protocol,
            });
        }
        if theirs.db_version != ours.db_version {
            return Err(SyncError::DbVersionMismatch {
                ours: ours.db_version,
                theirs: theirs.db_version,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
