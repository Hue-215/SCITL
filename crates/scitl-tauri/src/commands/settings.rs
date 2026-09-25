//! 設定画面の「一般」「APIプロバイダー」タブ向けIPCコマンド。規則と秘密情報の扱いは
//! `scitl_core::settings`にあり、ここはそれを1つ呼ぶだけ。

use secrecy::SecretString;
use tauri::State;

use scitl_core::config::{ApiFormat, Capability};
use scitl_core::settings::{NewProvider, SettingsView};

use super::with_settings;
use crate::AppState;

/// ロックを一瞬取るだけでI/Oを伴わないため、同期コマンドのままにする。
#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> SettingsView {
    state.settings.view()
}

#[tauri::command]
pub async fn update_general_settings(
    state: State<'_, AppState>,
    system_prompt: Option<String>,
    task_chat_system_prompt: Option<String>,
    response_timeout_secs: Option<u64>,
) -> Result<SettingsView, String> {
    with_settings(&state, move |s| {
        s.update_general(
            system_prompt,
            task_chat_system_prompt,
            response_timeout_secs,
        )
    })
    .await
}

#[tauri::command]
pub async fn update_tool_settings(
    state: State<'_, AppState>,
    max_rounds_per_turn: Option<u32>,
    total_timeout_secs: Option<u64>,
) -> Result<SettingsView, String> {
    with_settings(&state, move |s| {
        s.update_tools(max_rounds_per_turn, total_timeout_secs)
    })
    .await
}

#[tauri::command]
pub async fn add_provider(
    state: State<'_, AppState>,
    name: String,
    api_format: ApiFormat,
    base_url: String,
    api_key: Option<String>,
) -> Result<SettingsView, String> {
    let new = NewProvider {
        name,
        api_format,
        base_url,
        api_key: api_key.map(SecretString::from),
    };
    with_settings(&state, move |s| s.add_provider(new)).await
}

#[tauri::command]
pub async fn delete_provider(
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<SettingsView, String> {
    with_settings(&state, move |s| s.delete_provider(&provider_id)).await
}

#[tauri::command]
pub async fn set_active_provider(
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<SettingsView, String> {
    with_settings(&state, move |s| s.set_active_provider(&provider_id)).await
}

#[tauri::command]
pub async fn add_model(
    state: State<'_, AppState>,
    provider_id: String,
    model: String,
) -> Result<SettingsView, String> {
    with_settings(&state, move |s| s.add_model(&provider_id, &model)).await
}

#[tauri::command]
pub async fn remove_model(
    state: State<'_, AppState>,
    provider_id: String,
    model: String,
) -> Result<SettingsView, String> {
    with_settings(&state, move |s| s.remove_model(&provider_id, &model)).await
}

#[tauri::command]
pub async fn set_active_model(
    state: State<'_, AppState>,
    provider_id: String,
    model: String,
) -> Result<SettingsView, String> {
    with_settings(&state, move |s| s.set_active_model(&provider_id, &model)).await
}

#[tauri::command]
pub async fn set_model_visible(
    state: State<'_, AppState>,
    provider_id: String,
    model: String,
    visible: bool,
) -> Result<SettingsView, String> {
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
) -> Result<SettingsView, String> {
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
    context_length: Option<u32>,
) -> Result<SettingsView, String> {
    with_settings(&state, move |s| {
        s.set_model_context_length(&provider_id, &model, context_length)
    })
    .await
}

#[tauri::command]
pub async fn reset_model_capabilities(
    state: State<'_, AppState>,
    provider_id: String,
    model: String,
) -> Result<SettingsView, String> {
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
) -> Result<SettingsView, String> {
    state
        .settings
        .detect_model_capabilities(&provider_id)
        .await
        .map_err(|e| e.to_string())
}
