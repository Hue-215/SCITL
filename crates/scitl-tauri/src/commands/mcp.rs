//! 設定「ツール/MCP」タブ向けIPCコマンド(Issue #28)。
//!
//! このコマンド群が担うのは「登録・秘密情報の保存・接続してツール一覧を取得」まで。
//! 実際のターン中のツール呼び出しは`orchestration::turn`側にあり(Issue #44)、
//! ここで取得した一覧のキャッシュ(`AppState::mcp_tools`)を共有する。

use secrecy::SecretString;
use serde::Deserialize;
use tauri::State;

use scitl_core::config::{validate_mcp_server_name, McpEndpoint, McpServerConfig, SecretRef};
use scitl_core::mcp;
use scitl_core::secrets;

use crate::commands::settings::{persist_and_rebuild, SettingsView};
use crate::AppState;

/// サーバー追加フォームからの入力。接続方式ごとに必要な値だけを受け取る
/// (`McpEndpoint`と同じタグ付きenumにすることで、フロントエンドが送る形と
/// Rust側の型を対応させる)。
#[derive(Debug, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum NewMcpEndpoint {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: Vec<(String, String)>,
    },
    StreamableHttp {
        url: String,
        #[serde(default)]
        headers: Vec<(String, String)>,
    },
}

/// 秘密情報の値を`secrets.rs`へ保存し、`(name, key_ref)`の組に変換する。途中で失敗したら
/// それまでに保存した分を削除してからエラーを返す(孤児を残さない)。
fn store_secret_refs(pairs: Vec<(String, String)>) -> Result<Vec<SecretRef>, String> {
    let mut refs = Vec::with_capacity(pairs.len());
    for (name, value) in pairs {
        let key_ref = format!("mcp:{}", ulid::Ulid::new());
        match secrets::store(&key_ref, &SecretString::from(value)) {
            Ok(()) => refs.push(SecretRef { name, key_ref }),
            Err(e) => {
                delete_secret_refs(&refs);
                return Err(e.to_string());
            }
        }
    }
    Ok(refs)
}

/// `refs`が指す秘密情報をすべて削除する。1件が失敗しても残りは試す(複数件あり得るため、
/// `delete_provider`のような早期returnはしない)。
fn delete_secret_refs(refs: &[SecretRef]) {
    for r in refs {
        if let Err(e) = secrets::delete(&r.key_ref) {
            eprintln!(
                "failed to delete MCP secret '{}' from secret store: {e}",
                r.name
            );
        }
    }
}

fn endpoint_secret_refs(endpoint: &McpEndpoint) -> &[SecretRef] {
    match endpoint {
        McpEndpoint::Stdio { env_refs, .. } => env_refs,
        McpEndpoint::StreamableHttp { header_refs, .. } => header_refs,
    }
}

#[tauri::command]
pub fn add_mcp_server(
    state: State<'_, AppState>,
    name: String,
    endpoint: NewMcpEndpoint,
) -> Result<SettingsView, String> {
    let name = name.trim().to_string();
    validate_mcp_server_name(&name).map_err(|e| e.to_string())?;

    // 重複チェックはkeyringへのI/Oより先に行う(安価なチェックを先に。ここで弾ければ
    // 秘密情報を1件も保存せずに済む)。ただしロックを持ったままI/Oを跨がせないため、
    // 保存後にもう一度同じチェックをやり直す(重複登録で孤児を残さない)。
    {
        let runtime = state.runtime.lock().expect("runtime mutex poisoned");
        if runtime.config.mcp_servers.iter().any(|s| s.name == name) {
            return Err(format!("MCP server name already registered: {name}"));
        }
    }

    let endpoint = match endpoint {
        NewMcpEndpoint::Stdio { command, args, env } => {
            let command = command.trim().to_string();
            if command.is_empty() {
                return Err("command must not be empty".to_string());
            }
            McpEndpoint::Stdio {
                command,
                args,
                env_refs: store_secret_refs(env)?,
            }
        }
        NewMcpEndpoint::StreamableHttp { url, headers } => {
            mcp::validate_streamable_http_url(&url).map_err(|e| e.to_string())?;
            for (name, value) in &headers {
                mcp::validate_header_name(name).map_err(|e| e.to_string())?;
                mcp::validate_header_value(value).map_err(|e| e.to_string())?;
            }
            McpEndpoint::StreamableHttp {
                url,
                header_refs: store_secret_refs(headers)?,
            }
        }
    };

    let mut runtime = state.runtime.lock().expect("runtime mutex poisoned");
    if runtime.config.mcp_servers.iter().any(|s| s.name == name) {
        // 秘密情報の保存中に別の呼び出しが同名で登録を終えた場合(通常のUI操作では
        // まず起きないが、保証として)。保存済みの分を削除してから中断する。
        delete_secret_refs(endpoint_secret_refs(&endpoint));
        return Err(format!("MCP server name already registered: {name}"));
    }

    runtime.config.mcp_servers.push(McpServerConfig {
        id: ulid::Ulid::new().to_string(),
        name,
        enabled: true,
        endpoint,
        enabled_tools: Default::default(),
    });

    persist_and_rebuild(&state.config_path, runtime, &state.mcp_tools)
}

