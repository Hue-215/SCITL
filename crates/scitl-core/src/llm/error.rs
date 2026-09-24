//! LLM呼び出しの失敗の種類(Issue #180)。種類はアダプタが決めて返し、
//! `orchestration::turn_error`は種類から文言と`error_kind`を選ぶだけにする。
//!
//! 方言によらない失敗(通信の失敗と、状態コードだけで決まるもの)の変換はここに置き、
//! 全アダプタがこれを呼ぶ。応答本文でしか分からない失敗(コンテキスト超過等)の判定は
//! 各`providers/*.rs`が持つ(architecture.md 3節)。

use std::error::Error as _;
use std::fmt;

use reqwest::StatusCode;

/// 詳細に載せる文字列の上限。
const MAX_DETAIL_CHARS: usize = 512;
/// 送信した鍵が現れたときの置き換え先。
const REDACTED: &str = "[redacted]";
/// 要求URLが現れたときの置き換え先。
const URL_PLACEHOLDER: &str = "[url]";

/// LLM呼び出しの失敗。`CoreError::Llm`として上位へ返る。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LlmError {
    /// 送る前にリクエストを組み立てられなかった。起きうるのは、APIキーにヘッダーへ
    /// 載せられない文字が含まれる場合。
    #[error("failed to build the request: {0}")]
    InvalidRequest(ErrorDetail),
    /// 応答タイムアウト(`config::GeneralConfig::response_timeout`)までに応答を
    /// 読み切れなかった。
    #[error("timed out waiting for the response: {0}")]
    Timeout(ErrorDetail),
    /// 接続先に届かなかった(接続の拒否・名前解決・TLS・接続確立の上限)か、応答の途中で
    /// 接続が切れた。
    #[error("failed to communicate with the provider: {0}")]
    Connection(ErrorDetail),
    /// 応答は届いたが、期待した形として読めなかった。
    #[error("failed to parse the response: {0}")]
    InvalidResponse(ErrorDetail),
    /// 応答に返信の候補が1つも無かった。
    #[error("the response contained no reply")]
    EmptyResponse,
    #[error("the conversation exceeds the context length: {0}")]
    ContextExceeded(ErrorDetail),
    #[error("authentication failed: {0}")]
    Auth(ErrorDetail),
    #[error("rate limited: {0}")]
    RateLimit(ErrorDetail),
    /// 上記のいずれにも当たらない非成功の状態コード。
    #[error("the provider returned an error: {0}")]
    Http(ErrorDetail),
}

impl LlmError {
    /// reqwestが返した通信の失敗(応答を受け取る前と、本文を読み切る前)を種類付きにする。
    ///
    /// 判定の順序に意味がある。
    /// - 接続を先に見る。接続確立の上限(`net::hardened_client`)を超えた場合も
    ///   タイムアウトとして報告されるが、応答タイムアウトの設定ではなく、接続先に届かない
    ///   ことを表す
    /// - タイムアウトを解釈失敗より先に見る。本文を読む途中で応答タイムアウトに達すると、
    ///   reqwestは解釈失敗として返す
    pub fn from_transport(e: reqwest::Error, api_key: &str) -> Self {
        let (builder, connect, timeout, decode) = (
            e.is_builder(),
            e.is_connect(),
            e.is_timeout(),
            e.is_decode(),
        );
        let detail = ErrorDetail::transport(e, api_key);
        if builder {
            Self::InvalidRequest(detail)
        } else if connect {
            Self::Connection(detail)
        } else if timeout {
            Self::Timeout(detail)
        } else if decode {
            Self::InvalidResponse(detail)
        } else {
            Self::Connection(detail)
        }
    }

    /// 非成功の状態コードを、状態コードだけで決まる範囲で分類する。本文でしか分からない
    /// 種類は、アダプタがこれを呼ぶ前に判定する。
    pub fn from_status(status: StatusCode, body: &str, api_key: &str) -> Self {
        let detail = ErrorDetail::http(status, body, api_key);
        match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Self::Auth(detail),
            StatusCode::TOO_MANY_REQUESTS => Self::RateLimit(detail),
            _ => Self::Http(detail),
        }
    }
}

