//! 設定画面(Issue #22)向けIPCコマンド。「一般」「APIプロバイダー」タブに対応する。
//! 「ツール/MCP」タブのコマンドは`commands::mcp`にある(MCPクライアントという新しい
//! 外部通信手段の導入を伴い、CLAUDE.mdの規定によりOpusレビューを要したためIssue #28として
//! 別に実装したが、`SettingsView`自体は1つの設定画面に対応する1つの構造として共有する)。
//!
//! フロントエンドには`key_ref`も平文APIキー・秘密情報も渡さない。プロバイダーに鍵が
//! 設定済みかどうかは`has_api_key`という真偽値だけで伝える(architecture.md 7節
//! 「フロントエンドは秘密情報を一切受け取らない」)。MCPサーバーの環境変数・ヘッダーも
//! 同様に名前だけを伝え、値・key_refは伝えない(Opusレビュー指摘)。

use std::sync::MutexGuard;

use secrecy::SecretString;
use serde::Serialize;
use tauri::State;

use scitl_core::config::{self, ApiFormat, Config, GeneralConfig, McpEndpoint, ProviderConfig};
use scitl_core::llm::providers::openai_compat::validate_base_url;
use scitl_core::secrets;

use crate::{build_active_adapter, AppState, Runtime};

#[derive(Debug, Serialize)]
pub struct ProviderView {
    pub id: String,
    pub name: String,
    pub api_format: ApiFormat,
    pub base_url: String,
    pub models: Vec<String>,
    pub active_model: Option<String>,
    pub has_api_key: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum McpEndpointView {
    Stdio {
        command: String,
        args: Vec<String>,
        env_names: Vec<String>,
    },
    StreamableHttp {
        url: String,
        header_names: Vec<String>,
    },
}

/// 設定画面へ渡すツール1件。引数スキーマは表示に使わないので渡さない
/// (表示に不要なサーバー由来のデータをWebViewへ出さない)。
#[derive(Debug, Serialize)]
pub struct McpToolView {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct McpServerView {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub endpoint: McpEndpointView,
    pub enabled_tools: Vec<String>,
    /// 取得済みのツール一覧(Issue #104)。`None`は「まだ取得していない」。
    /// 画面はこれを描くだけで、自前では保持しない。
    pub tools: Option<Vec<McpToolView>>,
}

#[derive(Debug, Serialize)]
pub struct SettingsView {
    pub general: GeneralConfig,
    pub providers: Vec<ProviderView>,
    pub active_provider_id: Option<String>,
    pub mcp_servers: Vec<McpServerView>,
}

fn to_mcp_view(
    s: &scitl_core::config::McpServerConfig,
    catalog: &scitl_core::mcp::ToolCatalog,
) -> McpServerView {
    let endpoint = match &s.endpoint {
        McpEndpoint::Stdio {
            command,
            args,
            env_refs,
        } => McpEndpointView::Stdio {
            command: command.clone(),
            args: args.clone(),
            env_names: env_refs.iter().map(|r| r.name.clone()).collect(),
        },
        McpEndpoint::StreamableHttp { url, header_refs } => McpEndpointView::StreamableHttp {
            url: url.clone(),
            header_names: header_refs.iter().map(|r| r.name.clone()).collect(),
        },
    };
    McpServerView {
        id: s.id.clone(),
        name: s.name.clone(),
        enabled: s.enabled,
        endpoint,
        enabled_tools: s.enabled_tools.iter().cloned().collect(),
        tools: catalog.get(&s.id).map(|tools| {
            tools
                .into_iter()
                .map(|t| McpToolView {
                    name: t.name,
                    description: t.description,
                })
                .collect()
        }),
    }
}

pub(crate) fn to_view(config: &Config, catalog: &scitl_core::mcp::ToolCatalog) -> SettingsView {
    SettingsView {
        general: config.general.clone(),
        providers: config
            .providers
            .iter()
            .map(|p| ProviderView {
                id: p.id.clone(),
                name: p.name.clone(),
                api_format: p.api_format,
                base_url: p.base_url.clone(),
                models: p.models.clone(),
                active_model: p.active_model.clone(),
                has_api_key: p.key_ref.is_some(),
            })
            .collect(),
        active_provider_id: config.active_provider_id.clone(),
        mcp_servers: config
            .mcp_servers
            .iter()
            .map(|s| to_mcp_view(s, catalog))
            .collect(),
    }
}

fn find_provider_mut<'a>(
    config: &'a mut Config,
    provider_id: &str,
) -> Result<&'a mut ProviderConfig, String> {
    config
        .providers
        .iter_mut()
        .find(|p| p.id == provider_id)
        .ok_or_else(|| format!("provider not found: {provider_id}"))
}

