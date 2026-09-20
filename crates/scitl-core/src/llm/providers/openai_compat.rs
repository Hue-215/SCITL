use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use crate::db::error::CoreError;
use crate::llm::{ChatMessage, FinishReason, LlmAdapter, ResponseEvent, ToolSchema};

// reqwestの既定はタイムアウト無制限。応答しないエンドポイント1つでターンが
// 永久に固まるのを避ける(生成が長い非ストリーミング応答も想定し余裕を持たせる)。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// HTTPエラー時にエラー文へ載せるプロバイダ応答本文の上限。
const MAX_ERROR_BODY_CHARS: usize = 512;

/// LLMプロバイダ第一弾: OpenAI互換チャットコンプリーションAPI
/// (docs/spec/rebuild/architecture.md 2節)。方言吸収はこのファイル内に閉じ込め、
/// `orchestration::turn`は本アダプタの存在を知らない。
pub struct OpenAiCompatAdapter {
    client: reqwest::Client,
    base_url: String,
    api_key: SecretString,
    model: String,
}

impl OpenAiCompatAdapter {
    /// `api_key`は呼び出し元(`secrets.rs`経由)から受け取る。このアダプタ自身は
    /// keyringに触れない(architecture.md 6節)。保持中は`SecretString`に包み、
    /// Debug出力への露出とDrop後のメモリ残留を防ぐ。
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self, CoreError> {
        let base_url = base_url.into();
        validate_base_url(&base_url)?;

        let client = reqwest::Client::builder()
            .no_proxy()
            // architecture.md 5節が求めるのは「クロスホストのリダイレクトを拒否」だが、
            // チャットコンプリーションAPIが正当な理由でリダイレクトを返すことは
            // 想定していないため、同一ホスト内も含めて一律拒否する方が単純で安全。
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("reqwest client construction failed");
        Ok(Self {
            client,
            base_url,
            api_key: SecretString::from(api_key.into()),
            model: model.into(),
        })
    }
}

/// 非ループバックの`http://`宛に`bearer_auth`で鍵を送らないための検証。
/// ローカル推論サーバー向けにhttpを許す必要はあるが、その用途はループバックに限られる
/// (principles.md 4節、architecture.md 5節)。
///
/// query/fragment/userinfoも拒否する。エンドポイントは文字列連結ではなく`Url::join`で
/// 組み立てるため、これらが混ざっているとリクエストパスが鍵の置き場所として使われかねない
/// (Opusレビュー指摘: 「クエリに鍵を置く構成」を入口で消す)。
fn validate_base_url(base_url: &str) -> Result<(), CoreError> {
    let url = reqwest::Url::parse(base_url)
        .map_err(|e| CoreError::ProviderConfig(format!("base_url is not a valid URL: {e}")))?;

    if url.query().is_some() || url.fragment().is_some() || !url.username().is_empty() || url.password().is_some() {
        return Err(CoreError::ProviderConfig(
            "base_url must not contain a query, fragment, or userinfo".to_string(),
        ));
    }

    match url.scheme() {
        "https" => Ok(()),
        "http" if is_loopback(&url) => Ok(()),
        "http" => Err(CoreError::ProviderConfig(
            "http base_url is allowed only for loopback hosts".to_string(),
        )),
        other => Err(CoreError::ProviderConfig(format!(
            "unsupported base_url scheme: {other}"
        ))),
    }
}

/// `base_url`と`chat/completions`を安全に連結する。文字列の`format!`連結は末尾スラッシュの
/// 有無で壊れやすく(`//chat/completions`等)、`validate_base_url`が防ぐ意図(パスがクエリの
/// 置き場所にならないこと)とも噛み合わないため`Url::join`を使う。
fn completions_endpoint(base_url: &str) -> Result<reqwest::Url, CoreError> {
    let mut url = reqwest::Url::parse(base_url)
        .map_err(|e| CoreError::ProviderConfig(format!("base_url is not a valid URL: {e}")))?;
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    url.join("chat/completions")
        .map_err(|e| CoreError::ProviderConfig(format!("failed to build endpoint: {e}")))
}

fn is_loopback(url: &reqwest::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

/// reqwestのエラーDisplayは要求URLを含む。base_urlにクエリ形式で鍵を置く構成の
/// プロバイダでは鍵がエラー文に混入するため、URLを剥がしてから文字列化する。
fn provider_error(e: reqwest::Error) -> CoreError {
    CoreError::Llm(e.without_url().to_string())
}

/// プロバイダ制御下の応答本文をそのままエラーに載せると、表示側でのサニタイズが前提に
/// なる。本文はプロバイダ側の失敗理由を知るために残すが、長さを制限し制御文字を潰し、
/// 送信した鍵そのものが含まれていれば伏せ字にしてから載せる(principles.md 4節)。
/// ゲートウェイがリクエストヘッダをエコーバックする構成だと`Authorization`ヘッダの
/// 値がそのまま本文に現れうるため、512文字というサイズ制限だけでは防げない
/// (Opusレビュー指摘)。
fn sanitize_error_body(body: &str, api_key: &str) -> String {
    let redacted = if api_key.is_empty() {
        body.to_string()
    } else {
        body.replace(api_key, "[redacted]")
    };
    let mut sanitized: String = redacted
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_ERROR_BODY_CHARS)
        .collect();
    if redacted.chars().nth(MAX_ERROR_BODY_CHARS).is_some() {
        sanitized.push('…');
    }
    sanitized
}

