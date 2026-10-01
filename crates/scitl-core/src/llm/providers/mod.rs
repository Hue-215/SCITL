pub mod anthropic;
pub mod gemini;
mod local_server;
pub mod openai_compat;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use reqwest::StatusCode;
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use serde_json::value::RawValue;

use crate::config::{ApiFormat, Config, ProviderConfig};
use crate::error::CoreError;
use crate::llm::{DetectedCapabilities, ErrorDetail, LlmAdapter, LlmError};
use crate::net::ExternalUrl;
use crate::secrets;

#[cfg(test)]
mod test_server;

use anthropic::AnthropicAdapter;
use gemini::GeminiAdapter;
use openai_compat::OpenAiCompatAdapter;

pub type SharedAdapter = Arc<dyn LlmAdapter + Send + Sync>;

pub struct ActiveAdapter {
    pub adapter: Option<SharedAdapter>,
    /// 鍵を読めずに鍵無しで組み立てた。資格情報ストアのロック解除後などに読み直せるよう、
    /// 呼び出し元は次の機会に組み立て直す。
    pub key_unavailable: bool,
}

/// 登録前の`base_url`の検証。平文の`http://`で鍵を送れる範囲(ループバックとプライベート
/// IPリテラル)は[`ExternalUrl::parse`]が決める。今はどの方言も同じ規則。
pub fn validate_base_url(api_format: ApiFormat, base_url: &str) -> Result<(), CoreError> {
    match api_format {
        ApiFormat::OpenAiCompat | ApiFormat::Anthropic | ApiFormat::Gemini => {
            parse_base_url(base_url).map(drop)
        }
    }
}

fn parse_base_url(base_url: &str) -> Result<ExternalUrl, CoreError> {
    ExternalUrl::parse(base_url).map_err(CoreError::ProviderConfig)
}

/// 送り先の識別に使う、要求URLのオリジン(`AdapterIdentity::server`)。
fn server(base_url: &ExternalUrl) -> String {
    base_url.as_url().origin().ascii_serialization()
}

/// `base_url`の下の`chat/completions`等のパス。
fn endpoint(base_url: &ExternalUrl, path: &str) -> Result<reqwest::Url, CoreError> {
    base_url.join(path).map_err(CoreError::ProviderConfig)
}

/// モデルの一覧・能力の問い合わせの上限。生成を待たずに返るので、生成を待つための応答
/// タイムアウト(`config::GeneralConfig::response_timeout`)は使わない。
const METADATA_TIMEOUT: Duration = Duration::from_secs(15);

/// 一覧・能力の問い合わせの応答を読む。失敗は状態コードだけで分類する。
async fn read_success_json<T: DeserializeOwned>(
    response: reqwest::Response,
    api_key: &SecretString,
) -> Result<T, CoreError> {
    read_success_json_with(response, api_key, LlmError::from_status).await
}

/// [`read_success_json`]の、失敗を`classify`(状態コード・本文・伏せる鍵から分類する)で分類する形。
async fn read_success_json_with<T: DeserializeOwned>(
    response: reqwest::Response,
    api_key: &SecretString,
    classify: fn(StatusCode, &str, &str) -> LlmError,
) -> Result<T, CoreError> {
    let key = api_key.expose_secret();
    let response = reject_failure(response, |status, body| classify(status, body, key)).await?;
    Ok(read_json(response, api_key).await?)
}

/// リクエストに並べる要素(Anthropic形式のブロック・Gemini形式のステップ)。組み立てたものか、
/// 受け取ったまま送り返すもの([`crate::llm::Replay`])か。
#[derive(serde::Serialize)]
#[serde(untagged)]
enum RequestPart {
    Built(serde_json::Value),
    Received(Box<RawValue>),
}

fn built(parts: impl IntoIterator<Item = serde_json::Value>) -> Vec<RequestPart> {
    parts.into_iter().map(RequestPart::Built).collect()
}

fn received(replay: &crate::llm::Replay) -> Vec<RequestPart> {
    replay
        .elements()
        .iter()
        .cloned()
        .map(RequestPart::Received)
        .collect()
}

