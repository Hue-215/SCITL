//! モデルの登録・選択と、能力(自動検出・手動設定)の扱い。

use super::provider_settings::find_provider_mut;
use super::{input, invalid, view, AvailableModel, ChatModelsView, Settings, SettingsView};
use crate::config::{
    Capability, Config, ModelConfig, ModelOverrides, ProviderConfig, ReasoningEffort,
};
use crate::error::{CoreError, Result};
use crate::llm::providers;
use crate::llm::{self, ModelCapabilities};

impl Settings {
    /// チャット入力欄の下のモデル選択。思考の強さを選べるかは能力で決まるので、
    /// ターンの開始と同じく、アクティブなモデルを先に問い合わせる。失敗したら自動検出より
    /// 下の層の値で出す。
    pub async fn chat_models(&self) -> ChatModelsView {
        self.detect_active_model_once().await;
        view::chat_models(&self.current().config, &self.detected)
    }

    /// アクティブなモデルの能力を、まだ推論サーバーに問い合わせていなければ問い合わせる
    /// (アプリ起動後、モデルごとに最初の1回)。失敗は覚えないので、次の機会に問い合わせ直す。
    ///
    /// 鍵を読めていない間は問い合わせない(問い合わせのたびに資格情報ストアを読みに行かない。
    /// 読み直すのはターンの開始・タスクの追加と設定の変更だけ)。
    pub(super) async fn detect_active_model_once(&self) {
        let current = self.current();
        if current.adapter.key_error().is_some() {
            return;
        }
        let config = current.config;
        let target = config.active_model().and_then(|(p, model)| {
            (providers::can_detect_capabilities(p)
                && self.detected.get(&p.id, &model.name).is_none())
            .then(|| (p.clone(), model.name.clone()))
        });
        if let Some((provider, model)) = target {
            if let Err(e) = self.detect(&provider, vec![model]).await {
                crate::diagnostics::report(format_args!(
                    "failed to detect model capabilities from '{}': {e}",
                    provider.name
                ));
            }
        }
    }

    /// プロバイダーの全モデルの能力を推論サーバーに問い合わせ直す(設定画面)。
    pub async fn detect_model_capabilities(&self, provider_id: &str) -> Result<SettingsView> {
        let _in_flight = self
            .detecting
            .try_begin(provider_id.to_string())
            .ok_or_else(|| invalid("already detecting capabilities for this provider"))?;
        let provider = self.provider(provider_id)?;
        if !providers::can_detect_capabilities(&provider) {
            return Err(invalid(
                "capabilities can be detected only from servers on this machine or the local network",
            ));
        }
        let models = provider.models.iter().map(|m| m.name.clone()).collect();
        self.detect(&provider, models).await?;
        Ok(self.view())
    }

    /// プロバイダーが提供するモデル名を問い合わせる。登録済みのものも含めて名前順に返し、
    /// 設定には書かない(登録は利用者が選んで[`Self::add_models`]で行う)。
    pub async fn list_provider_models(&self, provider_id: &str) -> Result<Vec<AvailableModel>> {
        let provider = self.provider(provider_id)?;
        Ok(view::available_models(
            providers::list_models(&provider).await?,
        ))
    }

    /// 問い合わせた全モデルの結果を置き換える。サーバーが答えなかったモデルも「検出した
    /// 項目なし」として覚え、ターンのたびに問い合わせ直さない。問い合わせに失敗したら
    /// 何も置き換えない(一時的な失敗で、取れていた結果を空にしない)。
    async fn detect(&self, provider: &ProviderConfig, models: Vec<String>) -> Result<()> {
        let mut found = providers::detect_capabilities(provider, &models)
            .await?
            .unwrap_or_default();
        for model in models {
            let detected = found.remove(&model).unwrap_or_default();
            self.detected.store(&provider.id, &model, detected);
        }
        Ok(())
    }

    /// アクティブなモデルが無ければ既定値を返す(その場合ターンはアダプタの段階で
    /// 失敗するので、この値は使われない)。
    pub(super) fn active_model_capabilities(&self, config: &Config) -> ModelCapabilities {
        config
            .active_model()
            .map(|(p, model)| {
                llm::resolve_capabilities(model, self.detected.get(&p.id, &model.name).as_ref())
            })
            .unwrap_or(llm::DEFAULT_CAPABILITIES)
    }

    /// 手動設定より下の層(自動検出 → 既定値)で決まる値。
    fn fallback_capabilities(&self, provider_id: &str, model: &str) -> ModelCapabilities {
        llm::fallback_capabilities(self.detected.get(provider_id, model).as_ref())
    }

