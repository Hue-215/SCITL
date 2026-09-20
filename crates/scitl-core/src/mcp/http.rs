//! streamable_http方式のMCPサーバーへの接続とツール一覧取得。

use std::collections::HashMap;

use reqwest::header::{HeaderName, HeaderValue};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::ServiceExt;
use secrecy::ExposeSecret;

use crate::config::SecretRef;
use crate::db::error::CoreError;

use super::{resolve_secrets, sanitize_tool_text, McpToolInfo, LIST_TOOLS_TIMEOUT};

pub(super) async fn list_tools(
    url: &str,
    header_refs: &[SecretRef],
) -> Result<Vec<McpToolInfo>, CoreError> {
    let client =
        crate::net::hardened_client(url, LIST_TOOLS_TIMEOUT).map_err(|e| CoreError::Mcp(e.to_string()))?;

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
        .control_request_timeout(LIST_TOOLS_TIMEOUT)
        .custom_headers(headers);
    let transport = StreamableHttpClientTransport::with_client(client, config);

    let service = ()
        .serve(transport)
        .await
        .map_err(|e| CoreError::Mcp(format!("failed to connect: {e}")))?;

    let result = service.list_tools(None).await;
    // 一覧取得の成否によらず、開いたセッションは必ず閉じる(取得の都度接続する
    // ステートレス設計。常駐接続やコネクションプールは持たない)。
    let _ = service.cancel().await;

    let result = result.map_err(|e| CoreError::Mcp(format!("failed to list tools: {e}")))?;
    Ok(result
        .tools
        .into_iter()
        .map(|t| McpToolInfo {
            name: sanitize_tool_text(&t.name),
            description: t.description.as_deref().map(sanitize_tool_text),
        })
        .collect())
}