/// プレビューの本文で、読めない大きな値(画像の実体・思考の署名)だけを長さに縮める
/// (`LlmAdapter::request_preview`)。`blocks`は方言の発言列で、その要素と、要素の`nested`の欄に
/// ある列の要素だけを見る(ツールの引数・定義の中は見ない)。`targets`は、`type`がその値の要素で
/// 縮める欄の位置の組。
fn abbreviate(blocks: &mut serde_json::Value, nested: &[&str], targets: &[(&str, &[&str])]) {
    let Some(blocks) = blocks.as_array_mut() else {
        return;
    };
    for block in blocks {
        let target = targets
            .iter()
            .find(|(t, _)| block.get("type").and_then(serde_json::Value::as_str) == Some(*t));
        if let Some((_, path)) = target {
            let field = path.iter().try_fold(&mut *block, |v, key| v.get_mut(*key));
            if let Some(serde_json::Value::String(text)) = field {
                *text = format!("… ({} bytes)", text.len());
            }
        }
        for key in nested {
            if let Some(list) = block.get_mut(*key) {
                abbreviate(list, nested, targets);
            }
        }
    }
}

/// 受け取った応答の要素を読む。生のJSONとして正しくても`Value`に読めない要素(範囲を超える
/// 数値等)があれば、応答の解釈の失敗にする。
fn read_elements(
    elements: &[Box<RawValue>],
    api_key: &SecretString,
) -> Result<Vec<serde_json::Value>, LlmError> {
    elements
        .iter()
        .map(|raw| {
            serde_json::from_str(raw.get()).map_err(|e| {
                LlmError::InvalidResponse(ErrorDetail::http(
                    StatusCode::OK,
                    &e.to_string(),
                    api_key.expose_secret(),
                ))
            })
        })
        .collect()
}

/// ツール呼び出しの引数を、オブジェクトしか受け付けない方言に渡す形にする。その方言の応答から
/// 来た呼び出しは常にオブジェクトなので、そうでないのは別の方言で実行した記録だけで、空の
/// オブジェクトとして送る。
fn object_arguments(arguments: &crate::llm::ToolArguments) -> serde_json::Value {
    match arguments {
        crate::llm::ToolArguments::Valid { value } if value.is_object() => value.clone(),
        _ => serde_json::json!({}),
    }
}

/// アダプタの組み立てに使う設定値。[`build_active_adapter`]は設定からこれだけを読む。
/// 設定の変更でアダプタを組み立て直すかどうかは、これが変わったかで決める
/// (モデルの表示や能力の切り替えのたびに資格情報ストアを読みに行かないように)。
#[derive(PartialEq)]
pub struct AdapterInputs<'a> {
    provider: Option<ProviderInputs<'a>>,
    timeout: Duration,
}

#[derive(PartialEq)]
struct ProviderInputs<'a> {
    api_format: ApiFormat,
    base_url: &'a str,
    key_ref: Option<&'a str>,
    model: &'a str,
}

impl<'a> AdapterInputs<'a> {
    pub fn of(config: &'a Config) -> Self {
        Self {
            provider: config.active_provider().map(|p| ProviderInputs {
                api_format: p.api_format,
                base_url: &p.base_url,
                key_ref: p.key_ref.as_deref(),
                model: p.resolved_model().unwrap_or_default(),
            }),
            timeout: config.general.response_timeout(),
        }
    }
}

/// 現在の`active_provider_id`からアダプタを組み立てる。アクティブなプロバイダーが無ければ
/// `None`(チャット送信時にエラー発言になる)。
///
/// 資格情報ストアを使えなくても、鍵無しで組み立てて起動を続ける(API呼び出し時に認証エラー
/// として表に出る)。
pub fn build_active_adapter(config: &Config) -> Result<ActiveAdapter, CoreError> {
    let AdapterInputs { provider, timeout } = AdapterInputs::of(config);
    let Some(provider) = provider else {
        return Ok(ActiveAdapter {
            adapter: None,
            key_unavailable: false,
        });
    };
    let (api_key, key_unavailable) = load_api_key(provider.key_ref);

    // 方言を足したらここがコンパイルエラーになり、黙ってOpenAI互換で組み立てることはない。
    let adapter: SharedAdapter = match provider.api_format {
        ApiFormat::OpenAiCompat => Arc::new(OpenAiCompatAdapter::new(
            provider.base_url.to_string(),
            api_key,
            provider.model,
            timeout,
        )?),
        ApiFormat::Anthropic => Arc::new(AnthropicAdapter::new(
            provider.base_url.to_string(),
            api_key,
            provider.model,
            timeout,
        )?),
        ApiFormat::Gemini => Arc::new(GeminiAdapter::new(
            provider.base_url.to_string(),
            api_key,
            provider.model,
            timeout,
        )?),
    };
    Ok(ActiveAdapter {
        adapter: Some(adapter),
        key_unavailable,
    })
}