#[derive(Serialize)]
struct RequestBody<'a> {
    model: &'a str,
    messages: Vec<RequestMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<RequestTool>,
    stream: bool,
}

#[derive(Serialize)]
struct RequestMessage {
    role: &'static str,
    content: String,
}

#[derive(Serialize)]
struct RequestTool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: RequestFunction,
}

#[derive(Serialize)]
struct RequestFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Deserialize)]
struct CompletionResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ResponseToolCall>,
}

#[derive(Deserialize)]
struct ResponseToolCall {
    id: Option<String>,
    function: ResponseFunctionCall,
}

#[derive(Deserialize)]
struct ResponseFunctionCall {
    name: String,
    arguments: String,
}

#[async_trait::async_trait]
impl LlmAdapter for OpenAiCompatAdapter {
    async fn send(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSchema],
    ) -> Result<Vec<ResponseEvent>, CoreError> {
        let body = RequestBody {
            model: &self.model,
            messages: messages
                .iter()
                .map(|m| RequestMessage {
                    role: m.role,
                    content: m.content.clone(),
                })
                .collect(),
            tools: tools
                .iter()
                .map(|t| RequestTool {
                    kind: "function",
                    function: RequestFunction {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        parameters: t.parameters.clone(),
                    },
                })
                .collect(),
            // 非ストリーミングでも戻り値はイベント列に組み立て直す(本モジュール冒頭コメント参照)。
            stream: false,
        };

        let endpoint = completions_endpoint(&self.base_url)?;
        let response = self
            .client
            .post(endpoint)
            .bearer_auth(self.api_key.expose_secret())
            .json(&body)
            .send()
            .await
            .map_err(provider_error)?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(CoreError::Llm(format!(
                "http {status}: {}",
                sanitize_error_body(&text, self.api_key.expose_secret())
            )));
        }

        let parsed: CompletionResponse = response.json().await.map_err(provider_error)?;

        let choice = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| CoreError::Llm("empty choices".to_string()))?;

        let mut events = Vec::new();
        if let Some(text) = choice.message.content {
            if !text.is_empty() {
                events.push(ResponseEvent::TextDelta { text });
            }
        }
        for call in choice.message.tool_calls {
            // 引数の型が期待と違う場合は変換を試みず、エラーとして返す(principles.md 3節)。
            // 空オブジェクトへのフォールバックは、引数を伴うツールを引数無しで
            // 発火させてしまうため避ける。
            let arguments: serde_json::Value = serde_json::from_str(&call.function.arguments)
                .map_err(|e| {
                    CoreError::Llm(format!("invalid tool call arguments from provider: {e}"))
                })?;
            events.push(ResponseEvent::ToolCall {
                id: call.id,
                name: call.function.name,
                arguments,
            });
        }

        let finish_reason = match choice.finish_reason.as_deref() {
            Some("tool_calls") => FinishReason::ToolCall,
            Some("length") => FinishReason::Length,
            Some("stop") | None => FinishReason::Stop,
            Some(_) => FinishReason::Stop,
        };
        events.push(ResponseEvent::Done { finish_reason });

        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_https_base_url() {
        assert!(validate_base_url("https://api.openai.com/v1").is_ok());
    }

    #[test]
    fn accepts_http_loopback_base_url() {
        assert!(validate_base_url("http://127.0.0.1:8080/v1").is_ok());
        assert!(validate_base_url("http://localhost:8080/v1").is_ok());
        assert!(validate_base_url("http://[::1]:8080/v1").is_ok());
    }

    #[test]
    fn rejects_http_non_loopback_base_url() {
        let err = validate_base_url("http://example.com/v1").unwrap_err();
        assert!(matches!(err, CoreError::ProviderConfig(_)));
    }

    #[test]
    fn rejects_unsupported_scheme() {
        let err = validate_base_url("ftp://example.com/v1").unwrap_err();
        assert!(matches!(err, CoreError::ProviderConfig(_)));
    }

    #[test]
    fn rejects_base_url_with_query_fragment_or_userinfo() {
        assert!(validate_base_url("https://api.example.com/v1?key=secret").is_err());
        assert!(validate_base_url("https://api.example.com/v1#frag").is_err());
        assert!(validate_base_url("https://user:pass@api.example.com/v1").is_err());
    }

    #[test]
    fn completions_endpoint_joins_regardless_of_trailing_slash() {
        assert_eq!(
            completions_endpoint("https://api.openai.com/v1").unwrap().as_str(),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            completions_endpoint("https://api.openai.com/v1/").unwrap().as_str(),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn sanitize_error_body_strips_control_chars_and_truncates() {
        let body = format!("line1\nline2\x07{}", "x".repeat(600));
        let sanitized = sanitize_error_body(&body, "unused-key");
        assert!(!sanitized.contains('\n'));
        assert!(!sanitized.contains('\x07'));
        assert!(sanitized.ends_with('…'));
        assert!(sanitized.chars().count() <= MAX_ERROR_BODY_CHARS + 1);
    }

    #[test]
    fn sanitize_error_body_redacts_leaked_api_key() {
        let body = "upstream rejected token sk-supersecret1234 for this request";
        let sanitized = sanitize_error_body(body, "sk-supersecret1234");
        assert!(!sanitized.contains("sk-supersecret1234"));
        assert!(sanitized.contains("[redacted]"));
    }
}
