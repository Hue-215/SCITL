//! 複数の外部通信経路(LLMプロバイダー、MCP streamable_http)に共通するURL検証と
//! HTTPクライアントのハードニング。architecture.md 5節を単一の正とし、全経路が
//! ここを1箇所として通る(LLMアダプタとMCPクライアントは同じreqwestバージョンを
//! 使っており、`reqwest::Client`という型そのものを共有できる)。

use std::time::Duration;

use url::Url;

use crate::error::CoreError;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// ホストの分類。平文http可否の判定(このファイル)と、将来のオフラインスイッチ
/// (Issue #3、宛先の段階: 外部通信許可/プライベートIPのみ/localhostのみ)の両方が
/// この分類を読む(1つの機能に関わる判断を1箇所に閉じる。principles.md 5節)。
/// 2つの軸は独立: 宛先の段階を緩めても、平文httpが許されるかどうかは
/// `classify_host`だけが決める(ANDで合成する。architecture.md 5節)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostClass {
    /// ループバック(127.0.0.0/8, ::1)またはホスト名`localhost`。
    Loopback,
    /// プライベートIPアドレスの**リテラル**(RFC1918: 10/8・172.16/12・192.168/16、
    /// IPv6 ULA: fc00::/7)。ホスト名は対象外(下記コメント参照)。
    PrivateLiteral,
    /// 上記のいずれでもない(パブリックIP、ホスト名)。
    Other,
}

/// ホストを分類する。ホスト名は`localhost`以外すべて`Other`として扱う
/// (DNSリバインディング対策): プライベートIPかどうかをホスト名の名前解決結果で
/// 判定すると、検証時と接続時で解決結果が変わりうる(検証だけ通してから接続先を
/// すり替える攻撃が成立する)。IPアドレスとして直接書かれたリテラルだけを見れば、
/// 検証した対象と実際に接続する対象が一致することが構造的に保証される。
///
/// IPv4のリンクローカル(169.254.0.0/16)は`PrivateLiteral`に含めない。
/// 169.254.169.254はAWS/GCP/Azureのメタデータエンドポイント(IMDS)であり、
/// 平文httpしか話さない代表的なSSRF標的のため、緩和の対象から明示的に外す
/// (IPv6側もfe80::/10は`url`crateがパースできず対象外であり、IPv4だけ
/// リンクローカルを許すと軸が揃わない)。
pub fn classify_host(url: &Url) -> HostClass {
    match url.host() {
        Some(url::Host::Ipv4(ip)) if ip.is_loopback() => HostClass::Loopback,
        Some(url::Host::Ipv6(ip)) if ip.is_loopback() => HostClass::Loopback,
        Some(url::Host::Domain(domain)) if domain.eq_ignore_ascii_case("localhost") => {
            HostClass::Loopback
        }
        Some(url::Host::Ipv4(ip)) if ip.is_private() => HostClass::PrivateLiteral,
        Some(url::Host::Ipv6(ip)) if ip.is_unique_local() => HostClass::PrivateLiteral,
        _ => HostClass::Other,
    }
}

/// スキーム・ホスト・query/fragment/userinfoの検証。LLMプロバイダーのbase_url、
/// MCP streamable_httpのURLの両方に適用する(principles.md 4節、architecture.md 5節)。
///
/// query/fragment/userinfoを拒否する理由: エンドポイントは`Url::join`で組み立てるため、
/// これらが混ざっているとリクエストパスや認証情報の置き場所として悪用されかねない
/// (「クエリに鍵を置く構成」を入口で消す)。
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
        "http" => match classify_host(url) {
            HostClass::Loopback | HostClass::PrivateLiteral => Ok(()),
            HostClass::Other if matches!(url.host(), Some(url::Host::Domain(_))) => {
                Err("http URL with a hostname is not allowed; use https, or an IP literal for loopback/private addresses".to_string())
            }
            HostClass::Other => {
                Err("http URL is allowed only for loopback or private IP addresses".to_string())
            }
        },
        other => Err(format!("unsupported URL scheme: {other}")),
    }
}