/// モデルの能力を推論サーバーに問い合わせられるプロバイダーか(能力解決の「自動検出」の層)。
pub fn can_detect_capabilities(provider: &ProviderConfig) -> bool {
    match provider.api_format {
        ApiFormat::OpenAiCompat => local_server::is_detectable(&provider.base_url),
        ApiFormat::Anthropic | ApiFormat::Gemini => true,
    }
}

/// `models`の能力を推論サーバーに問い合わせる。`Ok(None)`は能力を問い合わせられない
/// サーバー、`Err`はサーバーに繋がらないか、今は答えられない。サーバーが知らないモデルは
/// 結果に含めない。
pub async fn detect_capabilities(
    provider: &ProviderConfig,
    models: &[String],
) -> Result<Option<HashMap<String, DetectedCapabilities>>, CoreError> {
    if !can_detect_capabilities(provider) {
        return Ok(None);
    }
    let api_key = load_api_key_off_thread(provider).await?;
    match provider.api_format {
        ApiFormat::OpenAiCompat => local_server::detect(&provider.base_url, &api_key, models).await,
        ApiFormat::Anthropic => anthropic::detect(&provider.base_url, &api_key, models)
            .await
            .map(Some),
        ApiFormat::Gemini => gemini::detect(&provider.base_url, &api_key, models)
            .await
            .map(Some),
    }
}

/// プロバイダーが提供するモデル名の一覧。名前順で、登録済みのものも含む。
pub async fn list_models(provider: &ProviderConfig) -> Result<Vec<String>, CoreError> {
    let api_key = load_api_key_off_thread(provider).await?;
    match provider.api_format {
        ApiFormat::OpenAiCompat => openai_compat::list_models(&provider.base_url, &api_key).await,
        ApiFormat::Anthropic => anthropic::list_models(&provider.base_url, &api_key).await,
        ApiFormat::Gemini => gemini::list_models(&provider.base_url, &api_key).await,
    }
}

/// 非同期の問い合わせの前に鍵を読む。資格情報ストアの呼び出しはブロックするため
/// 別スレッドで行う。読めなければ鍵無しで進め、認証の失敗として表面化させる
/// ([`build_active_adapter`]と同じ扱い)。
async fn load_api_key_off_thread(provider: &ProviderConfig) -> Result<SecretString, CoreError> {
    let key_ref = provider.key_ref.clone();
    let (api_key, _) = crate::blocking::run(move || Ok(load_api_key(key_ref.as_deref()))).await?;
    Ok(api_key)
}

/// 会話がアシスタント発言から始まるときに、その前へ補うユーザー発言の本文。
const PLACEHOLDER_USER_TEXT: &str = "(The earlier part of this conversation is not available.)";

/// 鍵を載せるヘッダー。方言ごとに違う。
#[derive(Clone, Copy)]
enum KeyHeader {
    /// `Authorization: Bearer`
    Bearer,
    /// 鍵をそのまま値にする独自のヘッダー(`x-api-key`等)。
    Named(&'static str),
}

/// 鍵を添えて送る。届かなかったとき(接続・タイムアウト等)は、鍵を伏せた[`LlmError`]にする。
/// 応答の状態コードは見ない([`reject_failure`])。
async fn send_with_key(
    request: reqwest::RequestBuilder,
    api_key: &SecretString,
    header: KeyHeader,
) -> Result<reqwest::Response, LlmError> {
    let key = api_key.expose_secret();
    // 認証不要のローカル推論サーバー向けに、鍵が空なら鍵のヘッダーごと付けない
    // (`Bearer `だけを送ると、空の鍵を不正な鍵として弾くサーバーがある)。
    if key.is_empty() {
        return request
            .send()
            .await
            .map_err(|e| LlmError::from_transport(e, key));
    }
    // ヘッダーに載せられない鍵は、方言によらず送る前に同じ文言で断る(`bearer_auth`に任せると、
    // reqwestの組み立ての失敗として内部の文言のまま出る)。
    let mut value = reqwest::header::HeaderValue::from_str(key).map_err(|_| {
        LlmError::InvalidRequest(ErrorDetail::internal(
            "the API key contains characters that cannot be sent in a header",
        ))
    })?;
    let request = match header {
        KeyHeader::Bearer => request.bearer_auth(key),
        KeyHeader::Named(name) => {
            value.set_sensitive(true);
            request.header(name, value)
        }
    };
    request
        .send()
        .await
        .map_err(|e| LlmError::from_transport(e, key))
}

/// 非成功の状態コードの応答を、本文とともに`classify`で分類したエラーにする。本文でしか
/// 分からない種類を見ないなら、`classify`は[`LlmError::from_status`]でよい。
async fn reject_failure(
    response: reqwest::Response,
    classify: impl FnOnce(StatusCode, &str) -> LlmError,
) -> Result<reqwest::Response, LlmError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let retry_after = retry_after_secs(&response);
    let body = response.text().await.unwrap_or_default();
    Err(match (classify(status, &body), retry_after) {
        (LlmError::RateLimit(detail), Some(secs)) => {
            LlmError::RateLimit(detail.with_retry_after(secs))
        }
        (error, _) => error,
    })
}