/// 設定変更後の共通の後始末: `config.toml`へ保存し、アクティブプロバイダーからアダプタを
/// 作り直して`Runtime`へ差し替える。設定変更の経路をここ1箇所に閉じることで、
/// 「保存し忘れ」「アダプタの再構築し忘れ」を構造的に防ぐ(principles.md 5節)。
pub(crate) fn persist_and_rebuild(
    config_path: &std::path::Path,
    mut runtime: MutexGuard<'_, Runtime>,
    catalog: &scitl_core::mcp::ToolCatalog,
) -> Result<SettingsView, String> {
    config::save(config_path, &runtime.config).map_err(|e| e.to_string())?;
    runtime.adapter = build_active_adapter(&runtime.config).map_err(|e| e.to_string())?;
    Ok(to_view(&runtime.config, catalog))
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> SettingsView {
    let runtime = state.runtime.lock().expect("runtime mutex poisoned");
    to_view(&runtime.config, &state.mcp_tools)
}

#[tauri::command]
pub fn update_general_settings(
    state: State<'_, AppState>,
    system_prompt: Option<String>,
    task_chat_system_prompt: Option<String>,
    response_timeout_secs: Option<u64>,
) -> Result<SettingsView, String> {
    let mut runtime = state.runtime.lock().expect("runtime mutex poisoned");
    runtime.config.general = GeneralConfig {
        system_prompt: system_prompt.filter(|s| !s.is_empty()),
        task_chat_system_prompt: task_chat_system_prompt.filter(|s| !s.is_empty()),
        response_timeout_secs,
    };
    persist_and_rebuild(&state.config_path, runtime, &state.mcp_tools)
}

#[tauri::command]
pub fn add_provider(
    state: State<'_, AppState>,
    name: String,
    api_format: ApiFormat,
    base_url: String,
    api_key: Option<String>,
) -> Result<SettingsView, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("provider name must not be empty".to_string());
    }
    validate_base_url(&base_url).map_err(|e| e.to_string())?;

    // 鍵の保存に失敗したらプロバイダー自体の登録も中断する(legacy/frontend.md 3節)。
    // config.tomlへ書く前に確定させ、`key_ref`だけが浮いた状態を作らない。
    let key_ref = match api_key.filter(|k| !k.is_empty()) {
        Some(key) => {
            let key_ref = format!("provider:{}", ulid::Ulid::new());
            secrets::store(&key_ref, &SecretString::from(key)).map_err(|e| e.to_string())?;
            Some(key_ref)
        }
        None => None,
    };

    let mut runtime = state.runtime.lock().expect("runtime mutex poisoned");
    let id = ulid::Ulid::new().to_string();
    let activate = runtime.config.active_provider_id.is_none();
    runtime.config.providers.push(ProviderConfig {
        id: id.clone(),
        name,
        api_format,
        base_url,
        models: Vec::new(),
        active_model: None,
        key_ref,
    });
    if activate {
        runtime.config.active_provider_id = Some(id);
    }

    persist_and_rebuild(&state.config_path, runtime, &state.mcp_tools)
}

#[tauri::command]
pub fn delete_provider(
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<SettingsView, String> {
    let mut runtime = state.runtime.lock().expect("runtime mutex poisoned");
    let index = runtime
        .config
        .providers
        .iter()
        .position(|p| p.id == provider_id)
        .ok_or_else(|| format!("provider not found: {provider_id}"))?;
    let removed = runtime.config.providers.remove(index);

    // 保存済みAPIキーも同時に削除する(legacy/frontend.md 3節「削除には確認ダイアログを
    // 挟み、保存済みAPIキーも同時に消える旨を警告する」)。削除確認自体はフロントエンド側の責務。
    if let Some(key_ref) = &removed.key_ref {
        if let Err(e) = secrets::delete(key_ref) {
            eprintln!("failed to delete provider API key from secret store: {e}");
        }
    }

    if runtime.config.active_provider_id.as_deref() == Some(provider_id.as_str()) {
        runtime.config.active_provider_id =
            runtime.config.providers.first().map(|p| p.id.clone());
    }

    persist_and_rebuild(&state.config_path, runtime, &state.mcp_tools)
}

#[tauri::command]
pub fn set_active_provider(
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<SettingsView, String> {
    let mut runtime = state.runtime.lock().expect("runtime mutex poisoned");
    if !runtime.config.providers.iter().any(|p| p.id == provider_id) {
        return Err(format!("provider not found: {provider_id}"));
    }
    runtime.config.active_provider_id = Some(provider_id);
    persist_and_rebuild(&state.config_path, runtime, &state.mcp_tools)
}

#[tauri::command]
pub fn add_model(
    state: State<'_, AppState>,
    provider_id: String,
    model: String,
) -> Result<SettingsView, String> {
    let model = model.trim().to_string();
    if model.is_empty() {
        return Err("model name must not be empty".to_string());
    }

    let mut runtime = state.runtime.lock().expect("runtime mutex poisoned");
    let provider = find_provider_mut(&mut runtime.config, &provider_id)?;
    if provider.models.iter().any(|m| m == &model) {
        return Err(format!("model already registered: {model}"));
    }
    provider.models.push(model.clone());
    if provider.active_model.is_none() {
        provider.active_model = Some(model);
    }

    persist_and_rebuild(&state.config_path, runtime, &state.mcp_tools)
}

#[tauri::command]
pub fn remove_model(
    state: State<'_, AppState>,
    provider_id: String,
    model: String,
) -> Result<SettingsView, String> {
    let mut runtime = state.runtime.lock().expect("runtime mutex poisoned");
    let provider = find_provider_mut(&mut runtime.config, &provider_id)?;
    provider.models.retain(|m| m != &model);
    if provider.active_model.as_deref() == Some(model.as_str()) {
        provider.active_model = provider.models.first().cloned();
    }

    persist_and_rebuild(&state.config_path, runtime, &state.mcp_tools)
}

#[tauri::command]
pub fn set_active_model(
    state: State<'_, AppState>,
    provider_id: String,
    model: String,
) -> Result<SettingsView, String> {
    let mut runtime = state.runtime.lock().expect("runtime mutex poisoned");
    let provider = find_provider_mut(&mut runtime.config, &provider_id)?;
    if !provider.models.iter().any(|m| m == &model) {
        return Err(format!("model not registered: {model}"));
    }
    provider.active_model = Some(model);

    persist_and_rebuild(&state.config_path, runtime, &state.mcp_tools)
}
