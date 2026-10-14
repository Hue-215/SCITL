use std::net::Ipv4Addr;

use tokio::io::AsyncWriteExt;

use super::*;

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const LOCAL: LocalVersions = LocalVersions { db_version: 11 };

fn limits() -> Limits {
    Limits {
        listen_window: Duration::from_secs(10),
        handshake: Duration::from_secs(2),
        connect: Duration::from_secs(2),
        idle: Duration::from_secs(5),
        max_message: 1024,
    }
}

async fn listen() -> (
    SocketAddr,
    PairingCode,
    tokio::task::JoinHandle<Result<Session, SyncError>>,
) {
    let listener = Listener::bind(&[LOOPBACK], limits()).await.unwrap();
    let addr = listener.addresses()[0];
    let code = listener.code().clone();
    (addr, code, tokio::spawn(listener.accept(LOCAL)))
}

fn other_code(code: &PairingCode) -> PairingCode {
    let flipped = if code.as_str() == "000000" {
        "000001"
    } else {
        "000000"
    };
    PairingCode::parse(flipped).unwrap()
}

#[tokio::test]
async fn the_right_code_connects_and_messages_flow_both_ways() {
    let (addr, code, listening) = listen().await;
    let mut b = connect(&[addr], &code, LOCAL, limits()).await.unwrap();
    let mut a = listening.await.unwrap().unwrap();

    b.send(&"from b").await.unwrap();
    assert_eq!(a.recv::<String>().await.unwrap(), "from b");
    a.send(&"from a").await.unwrap();
    assert_eq!(b.recv::<String>().await.unwrap(), "from a");
    assert!(matches!(
        a.send(&"x".repeat(2000)).await,
        Err(SyncError::FrameTooLarge { max: 1024, .. })
    ));
}

#[tokio::test]
async fn a_wrong_code_fails_both_sides_and_ends_listening() {
    let (addr, code, listening) = listen().await;
    let result = connect(&[addr], &other_code(&code), LOCAL, limits()).await;
    assert!(matches!(result, Err(SyncError::CodeMismatch)), "{result:?}");
    assert!(matches!(
        listening.await.unwrap(),
        Err(SyncError::CodeMismatch)
    ));

    // 1回を使ったので、正しい数字でももうつながらない。
    assert!(matches!(
        connect(&[addr], &code, LOCAL, limits()).await,
        Err(SyncError::Connect(_))
    ));
}

#[tokio::test]
async fn connections_that_fail_before_the_pake_reply_do_not_use_up_the_attempt() {
    let (addr, code, listening) = listen().await;

    // TCPだけつないで閉じる。
    drop(TcpStream::connect(addr).await.unwrap());
    // TLSでないものを送る。
    let mut garbage = TcpStream::connect(addr).await.unwrap();
    garbage.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
    drop(garbage);
    // TLSを張ってから、上限を超える長さを送る。
    let connector = TlsConnector::from(tls::client_config().unwrap());
    let mut oversized = open_tls(
        &connector,
        addr,
        &limits(),
        Instant::now() + limits().handshake,
    )
    .await
    .unwrap();
    oversized.write_all(&1000u32.to_be_bytes()).await.unwrap();
    oversized.flush().await.unwrap();
    // TLSを張って何も送らない(相手を確かめる時間の上限で捨てられる)。
    let silent = open_tls(
        &connector,
        addr,
        &limits(),
        Instant::now() + limits().handshake,
    )
    .await
    .unwrap();

    let b = connect(&[addr], &code, LOCAL, limits()).await;
    assert!(b.is_ok(), "{:?}", b.err());
    assert!(listening.await.unwrap().is_ok());
    drop((oversized, silent));
}

#[tokio::test]
async fn a_second_peer_is_refused_once_one_is_confirmed() {
    let (addr, code, listening) = listen().await;
    let _b = connect(&[addr], &code, LOCAL, limits()).await.unwrap();
    let _a = listening.await.unwrap().unwrap();
    assert!(matches!(
        connect(&[addr], &code, LOCAL, limits()).await,
        Err(SyncError::Connect(_))
    ));
}

/// 間に入って両側と別々にTLSを張り、中身をそのまま中継する相手。数字が正しくても、TLS接続に
/// 固有の値が両側で食い違うので確かめが通らない。
#[tokio::test]
async fn a_relay_in_the_middle_is_detected_even_with_the_right_code() {
    let (addr, code, listening) = listen().await;

    let relay = TcpListener::bind((LOOPBACK, 0)).await.unwrap();
    let relay_addr = relay.local_addr().unwrap();
    tokio::spawn(async move {
        let (tcp, _) = relay.accept().await.unwrap();
        let mut from_b = TlsAcceptor::from(tls::server_config().unwrap())
            .accept(tcp)
            .await
            .unwrap();
        let connector = TlsConnector::from(tls::client_config().unwrap());
        let mut to_a = open_tls(
            &connector,
            addr,
            &limits(),
            Instant::now() + limits().handshake,
        )
        .await
        .unwrap();
        let _ = tokio::io::copy_bidirectional(&mut from_b, &mut to_a).await;
    });

    let result = connect(&[relay_addr], &code, LOCAL, limits()).await;
    assert!(matches!(result, Err(SyncError::CodeMismatch)), "{result:?}");
    assert!(matches!(
        listening.await.unwrap(),
        Err(SyncError::CodeMismatch)
    ));
}

#[tokio::test]
async fn a_different_db_version_is_refused_on_both_sides() {
    let (addr, code, listening) = listen().await;
    let newer = LocalVersions {
        db_version: LOCAL.db_version + 1,
    };
    assert!(matches!(
        connect(&[addr], &code, newer, limits()).await,
        Err(SyncError::DbVersionMismatch {
            ours: 12,
            theirs: 11
        })
    ));
    assert!(matches!(
        listening.await.unwrap(),
        Err(SyncError::DbVersionMismatch {
            ours: 11,
            theirs: 12
        })
    ));
}

#[tokio::test]
async fn the_next_candidate_is_tried_only_when_connecting_failed() {
    let closed = {
        let l = TcpListener::bind((LOOPBACK, 0)).await.unwrap();
        l.local_addr().unwrap()
    };
    let (addr, code, listening) = listen().await;
    assert!(connect(&[closed, addr], &code, LOCAL, limits())
        .await
        .is_ok());
    assert!(listening.await.unwrap().is_ok());
}

#[tokio::test]
async fn listening_ends_when_no_peer_comes() {
    let listener = Listener::bind(
        &[LOOPBACK],
        Limits {
            listen_window: Duration::from_millis(100),
            ..limits()
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        listener.accept(LOCAL).await,
        Err(SyncError::WindowExpired)
    ));
}

#[tokio::test]
async fn public_addresses_are_refused_on_both_sides() {
    let public = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
    assert!(matches!(
        Listener::bind(&[public], limits()).await,
        Err(SyncError::AddressNotAllowed(_))
    ));
    assert!(matches!(
        Listener::bind(&[], limits()).await,
        Err(SyncError::NoLocalAddress)
    ));
    assert!(matches!(
        connect(
            &[SocketAddr::new(public, 443)],
            &PairingCode::generate(),
            LOCAL,
            limits()
        )
        .await,
        Err(SyncError::AddressNotAllowed(_))
    ));
}