/// レート制限の応答が示す、送り直してよくなるまでの秒数(`Retry-After`)。日時の形は扱わない
/// (LLMのAPIは秒数で返す)。
fn retry_after_secs(response: &reqwest::Response) -> Option<u64> {
    if response.status() != StatusCode::TOO_MANY_REQUESTS {
        return None;
    }
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// 成功の応答の本文をJSONとして読む。読めなければ鍵を伏せた[`LlmError`]にする。
async fn read_json<T: DeserializeOwned>(
    response: reqwest::Response,
    api_key: &SecretString,
) -> Result<T, LlmError> {
    response
        .json()
        .await
        .map_err(|e| LlmError::from_transport(e, api_key.expose_secret()))
}

/// 2つ目は「鍵があるはずなのに読めなかった」。
fn load_api_key(key_ref: Option<&str>) -> (SecretString, bool) {
    let Some(key_ref) = key_ref else {
        return (SecretString::from(String::new()), false);
    };
    match secrets::load(key_ref) {
        Ok(key) => (key, false),
        Err(e) => {
            eprintln!("failed to read API key from secret store, continuing without it: {e}");
            (SecretString::from(String::new()), true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_https_base_url() {
        assert!(parse_base_url("https://api.openai.com/v1").is_ok());
    }

    #[test]
    fn accepts_http_loopback_base_url() {
        assert!(parse_base_url("http://127.0.0.1:8080/v1").is_ok());
        assert!(parse_base_url("http://localhost:8080/v1").is_ok());
        assert!(parse_base_url("http://[::1]:8080/v1").is_ok());
    }

    #[test]
    fn accepts_http_private_ip_literal_base_url() {
        // 境界値はnet.rsで確かめ、ここではLLMプロバイダー側にも効いていることだけを見る。
        assert!(parse_base_url("http://192.168.1.107:11434/v1").is_ok());
    }

    #[test]
    fn rejects_http_hostname_base_url() {
        // ホスト名(localhost以外)は名前解決しないため、平文では常に拒否する(IPリテラルは許す)。
        let err = parse_base_url("http://example.com/v1").unwrap_err();
        assert!(matches!(err, CoreError::ProviderConfig(_)));
    }

    #[test]
    fn rejects_unsupported_scheme() {
        let err = parse_base_url("ftp://example.com/v1").unwrap_err();
        assert!(matches!(err, CoreError::ProviderConfig(_)));
    }

    #[test]
    fn rejects_base_url_with_query_fragment_or_userinfo() {
        assert!(parse_base_url("https://api.example.com/v1?key=secret").is_err());
        assert!(parse_base_url("https://api.example.com/v1#frag").is_err());
        assert!(parse_base_url("https://user:pass@api.example.com/v1").is_err());
    }

    #[test]
    fn endpoint_joins_regardless_of_trailing_slash() {
        assert_eq!(
            endpoint(
                &parse_base_url("https://api.openai.com/v1").unwrap(),
                "chat/completions"
            )
            .unwrap()
            .as_str(),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            endpoint(
                &parse_base_url("https://api.openai.com/v1/").unwrap(),
                "chat/completions"
            )
            .unwrap()
            .as_str(),
            "https://api.openai.com/v1/chat/completions"
        );
    }
}
