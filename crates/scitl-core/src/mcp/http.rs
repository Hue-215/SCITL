//! streamable_http方式のMCPサーバーへの接続。

use std::collections::HashMap;

use reqwest::header::{HeaderName, HeaderValue};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::ServiceExt;
use secrecy::ExposeSecret;

use crate::config::SecretRef;
use crate::error::CoreError;

use super::{resolve_secrets, ClientService, CONNECT_TIMEOUT};

pub(super) async fn connect(
    url: &str,
    header_refs: &[SecretRef],
) -> Result<ClientService, CoreError> {
    // リクエスト全体の上限は掛けない(`net::hardened_client`参照)。各段の上限は
    // 呼び出し側(`mod.rs`)の`tokio::time::timeout`が持つ。
    let client =
        crate::net::hardened_client(url, None).map_err(|e| CoreError::Mcp(e.to_string()))?;

    let resolved = resolve_secrets(header_refs).await?;
    let mut headers = HashMap::with_capacity(resolved.len());
    for (name, secret) in &resolved {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| CoreError::Mcp(format!("invalid header name '{name}': {e}")))?;
        let header_value = HeaderValue::from_str(secret.expose_secret())
            .map_err(|_| CoreError::Mcp(format!("invalid header value for '{name}'")))?;
        headers.insert(header_name, header_value);
    }

    let config = StreamableHttpClientTransportConfig::with_uri(url.to_string())
        .control_request_timeout(CONNECT_TIMEOUT)
        .custom_headers(headers);
    let transport = StreamableHttpClientTransport::with_client(client, config);

    ().serve(transport).await.map_err(|e| {
        CoreError::Mcp(format!(
            "failed to connect: {}",
            super::describe_server_error(&e)
        ))
    })
}
