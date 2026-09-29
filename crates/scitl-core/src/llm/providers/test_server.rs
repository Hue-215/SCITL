//! アダプタのテスト用の、決めた応答を順に返すHTTPサーバー。

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::JoinHandle;

use serde_json::Value;

/// 受けたリクエスト1件。ヘッダー部(小文字)と本文。
pub(super) struct Received {
    pub headers: String,
    pub body: Value,
}

/// `responses`の数だけ接続を受け、順に`(状態コード, 本文)`を返す。受けたリクエストを返す。
/// 返り値の1つ目は`http://127.0.0.1:{port}`。
pub(super) fn spawn_server(
    responses: Vec<(u16, &'static str)>,
) -> (String, JoinHandle<Vec<Received>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let mut received = Vec::new();
        for (status, body) in responses {
            let (mut stream, _) = listener.accept().unwrap();
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
            let request_body = serde_json::from_slice(&raw[header_end..header_end + length])
                .unwrap_or(Value::Null);
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            received.push(Received {
                headers,
                body: request_body,
            });
        }
        received
    });
    (format!("http://{addr}"), handle)
}
