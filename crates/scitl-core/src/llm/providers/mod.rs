mod local_server;
pub mod openai_compat;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};

use crate::config::{ApiFormat, Config, ProviderConfig};
use crate::db::error::CoreError;
use crate::llm::{DetectedCapabilities, LlmAdapter};
use crate::secrets;

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

/// 現在の`active_provider_id`からアダプタを組み立てる。アクティブなプロバイダーが無い場合は
/// エラーではなく`None`を返す(全プロバイダー削除は有効な状態であり、チャット送信時に
/// 初めてエラー発言として表面化させる)。
///
/// 資格情報ストアが利用できない(OSユーザーが変わった、キーチェーンをクリアした等)
/// 場合でもアプリ自体は起動させる。鍵無し扱いに落とし、実際のAPI呼び出し時に
/// プロバイダー側の認証エラーとして表面化させる。
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
    }
}

/// `models`の能力を推論サーバーに問い合わせる。`Ok(None)`は能力を問い合わせられない
/// サーバー、`Err`はサーバーに繋がらない。サーバーが知らないモデルは結果に含めない。
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

/// 鍵を付ける。認証不要のローカル推論サーバー向けに、鍵が空なら`Authorization`ヘッダーごと
/// 付けない(`Bearer `だけを送ると、空の鍵を不正な鍵として弾くサーバーがある)。
fn with_api_key(
    request: reqwest::RequestBuilder,
    api_key: &SecretString,
) -> reqwest::RequestBuilder {
    let key = api_key.expose_secret();
    if key.is_empty() {
        request
    } else {
        request.bearer_auth(key)
    }
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
