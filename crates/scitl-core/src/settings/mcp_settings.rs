//! 外部ツールサーバー(MCP)の登録と、公開するツールの選択。

use std::sync::Arc;

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use super::rejection::{refuse_if_any, rejected, InputRejection};
use super::{input, invalid, HeaderInput, Settings, SettingsView};
use crate::blocking;
use crate::config::{
    validate_mcp_server_name, Config, McpEndpoint, McpServerConfig, SecretRef,
    MCP_SERVER_NAME_MAX_CHARS,
};
use crate::error::{CoreError, Result};
use crate::mcp;
use crate::tools::external;

/// 秘密情報の`key_ref`の接頭辞と、削除に失敗したときの診断に出す名前。
const SECRET_PREFIX: &str = "mcp";
const SECRET_WHAT: &str = "MCP secret";

/// サーバー追加フォームからの入力。`McpEndpoint`と同じく、接続方式ごとに必要な値だけを
/// 受け取る。ヘッダーの値は秘密情報で、保存後は`key_ref`に置き換わる。値を含むため
/// `Debug`は付けない(ログに出す経路を作らない)。
#[derive(Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum NewMcpEndpoint {
    StreamableHttp {
        url: String,
        /// 画面からはヘッダーの欄の文字列で届く([`HeaderInput`])。
        #[serde(default)]
        #[cfg_attr(test, ts(type = "string"))]
        headers: HeaderInput,
    },
}

/// [`Settings::add_mcp_server`]の結果。`tools_error`は、登録のあとのツール一覧の取得に
/// 失敗した理由(画面は追加したサーバーのカードに出す)。
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct McpServerAdded {
    pub settings: SettingsView,
    pub server_id: String,
    pub tools_error: Option<String>,
}

impl Settings {
    /// サーバーを登録し([`Self::register_mcp_server`])、続けて1回ツール一覧を取得する
    /// (公開するツールを選べるように)。取得に失敗しても登録は残し、理由を結果に添える。
    pub async fn add_mcp_server(
        self: &Arc<Self>,
        name: String,
        endpoint: NewMcpEndpoint,
    ) -> Result<McpServerAdded> {
        let settings = Arc::clone(self);
        let (server_id, added) =
            blocking::run(move || settings.register_mcp_server(&name, endpoint)).await?;
        let (settings, tools_error) = match self.fetch_mcp_tools(&server_id).await {
            Ok(fetched) => (fetched, None),
            Err(e) => (added, Some(e.to_string())),
        };
        Ok(McpServerAdded {
            settings,
            server_id,
            tools_error,
        })
    }

    /// 検証→重複確認→秘密情報の保存→登録の順。重複確認から登録までを書き込みロックの中で
    /// 行うので、同名の登録が割り込んで秘密情報が孤児になることはない。登録に失敗したら
    /// 保存した秘密情報を消す。登録したサーバーのIDと、登録後の設定を返す。
    pub(super) fn register_mcp_server(
        &self,
        name: &str,
        endpoint: NewMcpEndpoint,
    ) -> Result<(String, SettingsView)> {
        let name = name.trim().to_string();
        let NewMcpEndpoint::StreamableHttp { url, headers } = endpoint;
        let url = url.trim().to_string();
        // 画面が欄の近くに出す誤りは、他の検証より先にまとめて見る。
        let mut reasons = Vec::new();
        if name.is_empty() {
            reasons.push(InputRejection::McpServerNameRequired);
        } else if validate_mcp_server_name(&name).is_err() {
            reasons.push(InputRejection::McpServerNameInvalid {
                max_chars: MCP_SERVER_NAME_MAX_CHARS,
            });
        } else if let Some(taken) = name_taken(&self.current().config, &name) {
            reasons.push(taken);
        }
        if url.is_empty() {
            reasons.push(InputRejection::UrlRequired);
        }
        let headers = headers.into_pairs().unwrap_or_else(|lines| {
            reasons.extend(lines);
            Vec::new()
        });
        refuse_if_any(reasons)?;
        let endpoint = validate_endpoint(url, headers)?;

        let mut draft = self.edit();
        // 先の確かめから書き込みロックを取るまでに、同じ名前が登録されているかもしれない。
        if let Some(taken) = name_taken(&draft.config, &name) {
            return Err(rejected(vec![taken]));
        }
        let endpoint = store_endpoint_secrets(endpoint)?;
        let refs = endpoint_secret_refs(&endpoint).to_vec();
        let id = ulid::Ulid::new().to_string();
        draft.config.mcp_servers.push(McpServerConfig {
            id: id.clone(),
            name,
            enabled: true,
            endpoint,
            enabled_tools: Default::default(),
        });
        let view = draft.commit().inspect_err(|_| delete_secret_refs(&refs))?;
        Ok((id, view))
    }

    /// 保存済みの秘密情報も消す。`delete_provider`と同じく、設定の保存が済んでから消す。
    /// 削除したサーバーのツール一覧がキャッシュに残らないよう、ここで捨てる。
    pub fn delete_mcp_server(&self, server_id: &str) -> Result<SettingsView> {
        let mut draft = self.edit();
        let index = draft
            .config
            .mcp_servers
            .iter()
            .position(|s| s.id == server_id)
            .ok_or_else(|| mcp_server_not_found(server_id))?;
        let removed = draft.config.mcp_servers.remove(index);

        let view = draft.commit()?;
        delete_secret_refs(endpoint_secret_refs(&removed.endpoint));
        self.mcp_tools.forget(&removed.id);
        Ok(view)
    }

