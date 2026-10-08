//! 設定画面の「ツール/MCP」タブ向けIPCコマンド。登録の規則と秘密情報の扱いは
//! `scitl_core::settings`にあり、ここはそれを1つ呼ぶだけ。ターン中のツール呼び出しは
//! `orchestration::turn`側にあり、ここで取得した一覧のキャッシュを共有する。

use tauri::State;

use scitl_core::settings::{FormOutcome, McpServerAdded, NewMcpEndpoint, SettingsView};

use super::{with_settings, CommandResult};
use crate::AppState;

/// 登録したら続けてツール一覧を取得する。接続を待つので`with_settings`を通さない
/// (登録の保存は`Settings::add_mcp_server`の中で`blocking::run`に逃がす)。識別子・URL・
/// ヘッダーの欄の誤りは`FormOutcome::Rejected`で返す。
#[tauri::command]
pub async fn add_mcp_server(
    state: State<'_, AppState>,
    name: String,
    endpoint: NewMcpEndpoint,
) -> CommandResult<FormOutcome<McpServerAdded>> {
    Ok(FormOutcome::from_result(
        state.settings.add_mcp_server(name, endpoint).await,
    )?)
}

#[tauri::command]
pub async fn delete_mcp_server(
    state: State<'_, AppState>,
    server_id: String,
) -> CommandResult<SettingsView> {
    with_settings(&state, move |s| s.delete_mcp_server(&server_id)).await
}

#[tauri::command]
pub async fn set_mcp_server_enabled(
    state: State<'_, AppState>,
    server_id: String,
    enabled: bool,
) -> CommandResult<SettingsView> {
    with_settings(&state, move |s| {
        s.set_mcp_server_enabled(&server_id, enabled)
    })
    .await
}

#[tauri::command]
pub async fn set_mcp_tool_enabled(
    state: State<'_, AppState>,
    server_id: String,
    tool_name: String,
    enabled: bool,
) -> CommandResult<SettingsView> {
    with_settings(&state, move |s| {
        s.set_mcp_tool_enabled(&server_id, &tool_name, enabled)
    })
    .await
}

/// 接続の待ちは非同期で、ブロッキングするI/Oを含まないため`with_settings`を通さない。
#[tauri::command]
pub async fn fetch_mcp_tools(
    state: State<'_, AppState>,
    server_id: String,
) -> CommandResult<SettingsView> {
    Ok(state.settings.fetch_mcp_tools(&server_id).await?)
}
