//! 複数の外部通信経路(LLMプロバイダー、MCP streamable_http)に共通するURL検証。
//! architecture.md 5節を単一の正とし、経路ごとの検証コードはここを呼ぶ。
//!
//! `reqwest::Client`本体(タイムアウト・プロキシ・リダイレクト設定)は各経路で個別に
//! 組み立てる(`llm/providers/openai_compat.rs`と`mcp/net.rs`)。rmcpが要求する
//! `reqwest`はワークスペース既定(LLMアダプタが使う版)とメジャーバージョンが異なり
//! (`Cargo.toml`のコメント参照)、`reqwest::Client`という型そのものを共有できないため。
//! `url::Url`はどちらのバージョンの`reqwest`からも同じ`url`クレートの型として
//! 再エクスポートされているため、検証ロジックだけはここに集約できる。

use url::Url;

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

#[cfg(test)]
mod tests {
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
}