/// URLを検証してから、ハードニング済み`reqwest::Client`を組み立てる。LLMプロバイダー
/// (`llm/providers/openai_compat.rs`)とMCP streamable_http(`mcp/http.rs`)の両方が
/// これを呼ぶ(同じ設定を2箇所に書くと片方だけ直される未来が来る)。
///
/// `request_timeout`はリクエスト全体(応答本文の読み切りまで)の上限。MCPは接続・一覧取得・
/// 呼び出し・切断をそれぞれ`tokio::time::timeout`で囲んでおり、長寿命のSSEストリームも
/// 使うため`None`を渡す。接続確立の上限([`CONNECT_TIMEOUT`])はどちらにも掛かる。
pub fn hardened_client(
    url: &str,
    request_timeout: Option<Duration>,
) -> Result<reqwest::Client, CoreError> {
    let parsed =
        Url::parse(url).map_err(|e| CoreError::Config(format!("url is not a valid URL: {e}")))?;
    validate_external_url(&parsed).map_err(CoreError::Config)?;

    let mut builder = reqwest::Client::builder()
        .no_proxy()
        // architecture.md 5節: クロスホストのリダイレクトは拒否する。チャット
        // コンプリーションAPI・MCPサーバーいずれも正当な理由でリダイレクトを返すことは
        // 想定していないため、同一ホスト内も含めて一律拒否する方が単純で安全。
        // 平文httpをプライベートIPまで許すため、ここを緩めると登録先のLANサーバーが
        // 公開ホストへ302を返すだけで通信先が広がる。
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT);
    if let Some(timeout) = request_timeout {
        builder = builder.timeout(timeout);
    }
    builder
        .build()
        .map_err(|e| CoreError::Config(format!("failed to build HTTP client: {e}")))
}