    /// 有効にし直したら、ターンでの一覧取得に続けて失敗した回数を数え直す(試すのをやめていた
    /// サーバーも、次のターンでまた試す。`mcp::MAX_CONSECUTIVE_FAILURES`)。
    pub fn set_mcp_server_enabled(&self, server_id: &str, enabled: bool) -> Result<SettingsView> {
        let mut draft = self.edit();
        find_mcp_server_mut(&mut draft.config, server_id)?.enabled = enabled;
        let view = draft.commit()?;
        if enabled {
            self.mcp_tools.retry(server_id);
        }
        Ok(view)
    }

    pub fn set_mcp_tool_enabled(
        &self,
        server_id: &str,
        tool_name: &str,
        enabled: bool,
    ) -> Result<SettingsView> {
        let mut draft = self.edit();
        let enabled_count: usize = draft
            .config
            .mcp_servers
            .iter()
            .map(|s| s.enabled_tools.len())
            .sum();
        let server = find_mcp_server_mut(&mut draft.config, server_id)?;
        if enabled && !server.enabled_tools.contains(tool_name) {
            // 画面でも有効にできないようにしているが、判定を画面に任せない。一覧が未取得なら
            // 名前だけで判定する(引数スキーマはターンで公開するときにも見る)。
            let listed = self
                .mcp_tools
                .get(server_id)
                .and_then(|tools| tools.into_iter().find(|t| t.name == tool_name));
            let exposable = match listed {
                Some(tool) => external::is_exposable(&server.name, &tool),
                None => external::exposed_name(&server.name, tool_name).is_some(),
            };
            if !exposable {
                return Err(invalid(
                    "this tool cannot be enabled because its name or argument schema cannot be exposed to the model",
                ));
            }
            // 無効なサーバーのツールも数える。サーバーを有効に戻したときに上限を超えないため。
            if enabled_count >= external::MAX_EXTERNAL_TOOLS {
                return Err(invalid(format!(
                    "at most {} external tools can be enabled",
                    external::MAX_EXTERNAL_TOOLS
                )));
            }
            server.enabled_tools.insert(tool_name.to_string());
        } else {
            server.enabled_tools.remove(tool_name);
        }
        draft.commit()
    }

    /// サーバーに接続してツール一覧を取得し、キャッシュへ載せて設定の状態ごと返す。
    /// config.tomlには書き込まない。ロックはサーバー設定を複製するまでだけ持ち、接続の
    /// `.await`をまたがせない。
    pub async fn fetch_mcp_tools(&self, server_id: &str) -> Result<SettingsView> {
        let _in_flight = self
            .fetching
            .try_begin(server_id.to_string())
            .ok_or_else(|| invalid("already fetching tools for this server"))?;
        let server = self
            .current()
            .config
            .mcp_servers
            .iter()
            .find(|s| s.id == server_id)
            .cloned()
            .ok_or_else(|| mcp_server_not_found(server_id))?;

        let tools = mcp::list_tools(&server).await?;
        self.mcp_tools.store(server_id, tools);
        Ok(self.view())
    }
}

/// 同じ識別子のサーバーが登録済みなら、その理由。
fn name_taken(config: &Config, name: &str) -> Option<InputRejection> {
    config
        .mcp_servers
        .iter()
        .any(|s| s.name == name)
        .then(|| InputRejection::McpServerNameTaken {
            name: name.to_string(),
        })
}

/// 秘密情報に触れる前に済ませられる検証をすべて行う。
fn validate_endpoint(url: String, headers: Vec<(String, SecretString)>) -> Result<CheckedEndpoint> {
    mcp::validate_streamable_http_url(&url)?;
    for (name, value) in &headers {
        mcp::validate_header_name(name)?;
        mcp::validate_header_value(value.expose_secret())?;
    }
    input::unique_names(&headers, "header", str::to_ascii_lowercase)?;
    Ok(CheckedEndpoint { url, headers })
}

/// 検証を済ませた接続先(streamable HTTP)。
struct CheckedEndpoint {
    url: String,
    headers: Vec<(String, SecretString)>,
}

fn store_endpoint_secrets(endpoint: CheckedEndpoint) -> Result<McpEndpoint> {
    let CheckedEndpoint { url, headers } = endpoint;
    Ok(McpEndpoint::StreamableHttp {
        url,
        header_refs: super::store_secret_refs(headers, SECRET_PREFIX, SECRET_WHAT)?,
    })
}

/// 1件が失敗しても残りは試す。
fn delete_secret_refs(refs: &[SecretRef]) {
    super::delete_secret_refs(refs, SECRET_WHAT);
}

fn endpoint_secret_refs(endpoint: &McpEndpoint) -> &[SecretRef] {
    let McpEndpoint::StreamableHttp { header_refs, .. } = endpoint;
    header_refs
}

fn find_mcp_server_mut<'a>(
    config: &'a mut Config,
    server_id: &str,
) -> Result<&'a mut McpServerConfig> {
    config
        .mcp_servers
        .iter_mut()
        .find(|s| s.id == server_id)
        .ok_or_else(|| mcp_server_not_found(server_id))
}

fn mcp_server_not_found(server_id: &str) -> CoreError {
    invalid(format!("MCP server not found: {server_id}"))
}