#[tauri::command]
pub fn delete_mcp_server(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<SettingsView, String> {
    let mut runtime = state.runtime.lock().expect("runtime mutex poisoned");
    let index = runtime
        .config
        .mcp_servers
        .iter()
        .position(|s| s.id == server_id)
        .ok_or_else(|| format!("MCP server not found: {server_id}"))?;
    let removed = runtime.config.mcp_servers.remove(index);

    // 保存済みの秘密情報も同時に削除する(legacy/frontend.md 4節「削除には確認ダイアログを
    // 挟み、保存済みの秘密情報も消える旨を警告する」)。
    delete_secret_refs(endpoint_secret_refs(&removed.endpoint));
    // 取得済みツール一覧のキャッシュも捨てる(同じIDのサーバーを登録し直したときに、
    // 前のサーバーの一覧が残っていてはならない。Issue #104)。
    state.mcp_tools.forget(&removed.id);

    persist_and_rebuild(&state.config_path, runtime, &state.mcp_tools)
}

#[tauri::command]
pub fn set_mcp_server_enabled(
    state: State<'_, AppState>,
    server_id: String,
    enabled: bool,
) -> Result<SettingsView, String> {
    let mut runtime = state.runtime.lock().expect("runtime mutex poisoned");
    let server = runtime
        .config
        .mcp_servers
        .iter_mut()
        .find(|s| s.id == server_id)
        .ok_or_else(|| format!("MCP server not found: {server_id}"))?;
    server.enabled = enabled;

    persist_and_rebuild(&state.config_path, runtime, &state.mcp_tools)
}

#[tauri::command]
pub fn set_mcp_tool_enabled(
    state: State<'_, AppState>,
    server_id: String,
    tool_name: String,
    enabled: bool,
) -> Result<SettingsView, String> {
    let mut runtime = state.runtime.lock().expect("runtime mutex poisoned");
    let server = runtime
        .config
        .mcp_servers
        .iter_mut()
        .find(|s| s.id == server_id)
        .ok_or_else(|| format!("MCP server not found: {server_id}"))?;
    if enabled {
        server.enabled_tools.insert(tool_name);
    } else {
        server.enabled_tools.remove(&tool_name);
    }

    persist_and_rebuild(&state.config_path, runtime, &state.mcp_tools)
}

/// サーバーに接続してツール一覧を取得し、キャッシュへ載せて設定画面の状態ごと返す
/// (Issue #104)。config.tomlには書き込まない(`scitl_core::mcp`のドキュメント参照)。
///
/// ロックはサーバー設定を複製するまでだけ持ち、接続の`.await`をまたがせない
/// (`commands::chat::send_task_chat_message`と同じ規律。main.rsの`AppState`ドキュメント参照)。
/// サーバーごとに同時実行を1本に絞るガードを設ける(UIのボタン無効化は連打防止で
/// あって保証ではないため)。
#[tauri::command]
pub async fn fetch_mcp_tools(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<SettingsView, String> {
    {
        let mut in_flight = state.mcp_fetch_in_flight.lock().expect("mutex poisoned");
        if !in_flight.insert(server_id.clone()) {
            return Err("already fetching tools for this server".to_string());
        }
    }

    let server = {
        let runtime = state.runtime.lock().expect("runtime mutex poisoned");
        runtime
            .config
            .mcp_servers
            .iter()
            .find(|s| s.id == server_id)
            .cloned()
    };

    let result = match server {
        Some(server) => mcp::list_tools(&server).await.map_err(|e| e.to_string()),
        None => Err(format!("MCP server not found: {server_id}")),
    };

    state
        .mcp_fetch_in_flight
        .lock()
        .expect("mutex poisoned")
        .remove(&server_id);

    let tools = result?;
    state.mcp_tools.store(&server_id, tools);

    let runtime = state.runtime.lock().expect("runtime mutex poisoned");
    Ok(crate::commands::settings::to_view(
        &runtime.config,
        &state.mcp_tools,
    ))
}