/// エラー発言の詳細(`messages.error_detail`)になる文字列。プロバイダーや通信経路から来た
/// 文字列は、DBに残り画面にも出る(Issue #159。`data-model.md` messages「error_detail」)。
/// サニタイズするコンストラクタでしか作れないようにし、アダプタが掛け忘れる経路を作らない。
///
/// 掛けるのは、送信した鍵の伏せ字と、画面に出す診断文字列としての整形(1行に畳み、
/// 不可視の書式文字を除き、長さを制限する。architecture.md 10節)。伏せ字が要るのは、
/// ゲートウェイがリクエストヘッダーをエコーバックする構成だと`Authorization`ヘッダーの値が
/// 本文にそのまま現れうるため(principles.md 4節)。鍵をURLに置く構成は
/// `net::validate_external_url`がクエリ・userinfoを拒否して塞いでいるため、対象は鍵1つで足りる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorDetail(String);

impl ErrorDetail {
    /// プロバイダーが非成功の状態コードとともに返した本文。
    pub fn http(status: StatusCode, body: &str, api_key: &str) -> Self {
        Self(format!(
            "HTTP {}: {}",
            status.as_u16(),
            sanitize(body, api_key)
        ))
    }

    /// reqwest自身の表示は"error sending request"等の固定文言だけで、接続の拒否・
    /// タイムアウト・証明書エラーの区別は`source()`の先にしか無いため、原因まで連ねる。
    /// reqwestは表示に要求URLを含めるため取り除く(パスに鍵を置くゲートウェイがある)。
    fn transport(e: reqwest::Error, api_key: &str) -> Self {
        let url = e.url().map(|url| url.to_string());
        let e = e.without_url();
        let mut text = e.to_string();
        let mut source = e.source();
        while let Some(cause) = source {
            let cause_text = cause.to_string();
            // 自分の表示に原因の表示を含めるエラーがあり、そのまま連ねると同じ文が重なる。
            if !text.contains(&cause_text) {
                text.push_str(": ");
                text.push_str(&cause_text);
            }
            source = cause.source();
        }
        // 連鎖の途中のエラーが要求URLを表示に含めても載せない。
        if let Some(url) = url {
            text = text.replace(&url, URL_PLACEHOLDER);
        }
        Self(sanitize(&text, api_key))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ErrorDetail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn sanitize(text: &str, api_key: &str) -> String {
    if api_key.is_empty() {
        return crate::text::display_label(text, MAX_DETAIL_CHARS);
    }
    // 伏せ字は整える前と後の両方で掛ける。後で掛けるのは、鍵の途中に見えない文字を挟んだ形が
    // 除いた時点で鍵として現れるため。その照合は整えた鍵で行う(鍵の前後に空白が付いたまま
    // 保存されていても、ヘッダー値としては空白を落とした形で送られ、そのまま返ってくる)。
    // 切り詰めは伏せ字の後に行い、境界で鍵の一部が残らないようにする。
    let visible = crate::text::visible_line(&text.replace(api_key, REDACTED));
    let visible_key = crate::text::visible_line(api_key);
    let redacted = if visible_key.is_empty() {
        visible
    } else {
        visible.replace(&visible_key, REDACTED)
    };
    crate::text::ellipsize(&redacted, MAX_DETAIL_CHARS)
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    use super::*;

    /// 1回だけ接続を受け、`delay`だけ待ってから`response`をそのまま書いて閉じる。
    fn spawn_once(response: &'static str, delay: Duration) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                std::thread::sleep(delay);
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{addr}/v1/chat/completions")
    }

    /// 何も待ち受けていないポートのURL。
    fn refused_url() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        format!("http://{addr}/v1/chat/completions")
    }

    fn client(url: &str, timeout: Duration) -> reqwest::Client {
        crate::net::hardened_client(url, Some(timeout)).unwrap()
    }

    #[tokio::test]
    async fn refused_connection_is_a_connection_failure_with_its_cause() {
        let url = refused_url();
        let err = client(&url, Duration::from_secs(5))
            .get(&url)
            .send()
            .await
            .unwrap_err();
        let LlmError::Connection(detail) = LlmError::from_transport(err, "") else {
            panic!("expected LlmError::Connection");
        };
        // reqwest自身の固定文言だけで終わらず、原因が連なる。URLは載せない。
        assert!(
            detail.as_str().starts_with("error sending request: "),
            "{detail}"
        );
        assert!(!detail.as_str().contains("127.0.0.1"), "{detail}");
    }

    #[tokio::test]
    async fn waiting_past_the_response_timeout_is_a_timeout() {
        let url = spawn_once(
            "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n",
            Duration::from_millis(500),
        );
        let err = client(&url, Duration::from_millis(100))
            .get(&url)
            .send()
            .await
            .unwrap_err();
        let failure = LlmError::from_transport(err, "");
        assert!(matches!(failure, LlmError::Timeout(_)), "{failure:?}");
        assert!(!failure.to_string().contains("127.0.0.1"), "{failure}");
    }

    #[tokio::test]
    async fn unreadable_body_is_an_invalid_response() {
        let url = spawn_once(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 6\r\nConnection: close\r\n\r\n<html>",
            Duration::ZERO,
        );
        let response = client(&url, Duration::from_secs(5))
            .get(&url)
            .send()
            .await
            .unwrap();
        let err = response.json::<serde_json::Value>().await.unwrap_err();
        let failure = LlmError::from_transport(err, "");
        assert!(
            matches!(failure, LlmError::InvalidResponse(_)),
            "{failure:?}"
        );
    }

    /// ヘッダーに載せられない文字を含む鍵は、送る前に失敗する。
    #[tokio::test]
    async fn a_key_that_cannot_be_a_header_is_an_invalid_request() {
        let url = refused_url();
        let err = client(&url, Duration::from_secs(5))
            .get(&url)
            .bearer_auth("sk-bad\nkey")
            .send()
            .await
            .unwrap_err();
        let failure = LlmError::from_transport(err, "sk-bad\nkey");
        assert!(
            matches!(failure, LlmError::InvalidRequest(_)),
            "{failure:?}"
        );
    }

    #[test]
    fn status_codes_decide_auth_and_rate_limit() {
        for status in [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN] {
            assert!(matches!(
                LlmError::from_status(status, "", ""),
                LlmError::Auth(_)
            ));
        }
        assert!(matches!(
            LlmError::from_status(StatusCode::TOO_MANY_REQUESTS, "", ""),
            LlmError::RateLimit(_)
        ));
        for status in [StatusCode::BAD_REQUEST, StatusCode::INTERNAL_SERVER_ERROR] {
            assert!(matches!(
                LlmError::from_status(status, "", ""),
                LlmError::Http(_)
            ));
        }
    }

    #[test]
    fn http_detail_carries_status_and_sanitized_body() {
        let detail = ErrorDetail::http(
            StatusCode::UNAUTHORIZED,
            "bad token sk-secret\n",
            "sk-secret",
        );
        assert_eq!(detail.as_str(), "HTTP 401: bad token [redacted]");
    }

    #[test]
    fn sanitize_strips_control_chars_and_truncates() {
        let body = format!("line1\nline2\x07{}", "x".repeat(600));
        let sanitized = sanitize(&body, "unused-key");
        assert!(!sanitized.contains('\n'));
        assert!(!sanitized.contains('\x07'));
        assert!(sanitized.ends_with('…'));
        assert!(sanitized.chars().count() <= MAX_DETAIL_CHARS + 1);
    }

    #[test]
    fn sanitize_removes_bidi_and_zero_width_chars() {
        let body = "a\u{202E}b\u{2066}c\u{200B}d\u{FEFF}e";
        assert_eq!(sanitize(body, ""), "abcde");
    }

    #[test]
    fn sanitize_redacts_key_saved_with_surrounding_spaces() {
        let body = "token sk-supersecret1234 rejected";
        let sanitized = sanitize(body, " sk-supersecret1234\t");
        assert!(!sanitized.contains("sk-supersecret1234"));
        assert!(sanitized.contains("[redacted]"));
    }

    #[test]
    fn sanitize_redacts_key_split_by_invisible_chars() {
        let body = "token sk-super\u{200B}secret1234 rejected";
        let sanitized = sanitize(body, "sk-supersecret1234");
        assert!(!sanitized.contains("sk-supersecret1234"));
        assert!(sanitized.contains("[redacted]"));
    }

    #[test]
    fn sanitize_redacts_leaked_api_key() {
        let body = "upstream rejected token sk-supersecret1234 for this request";
        let sanitized = sanitize(body, "sk-supersecret1234");
        assert!(!sanitized.contains("sk-supersecret1234"));
        assert!(sanitized.contains("[redacted]"));
    }
}