    /// 手動追加(1件)と、取得した一覧から選んだ分(複数件)の両方が通る。1件でも登録できない
    /// 名前があれば何も登録しない。モデルが無かったプロバイダーでは、最初の1件を
    /// アクティブにする。アクティブなプロバイダーにモデルが無ければ、選択をこのプロバイダーへ
    /// 移す([`Config::reselect_active_provider`])。
    pub fn add_models<S: AsRef<str>>(
        &self,
        provider_id: &str,
        models: &[S],
    ) -> Result<SettingsView> {
        let mut draft = self.edit();
        let provider = find_provider_mut(&mut draft.config, provider_id)?;
        for model in models {
            let model = input::name(model.as_ref(), "model name", input::MODEL_NAME_MAX_CHARS)?;
            if provider.model(&model).is_some() {
                return Err(invalid(format!("model already registered: {model}")));
            }
            provider.models.push(ModelConfig::new(model));
        }
        if provider.active_model.is_none() {
            provider.active_model = provider.models.first().map(|m| m.name.clone());
        }
        draft.config.reselect_active_provider();
        draft.commit()
    }

    /// アクティブなモデルを消したら先頭をアクティブにする。プロバイダーの最後のモデルを
    /// 消したら、選択を移す([`Config::reselect_active_provider`])。
    pub fn remove_model(&self, provider_id: &str, model: &str) -> Result<SettingsView> {
        let mut draft = self.edit();
        let provider = find_provider_mut(&mut draft.config, provider_id)?;
        provider.models.retain(|m| m.name != model);
        if provider.active_model.as_deref() == Some(model) {
            provider.active_model = provider.models.first().map(|m| m.name.clone());
        }
        draft.config.reselect_active_provider();
        let view = draft.commit()?;
        self.detected.forget(provider_id, model);
        Ok(view)
    }

    /// チャット入力欄の下で選んだモデルに切り替える。一覧はプロバイダーを跨ぐので、
    /// アクティブなプロバイダーとそのモデルを1回の保存で切り替える。2回に分けると、間で
    /// 落ちたときに意図しない組が残る。
    pub fn select_chat_model(&self, provider_id: &str, model: &str) -> Result<()> {
        let mut draft = self.edit();
        let provider = find_provider_mut(&mut draft.config, provider_id)?;
        if provider.model(model).is_none() {
            return Err(model_not_found(model));
        }
        provider.active_model = Some(model.to_string());
        draft.config.active_provider_id = Some(provider_id.to_string());
        draft.commit().map(drop)
    }

    /// 思考に対応しないモデルにも保存はできる(送るときに外す。[`Snapshot::turn_context`])。
    /// 能力は手動設定で後から変わりうるため、選んだ値は捨てずに残す。
    pub fn set_reasoning_effort(
        &self,
        provider_id: &str,
        model: &str,
        effort: ReasoningEffort,
    ) -> Result<()> {
        let mut draft = self.edit();
        find_model_mut(&mut draft.config, provider_id, model)?.reasoning_effort = effort;
        draft.commit().map(drop)
    }

    /// チャットのモデル一覧に出すかどうか。
    pub fn set_model_visible(
        &self,
        provider_id: &str,
        model: &str,
        visible: bool,
    ) -> Result<SettingsView> {
        let mut draft = self.edit();
        find_model_mut(&mut draft.config, provider_id, model)?.visible = visible;
        draft.commit()
    }

    /// 手動設定より下の層と同じ値にしたら、手動設定を外す。
    pub fn set_model_capability(
        &self,
        provider_id: &str,
        model: &str,
        capability: Capability,
        supported: bool,
    ) -> Result<SettingsView> {
        let fallback = self
            .fallback_capabilities(provider_id, model)
            .flag(capability);
        let mut draft = self.edit();
        let entry = find_model_mut(&mut draft.config, provider_id, model)?;
        *entry.overrides.flag_mut(capability) = (supported != fallback).then_some(supported);
        draft.commit()
    }

    /// `None`(空欄)は手動設定を外す。下の層と同じ値の扱いは[`Self::set_model_capability`]と同じ。
    pub fn set_model_context_length(
        &self,
        provider_id: &str,
        model: &str,
        context_length: Option<u32>,
    ) -> Result<SettingsView> {
        if context_length == Some(0) {
            return Err(invalid("context length must be 1 or greater"));
        }
        let fallback = self
            .fallback_capabilities(provider_id, model)
            .context_length;
        let mut draft = self.edit();
        let entry = find_model_mut(&mut draft.config, provider_id, model)?;
        entry.overrides.context_length = context_length.filter(|n| *n != fallback);
        draft.commit()
    }

    /// 能力の手動設定(コンテキスト長を含む)をすべて外す。表示/非表示は能力ではないので残す。
    pub fn reset_model_capabilities(&self, provider_id: &str, model: &str) -> Result<SettingsView> {
        let mut draft = self.edit();
        find_model_mut(&mut draft.config, provider_id, model)?.overrides =
            ModelOverrides::default();
        draft.commit()
    }
}

fn find_model_mut<'a>(
    config: &'a mut Config,
    provider_id: &str,
    model: &str,
) -> Result<&'a mut ModelConfig> {
    find_provider_mut(config, provider_id)?
        .models
        .iter_mut()
        .find(|m| m.name == model)
        .ok_or_else(|| model_not_found(model))
}

fn model_not_found(model: &str) -> CoreError {
    invalid(format!("model not registered: {model}"))
}
