//! 設定画面・CLIへ見せる設定の形。
//!
//! `key_ref`も平文の秘密情報も含めない。プロバイダーに鍵が設定済みかどうかは
//! `has_api_key`という真偽値だけで伝え、MCPサーバーのヘッダーも名前だけを伝える。

use serde::Serialize;

use crate::attachments::{self, Delivery};
use crate::config::{
    ApiFormat, Config, McpEndpoint, McpServerConfig, ModelConfig, ProviderConfig, ReasoningEffort,
    DEFAULT_RESPONSE_TIMEOUT_SECS, MCP_SERVER_NAME_MAX_CHARS,
};
use crate::db::attachments::AttachmentKind;
use crate::i18n::Language;
use crate::llm::providers;
use crate::llm::{self, DetectedCapabilities, DetectedCatalog, ModelCapabilities};
use crate::mcp::ToolCatalog;
use crate::orchestration::{
    default_opening_message, default_task_chat_prompt, DEFAULT_MAX_ROUNDS_PER_TURN,
    DEFAULT_TOTAL_TIMEOUT_SECS,
};
use crate::text;
use crate::tools::external;

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct ProviderView {
    pub id: String,
    pub name: String,
    pub api_format: ApiFormat,
    pub base_url: String,
    pub models: Vec<ModelView>,
    pub active_model: Option<String>,
    pub has_api_key: bool,
    /// 登録したカスタムヘッダーの名前。値は秘密情報なので渡さない。
    pub header_names: Vec<String>,
    /// モデルの能力を推論サーバーに問い合わせられる(「能力を検出」を出す)。
    pub can_detect_capabilities: bool,
    /// このプロバイダーをアクティブにしているが、組み立てられない理由。
    pub error: Option<String>,
    /// このプロバイダーをアクティブにしているが、鍵(またはカスタムヘッダーの値)を
    /// 資格情報ストアから読めない理由。
    /// `error`と違い、プロバイダーの設定ではなく資格情報ストアの側の問題で、ストアの
    /// ロックを解除すれば次の送信で直ることがある。
    pub key_error: Option<String>,
}

