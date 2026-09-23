pub mod openai_compat;

use std::sync::Arc;

use secrecy::SecretString;

use crate::config::{ApiFormat, Config, ProviderConfig};
use crate::db::error::CoreError;
use crate::llm::LlmAdapter;
use crate::secrets;

use openai_compat::OpenAiCompatAdapter;

pub type SharedAdapter = Arc<dyn LlmAdapter + Send + Sync>;

/// 登録前の`base_url`の検証。方言ごとの規則は各アダプタが持ち、ここは振り分けるだけ。
pub fn validate_base_url(api_format: ApiFormat, base_url: &str) -> Result<(), CoreError> {
    match api_format {
        ApiFormat::OpenAiCompat => openai_compat::validate_base_url(base_url),
    }
}

/// 現在の`active_provider_id`からアダプタを組み立てる。アクティブなプロバイダーが無い場合は
/// エラーではなく`None`を返す(全プロバイダー削除は有効な状態であり、チャット送信時に
/// 初めてエラー発言として表面化させる)。
///
/// 資格情報ストアが利用できない(OSユーザーが変わった、キーチェーンをクリアした等)
/// 場合でもアプリ自体は起動させる。鍵無し扱いに落とし、実際のAPI呼び出し時に
/// プロバイダー側の認証エラーとして表面化させる。
pub fn build_active_adapter(config: &Config) -> Result<Option<SharedAdapter>, CoreError> {
    let Some(provider) = config.active_provider() else {
        return Ok(None);
    };
    let api_key = load_api_key(provider);
    let model = provider.resolved_model().unwrap_or_default();
    let timeout = config.general.response_timeout();

    // 方言を足したらここがコンパイルエラーになり、黙ってOpenAI互換で組み立てることはない。
    let adapter: SharedAdapter = match provider.api_format {
        ApiFormat::OpenAiCompat => Arc::new(OpenAiCompatAdapter::new(
            provider.base_url.clone(),
            api_key,
            model,
            timeout,
        )?),
    };
    Ok(Some(adapter))
}

fn load_api_key(provider: &ProviderConfig) -> SecretString {
    let Some(key_ref) = &provider.key_ref else {
        return SecretString::from(String::new());
    };
    secrets::load(key_ref).unwrap_or_else(|e| {
        eprintln!("failed to read API key from secret store, continuing without it: {e}");
        SecretString::from(String::new())
    })
}
