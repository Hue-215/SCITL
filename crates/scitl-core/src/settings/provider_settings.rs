//! プロバイダーの登録と削除(設定画面「プロバイダー」)。モデルの登録は`model_settings`。

use secrecy::{ExposeSecret, SecretString};

use super::{delete_secret, input, invalid, Settings, SettingsView};
use crate::config::{ApiFormat, Config, ProviderConfig};
use crate::error::{CoreError, Result};
use crate::llm::providers;
use crate::secrets;

/// プロバイダー追加フォームからの入力。
pub struct NewProvider {
    pub name: String,
    pub api_format: ApiFormat,
    pub base_url: String,
    pub api_key: Option<SecretString>,
}

impl Settings {
    /// 非同期の問い合わせに使う、ある時点のプロバイダー設定の複製。ロックを`.await`に
    /// またがせないため、複製してから問い合わせる。
    pub(super) fn provider(&self, provider_id: &str) -> Result<ProviderConfig> {
        self.current()
            .config
            .providers
            .iter()
            .find(|p| p.id == provider_id)
            .cloned()
            .ok_or_else(|| provider_not_found(provider_id))
    }

    /// 最初に登録したプロバイダーをアクティブにする
    /// ([`Config::reselect_active_provider`])。同じ名前のプロバイダーは登録できない
    /// (チャットのモデル選択で見分けられなくなる)。鍵の保存に失敗したらプロバイダー自体の
    /// 登録も中断し、登録に失敗したら保存した鍵を消す。どちらでも`key_ref`と鍵の片方だけが
    /// 残る状態を作らない。
    pub fn add_provider(&self, new: NewProvider) -> Result<SettingsView> {
        let name = input::name(&new.name, "provider name", input::PROVIDER_NAME_MAX_CHARS)?;
        let base_url = new.base_url.trim().to_string();
        providers::validate_base_url(&base_url)?;
        // 鍵を保存する前にも確かめ、登録できないと分かっている名前のために資格情報ストアへ
        // 書かない。
        refuse_registered_provider_name(&self.current().config, &name)?;

        let key_ref = match new.api_key {
            Some(key) if !key.expose_secret().is_empty() => {
                providers::validate_api_key(&key)?;
                let key_ref = format!("provider:{}", ulid::Ulid::new());
                secrets::store(&key_ref, &key)?;
                Some(key_ref)
            }
            _ => None,
        };

        self.register_provider(ProviderConfig {
            id: ulid::Ulid::new().to_string(),
            name,
            api_format: new.api_format,
            base_url,
            models: Vec::new(),
            active_model: None,
            key_ref: key_ref.clone(),
        })
        .inspect_err(|_| {
            if let Some(key_ref) = &key_ref {
                delete_secret(key_ref, "provider API key");
            }
        })
    }

    /// 名前の重複確認から登録までを書き込みロックの中で行う。
    fn register_provider(&self, provider: ProviderConfig) -> Result<SettingsView> {
        let mut draft = self.edit();
        refuse_registered_provider_name(&draft.config, &provider.name)?;
        draft.config.providers.push(provider);
        draft.config.reselect_active_provider();
        draft.commit()
    }

    /// アクティブなプロバイダーを消したら、選択を移す([`Config::reselect_active_provider`])。
    /// 保存済みAPIキーも消す。鍵は設定の保存が済んでから消す(先に消すと、保存に失敗したときに設定が消えた鍵を
    /// 指して残る)。
    pub fn delete_provider(&self, provider_id: &str) -> Result<SettingsView> {
        let mut draft = self.edit();
        let config = &mut draft.config;
        let index = config
            .providers
            .iter()
            .position(|p| p.id == provider_id)
            .ok_or_else(|| provider_not_found(provider_id))?;
        let removed = config.providers.remove(index);
        config.reselect_active_provider();

        let view = draft.commit()?;
        self.detected.forget_provider(provider_id);
        if let Some(key_ref) = &removed.key_ref {
            delete_secret(key_ref, "provider API key");
        }
        Ok(view)
    }
}

fn refuse_registered_provider_name(config: &Config, name: &str) -> Result<()> {
    if config.providers.iter().any(|p| p.name == name) {
        return Err(invalid(format!("provider name already registered: {name}")));
    }
    Ok(())
}

pub(super) fn find_provider_mut<'a>(
    config: &'a mut Config,
    provider_id: &str,
) -> Result<&'a mut ProviderConfig> {
    config
        .providers
        .iter_mut()
        .find(|p| p.id == provider_id)
        .ok_or_else(|| provider_not_found(provider_id))
}

fn provider_not_found(provider_id: &str) -> CoreError {
    invalid(format!("provider not found: {provider_id}"))
}
