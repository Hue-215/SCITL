pub mod anthropic;
mod local_server;
pub mod openai_compat;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use reqwest::StatusCode;
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;

use crate::config::{ApiFormat, Config, ProviderConfig};
use crate::error::CoreError;
use crate::llm::{DetectedCapabilities, LlmAdapter, LlmError};
use crate::secrets;

use anthropic::AnthropicAdapter;
use openai_compat::OpenAiCompatAdapter;

pub type SharedAdapter = Arc<dyn LlmAdapter + Send + Sync>;

pub struct ActiveAdapter {
    pub adapter: Option<SharedAdapter>,
    /// 鍵を読めずに鍵無しで組み立てた。資格情報ストアのロック解除後などに読み直せるよう、
    /// 呼び出し元は次の機会に組み立て直す。
    pub key_unavailable: bool,
}

/// 登録前の`base_url`の検証。方言ごとの規則は各アダプタが持ち、ここは振り分けるだけ。
pub fn validate_base_url(api_format: ApiFormat, base_url: &str) -> Result<(), CoreError> {
    match api_format {
        ApiFormat::OpenAiCompat => openai_compat::validate_base_url(base_url),
        ApiFormat::Anthropic => anthropic::validate_base_url(base_url),
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
        ApiFormat::Anthropic => true,
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
    }
}

/// プロバイダーが提供するモデル名の一覧。名前順で、登録済みのものも含む。
pub async fn list_models(provider: &ProviderConfig) -> Result<Vec<String>, CoreError> {
    let api_key = load_api_key_off_thread(provider).await?;
    match provider.api_format {
        ApiFormat::OpenAiCompat => openai_compat::list_models(&provider.base_url, &api_key).await,
        ApiFormat::Anthropic => anthropic::list_models(&provider.base_url, &api_key).await,
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
    /// `x-api-key`
    XApiKey,
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
    let request = match header {
        _ if key.is_empty() => request,
        KeyHeader::Bearer => request.bearer_auth(key),
        // 載せられない文字を含む鍵は、送るときに組み立ての失敗(`InvalidRequest`)になる。
        KeyHeader::XApiKey => match reqwest::header::HeaderValue::from_str(key) {
            Ok(mut value) => {
                value.set_sensitive(true);
                request.header("x-api-key", value)
            }
            Err(_) => request.header("x-api-key", key),
        },
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
    let body = response.text().await.unwrap_or_default();
    Err(classify(status, &body))
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
