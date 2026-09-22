//! 外部(MCP)ツールをモデルへ公開する形に整える(Issue #44)。
//!
//! ここが決めるのは「どのツールを、どの名前で公開し、呼び出しをどのサーバーへ振り分けるか」
//! だけで、接続そのものは`crate::mcp`が持つ(判断を1箇所に閉じる。principles.md 5節)。
//!
//! ツールの説明文はサーバーが書いた文字列であり、そのままモデルのプロンプトに入る。
//! 内容の検証はしない(信頼境界は「ユーザーが登録したこと」自体に置く。
//! principles.md 4節「外部連携の境界を明確にする」。設定画面にも同じ趣旨の案内文がある)。

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::config::McpServerConfig;
use crate::llm::ToolSchema;
use crate::mcp::McpToolInfo;

/// サーバー識別子とツール名の区切り。名前空間化の目的は、内部ツール・他サーバーの
/// ツールとの衝突を避けることと、呼び出し先APIの命名規則に収めること
/// (legacy/backend.md 9節)。
const SEPARATOR: &str = "__";

/// OpenAI互換APIのツール名に使える文字と長さ。ここに収まらない名前は公開しない
/// (名前を機械的に丸めると、別のツールと同じ名前になり得るため)。
const MAX_EXPOSED_NAME_LEN: usize = 64;

struct Entry {
    server_id: String,
    tool_name: String,
    schema: ToolSchema,
}

/// このターンでモデルへ公開する外部ツールの集合。
#[derive(Default)]
pub struct ExternalToolset {
    entries: Vec<Entry>,
    routes: HashMap<String, usize>,
}

impl ExternalToolset {
    /// サーバーと取得済みツール一覧の組から組み立てる。`reserved`には内部ツールの名前を
    /// 渡す(同じ名前のツールが2つ公開されると、どちらが呼ばれたか判別できないため)。
    pub fn build<'a>(
        servers: impl IntoIterator<Item = (&'a McpServerConfig, Vec<McpToolInfo>)>,
        reserved: &[String],
    ) -> Self {
        let mut toolset = Self::default();
        for (server, tools) in servers {
            for tool in tools {
                // 有効化されていないツールは公開しない(config.rsの`enabled_tools`は
                // opt-in。サーバーが後からツールを増やしても勝手には使わない)。
                if !server.enabled_tools.contains(&tool.name) {
                    continue;
                }
                let exposed_name = format!("{}{}{}", server.name, SEPARATOR, tool.name);
                if !is_valid_exposed_name(&exposed_name) {
                    continue;
                }
                if reserved.contains(&exposed_name) || toolset.routes.contains_key(&exposed_name) {
                    continue;
                }
                let index = toolset.entries.len();
                toolset.routes.insert(exposed_name.clone(), index);
                toolset.entries.push(Entry {
                    schema: ToolSchema {
                        name: exposed_name,
                        description: tool.description.unwrap_or_default(),
                        parameters: parameters_of(tool.input_schema),
                    },
                    server_id: server.id.clone(),
                    tool_name: tool.name,
                });
            }
        }
        toolset
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.entries.iter().map(|e| e.schema.clone()).collect()
    }

    /// モデルが呼んだ名前から、接続先サーバーIDと元のツール名を引く。
    /// 内部ツールの呼び出しでは`None`が返り、呼び出し側が内部の実行へ回す。
    pub fn route(&self, exposed_name: &str) -> Option<(&str, &str)> {
        let entry = &self.entries[*self.routes.get(exposed_name)?];
        Some((&entry.server_id, &entry.tool_name))
    }

    #[cfg(test)]
    fn exposed_names(&self) -> Vec<&str> {
        self.entries.iter().map(|e| e.schema.name.as_str()).collect()
    }
}

fn is_valid_exposed_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_EXPOSED_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// サーバーが宣言した引数スキーマをそのまま使う。オブジェクトでない場合だけ、
/// 引数なしのスキーマに置き換える(プロバイダー側が壊れたスキーマを拒否して
/// ターン全体が失敗するのを避ける)。
fn parameters_of(input_schema: Value) -> Value {
    if input_schema.is_object() {
        input_schema
    } else {
        json!({ "type": "object", "properties": {} })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::config::McpEndpoint;

    fn server(id: &str, name: &str, enabled_tools: &[&str]) -> McpServerConfig {
        McpServerConfig {
            id: id.to_string(),
            name: name.to_string(),
            enabled: true,
            endpoint: McpEndpoint::Stdio {
                command: "true".to_string(),
                args: Vec::new(),
                env_refs: Vec::new(),
            },
            enabled_tools: enabled_tools.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>(),
        }
    }

    fn tool(name: &str) -> McpToolInfo {
        McpToolInfo {
            name: name.to_string(),
            description: Some("desc".to_string()),
            input_schema: json!({ "type": "object" }),
        }
    }

    #[test]
    fn exposes_only_enabled_tools_with_namespaced_names() {
        let s = server("id1", "files", &["read"]);
        let toolset = ExternalToolset::build([(&s, vec![tool("read"), tool("write")])], &[]);
        assert_eq!(toolset.exposed_names(), vec!["files__read"]);
        assert_eq!(toolset.route("files__read"), Some(("id1", "read")));
        assert_eq!(toolset.route("files__write"), None);
    }

    #[test]
    fn skips_names_that_cannot_be_exposed() {
        // 呼び出し先APIの命名規則に収まらない名前(記号入り・長すぎる)は公開しない。
        let s = server("id1", "files", &["read file", &"x".repeat(80)]);
        let toolset = ExternalToolset::build(
            [(&s, vec![tool("read file"), tool(&"x".repeat(80))])],
            &[],
        );
        assert!(toolset.is_empty());
    }

    #[test]
    fn skips_names_colliding_with_internal_or_earlier_tools() {
        // 名前が衝突した場合は公開しない(どちらが呼ばれたか判別できないため)。
        let a = server("id1", "a__b", &["c"]);
        let b = server("id2", "a", &["b__c"]);
        let toolset = ExternalToolset::build([(&a, vec![tool("c")]), (&b, vec![tool("b__c")])], &[]);
        assert_eq!(toolset.exposed_names(), vec!["a__b__c"]);
        assert_eq!(toolset.route("a__b__c"), Some(("id1", "c")));

        let s = server("id1", "files", &["read"]);
        let toolset =
            ExternalToolset::build([(&s, vec![tool("read")])], &["files__read".to_string()]);
        assert!(toolset.is_empty());
    }
}
