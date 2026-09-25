//! 設定画面・CLIへ見せる設定の形。
//!
//! `key_ref`も平文の秘密情報も含めない。プロバイダーに鍵が設定済みかどうかは
//! `has_api_key`という真偽値だけで伝え、MCPサーバーの環境変数・ヘッダーも名前だけを伝える
//! (architecture.md 7節「フロントエンドは秘密情報を一切受け取らない」)。

use serde::Serialize;

use crate::config::{
    ApiFormat, Config, McpEndpoint, McpServerConfig, ModelConfig, ProviderConfig, ReasoningEffort,
    DEFAULT_RESPONSE_TIMEOUT_SECS,
};
use crate::llm::providers;
use crate::llm::{self, DetectedCapabilities, DetectedCatalog, ModelCapabilities};
use crate::mcp::ToolCatalog;
use crate::orchestration::{DEFAULT_MAX_ROUNDS_PER_TURN, DEFAULT_TOTAL_TIMEOUT_SECS};
use crate::text;
use crate::tools::external;

#[derive(Debug, Serialize)]
pub struct ProviderView {
    pub id: String,
    pub name: String,
    pub api_format: ApiFormat,
    pub base_url: String,
    pub models: Vec<ModelView>,
    pub active_model: Option<String>,
    pub has_api_key: bool,
    /// モデルの能力を推論サーバーに問い合わせられる(「能力を検出」を出す)。
    pub can_detect_capabilities: bool,
    /// このプロバイダーをアクティブにしているが、組み立てられない理由(Issue #155)。
    pub error: Option<String>,
}

/// モデル表の1行(Issue #65)。能力は解決済みの値を渡し、画面は3層の解決を自前で行わない。
#[derive(Debug, Serialize)]
pub struct ModelView {
    /// 登録した名前。操作の鍵として送り返すだけで、画面には描かない(描くのは`label`)。
    pub name: String,
    /// 画面に出す名前。プロバイダーの一覧から選んだ名前はサーバーが書いた文字列なので、
    /// 見えない文字を除いた写しを渡す(architecture.md 10節)。
    pub label: String,
    pub visible: bool,
    pub capabilities: ModelCapabilities,
    /// 手動設定が無いとき(自動検出 → 既定値)のコンテキスト長。入力欄のプレースホルダに出す。
    pub default_context_length: u32,
    /// 能力に手動設定がある(「初期値に戻す」を出す)。
    pub overridden: bool,
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
    /// サーバーが返したままの名前。有効化を切り替えるときの鍵として送り返すだけで、
    /// 画面には描かない(描くのは`label`)。
    pub name: String,
    /// 画面に出す名前と説明。サーバーが書いた文字列なので、見えない文字を除いた写しを渡す
    /// (architecture.md 10節)。
    pub label: String,
    pub description: Option<String>,
    /// モデルへ公開できる名前か。公開できないツールは有効にできない。
    pub exposable: bool,
}

