//! streamable_http方式のMCPサーバーへの接続。

use std::collections::HashMap;

use reqwest::header::{HeaderName, HeaderValue};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::ServiceExt;
use secrecy::ExposeSecret;

use crate::config::SecretRef;
use crate::error::CoreError;
use crate::net::ExternalUrl;

use super::{resolve_secrets, ClientService, CONNECT_TIMEOUT};

/// 登録したヘッダーの秘密情報を資格情報ストアから読み、送るヘッダーにする。
pub(super) async fn headers(
    header_refs: &[SecretRef],
) -> Result<HashMap<HeaderName, HeaderValue>, CoreError> {
    let resolved = resolve_secrets(header_refs).await?;
    let mut headers = HashMap::with_capacity(resolved.len());
    for (name, secret) in &resolved {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| CoreError::Mcp(format!("invalid header name '{name}': {e}")))?;
        let header_value = crate::net::secret_header_value(secret.expose_secret())
            .ok_or_else(|| CoreError::Mcp(format!("{} ('{name}')", super::HEADER_VALUE_REFUSED)))?;
        headers.insert(header_name, header_value);
    }
    Ok(headers)
}

/// [`headers`]で用意したヘッダーを付けて接続する。
pub(super) async fn connect(
    url: &str,
    headers: HashMap<HeaderName, HeaderValue>,
) -> Result<ClientService, CoreError> {
    // リクエスト全体の上限は掛けない(`net::hardened_client`参照)。各段の上限は
    // 呼び出し側(`mod.rs`)の`tokio::time::timeout`が持つ。
    let parsed = ExternalUrl::parse(url).map_err(CoreError::Mcp)?;
    let client =
        crate::net::hardened_client(&parsed, None).map_err(|e| CoreError::Mcp(e.to_string()))?;

    let config = StreamableHttpClientTransportConfig::with_uri(url.to_string())
        .control_request_timeout(CONNECT_TIMEOUT)
        .custom_headers(headers);
    let transport = StreamableHttpClientTransport::with_client(client, config);

    super::client_config().serve(transport).await.map_err(|e| {
        CoreError::Mcp(format!(
            "failed to connect: {}",
            super::describe_server_error(&e)
        ))
    })
}
