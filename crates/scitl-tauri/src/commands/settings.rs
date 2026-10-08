//! 設定画面の「一般」「APIプロバイダー」タブ向けIPCコマンド。規則と秘密情報の扱いは
//! `scitl_core::settings`にあり、ここはそれを1つ呼ぶだけ。

use secrecy::SecretString;
use tauri::State;

use scitl_core::config::{ApiFormat, Capability, ReasoningEffort};
use scitl_core::i18n::Language;
use scitl_core::llm::providers::{self, BaseUrlHint};
use scitl_core::settings::{
    AvailableModel, ChatModelsView, FormOutcome, GeneralUpdate, HeaderInput, NewProvider,
    SettingsView,
};

use super::{with_settings, CommandResult};
use crate::AppState;

/// ロックを一瞬取るだけでI/Oを伴わないため、同期コマンドのままにする。
#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> SettingsView {
    state.settings.view()
}

/// 数値の欄は文字列のまま受け取る(解釈はcore。`None`は保存済みの値のまま)。欄の誤りは
/// `FormOutcome::Rejected`で返す。
#[tauri::command]
pub async fn update_general_settings(
    state: State<'_, AppState>,
    system_prompt: Option<String>,
    task_chat_system_prompt: Option<String>,
    task_opening_message: Option<String>,
    response_timeout_secs: Option<String>,
) -> CommandResult<FormOutcome<SettingsView>> {
    let update = GeneralUpdate {
        system_prompt,
        task_chat_system_prompt,
        task_opening_message,
        response_timeout_secs,
    };
    with_settings(&state, move |s| {
        FormOutcome::from_result(s.update_general(update))
    })
    .await
}

/// 画面が起動時に1度だけ読む表示言語。`get_settings`と同じく、I/Oを伴わないので同期のまま。
#[tauri::command]
pub fn get_display_language(state: State<'_, AppState>) -> Language {
    state.settings.display_language()
}

#[tauri::command]
pub async fn update_language(
    state: State<'_, AppState>,
    language: Language,
) -> CommandResult<SettingsView> {
    with_settings(&state, move |s| s.update_language(language)).await
}

/// 数値の欄は文字列のまま受け取る(`update_general_settings`と同じ)。
#[tauri::command]
pub async fn update_tool_settings(
    state: State<'_, AppState>,
    max_rounds_per_turn: Option<String>,
    total_timeout_secs: Option<String>,
) -> CommandResult<FormOutcome<SettingsView>> {
    with_settings(&state, move |s| {
        FormOutcome::from_result(s.update_tools(
            max_rounds_per_turn.as_deref(),
            total_timeout_secs.as_deref(),
        ))
    })
    .await
}

/// ヘッダーの欄は文字列のまま、秘密情報として受け取る(分けるのはcore)。
#[tauri::command]
pub async fn add_provider(
    state: State<'_, AppState>,
    name: String,
    api_format: ApiFormat,
    base_url: String,
    api_key: Option<SecretString>,
    headers: HeaderInput,
) -> CommandResult<FormOutcome<SettingsView>> {
    let new = NewProvider {
        name,
        api_format,
        base_url,
        api_key,
        headers,
    };
    with_settings(&state, move |s| {
        FormOutcome::from_result(s.add_provider(new))
    })
    .await
}

/// 登録フォームで入力中のベースURLへのヒント。判定はアダプタの知識なのでcoreが持つ
/// (`llm::providers::base_url_hint`)。I/Oを伴わないので同期のまま。
#[tauri::command]
pub fn get_base_url_hint(api_format: ApiFormat, base_url: String) -> Option<BaseUrlHint> {
    providers::base_url_hint(api_format, &base_url)
}

#[tauri::command]
pub async fn delete_provider(
    state: State<'_, AppState>,
    provider_id: String,
) -> CommandResult<SettingsView> {
    with_settings(&state, move |s| s.delete_provider(&provider_id)).await
}

