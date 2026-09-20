//! 複数の外部通信経路(LLMプロバイダー、MCP streamable_http)に共通するURL検証と
//! HTTPクライアントのハードニング。architecture.md 5節を単一の正とし、全経路が
//! ここを1箇所として通る(LLMアダプタとMCPクライアントは同じreqwestバージョンを
//! 使っており、`reqwest::Client`という型そのものを共有できる。Opusレビュー指摘)。

use std::time::Duration;

use url::Url;

use crate::db::error::CoreError;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// スキーム・ループバック・query/fragment/userinfoの検証。LLMプロバイダーのbase_url、
/// MCP streamable_httpのURLの両方に適用する(principles.md 4節、architecture.md 5節)。
///
/// query/fragment/userinfoを拒否する理由: エンドポイントは`Url::join`で組み立てるため、
/// これらが混ざっているとリクエストパスや認証情報の置き場所として悪用されかねない
/// (Opusレビュー指摘: 「クエリに鍵を置く構成」を入口で消す)。
pub fn validate_external_url(url: &Url) -> Result<(), String> {
    if url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("URL must not contain a query, fragment, or userinfo".to_string());
    }

    match url.scheme() {
        "https" => Ok(()),
        "http" if is_loopback(url) => Ok(()),
        "http" => Err("http URL is allowed only for loopback hosts".to_string()),
        other => Err(format!("unsupported URL scheme: {other}")),
    }
}

pub fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

/// URLを検証してから、ハードニング済み`reqwest::Client`を組み立てる。LLMプロバイダー
/// (`llm/providers/openai_compat.rs`)とMCP streamable_http(`mcp/http.rs`)の両方が
/// これを呼ぶ(Opusレビュー指摘: 同じ設定を2箇所に書くと片方だけ直される未来が来る)。
pub fn hardened_client(url: &str, request_timeout: Duration) -> Result<reqwest::Client, CoreError> {
    let parsed =
        Url::parse(url).map_err(|e| CoreError::Config(format!("url is not a valid URL: {e}")))?;
    validate_external_url(&parsed).map_err(CoreError::Config)?;

    reqwest::Client::builder()
        .no_proxy()
        // architecture.md 5節: クロスホストのリダイレクトは拒否する。チャット
        // コンプリーションAPI・MCPサーバーいずれも正当な理由でリダイレクトを返すことは
        // 想定していないため、同一ホスト内も含めて一律拒否する方が単純で安全
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(request_timeout)
        .build()
        .map_err(|e| CoreError::Config(format!("failed to build HTTP client: {e}")))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::*;

    #[test]
    fn rejects_query_fragment_userinfo() {
        assert!(validate_external_url(&Url::parse("https://example.com/?a=b").unwrap()).is_err());
        assert!(validate_external_url(&Url::parse("https://example.com/#f").unwrap()).is_err());
        assert!(
            validate_external_url(&Url::parse("https://user:pass@example.com/").unwrap())
                .is_err()
        );
    }

    #[test]
    fn allows_https_any_host() {
        assert!(validate_external_url(&Url::parse("https://example.com/mcp").unwrap()).is_ok());
    }

    #[test]
    fn allows_http_only_for_loopback() {
        assert!(validate_external_url(&Url::parse("http://localhost:8080/mcp").unwrap()).is_ok());
        assert!(validate_external_url(&Url::parse("http://127.0.0.1:8080/mcp").unwrap()).is_ok());
        assert!(validate_external_url(&Url::parse("http://example.com/mcp").unwrap()).is_err());
    }

    // 以下は、`hardened_client`が実際に組み立てる`reqwest::Client`が全経路
    // (LLMアダプタ・MCPクライアント双方)で共有される前提で、トランスポートの外側からは
    // 検証できない「実際にリダイレクトを追わないか」「プロキシ環境変数を無視するか」を
    // クライアント単体に対して確認する(Opusレビュー指摘: ドキュメントではなくテストで
    // 担保する)。

    /// 1回だけ接続を受け、`response`をそのまま書いて閉じる最小限のHTTPサーバー。
    fn spawn_once(response: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{addr}/")
    }

    #[tokio::test]
    async fn cross_host_redirect_is_not_followed() {
        let url = spawn_once(
            "HTTP/1.1 302 Found\r\nLocation: http://example.invalid/elsewhere\r\nContent-Length: 0\r\n\r\n",
        );
        let client = hardened_client(&url, Duration::from_secs(5)).unwrap();
        // リダイレクトを追っていれば別ホストへの接続を試みて失敗するはずが、
        // ここでは追わずに302がそのまま返ってくることを確認する
        let response = client.get(&url).send().await.unwrap();
        assert_eq!(response.status().as_u16(), 302);
    }

    #[tokio::test]
    async fn proxy_env_var_is_ignored() {
        let url = spawn_once("HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n");
        // このプロセス内の他のテストはenv varを触らないため、並行実行下でも安全
        // (今後env varを操作するテストを足す場合は要注意)。
        // SAFETY: `set_var`/`remove_var`はプロセス全体のグローバル状態を変更するため
        // unsafeとされているが、このテストバイナリ内で環境変数を操作する他のテストは
        // 無く、競合は起きない。
        unsafe { std::env::set_var("http_proxy", "http://127.0.0.1:1") };
        let client = hardened_client(&url, Duration::from_secs(5)).unwrap();
        let result = client.get(&url).send().await;
        unsafe { std::env::remove_var("http_proxy") };
        // プロキシ(存在しない127.0.0.1:1)を経由していれば失敗するはずが、成功する
        assert!(result.is_ok());
    }
}