/// 接続を拒否されるループバックのアドレスと、そのポートを握っているソケット。
///
/// ポートを確保してすぐ手放すと、並列に走る別のテストが同じポートで待ち受け直し、
/// 拒否されるはずの接続を横取りすることがある。bindしたままlistenしないソケットを
/// 持ち続ければ、ポートは他に割り当てられず、接続はRSTで拒否される。ソケットは
/// 接続を試し終えるまで生かしておくこと。
#[cfg(test)]
pub(crate) fn refused_addr() -> (std::net::SocketAddr, tokio::net::TcpSocket) {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind(([127, 0, 0, 1], 0).into()).unwrap();
    (socket.local_addr().unwrap(), socket)
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
            validate_external_url(&Url::parse("https://user:pass@example.com/").unwrap()).is_err()
        );
    }

    #[test]
    fn allows_https_any_host() {
        assert!(validate_external_url(&Url::parse("https://example.com/mcp").unwrap()).is_ok());
    }

    #[test]
    fn allows_http_for_loopback() {
        assert!(validate_external_url(&Url::parse("http://localhost:8080/mcp").unwrap()).is_ok());
        assert!(validate_external_url(&Url::parse("http://127.0.0.1:8080/mcp").unwrap()).is_ok());
        assert!(validate_external_url(&Url::parse("http://[::1]:8080/mcp").unwrap()).is_ok());
    }

    #[test]
    fn allows_http_for_private_ip_literal() {
        // RFC1918: 10/8, 172.16/12, 192.168/16
        assert!(validate_external_url(&Url::parse("http://10.0.0.1/mcp").unwrap()).is_ok());
        assert!(validate_external_url(&Url::parse("http://172.16.0.0/mcp").unwrap()).is_ok());
        assert!(validate_external_url(&Url::parse("http://172.31.255.255/mcp").unwrap()).is_ok());
        assert!(validate_external_url(&Url::parse("http://192.168.1.107/mcp").unwrap()).is_ok());
        // IPv6 ULA: fc00::/7
        assert!(validate_external_url(&Url::parse("http://[fc00::1]/mcp").unwrap()).is_ok());
        assert!(validate_external_url(&Url::parse("http://[fd12::1]/mcp").unwrap()).is_ok());
    }

    #[test]
    fn rejects_http_outside_private_ranges() {
        // 172.16/12の外側
        assert!(validate_external_url(&Url::parse("http://172.15.255.255/mcp").unwrap()).is_err());
        assert!(validate_external_url(&Url::parse("http://172.32.0.0/mcp").unwrap()).is_err());
        // 192.168/16の外側
        assert!(validate_external_url(&Url::parse("http://192.167.0.1/mcp").unwrap()).is_err());
        // パブリックIP
        assert!(validate_external_url(&Url::parse("http://8.8.8.8/mcp").unwrap()).is_err());
        // ホスト名(localhost以外)は名前解決しないため常に拒否
        assert!(validate_external_url(&Url::parse("http://example.com/mcp").unwrap()).is_err());
        assert!(validate_external_url(&Url::parse("http://nas.local/mcp").unwrap()).is_err());
    }

    #[test]
    fn rejects_http_for_ipv4_link_local() {
        // 169.254.169.254はクラウド各社のメタデータエンドポイント(IMDS)。SSRF対策として
        // リンクローカル全体を対象から外す。
        assert!(validate_external_url(&Url::parse("http://169.254.169.254/").unwrap()).is_err());
        assert!(validate_external_url(&Url::parse("http://169.254.1.1/").unwrap()).is_err());
    }

    #[test]
    fn rejects_http_for_ipv6_link_local() {
        // `url`クレートがスコープIDなしのfe80::をパースする場合に備えた回帰確認
        // (スコープID付きは`url::Url::parse`自体が失敗するため、ここでは対象外)。
        assert!(validate_external_url(&Url::parse("http://[fe80::1]/").unwrap()).is_err());
    }

    #[test]
    fn rejects_http_for_ipv4_mapped_ipv6_literal() {
        // IPv4射影IPv6アドレスは`Ipv6Addr::is_unique_local`の対象にならないため拒否される。
        // プライベートアドレスに接続したい場合はIPv4リテラルで書く必要がある。
        assert!(
            validate_external_url(&Url::parse("http://[::ffff:192.168.1.7]/").unwrap()).is_err()
        );
    }

    // 以下は、`hardened_client`が実際に組み立てる`reqwest::Client`が全経路
    // (LLMアダプタ・MCPクライアント双方)で共有される前提で、トランスポートの外側からは
    // 検証できない「実際にリダイレクトを追わないか」「プロキシ環境変数を無視するか」を
    // クライアント単体に対して確認する(ドキュメントではなくテストで
    // 担保する)。

    /// 1回だけ接続を受け、`response`をそのまま書いて閉じる最小限のHTTPサーバー。
    fn spawn_once(response: &'static str) -> String {
        spawn_once_delayed(response, Duration::ZERO)
    }

    /// `spawn_once`と同じだが、応答を返す前に`delay`だけ待つ。
    fn spawn_once_delayed(response: &'static str, delay: Duration) -> String {
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
        format!("http://{addr}/")
    }

    const NO_CONTENT: &str = "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n";

    #[tokio::test]
    async fn request_timeout_applies_when_given() {
        let url = spawn_once_delayed(NO_CONTENT, Duration::from_millis(500));
        let client = hardened_client(&url, Some(Duration::from_millis(100))).unwrap();
        let err = client.get(&url).send().await.unwrap_err();
        assert!(err.is_timeout());
    }

    #[tokio::test]
    async fn no_request_timeout_when_none() {
        // MCPは`None`を渡し、各段の上限を呼び出し側の`tokio::time::timeout`に任せる。
        // reqwest側に全体の上限が残っていると、呼び出し側の上限より先に切れる。
        let url = spawn_once_delayed(NO_CONTENT, Duration::from_millis(500));
        let client = hardened_client(&url, None).unwrap();
        let response = client.get(&url).send().await.unwrap();
        assert_eq!(response.status().as_u16(), 204);
    }

    #[tokio::test]
    async fn cross_host_redirect_is_not_followed() {
        let url = spawn_once(
            "HTTP/1.1 302 Found\r\nLocation: http://example.invalid/elsewhere\r\nContent-Length: 0\r\n\r\n",
        );
        let client = hardened_client(&url, Some(Duration::from_secs(5))).unwrap();
        // リダイレクトを追っていれば別ホストへの接続を試みて失敗するはずが、
        // ここでは追わずに302がそのまま返ってくることを確認する
        let response = client.get(&url).send().await.unwrap();
        assert_eq!(response.status().as_u16(), 302);
    }

    #[tokio::test]
    async fn proxy_env_var_is_ignored() {
        let url = spawn_once(NO_CONTENT);
        // このプロセス内の他のテストはenv varを触らないため、並行実行下でも安全
        // (今後env varを操作するテストを足す場合は要注意)。
        // SAFETY: `set_var`/`remove_var`はプロセス全体のグローバル状態を変更するため
        // unsafeとされているが、このテストバイナリ内で環境変数を操作する他のテストは
        // 無く、競合は起きない。
        unsafe { std::env::set_var("http_proxy", "http://127.0.0.1:1") };
        let client = hardened_client(&url, Some(Duration::from_secs(5))).unwrap();
        let result = client.get(&url).send().await;
        unsafe { std::env::remove_var("http_proxy") };
        // プロキシ(存在しない127.0.0.1:1)を経由していれば失敗するはずが、成功する
        assert!(result.is_ok());
    }
}
