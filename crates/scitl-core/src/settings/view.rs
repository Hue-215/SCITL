//! 設定画面・CLIへ見せる設定の形。
//!
//! `key_ref`も平文の秘密情報も含めない。プロバイダーに鍵が設定済みかどうかは
//! `has_api_key`という真偽値だけで伝え、MCPサーバーの環境変数・ヘッダーも名前だけを伝える
//! (architecture.md 7節「フロントエンドは秘密情報を一切受け取らない」)。

use serde::Serialize;

use crate::config::{
    ApiFormat, Config, McpEndpoint, McpServerConfig, DEFAULT_RESPONSE_TIMEOUT_SECS,
};
use crate::mcp::ToolCatalog;
use crate::orchestration::{DEFAULT_MAX_ROUNDS_PER_TURN, DEFAULT_TOTAL_TIMEOUT_SECS};

#[derive(Debug, Serialize)]
pub struct ProviderView {
    pub id: String,
    pub name: String,
    pub api_format: ApiFormat,
    pub base_url: String,
    pub models: Vec<String>,
    pub active_model: Option<String>,
    pub has_api_key: bool,
    /// このプロバイダーをアクティブにしているが、組み立てられない理由(Issue #155)。
    pub error: Option<String>,
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

/// 一般設定。応答タイムアウトは`ToolSettingsView`と同じく、設定値(未設定は`None`)と
/// 未設定時に実際に使われる既定値の両方を渡す(既定値をTS側に書き写さない理由も同じ)。
#[derive(Debug, Serialize)]
pub struct GeneralSettingsView {
    pub system_prompt: Option<String>,
    pub task_chat_system_prompt: Option<String>,
    pub response_timeout_secs: Option<u64>,
    pub default_response_timeout_secs: u64,
}

/// ツール呼び出しの上限(Issue #71)。設定値そのもの(未設定は`None`)に加え、未設定時に
/// 実際に使われる既定値も渡す。画面はプレースホルダにこれを出すだけで、既定値を
/// TS側に書き写さない(2箇所に持つと必ずどちらかが古くなる)。
#[derive(Debug, Serialize)]
pub struct ToolSettingsView {
    pub max_rounds_per_turn: Option<u32>,
    pub total_timeout_secs: Option<u64>,
    pub default_max_rounds_per_turn: u32,
    pub default_total_timeout_secs: u64,
}

#[derive(Debug, Serialize)]
pub struct SettingsView {
    /// 起動時に設定ファイルを読めなかった理由(Issue #155)。あれば設定は保存されない。
    pub config_error: Option<String>,
    pub general: GeneralSettingsView,
    pub tools: ToolSettingsView,
    pub providers: Vec<ProviderView>,
    pub active_provider_id: Option<String>,
    pub mcp_servers: Vec<McpServerView>,
}

/// 設定の問題(`settings`モジュール冒頭)。
pub(super) struct Problems<'a> {
    pub config_error: Option<&'a str>,
    pub active_provider_error: Option<&'a str>,
}

pub(super) fn build(
    config: &Config,
    catalog: &ToolCatalog,
    problems: Problems<'_>,
) -> SettingsView {
    SettingsView {
        config_error: problems.config_error.map(str::to_string),
        general: GeneralSettingsView {
            system_prompt: config.general.system_prompt.clone(),
            task_chat_system_prompt: config.general.task_chat_system_prompt.clone(),
            response_timeout_secs: config.general.response_timeout_secs,
            default_response_timeout_secs: DEFAULT_RESPONSE_TIMEOUT_SECS,
        },
        tools: ToolSettingsView {
            max_rounds_per_turn: config.tools.max_rounds_per_turn,
            total_timeout_secs: config.tools.total_timeout_secs,
            default_max_rounds_per_turn: DEFAULT_MAX_ROUNDS_PER_TURN,
            default_total_timeout_secs: DEFAULT_TOTAL_TIMEOUT_SECS,
        },
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
                error: problems
                    .active_provider_error
                    .filter(|_| config.active_provider_id.as_deref() == Some(p.id.as_str()))
                    .map(str::to_string),
            })
            .collect(),
        active_provider_id: config.active_provider_id.clone(),
        mcp_servers: config
            .mcp_servers
            .iter()
            .map(|s| mcp_server_view(s, catalog))
            .collect(),
    }
}

fn mcp_server_view(s: &McpServerConfig, catalog: &ToolCatalog) -> McpServerView {
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