/// モデル表の1行。能力は解決済みの値を渡し、画面は3層の解決を自前で行わない。
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct ModelView {
    /// 登録した名前。操作の鍵として送り返すだけで、画面には描かない(描くのは`label`)。
    pub name: String,
    /// 画面に出す名前。プロバイダーの一覧から選んだ名前はサーバーが書いた文字列なので、
    /// 見えない文字を除いた写しを渡す。
    pub label: String,
    pub visible: bool,
    pub capabilities: ModelCapabilities,
    /// 手動設定が無いとき(自動検出 → 既定値)のコンテキスト長。入力欄のプレースホルダに出す。
    pub default_context_length: u32,
    /// 能力に手動設定がある(「初期値に戻す」を出す)。
    pub overridden: bool,
    /// 自動検出でツール呼び出しに対応しないと分かった。警告を出すだけで、使うことは止めない
    /// (サーバーがツール付きのリクエストを拒めば、そのターンのエラー発言になる)。
    pub lacks_tools: bool,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum McpEndpointView {
    StreamableHttp {
        url: String,
        header_names: Vec<String>,
    },
}

/// 設定画面へ渡すツール1件。引数スキーマは表示に使わないので渡さない。
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct McpToolView {
    /// サーバーが返したままの名前。有効化を切り替えるときの鍵として送り返すだけで、
    /// 画面には描かない(描くのは`label`)。
    pub name: String,
    /// 画面に出す名前と説明。サーバーが書いた文字列なので、見えない文字を除いた写しを渡す。
    pub label: String,
    pub description: Option<String>,
    /// モデルへ公開できるか(名前と引数スキーマ。`tools::external::is_exposable`)。公開できない
    /// ツールは有効にできない。
    pub exposable: bool,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct McpServerView {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub endpoint: McpEndpointView,
    pub enabled_tools: Vec<String>,
    /// 画面に出すツール一覧。取得済みの一覧に、そこに無い有効化済みのツールを足したもの
    /// (再起動直後の未取得の間も、有効化済みのツールを確かめて外せるように)。画面はこれを
    /// 描くだけで、自前では組み立てない。
    pub tools: Vec<McpToolView>,
    pub tools_fetched: bool,
}

/// 一般設定。既定値を持つものは、`ToolSettingsView`と同じく設定値(未設定は`None`)と
/// 未設定時の既定値の両方を渡す。
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct GeneralSettingsView {
    pub system_prompt: Option<String>,
    pub task_chat_system_prompt: Option<String>,
    /// 表示言語の既定の文面(`default_task_opening_message`も同じ)。
    pub default_task_chat_system_prompt: &'static str,
    pub task_opening_message: Option<String>,
    pub default_task_opening_message: &'static str,
    pub response_timeout_secs: Option<u64>,
    pub default_response_timeout_secs: u64,
    /// 未設定・知らない値なら既定の言語に解決した値。画面は「未設定」を扱わない。
    pub language: Language,
    /// 設定ファイルに書かれているが使えない表示言語の値(見えない文字を除き、切り詰めた写し)。
    /// あれば、既定の言語で表示していることを画面に出す。
    pub unknown_language: Option<String>,
}

/// [`GeneralSettingsView::unknown_language`]に出す長さの上限。
const UNKNOWN_LANGUAGE_MAX_CHARS: usize = 32;

/// ツール呼び出しの上限。設定値(未設定は`None`)と未設定時の既定値の両方を渡し、画面は
/// 既定値をプレースホルダに出す(既定値をTS側に書き写さないため)。
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct ToolSettingsView {
    pub max_rounds_per_turn: Option<u32>,
    pub total_timeout_secs: Option<u64>,
    pub default_max_rounds_per_turn: u32,
    pub default_total_timeout_secs: u64,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct SettingsView {
    /// 起動時に設定ファイルを読めなかった理由。あれば設定は保存されない。
    pub config_error: Option<String>,
    pub general: GeneralSettingsView,
    pub tools: ToolSettingsView,
    pub providers: Vec<ProviderView>,
    pub active_provider_id: Option<String>,
    pub mcp_servers: Vec<McpServerView>,
    /// サーバー識別子の長さの上限。画面は入力欄の上限と案内文に使い、値を写さない。
    pub mcp_server_name_max_chars: usize,
}

/// チャット入力欄の下のモデル選択・思考の強さ選択。設定画面の[`SettingsView`]とは別に持ち、
/// 選ぶのに要るものだけを渡す。
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct ChatModelsView {
    /// 一覧に出すモデル(設定画面で表示にしたもの)。プロバイダーの登録順、その中はモデルの
    /// 登録順。
    pub choices: Vec<ModelChoice>,
    /// チャットで使うモデル。一覧から隠したモデルでも、使っていれば入る。
    pub selected: Option<SelectedModel>,
    /// 送る発言の添付を、種別ごとにモデルへどう渡すか(`attachments::delivery`)。
    /// 画面は`name_only`の種別に警告を出す(送信は止めない)。
    pub attachments: AttachmentDeliveries,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
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
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct SelectedModel {
    #[serde(flatten)]
    pub choice: ModelChoice,
    /// 思考に対応する(3層で解決済み)。対応しなければ思考の強さは選べない。
    pub thinking: bool,
    pub reasoning_effort: ReasoningEffort,
}

/// 種別ごとの渡し方。`None`は、モデルが未選択で決まらない。
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct AttachmentDeliveries {
    pub text: Option<Delivery>,
    pub image: Option<Delivery>,
    pub other: Option<Delivery>,
}

impl AttachmentDeliveries {
    /// これから送る発言は、送った時点で直近のユーザー発言になる。`image_input`はモデルが画像入力に
    /// 対応するかで、モデルが未選択なら`None`。
    fn for_next_message(image_input: Option<bool>) -> Self {
        let of = |kind| match image_input {
            Some(image_input) => Some(attachments::delivery(kind, image_input, true)),
            None => attachments::delivery_without_model(kind),
        };
        Self {
            text: of(AttachmentKind::Text),
            image: of(AttachmentKind::Image),
            other: of(AttachmentKind::Other),
        }
    }
}

pub(super) fn chat_models(config: &Config, detected: &DetectedCatalog) -> ChatModelsView {
    let active = config.active_model().map(|(p, m)| {
        let capabilities = llm::resolve_capabilities(m, detected.get(&p.id, &m.name).as_ref());
        (p, m, capabilities)
    });
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
        selected: active.as_ref().map(|(p, m, capabilities)| SelectedModel {
            choice: ModelChoice::of(p, m),
            thinking: capabilities.thinking,
            reasoning_effort: m.reasoning_effort,
        }),
        attachments: AttachmentDeliveries::for_next_message(
            active
                .as_ref()
                .map(|(_, _, capabilities)| capabilities.image),
        ),
    }
}

/// 設定の問題(`settings`モジュール冒頭)。
pub(super) struct Problems<'a> {
    pub config_error: Option<&'a str>,
    pub active_provider_error: Option<&'a str>,
    pub active_provider_key_error: Option<&'a str>,
}

/// アクティブなプロバイダーの問題を、そのプロバイダーの行にだけ載せる。
fn active_only(
    config: &Config,
    provider: &ProviderConfig,
    problem: Option<&str>,
) -> Option<String> {
    problem
        .filter(|_| config.active_provider_id.as_deref() == Some(provider.id.as_str()))
        .map(str::to_string)
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
            default_task_chat_system_prompt: default_task_chat_prompt(config.general.language()),
            task_opening_message: config.general.task_opening_message.clone(),
            default_task_opening_message: default_opening_message(config.general.language()),
            response_timeout_secs: config.general.response_timeout_secs,
            default_response_timeout_secs: DEFAULT_RESPONSE_TIMEOUT_SECS,
            language: config.general.language(),
            unknown_language: config
                .general
                .unknown_language()
                .map(|code| text::display_label(code, UNKNOWN_LANGUAGE_MAX_CHARS)),
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
                header_names: p.header_refs.iter().map(|r| r.name.clone()).collect(),
                can_detect_capabilities: providers::can_detect_capabilities(p),
                error: active_only(config, p, problems.active_provider_error),
                key_error: active_only(config, p, problems.active_provider_key_error),
            })
            .collect(),
        active_provider_id: config.active_provider_id.clone(),
        mcp_servers: config
            .mcp_servers
            .iter()
            .map(|s| mcp_server_view(s, catalog))
            .collect(),
        mcp_server_name_max_chars: MCP_SERVER_NAME_MAX_CHARS,
    }
}