/// 登録したら、検出できるプロバイダーなら続けて能力を検出する。推論サーバーへの問い合わせを
/// 待つので`with_settings`を通さない(登録の保存は`Settings::add_models`の中で逃がす)。
#[tauri::command]
pub async fn add_models(
    state: State<'_, AppState>,
    provider_id: String,
    models: Vec<String>,
) -> CommandResult<SettingsView> {
    Ok(state.settings.add_models(&provider_id, models).await?)
}

#[tauri::command]
pub async fn remove_model(
    state: State<'_, AppState>,
    provider_id: String,
    model: String,
) -> CommandResult<SettingsView> {
    with_settings(&state, move |s| s.remove_model(&provider_id, &model)).await
}

#[tauri::command]
pub async fn set_model_visible(
    state: State<'_, AppState>,
    provider_id: String,
    model: String,
    visible: bool,
) -> CommandResult<SettingsView> {
    with_settings(&state, move |s| {
        s.set_model_visible(&provider_id, &model, visible)
    })
    .await
}

#[tauri::command]
pub async fn set_model_capability(
    state: State<'_, AppState>,
    provider_id: String,
    model: String,
    capability: Capability,
    supported: bool,
) -> CommandResult<SettingsView> {
    with_settings(&state, move |s| {
        s.set_model_capability(&provider_id, &model, capability, supported)
    })
    .await
}

#[tauri::command]
pub async fn set_model_context_length(
    state: State<'_, AppState>,
    provider_id: String,
    model: String,
    context_length: String,
) -> CommandResult<FormOutcome<SettingsView>> {
    with_settings(&state, move |s| {
        FormOutcome::from_result(s.set_model_context_length(&provider_id, &model, &context_length))
    })
    .await
}

#[tauri::command]
pub async fn reset_model_capabilities(
    state: State<'_, AppState>,
    provider_id: String,
    model: String,
) -> CommandResult<SettingsView> {
    with_settings(&state, move |s| {
        s.reset_model_capabilities(&provider_id, &model)
    })
    .await
}

/// 推論サーバーへの問い合わせは非同期で、`fetch_mcp_tools`と同じく`with_settings`を通さない。
#[tauri::command]
pub async fn detect_model_capabilities(
    state: State<'_, AppState>,
    provider_id: String,
) -> CommandResult<SettingsView> {
    Ok(state
        .settings
        .detect_model_capabilities(&provider_id)
        .await?)
}

/// プロバイダーへの問い合わせは非同期で、`detect_model_capabilities`と同じく
/// `with_settings`を通さない。
#[tauri::command]
pub async fn list_provider_models(
    state: State<'_, AppState>,
    provider_id: String,
) -> CommandResult<Vec<AvailableModel>> {
    Ok(state.settings.list_provider_models(&provider_id).await?)
}

/// チャット入力欄の下のモデル選択。アクティブなモデルを推論サーバーに
/// 問い合わせることがあるため非同期で、`with_settings`を通さない。
#[tauri::command]
pub async fn get_chat_models(state: State<'_, AppState>) -> CommandResult<ChatModelsView> {
    Ok(state.settings.chat_models().await)
}

/// 選び直した後の一覧は`get_chat_models`で引き直す(問い合わせを伴うため、保存とは分ける)。
#[tauri::command]
pub async fn select_chat_model(
    state: State<'_, AppState>,
    provider_id: String,
    model: String,
) -> CommandResult<()> {
    with_settings(&state, move |s| s.select_chat_model(&provider_id, &model)).await
}

#[tauri::command]
pub async fn set_reasoning_effort(
    state: State<'_, AppState>,
    provider_id: String,
    model: String,
    effort: ReasoningEffort,
) -> CommandResult<()> {
    with_settings(&state, move |s| {
        s.set_reasoning_effort(&provider_id, &model, effort)
    })
    .await
}