#[derive(Debug, Serialize)]
pub struct McpServerView {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub endpoint: McpEndpointView,
    pub enabled_tools: Vec<String>,
    /// 画面に出すツール一覧。取得済みの一覧(Issue #104)に、そこに無い有効化済みのツールを
    /// 足したもの。有効化済みのツールを必ず出すのは、出さないと確認することも外すことも
    /// できないため(一覧のキャッシュはアプリ起動中だけなので、再起動直後は未取得になる。
    /// サーバーが消したツールは、同じ名前のツールが後から足されると選び直さずに公開される)。
    /// 一覧に無いツールの説明はサーバーに聞かないと分からないので無い。画面はこれを描く
    /// だけで、自前では組み立てない。
    pub tools: Vec<McpToolView>,
    pub tools_fetched: bool,
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

/// チャット入力欄の下のモデル選択・思考の強さ選択(Issue #64)。設定画面の[`SettingsView`]
/// とは別に持ち、選ぶのに要るものだけを渡す。
#[derive(Debug, Serialize)]
pub struct ChatModelsView {
    /// 一覧に出すモデル(設定画面で表示にしたもの)。プロバイダーの登録順、その中はモデルの
    /// 登録順。
    pub choices: Vec<ModelChoice>,
    /// チャットで使うモデル。一覧から隠したモデルでも、使っていれば入る。
    pub selected: Option<SelectedModel>,
}

#[derive(Debug, Serialize)]
pub struct ModelChoice {
    pub provider_id: String,
    pub provider_name: String,
    /// 選ぶときに送り返す名前。画面に出すのは`label`([`ModelView`]と同じ)。
    pub model: String,
    pub label: String,
}

impl ModelChoice {
    fn of(provider: &ProviderConfig, model: &ModelConfig) -> Self {
        Self {
            provider_id: provider.id.clone(),
            provider_name: provider.name.clone(),
            model: model.name.clone(),
            label: model_label(&model.name),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SelectedModel {
    #[serde(flatten)]
    pub choice: ModelChoice,
    /// 思考に対応する(3層で解決済み)。対応しなければ思考の強さは選べない。
    pub thinking: bool,
    pub reasoning_effort: ReasoningEffort,
}

pub(super) fn chat_models(config: &Config, detected: &DetectedCatalog) -> ChatModelsView {
    ChatModelsView {
        choices: config
            .providers
            .iter()
            .flat_map(|p| {
                p.models
                    .iter()
                    .filter(|m| m.visible)
                    .map(|m| ModelChoice::of(p, m))
            })
            .collect(),
        selected: config.active_model().map(|(p, m)| SelectedModel {
            choice: ModelChoice::of(p, m),
            thinking: llm::resolve_capabilities(m, detected.get(&p.id, &m.name).as_ref()).thinking,
            reasoning_effort: m.reasoning_effort,
        }),
    }
}

/// 設定の問題(`settings`モジュール冒頭)。
pub(super) struct Problems<'a> {
    pub config_error: Option<&'a str>,
    pub active_provider_error: Option<&'a str>,
}

pub(super) fn build(
    config: &Config,
    catalog: &ToolCatalog,
    detected: &DetectedCatalog,
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
                models: p
                    .models
                    .iter()
                    .map(|m| model_view(m, detected.get(&p.id, &m.name).as_ref()))
                    .collect(),
                active_model: p.active_model.clone(),
                has_api_key: p.key_ref.is_some(),
                can_detect_capabilities: providers::can_detect_capabilities(p),
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

/// プロバイダーの一覧から取得したモデル1件(Issue #33)。
#[derive(Debug, Serialize)]
pub struct AvailableModel {
    /// サーバーが返したままの名前。登録するときに送り返す。
    pub name: String,
    pub label: String,
}

pub(super) fn available_models(names: Vec<String>) -> Vec<AvailableModel> {
    names
        .into_iter()
        .map(|name| AvailableModel {
            label: model_label(&name),
            name,
        })
        .collect()
}

fn model_label(name: &str) -> String {
    text::display_label(name, MAX_LABEL_CHARS)
}

fn model_view(m: &ModelConfig, detected: Option<&DetectedCapabilities>) -> ModelView {
    ModelView {
        name: m.name.clone(),
        label: model_label(&m.name),
        visible: m.visible,
        capabilities: llm::resolve_capabilities(m, detected),
        default_context_length: llm::fallback_capabilities(detected).context_length,
        overridden: !m.overrides.is_empty(),
    }
}

fn mcp_server_view(s: &McpServerConfig, catalog: &ToolCatalog) -> McpServerView {
    let fetched = catalog.get(&s.id);
    let listed = fetched.as_deref().unwrap_or_default();
    let mut tools: Vec<McpToolView> = listed
        .iter()
        .map(|t| mcp_tool_view(&s.name, &t.name, t.description.as_deref()))
        .collect();
    tools.extend(
        s.enabled_tools
            .iter()
            .filter(|name| !listed.iter().any(|t| &t.name == *name))
            .map(|name| mcp_tool_view(&s.name, name, None)),
    );
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
        tools,
        tools_fetched: fetched.is_some(),
    }
}

/// サーバーが書いた名前(ツール名・モデル名)と説明を画面に出すときの上限文字数。
const MAX_LABEL_CHARS: usize = 100;
const MAX_TOOL_DESCRIPTION_CHARS: usize = 2000;

fn mcp_tool_view(server_name: &str, name: &str, description: Option<&str>) -> McpToolView {
    McpToolView {
        name: name.to_string(),
        label: text::display_label(name, MAX_LABEL_CHARS),
        description: description.map(|d| text::display_block(d, MAX_TOOL_DESCRIPTION_CHARS)),
        exposable: external::exposed_name(server_name, name).is_some(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;

    use super::*;
    use crate::mcp::McpToolInfo;

    #[test]
    fn tool_view_shows_a_visible_copy_and_keeps_the_raw_name_as_the_key() {
        let server = McpServerConfig {
            id: "id1".to_string(),
            name: "files".to_string(),
            enabled: true,
            endpoint: McpEndpoint::Stdio {
                command: "true".to_string(),
                args: Vec::new(),
                env_refs: Vec::new(),
            },
            enabled_tools: BTreeSet::from(["gone".to_string()]),
        };
        let catalog = ToolCatalog::new();
        catalog.store(
            "id1",
            vec![McpToolInfo {
                name: "\u{202E}elif_eteled".to_string(),
                description: Some("line1\n\u{200B}line2".to_string()),
                input_schema: json!({ "type": "object" }),
            }],
        );

        let view = mcp_server_view(&server, &catalog);
        assert!(view.tools_fetched);
        let tool = &view.tools[0];
        assert_eq!(tool.name, "\u{202E}elif_eteled");
        assert_eq!(tool.label, "elif_eteled");
        assert_eq!(tool.description.as_deref(), Some("line1\nline2"));
        assert!(!tool.exposable);
        // サーバーの一覧から消えた有効化済みのツールも、外せるように出す。
        assert_eq!(view.tools.len(), 2);
        assert_eq!(view.tools[1].name, "gone");
        assert!(view.tools[1].description.is_none());
    }

    #[test]
    fn model_names_show_a_visible_copy_and_keep_the_raw_name_as_the_key() {
        let raw = "\u{202E}lmaet-model\u{200B}";
        let listed = available_models(vec![raw.to_string()]);
        assert_eq!(listed[0].name, raw);
        assert_eq!(listed[0].label, "lmaet-model");

        let model = ModelConfig::new(raw.to_string());
        let row = model_view(&model, None);
        assert_eq!(
            (row.name.as_str(), row.label.as_str()),
            (raw, "lmaet-model")
        );

        let provider = ProviderConfig {
            id: "p".to_string(),
            name: "Local".to_string(),
            api_format: ApiFormat::OpenAiCompat,
            base_url: "http://localhost:1234/v1".to_string(),
            models: vec![model.clone()],
            active_model: None,
            key_ref: None,
        };
        let choice = ModelChoice::of(&provider, &model);
        assert_eq!(
            (choice.model.as_str(), choice.label.as_str()),
            (raw, "lmaet-model")
        );
    }
}
