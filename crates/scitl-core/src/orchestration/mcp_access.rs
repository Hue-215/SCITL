//! ターンから見た外部ツールサーバー。
//!
//! `orchestration::turn`が知るのはこの型までで、接続の詳細は`crate::mcp`、
//! 公開する名前と振り分けは`crate::tools::external`が持つ。

use crate::config::McpServerConfig;
use crate::mcp::ToolCatalog;

#[derive(Default, Clone, Copy)]
pub struct McpAccess<'a> {
    /// 登録済みのサーバー。無効なサーバー・ツールを1つも有効化していないサーバーは
    /// ターン側で除く(判断は`turn::prepare_external_tools`に1箇所)。
    pub servers: &'a [McpServerConfig],
    /// 取得済みツール一覧のキャッシュ。`None`ならターンのたびに取得する。
    pub catalog: Option<&'a ToolCatalog>,
}

impl<'a> McpAccess<'a> {
    pub fn new(servers: &'a [McpServerConfig], catalog: &'a ToolCatalog) -> Self {
        Self {
            servers,
            catalog: Some(catalog),
        }
    }

    /// 外部ツールを使わないターン(MCP未登録、またはそれを前提にしたテスト)。
    pub fn none() -> Self {
        Self::default()
    }
}