/// プロバイダーの一覧から取得したモデル1件。
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
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
        lacks_tools: detected.is_some_and(DetectedCapabilities::lacks_tools),
    }
}

fn mcp_server_view(s: &McpServerConfig, catalog: &ToolCatalog) -> McpServerView {
    let fetched = catalog.get(&s.id);
    let listed = fetched.as_deref().unwrap_or_default();
    let mut tools: Vec<McpToolView> = listed
        .iter()
        .map(|t| {
            mcp_tool_view(
                &t.name,
                t.description.as_deref(),
                external::is_exposable(&s.name, t),
            )
        })
        .collect();
    tools.extend(
        s.enabled_tools
            .iter()
            .filter(|name| !listed.iter().any(|t| &t.name == *name))
            .map(|name| mcp_tool_view(name, None, external::exposed_name(&s.name, name).is_some())),
    );
    let McpEndpoint::StreamableHttp { url, header_refs } = &s.endpoint;
    let endpoint = McpEndpointView::StreamableHttp {
        url: url.clone(),
        header_names: header_refs.iter().map(|r| r.name.clone()).collect(),
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

/// サーバーが書いた名前(ツール名・モデル名)を画面に出すときの上限文字数。
const MAX_LABEL_CHARS: usize = 100;

/// `exposable`は公開できるかの判定(`tools::external`)の結果。一覧が未取得のツールは引数
/// スキーマが分からないので、名前だけで判定した結果を渡す。
fn mcp_tool_view(name: &str, description: Option<&str>, exposable: bool) -> McpToolView {
    McpToolView {
        name: name.to_string(),
        label: text::display_label(name, MAX_LABEL_CHARS),
        description: description.map(external::description_of),
        exposable,
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
            endpoint: McpEndpoint::StreamableHttp {
                url: "http://127.0.0.1:8000/mcp".to_string(),
                header_refs: Vec::new(),
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
            header_refs: Vec::new(),
        };
        let choice = ModelChoice::of(&provider, &model);
        assert_eq!(
            (choice.model.as_str(), choice.label.as_str()),
            (raw, "lmaet-model")
        );
    }
}
