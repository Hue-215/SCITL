//! 疑似API(LLM役が応答を書く。別リポジトリ Hue-215/Sham_llm)を相手に、本物のアダプタとターンの
//! 処理を通して会話を進める試験用のドライバー。GUIの送信と同じ入口(`create_task`・
//! `run_turn`)を呼ぶ。使い方は`.claude/skills/llm-relay/SKILL.md`。
//!
//! ```text
//! cargo run -p scitl-core --example relay_session -- <DATA_DIR> <BASE_URL> <STEP>...
//! ```
//!
//! STEPは`@new`(タスクを作って聞き取りを始める)・`@task <id>`(既にあるタスクの会話へ移る)・
//! `@general`(総合チャットへ移る)・`@base <文>`(以降のターンの基本のシステムプロンプトを
//! 差し替える)・`@tools on`/`@tools off`(以降のターンで外部ツールを1つ有効・無効にする。
//! ツール定義が変わる場面を起こすためで、サーバーの実体は無く、呼ばれたら失敗を返す)・
//! それ以外(今の会話へのユーザー発言)。DATA_DIRは`scitl-cli --data-dir`でそのまま読める。
//!
//! 方言は環境変数`RELAY_DIALECT`(`openai`・`anthropic`・`gemini`)で選ぶ。BASE_URLとモデルは
//! `.claude/skills/llm-relay/SKILL.md`。`RELAY_MODEL`でモデル名を変えられる(本物のサーバーを相手にするとき)。
//! `RELAY_CONTEXT_LENGTH`でモデルのコンテキスト長を変えられる(間引きを起こすため)。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use scitl_core::attachments::{AttachmentStore, Attachments};
use scitl_core::config::{GeneralConfig, McpEndpoint, McpServerConfig, ReasoningEffort};
use scitl_core::db::messages::Chat;
use scitl_core::db::{self, SharedConnection};
use scitl_core::in_flight::InFlightSet;
use scitl_core::llm::providers::anthropic::AnthropicAdapter;
use scitl_core::llm::providers::gemini::GeminiAdapter;
use scitl_core::llm::providers::openai_compat::OpenAiCompatAdapter;
use scitl_core::llm::providers::Credentials;
use scitl_core::llm::{LlmAdapter, ResponseEvent, DEFAULT_CAPABILITIES};
use scitl_core::mcp::{McpToolInfo, ToolCatalog};
use scitl_core::orchestration::{
    self, create_task, run_turn, McpAccess, SystemPrompts, TaskCreation, ToolLimits, TurnContext,
    TurnEvent,
};
use scitl_core::paths::DataLayout;
use secrecy::SecretString;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let data = DataLayout::new(PathBuf::from(args.next().expect("DATA_DIR")));
    let base_url = args.next().expect("BASE_URL");
    let steps: Vec<String> = args.collect();

    std::fs::create_dir_all(data.root()).unwrap();
    let db: SharedConnection = Arc::new(Mutex::new(db::open(data.database()).unwrap()));
    let key = Credentials::key_only(SecretString::from("relay-dummy-key".to_string()));
    // LLM役は人間並みに遅いので長めに待つ。
    let timeout = Duration::from_secs(900);
    let dialect = std::env::var("RELAY_DIALECT").unwrap_or_else(|_| "openai".to_string());
    let model = std::env::var("RELAY_MODEL").ok();
    let model = |default: &'static str| model.as_deref().unwrap_or(default);
    let adapter: Box<dyn LlmAdapter> = match dialect.as_str() {
        "openai" => {
            Box::new(OpenAiCompatAdapter::new(base_url, key, model("dummy-o"), timeout).unwrap())
        }
        "anthropic" => {
            Box::new(AnthropicAdapter::new(base_url, key, model("dummy-a"), timeout).unwrap())
        }
        "gemini" => Box::new(GeminiAdapter::new(base_url, key, model("dummy-g"), timeout).unwrap()),
        other => panic!("unknown RELAY_DIALECT: {other}"),
    };
    let general = GeneralConfig::default();
    let generating = InFlightSet::new();
    let attachments = Attachments::new(AttachmentStore::new(
        data.attachments(),
        data.root().join("revealed"),
    ));
    let events = |event: TurnEvent| match event {
        TurnEvent::Response {
            event: ResponseEvent::TextDelta { text },
        } => println!("  [assistant] {text}"),
        TurnEvent::Response {
            event: ResponseEvent::ReasoningDelta { text },
        } => println!("  [reasoning] {text}"),
        TurnEvent::ToolExecuted { execution, .. } => {
            println!("  [tool] {}", serde_json::to_string(&execution).unwrap())
        }
        TurnEvent::Response { .. } => {}
    };
    let adapter: &dyn LlmAdapter = adapter.as_ref();
    let mut capabilities = DEFAULT_CAPABILITIES;
    if let Ok(length) = std::env::var("RELAY_CONTEXT_LENGTH") {
        capabilities.context_length = length.parse().expect("RELAY_CONTEXT_LENGTH");
    }
    let defaults = SystemPrompts::from_config(&general);
    let mut base: Option<String> = None;
    // 一覧は取得済みとして置くので、ツールが呼ばれるまでサーバーへは繋ぎに行かない。
    let catalog = ToolCatalog::new();
    catalog.store(
        "relay",
        vec![McpToolInfo {
            name: "lookup".to_string(),
            description: Some("Looks up a word in the dictionary.".to_string()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"word": {"type": "string"}},
                "required": ["word"],
            }),
        }],
    );
    let mut servers = [McpServerConfig {
        id: "relay".to_string(),
        name: "relay".to_string(),
        enabled: false,
        endpoint: McpEndpoint::StreamableHttp {
            url: "http://127.0.0.1:1/mcp".to_string(),
            header_refs: Vec::new(),
        },
        enabled_tools: ["lookup".to_string()].into(),
    }];

    let mut chat = Chat::General;
    for step in steps {
        if let Some(text) = step.strip_prefix("@base ") {
            println!("> @base {text}");
            base = Some(text.to_string());
            continue;
        }
        if let Some(id) = step.strip_prefix("@task ") {
            println!("> @task {id}");
            chat = Chat::Task(id.parse().expect("@task <id>"));
            continue;
        }
        if let Some(switch) = step.strip_prefix("@tools ") {
            println!("> @tools {switch}");
            servers[0].enabled = match switch {
                "on" => true,
                "off" => false,
                other => panic!("unknown @tools switch: {other}"),
            };
            continue;
        }
        let ctx = TurnContext {
            adapter: Ok(adapter),
            prompts: SystemPrompts {
                base: base.as_deref().or(defaults.base),
                task_chat: defaults.task_chat,
            },
            opening_message: orchestration::opening_message(&general),
            capabilities,
            reasoning_effort: (dialect != "openai").then_some(ReasoningEffort::Medium),
            mcp: McpAccess::new(&servers, &catalog),
            limits: ToolLimits::default(),
            generating: &generating,
            attachments: &attachments,
            events: &events,
        };
        match step.as_str() {
            "@new" => {
                let creation = create_task(db.clone(), &ctx, |task| {
                    println!("> @new (task {})", task.id);
                })
                .await
                .unwrap();
                let TaskCreation::Created { task } = creation else {
                    panic!("the adapter is not ready");
                };
                chat = Chat::Task(task.id);
            }
            "@general" => {
                println!("> @general");
                chat = Chat::General;
            }
            text => {
                println!("> [user] {text}");
                run_turn(db.clone(), &ctx, chat, text.to_string())
                    .await
                    .unwrap();
            }
        }
    }
}
