//! streamable_http方式のMCPサーバーへの接続。

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use futures::stream::StreamExt;
use reqwest::header::{HeaderName, HeaderValue};
use rmcp::model::ClientJsonRpcMessage;
use rmcp::transport::common::client_side_sse::BoxedSseResponse;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClient, StreamableHttpClientTransportConfig, StreamableHttpError,
    StreamableHttpPostResponse,
};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::ServiceExt;
use secrecy::ExposeSecret;
use tokio::sync::watch;

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

/// MCPサーバーとの接続。`service`を閉じたあとに捨てると、rmcpの内部に残った要求とSSEの
/// ストリームも打ち切る([`AbortableClient`])。
pub(super) struct Connection {
    pub(super) service: ClientService,
    /// 捨てたら打ち切る合図の送り手。値は送らない。
    _alive: watch::Sender<()>,
}

/// [`headers`]で用意したヘッダーを付けて接続する。接続の途中でこの処理を捨てても(タイムアウト・
/// 停止の指示)、rmcpの内部の要求は打ち切られ、TCPの接続とヘッダーの値は残らない。
pub(super) async fn connect(
    url: &str,
    headers: HashMap<HeaderName, HeaderValue>,
) -> Result<Connection, CoreError> {
    // リクエスト全体の上限は掛けない(`net::hardened_client`参照)。各段の上限は
    // 呼び出し側(`mod.rs`)の`tokio::time::timeout`が持つ。
    let parsed = ExternalUrl::parse(url).map_err(CoreError::Mcp)?;
    let client = crate::net::hardened_client(&parsed, crate::net::RequestTimeout::None)
        .map_err(|e| CoreError::Mcp(e.to_string()))?;
    let (alive, receiver) = watch::channel(());
    let client = AbortableClient {
        client,
        alive: receiver,
    };

    let config = StreamableHttpClientTransportConfig::with_uri(url.to_string())
        .control_request_timeout(CONNECT_TIMEOUT)
        .custom_headers(headers);
    let transport = StreamableHttpClientTransport::with_client(client, config);

    let service = super::client_config().serve(transport).await.map_err(|e| {
        CoreError::Mcp(format!(
            "failed to connect: {}",
            super::describe_server_error(&e)
        ))
    })?;
    Ok(Connection {
        service,
        _alive: alive,
    })
}

/// 送り手([`Connection`]か、接続の途中の[`connect`])が捨てられたら、待っている要求とSSEの
/// ストリームを打ち切るHTTPクライアント(Issue #400)。セッションの後始末(`DELETE`)だけは
/// 打ち切らない。
///
/// rmcp(3.4.0)は、接続の最初の`initialize`のPOSTを、自分の打ち切りの合図と並べずに待つ。この
/// 要求を待つ処理は独立して`tokio::spawn`されているので、外から接続の試みを捨てても届かず、
/// 応答しないサーバーでは、サーバーが接続を閉じるまでTCPの接続とカスタムヘッダーの値が残る。
/// 要求を出すのはこのクライアントなので、ここで打ち切りの合図と並べる。
#[derive(Clone)]
struct AbortableClient {
    client: reqwest::Client,
    alive: watch::Receiver<()>,
}

type HttpError = StreamableHttpError<reqwest::Error>;
type SseStream = BoxedSseResponse;

impl AbortableClient {
    /// 送り手が捨てられたら終わる。値は送らないので、`changed`が返るのは捨てられたときだけ。
    fn aborted(&self) -> impl Future<Output = ()> + Send + 'static {
        let mut alive = self.alive.clone();
        async move { while alive.changed().await.is_ok() {} }
    }

    async fn unless_aborted<T>(
        &self,
        request: impl Future<Output = Result<T, HttpError>>,
    ) -> Result<T, HttpError> {
        // 打ち切りを先に見る。打ち切り済みなら、要求を始めて接続を張ることもしない。
        tokio::select! {
            biased;
            () = self.aborted() => Err(StreamableHttpError::TransportChannelClosed),
            result = request => result,
        }
    }

    /// SSEのストリームも、打ち切りの合図で終わらせる。
    fn limited(&self, stream: SseStream) -> SseStream {
        stream.take_until(self.aborted()).boxed()
    }

    fn limited_response(&self, response: StreamableHttpPostResponse) -> StreamableHttpPostResponse {
        match response {
            StreamableHttpPostResponse::Sse(stream, session) => {
                StreamableHttpPostResponse::Sse(self.limited(stream), session)
            }
            other => other,
        }
    }
}

impl StreamableHttpClient for AbortableClient {
    type Error = reqwest::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, HttpError> {
        let request =
            self.client
                .post_message(uri, message, session_id, auth_header, custom_headers);
        let response = self.unless_aborted(request).await?;
        Ok(self.limited_response(response))
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<StreamableHttpPostResponse, HttpError> {
        let request = self.client.post_message_with_max_sse_event_size(
            uri,
            message,
            session_id,
            auth_header,
            custom_headers,
            max_sse_event_size,
        );
        let response = self.unless_aborted(request).await?;
        Ok(self.limited_response(response))
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), HttpError> {
        // セッションの後始末は打ち切らない。rmcpが接続を捨てたあとに1回だけ送るもので、打ち切ると
        // サーバーの側にセッションが期限切れまで残る。rmcpが上限(5秒)を掛けているので有界。
        self.client
            .delete_session(uri, session_id, auth_header, custom_headers)
            .await
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<SseStream, HttpError> {
        let request =
            self.client
                .get_stream(uri, session_id, last_event_id, auth_header, custom_headers);
        Ok(self.limited(self.unless_aborted(request).await?))
    }

    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<SseStream, HttpError> {
        let request = self.client.get_stream_with_max_sse_event_size(
            uri,
            session_id,
            last_event_id,
            auth_header,
            custom_headers,
            max_sse_event_size,
        );
        Ok(self.limited(self.unless_aborted(request).await?))
    }
}
