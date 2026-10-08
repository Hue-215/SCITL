//! アダプタのテスト用の、決めた応答を順に返すHTTPサーバー。

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;

/// 受けたリクエスト1件。ヘッダー部(小文字)と本文。
pub(super) struct Received {
    pub headers: String,
    pub body: Value,
}

/// 1件のリクエストを読む(ヘッダー部と、`Content-Length`の分の本文)。
fn read_request(stream: &mut impl Read) -> Received {
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    let header_end = loop {
        let n = stream.read(&mut buf).unwrap();
        raw.extend_from_slice(&buf[..n]);
        if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let headers = String::from_utf8_lossy(&raw[..header_end]).to_ascii_lowercase();
    let length = headers
        .lines()
        .find_map(|l| l.strip_prefix("content-length: "))
        .map_or(0, |v| v.trim().parse::<usize>().unwrap());
    while raw.len() < header_end + length {
        let n = stream.read(&mut buf).unwrap();
        raw.extend_from_slice(&buf[..n]);
    }
    let body = serde_json::from_slice(&raw[header_end..header_end + length]).unwrap_or(Value::Null);
    Received { headers, body }
}

/// `responses`の数だけ接続を受け、順に`(状態コード, 本文)`を返す。受けたリクエストを返す。
/// 返り値の1つ目は`http://127.0.0.1:{port}`。
pub(super) fn spawn_server(
    responses: Vec<(u16, &'static str)>,
) -> (String, JoinHandle<Vec<Received>>) {
    spawn_server_with_headers(responses.into_iter().map(|(s, b)| (s, "", b)).collect())
}

/// [`spawn_server`]の、応答にヘッダーを足す形。2つ目は`Name: value\r\n`を並べたもの。
pub(super) fn spawn_server_with_headers(
    responses: Vec<(u16, &'static str, &'static str)>,
) -> (String, JoinHandle<Vec<Received>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let mut received = Vec::new();
        for (status, extra_headers, body) in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            received.push(request);
        }
        received
    });
    (format!("http://{addr}"), handle)
}

/// 1回だけ接続を受け、SSE(`text/event-stream`)の応答を`pieces`の順に、それぞれ前に`delay`だけ
/// 待ってから書いて閉じる。受けたリクエストを返す。返り値の1つ目は`http://127.0.0.1:{port}`。
pub(super) fn spawn_event_stream(
    pieces: Vec<(Duration, &'static [u8])>,
) -> (String, JoinHandle<Received>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        let _ = stream.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nConnection: close\r\n\r\n",
        );
        for (delay, piece) in pieces {
            std::thread::sleep(delay);
            if stream.write_all(piece).is_err() {
                break;
            }
        }
        request
    });
    (format!("http://{addr}"), handle)
}

/// 間を空けずに書く断片([`spawn_event_stream`])。
pub(super) fn now(pieces: &[&'static str]) -> Vec<(Duration, &'static [u8])> {
    pieces
        .iter()
        .map(|p| (Duration::ZERO, p.as_bytes()))
        .collect()
}
