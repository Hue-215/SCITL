use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use scitl_core::config::{McpEndpoint, McpServerConfig};
use scitl_core::db;
use scitl_core::db::error::CoreError;
use scitl_core::llm::{
    ChatMessage, FinishReason, LlmAdapter, Readiness, ResponseEvent, ToolArguments, ToolSchema,
};
use scitl_core::mcp::ToolCatalog;
use scitl_core::orchestration::{
    delete_message, edit_user_message, retry_assistant_message, run_turn, McpAccess, SystemPrompts,
    ToolLimits,
};
use serde_json::json;

fn system_prompt_content(message: &ChatMessage) -> &str {
    match message {
        ChatMessage::System(content) => content,
        other => panic!("expected the first message to be System, got {other:?}"),
    }
}

/// 各ラウンドで渡された発言列をまるごと記録するアダプタ。「最新状態は毎ターン渡す」
/// (docs/spec/principles.md 3節)に加え、同一ターン内のツール呼び出し往復
/// (docs/spec/rebuild/tools.md 4節)がturn.rs側で実際に組み立てられていることを検証する。
struct RecordingAdapter {
    calls: AtomicUsize,
    sent_messages: Mutex<Vec<Vec<ChatMessage>>>,
}

#[async_trait::async_trait]
impl LlmAdapter for RecordingAdapter {
    fn readiness(&self) -> Readiness {
        Readiness::Ready
    }

    async fn send(
        &self,
        messages: &[ChatMessage],
        _tools: &[ToolSchema],
    ) -> Result<Vec<ResponseEvent>, CoreError> {
        self.sent_messages.lock().unwrap().push(messages.to_vec());

        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            Ok(vec![
                ResponseEvent::ToolCall {
                    id: Some("call_1".to_string()),
                    name: "add_steps".to_string(),
                    arguments: json!({ "descriptions": ["買い出し"] }).into(),
                },
                ResponseEvent::Done {
                    finish_reason: FinishReason::ToolCall,
                },
            ])
        } else {
            Ok(vec![
                ResponseEvent::TextDelta {
                    text: "工程を追加しました".to_string(),
                },
                ResponseEvent::Done {
                    finish_reason: FinishReason::Stop,
                },
            ])
        }
    }
}

/// 1回目はupdate_taskの呼び出し、2回目はツール結果を踏まえた確定応答を返す
/// フェイクアダプタ。実プロバイダを使わずにturn.rsのループを検証する。
struct FakeAdapter {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl LlmAdapter for FakeAdapter {
    fn readiness(&self) -> Readiness {
        Readiness::Ready
    }

    async fn send(
        &self,
        _messages: &[ChatMessage],
        _tools: &[ToolSchema],
    ) -> Result<Vec<ResponseEvent>, CoreError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            Ok(vec![
                ResponseEvent::ToolCall {
                    id: Some("call_1".to_string()),
                    name: "update_task".to_string(),
                    arguments: json!({ "title": "買い物" }).into(),
                },
                ResponseEvent::Done {
                    finish_reason: FinishReason::ToolCall,
                },
            ])
        } else {
            Ok(vec![
                ResponseEvent::TextDelta {
                    text: "タイトルを更新しました".to_string(),
                },
                ResponseEvent::Done {
                    finish_reason: FinishReason::Stop,
                },
            ])
        }
    }
}

/// 1回目は失敗するツール呼び出し`failing_call`を出し、2回目はその結果を踏まえて
/// 言葉で答えるアダプタ。
struct FailingToolAdapter {
    calls: AtomicUsize,
    failing_call: ResponseEvent,
    tool_results: Mutex<Vec<String>>,
}

impl FailingToolAdapter {
    fn new(failing_call: ResponseEvent) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            failing_call,
            tool_results: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl LlmAdapter for FailingToolAdapter {
    fn readiness(&self) -> Readiness {
        Readiness::Ready
    }

    async fn send(
        &self,
        messages: &[ChatMessage],
        _tools: &[ToolSchema],
    ) -> Result<Vec<ResponseEvent>, CoreError> {
        for message in messages {
            if let ChatMessage::Tool { content, .. } = message {
                self.tool_results.lock().unwrap().push(content.clone());
            }
        }

        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            Ok(vec![
                self.failing_call.clone(),
                ResponseEvent::Done {
                    finish_reason: FinishReason::ToolCall,
                },
            ])
        } else {
            Ok(vec![
                ResponseEvent::TextDelta {
                    text: "その工程は見つかりませんでした".to_string(),
                },
                ResponseEvent::Done {
                    finish_reason: FinishReason::Stop,
                },
            ])
        }
    }
}

/// 1回の応答に複数のtool_callsが載るケース(取りこぼしの回帰検知)。
struct MultiToolCallAdapter {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl LlmAdapter for MultiToolCallAdapter {
    fn readiness(&self) -> Readiness {
        Readiness::Ready
    }

    async fn send(
        &self,
        _messages: &[ChatMessage],
        _tools: &[ToolSchema],
    ) -> Result<Vec<ResponseEvent>, CoreError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            Ok(vec![
                ResponseEvent::ToolCall {
                    id: Some("call_1".to_string()),
                    name: "add_steps".to_string(),
                    arguments: json!({ "descriptions": ["買い出し"] }).into(),
                },
                ResponseEvent::ToolCall {
                    id: Some("call_2".to_string()),
                    name: "update_task".to_string(),
                    arguments: json!({ "title": "買い物" }).into(),
                },
                ResponseEvent::Done {
                    finish_reason: FinishReason::ToolCall,
                },
            ])
        } else {
            Ok(vec![
                ResponseEvent::TextDelta {
                    text: "両方処理しました".to_string(),
                },
                ResponseEvent::Done {
                    finish_reason: FinishReason::Stop,
                },
            ])
        }
    }
}

/// APIプロバイダーが失敗を返すケース(Issue #40)。
struct FailingAdapter;

#[async_trait::async_trait]
impl LlmAdapter for FailingAdapter {
    fn readiness(&self) -> Readiness {
        Readiness::Ready
    }

    async fn send(
        &self,
        _messages: &[ChatMessage],
        _tools: &[ToolSchema],
    ) -> Result<Vec<ResponseEvent>, CoreError> {
        Err(CoreError::Llm("http 401: invalid api key".to_string()))
    }
}

/// テキストもツール呼び出しも無い応答を返すケース(Issue #40)。
struct EmptyResponseAdapter;

#[async_trait::async_trait]
impl LlmAdapter for EmptyResponseAdapter {
    fn readiness(&self) -> Readiness {
        Readiness::Ready
    }

    async fn send(
        &self,
        _messages: &[ChatMessage],
        _tools: &[ToolSchema],
    ) -> Result<Vec<ResponseEvent>, CoreError> {
        Ok(vec![ResponseEvent::Done {
            finish_reason: FinishReason::Stop,
        }])
    }
}

/// 毎ラウンドtool_callsを返し続け、ツール呼び出し回数の上限到達を起こすケース(Issue #40)。
struct AlwaysToolCallAdapter;

#[async_trait::async_trait]
impl LlmAdapter for AlwaysToolCallAdapter {
    fn readiness(&self) -> Readiness {
        Readiness::Ready
    }

    async fn send(
        &self,
        _messages: &[ChatMessage],
        _tools: &[ToolSchema],
    ) -> Result<Vec<ResponseEvent>, CoreError> {
        Ok(vec![
            ResponseEvent::ToolCall {
                id: Some("call_1".to_string()),
                name: "add_steps".to_string(),
                arguments: json!({ "descriptions": ["買い出し"] }).into(),
            },
            ResponseEvent::Done {
                finish_reason: FinishReason::ToolCall,
            },
        ])
    }
}

/// モデル未選択・APIキー未設定を模すケース(Issue #40)。
struct UnreadyAdapter(Readiness);

#[async_trait::async_trait]
impl LlmAdapter for UnreadyAdapter {
    fn readiness(&self) -> Readiness {
        self.0
    }

    async fn send(
        &self,
        _messages: &[ChatMessage],
        _tools: &[ToolSchema],
    ) -> Result<Vec<ResponseEvent>, CoreError> {
        panic!("readiness()がReadyでない場合、sendは呼ばれないはず");
    }
}

/// テキストのみを返す固定応答アダプタ(Issue #41: 編集・再試行のテスト用)。
/// ツール呼び出しループの検証は既存のFakeAdapter等が担っているため、ここでは
/// 「渡された文言をそのまま最終応答として返す」だけの単純なものにする。
struct TextAdapter {
    replies: Mutex<Vec<String>>,
}

impl TextAdapter {
    fn one(text: &str) -> Self {
        TextAdapter {
            replies: Mutex::new(vec![text.to_string()]),
        }
    }
}

#[async_trait::async_trait]
impl LlmAdapter for TextAdapter {
    fn readiness(&self) -> Readiness {
        Readiness::Ready
    }

    async fn send(
        &self,
        _messages: &[ChatMessage],
        _tools: &[ToolSchema],
    ) -> Result<Vec<ResponseEvent>, CoreError> {
        let text = self.replies.lock().unwrap().remove(0);
        Ok(vec![
            ResponseEvent::TextDelta { text },
            ResponseEvent::Done {
                finish_reason: FinishReason::Stop,
            },
        ])
    }
}

/// 1回目はツール呼び出しの前に思考を出し、2回目は思考の後に最終応答を出すケース
/// (Issue #42)。ラウンドごとに思考が正しい行に紐付き、モデルへの再送信には
/// 一切含まれないことを検証する。送信された発言列も記録し、再送信への非混入を確認する。
struct ReasoningAdapter {
    calls: AtomicUsize,
    sent_messages: Mutex<Vec<Vec<ChatMessage>>>,
}

#[async_trait::async_trait]
impl LlmAdapter for ReasoningAdapter {
    fn readiness(&self) -> Readiness {
        Readiness::Ready
    }

    async fn send(
        &self,
        messages: &[ChatMessage],
        _tools: &[ToolSchema],
    ) -> Result<Vec<ResponseEvent>, CoreError> {
        self.sent_messages.lock().unwrap().push(messages.to_vec());
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            Ok(vec![
                ResponseEvent::ReasoningDelta {
                    text: "工程を追加すべきか考える".to_string(),
                },
                ResponseEvent::ToolCall {
                    id: Some("call_1".to_string()),
                    name: "add_steps".to_string(),
                    arguments: json!({ "descriptions": ["買い出し"] }).into(),
                },
                ResponseEvent::Done {
                    finish_reason: FinishReason::ToolCall,
                },
            ])
        } else {
            Ok(vec![
                ResponseEvent::ReasoningDelta {
                    text: "結果を報告する文面を考える".to_string(),
                },
                ResponseEvent::TextDelta {
                    text: "工程を追加しました".to_string(),
                },
                ResponseEvent::Done {
                    finish_reason: FinishReason::Stop,
                },
            ])
        }
    }
}

fn seed_task(conn: &Connection) -> i64 {
    let now = db::now_iso8601();
    conn.execute(
        "INSERT INTO tasks (created_at, updated_at) VALUES (?1, ?1)",
        [&now],
    )
    .unwrap();
    conn.last_insert_rowid()
}

/// 外部(MCP)サーバーに繋がらなくても、そのターンは内部ツールだけで進む(Issue #44)。
/// 登録した1台が落ちているだけでチャットが使えなくなってはならない。
#[tokio::test]
async fn run_turn_continues_when_an_mcp_server_cannot_be_reached() {
    let conn = db::open_in_memory().unwrap();
    let task_id = seed_task(&conn);
    let adapter = FakeAdapter {
        calls: AtomicUsize::new(0),
    };
    let db = Arc::new(Mutex::new(conn));

    let servers = vec![McpServerConfig {
        id: "srv".to_string(),
        name: "broken".to_string(),
        enabled: true,
        endpoint: McpEndpoint::Stdio {
            // 存在しないコマンド。接続の時点で失敗する。
            command: "scitl-no-such-mcp-server".to_string(),
            args: Vec::new(),
            env_refs: Vec::new(),
        },
        enabled_tools: ["anything".to_string()].into_iter().collect(),
    }];
    let catalog = ToolCatalog::new();

    let events = run_turn(
        db.clone(),
        Some(&adapter),
        task_id,
        "タイトルを「買い物」にして".to_string(),
        &SystemPrompts::default(),
        &McpAccess::new(&servers, &catalog),
        ToolLimits::default(),
    )
    .await
    .unwrap();

    assert!(events
        .iter()
        .any(|e| matches!(e, ResponseEvent::TextDelta { text } if text.contains("更新しました"))));
    // 取得できなかったサーバーはキャッシュにも載せない(次のターンでもう一度試す)。
    assert!(catalog.get("srv").is_none());
}

#[tokio::test]
async fn run_turn_executes_tool_then_persists_final_reply() {
    let conn = db::open_in_memory().unwrap();
    let task_id = seed_task(&conn);
    let adapter = FakeAdapter {
        calls: AtomicUsize::new(0),
    };
    let db = Arc::new(Mutex::new(conn));

    let events = run_turn(
        db.clone(),
        Some(&adapter),
        task_id,
        "タイトルを「買い物」にして".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await
    .unwrap();

    assert!(events
        .iter()
        .any(|e| matches!(e, ResponseEvent::ToolCall { name, .. } if name == "update_task")));
    assert!(events
        .iter()
        .any(|e| matches!(e, ResponseEvent::TextDelta { text } if text.contains("更新しました"))));

    let conn = db.lock().unwrap();
    let task = db::tasks::get_task(&conn, task_id).unwrap();
    assert_eq!(task.title.as_deref(), Some("買い物"));

    let messages = db::messages::list_for_task(&conn, task_id).unwrap();
    let roles_kinds: Vec<_> = messages
        .iter()
        .map(|m| (m.role.as_str(), m.kind.as_str()))
        .collect();
    assert_eq!(
        roles_kinds,
        vec![
            ("user", "normal"),
            ("assistant", "tool_execution"),
            ("assistant", "normal"),
        ]
    );
}

#[tokio::test]
async fn run_turn_rebuilds_system_prompt_and_returns_tool_round_trip_within_the_turn() {
    let conn = db::open_in_memory().unwrap();
    let task_id = seed_task(&conn);
    let adapter = RecordingAdapter {
        calls: AtomicUsize::new(0),
        sent_messages: Mutex::new(Vec::new()),
    };
    let db = Arc::new(Mutex::new(conn));

    let prompts_config = SystemPrompts {
        base: Some("base prompt"),
        task_chat: Some("task chat prompt"),
    };
    run_turn(
        db.clone(),
        Some(&adapter),
        task_id,
        "工程を追加して".to_string(),
        &prompts_config,
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await
    .unwrap();

    let rounds = adapter.sent_messages.into_inner().unwrap();
    assert_eq!(rounds.len(), 2);

    // 1ラウンド目: システムプロンプトに基本/タスクチャット用の両方が入り、まだ工程は無い。
    let round1_system = system_prompt_content(&rounds[0][0]);
    assert!(round1_system.contains("base prompt"));
    assert!(round1_system.contains("task chat prompt"));
    assert!(!round1_system.contains("買い出し"));

    // 2ラウンド目: システムプロンプトの最新状態JSONにadd_stepsの結果が反映される
    // (次ターン以降の履歴の代替。principles.md 3節)。
    let round2_system = system_prompt_content(&rounds[1][0]);
    assert!(round2_system.contains("買い出し"));

    // 同時に、直前のツール呼び出しと結果が実メッセージとして返る
    // (同一ターン内のループはこちらが頼り。tools.md 4節)。これが無いと、
    // モデルが「自分がさっき呼んだ」ことを認識できず同じツールを呼び直してしまう
    // (Issue #38で実機確認された不具合)。
    let round2_tail = &rounds[1][rounds[1].len() - 2..];
    match &round2_tail[0] {
        ChatMessage::Assistant {
            content,
            tool_calls,
        } => {
            assert_eq!(tool_calls.len(), 1);
            assert_eq!(tool_calls[0].name, "add_steps");
            assert_eq!(tool_calls[0].id.as_deref(), Some("call_1"));
            assert!(content.is_none());
        }
        other => panic!("expected Assistant with tool_calls, got {other:?}"),
    }
    match &round2_tail[1] {
        ChatMessage::Tool {
            tool_call_id,
            content,
        } => {
            assert_eq!(tool_call_id.as_deref(), Some("call_1"));
            assert!(content.contains("買い出し"));
        }
        other => panic!("expected Tool, got {other:?}"),
    }

    // DBには実行記録(tool_execution)と最終応答(normal)だけが残る。往復用の
    // assistant(tool_calls)/toolはDBの行としては存在しない(このターン限りのため)。
    let conn = db.lock().unwrap();
    let messages = db::messages::list_for_task(&conn, task_id).unwrap();
    let roles_kinds: Vec<_> = messages
        .iter()
        .map(|m| (m.role.as_str(), m.kind.as_str()))
        .collect();
    assert_eq!(
        roles_kinds,
        vec![
            ("user", "normal"),
            ("assistant", "tool_execution"),
            ("assistant", "normal"),
        ]
    );
}

/// 送信日時はユーザー発言の`sent_at`として本文と分けて運ぶ(Issue #68)。
/// 本文には混ぜず、アシスタント発言には付けない。
#[tokio::test]
async fn history_carries_send_time_beside_the_user_text() {
    let conn = db::open_in_memory().unwrap();
    let task_id = seed_task(&conn);
    let adapter = RecordingAdapter {
        calls: AtomicUsize::new(0),
        sent_messages: Mutex::new(Vec::new()),
    };
    let db = Arc::new(Mutex::new(conn));

    for text in ["工程を追加して", "ありがとう"] {
        run_turn(
            db.clone(),
            Some(&adapter),
            task_id,
            text.to_string(),
            &SystemPrompts::default(),
            &McpAccess::none(),
            ToolLimits::default(),
        )
        .await
        .unwrap();
    }

    let stored_user_times: Vec<String> = {
        let conn = db.lock().unwrap();
        db::messages::list_for_task(&conn, task_id)
            .unwrap()
            .into_iter()
            .filter(|m| m.role == "user")
            .map(|m| m.created_at)
            .collect()
    };
    assert_eq!(stored_user_times.len(), 2);

    // 2ターン目の履歴: user(1ターン目) / assistant / user(2ターン目)。
    let rounds = adapter.sent_messages.into_inner().unwrap();
    let last = rounds.last().unwrap();
    let history = &last[1..];
    match &history[0] {
        ChatMessage::User { text, sent_at } => {
            assert_eq!(text, "工程を追加して");
            assert_eq!(sent_at.as_deref(), Some(stored_user_times[0].as_str()));
        }
        other => panic!("expected User, got {other:?}"),
    }
    match &history[1] {
        // アシスタント発言に日時は付けない(モデルが形を真似て応答に書き出すのを避ける)。
        ChatMessage::Assistant { content, .. } => {
            assert_eq!(content.as_deref(), Some("工程を追加しました"));
        }
        other => panic!("expected Assistant, got {other:?}"),
    }
    match &history[2] {
        ChatMessage::User { text, sent_at } => {
            assert_eq!(text, "ありがとう");
            assert_eq!(sent_at.as_deref(), Some(stored_user_times[1].as_str()));
        }
        other => panic!("expected User, got {other:?}"),
    }
}

#[tokio::test]
async fn run_turn_executes_every_tool_call_in_a_single_response() {
    let conn = db::open_in_memory().unwrap();
    let task_id = seed_task(&conn);
    let adapter = MultiToolCallAdapter {
        calls: AtomicUsize::new(0),
    };
    let db = Arc::new(Mutex::new(conn));

    run_turn(
        db.clone(),
        Some(&adapter),
        task_id,
        "工程を追加してタイトルも変えて".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await
    .unwrap();

    let conn = db.lock().unwrap();
    let task = db::tasks::get_task(&conn, task_id).unwrap();
    assert_eq!(task.title.as_deref(), Some("買い物"));
    let steps = db::task_steps::list_for_task(&conn, task_id).unwrap();
    assert_eq!(steps.len(), 1);

    // 2件とも実行記録が残る(1つ目で上書きされて取りこぼされない)。
    let messages = db::messages::list_for_task(&conn, task_id).unwrap();
    let tool_execution_count = messages
        .iter()
        .filter(|m| m.kind == "tool_execution")
        .count();
    assert_eq!(tool_execution_count, 2);
}

/// 内部ツール1件の失敗ではターンを止めず、`{"error": ...}`の結果として記録し、
/// モデルにも返して会話を続ける(Issue #58、docs/spec/principles.md 3節)。
#[tokio::test]
async fn run_turn_reports_internal_tool_failure_to_the_model_and_continues() {
    let conn = db::open_in_memory().unwrap();
    let task_id = seed_task(&conn);
    // 存在しない工程を指したupdate_step
    let adapter = FailingToolAdapter::new(ResponseEvent::ToolCall {
        id: Some("call_1".to_string()),
        name: "update_step".to_string(),
        arguments: json!({ "step_id": 9999, "done": true }).into(),
    });
    let db = Arc::new(Mutex::new(conn));

    let events = run_turn(
        db.clone(),
        Some(&adapter),
        task_id,
        "1番目の工程を完了にして".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await
    .unwrap();

    assert!(events.iter().any(
        |e| matches!(e, ResponseEvent::TextDelta { text } if text.contains("見つかりませんでした"))
    ));

    // 失敗はモデルへのツール結果として渡る(モデルが失敗を認識して続けられる)。
    let tool_results = adapter.tool_results.lock().unwrap();
    assert_eq!(tool_results.len(), 1);
    let sent: serde_json::Value = serde_json::from_str(&tool_results[0]).unwrap();
    assert!(sent.get("error").is_some(), "got {sent}");

    let conn = db.lock().unwrap();
    let messages = db::messages::list_for_task(&conn, task_id).unwrap();
    let roles_kinds: Vec<_> = messages
        .iter()
        .map(|m| (m.role.as_str(), m.kind.as_str()))
        .collect();
    assert_eq!(
        roles_kinds,
        vec![
            ("user", "normal"),
            ("assistant", "tool_execution"),
            ("assistant", "normal"),
        ]
    );

    // 実行記録の`result`に`error`キーが立つ(画面の「エラーの有無」表示の前提、Issue #42)。
    let record = messages
        .iter()
        .find(|m| m.kind == "tool_execution")
        .unwrap();
    let content: serde_json::Value = serde_json::from_str(&record.content).unwrap();
    assert!(content["result"].get("error").is_some(), "got {content}");
}

/// 引数がJSONとして読めないツール呼び出しは、実行せずに失敗としてモデルへ返し、
/// ターンを続ける(Issue #121、docs/spec/principles.md 3節)。
#[tokio::test]
async fn run_turn_reports_malformed_tool_arguments_to_the_model_without_running_the_tool() {
    let conn = db::open_in_memory().unwrap();
    let task_id = seed_task(&conn);
    let title_before = db::tasks::get_task(&conn, task_id).unwrap().title;
    let adapter = FailingToolAdapter::new(ResponseEvent::ToolCall {
        id: Some("call_1".to_string()),
        name: "update_task".to_string(),
        arguments: ToolArguments::parse("{\"title\": ".to_string()),
    });
    let db = Arc::new(Mutex::new(conn));

    let events = run_turn(
        db.clone(),
        Some(&adapter),
        task_id,
        "タイトルを変えて".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await
    .unwrap();

    assert!(!events.iter().any(|e| matches!(
        e,
        ResponseEvent::Done {
            finish_reason: FinishReason::Error
        }
    )));

    let tool_results = adapter.tool_results.lock().unwrap();
    assert_eq!(tool_results.len(), 1);
    let sent: serde_json::Value = serde_json::from_str(&tool_results[0]).unwrap();
    assert!(sent.get("error").is_some(), "got {sent}");

    let conn = db.lock().unwrap();
    assert_eq!(
        db::tasks::get_task(&conn, task_id).unwrap().title,
        title_before
    );
    let messages = db::messages::list_for_task(&conn, task_id).unwrap();
    let record = messages
        .iter()
        .find(|m| m.kind == "tool_execution")
        .unwrap();
    let content: serde_json::Value = serde_json::from_str(&record.content).unwrap();
    assert_eq!(content["arguments"], "{\"title\": ");
    assert!(content["result"].get("error").is_some(), "got {content}");
    assert!(messages
        .iter()
        .any(|m| m.role == "assistant" && m.kind == "normal"));
}

/// LLM呼び出しの失敗はErrで落とさず、エラー発言として保存される(Issue #40)。
#[tokio::test]
async fn run_turn_persists_error_message_instead_of_returning_err() {
    let conn = db::open_in_memory().unwrap();
    let task_id = seed_task(&conn);
    let db = Arc::new(Mutex::new(conn));

    let events = run_turn(
        db.clone(),
        Some(&FailingAdapter),
        task_id,
        "こんにちは".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await
    .unwrap();
    assert!(events.iter().any(|e| matches!(
        e,
        ResponseEvent::Done {
            finish_reason: FinishReason::Error
        }
    )));

    let conn = db.lock().unwrap();
    let messages = db::messages::list_for_task(&conn, task_id).unwrap();
    let error_message = messages.iter().find(|m| m.role == "error").unwrap();
    assert_eq!(error_message.error_kind.as_deref(), Some("auth"));
}

/// 空応答(テキストもツール呼び出しも無い)もエラー発言として保存される(Issue #40)。
#[tokio::test]
async fn run_turn_persists_error_message_for_empty_response() {
    let conn = db::open_in_memory().unwrap();
    let task_id = seed_task(&conn);
    let db = Arc::new(Mutex::new(conn));

    run_turn(
        db.clone(),
        Some(&EmptyResponseAdapter),
        task_id,
        "こんにちは".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await
    .unwrap();

    let conn = db.lock().unwrap();
    let messages = db::messages::list_for_task(&conn, task_id).unwrap();
    let error_message = messages.iter().find(|m| m.role == "error").unwrap();
    assert_eq!(error_message.error_kind.as_deref(), Some("empty_response"));
}

/// ツール呼び出しの上限到達もエラー発言として保存される(Issue #40)。
#[tokio::test]
async fn run_turn_persists_error_message_for_tool_round_limit() {
    let conn = db::open_in_memory().unwrap();
    let task_id = seed_task(&conn);
    let db = Arc::new(Mutex::new(conn));

    run_turn(
        db.clone(),
        Some(&AlwaysToolCallAdapter),
        task_id,
        "工程を追加して".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await
    .unwrap();

    let conn = db.lock().unwrap();
    let messages = db::messages::list_for_task(&conn, task_id).unwrap();
    let error_message = messages.iter().find(|m| m.role == "error").unwrap();
    assert_eq!(
        error_message.error_kind.as_deref(),
        Some("tool_round_limit")
    );
    // 既定値の4ラウンドぶん回ってから打ち切られる(1ラウンドにつきツール実行記録が1件)。
    assert_eq!(tool_execution_count(&messages), 4);
}

/// 設定したラウンド数の上限がそのまま効く(Issue #71)。`turn.rs`が定数ではなく
/// 渡された値を見ていることを、実際に回った回数で確かめる。
#[tokio::test]
async fn run_turn_honors_the_configured_max_tool_rounds() {
    let conn = db::open_in_memory().unwrap();
    let task_id = seed_task(&conn);
    let db = Arc::new(Mutex::new(conn));

    run_turn(
        db.clone(),
        Some(&AlwaysToolCallAdapter),
        task_id,
        "工程を追加して".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits {
            max_rounds_per_turn: 2,
            ..ToolLimits::default()
        },
    )
    .await
    .unwrap();

    let conn = db.lock().unwrap();
    let messages = db::messages::list_for_task(&conn, task_id).unwrap();
    assert_eq!(tool_execution_count(&messages), 2);
    let error_message = messages.iter().find(|m| m.role == "error").unwrap();
    assert_eq!(
        error_message.error_kind.as_deref(),
        Some("tool_round_limit")
    );
}

/// ツール実行に使える合計時間が最初から無ければ、ラウンド数に余裕があっても1回も
/// 呼ばずに打ち切る(Issue #71)。`ToolLimits::from_config`は0を未設定として弾くので、
/// この値は設定からは作れない。ここで確かめたいのは「使い切ったのに呼べる」状態を
/// 作らないことなので、上限そのものを直接渡す。
#[tokio::test]
async fn run_turn_persists_error_message_when_the_tool_time_budget_is_exhausted() {
    let conn = db::open_in_memory().unwrap();
    let task_id = seed_task(&conn);
    let db = Arc::new(Mutex::new(conn));

    run_turn(
        db.clone(),
        Some(&AlwaysToolCallAdapter),
        task_id,
        "工程を追加して".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits {
            total_timeout: std::time::Duration::ZERO,
            ..ToolLimits::default()
        },
    )
    .await
    .unwrap();

    let conn = db.lock().unwrap();
    let messages = db::messages::list_for_task(&conn, task_id).unwrap();
    let error_message = messages.iter().find(|m| m.role == "error").unwrap();
    assert_eq!(error_message.error_kind.as_deref(), Some("tool_timeout"));
    // 最初のツールを実行しきる前に打ち切るので、実行記録は残らない。
    assert_eq!(tool_execution_count(&messages), 0);
}

/// 使った時間が積み上がって上限に届いたら、次の呼び出しへ進まずに打ち切る(Issue #71)。
/// 上限を1ナノ秒にすると、1回目の実行は上限に届いていないので走り、その実行時間だけで
/// 必ず上限を超えるため、2回目の手前で打ち切られる。実時間の長さには依存しない。
#[tokio::test]
async fn run_turn_stops_before_the_next_tool_call_once_the_budget_is_used_up() {
    let conn = db::open_in_memory().unwrap();
    let task_id = seed_task(&conn);
    let db = Arc::new(Mutex::new(conn));

    run_turn(
        db.clone(),
        Some(&AlwaysToolCallAdapter),
        task_id,
        "工程を追加して".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits {
            total_timeout: std::time::Duration::from_nanos(1),
            ..ToolLimits::default()
        },
    )
    .await
    .unwrap();

    let conn = db.lock().unwrap();
    let messages = db::messages::list_for_task(&conn, task_id).unwrap();
    let error_message = messages.iter().find(|m| m.role == "error").unwrap();
    assert_eq!(error_message.error_kind.as_deref(), Some("tool_timeout"));
    // 1回目は最後まで走る(途中で打ち切らないので、実行記録が必ず残る)。
    assert_eq!(tool_execution_count(&messages), 1);
}

fn tool_execution_count(messages: &[db::messages::Message]) -> usize {
    messages
        .iter()
        .filter(|m| m.kind == "tool_execution")
        .count()
}

/// プロバイダー未選択(`None`)はエラー発言として保存され、`send`は一切呼ばれない
/// (Issue #40)。
#[tokio::test]
async fn run_turn_persists_error_message_when_no_provider_is_configured() {
    let conn = db::open_in_memory().unwrap();
    let task_id = seed_task(&conn);
    let db = Arc::new(Mutex::new(conn));

    run_turn(
        db.clone(),
        None,
        task_id,
        "こんにちは".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await
    .unwrap();

    let conn = db.lock().unwrap();
    let messages = db::messages::list_for_task(&conn, task_id).unwrap();
    let error_message = messages.iter().find(|m| m.role == "error").unwrap();
    assert_eq!(error_message.error_kind.as_deref(), Some("no_provider"));
}

/// モデル未選択・APIキー未設定は`send`を呼ぶ前に検知され、エラー発言として保存される
/// (Issue #40)。
#[tokio::test]
async fn run_turn_persists_error_message_for_unready_adapter_without_calling_send() {
    let conn = db::open_in_memory().unwrap();
    let task_id = seed_task(&conn);
    let db = Arc::new(Mutex::new(conn));

    run_turn(
        db.clone(),
        Some(&UnreadyAdapter(Readiness::NoModel)),
        task_id,
        "こんにちは".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await
    .unwrap();

    let conn = db.lock().unwrap();
    let messages = db::messages::list_for_task(&conn, task_id).unwrap();
    let error_message = messages.iter().find(|m| m.role == "error").unwrap();
    assert_eq!(error_message.error_kind.as_deref(), Some("no_model"));
}

/// エラー発言は次ターンのAPI送信用履歴に混入しない(`legacy/backend.md` 4節手順2)。
#[tokio::test]
async fn error_messages_are_excluded_from_the_next_turns_history() {
    let conn = db::open_in_memory().unwrap();
    let task_id = seed_task(&conn);
    let db = Arc::new(Mutex::new(conn));

    run_turn(
        db.clone(),
        Some(&FailingAdapter),
        task_id,
        "1回目".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await
    .unwrap();

    let adapter = RecordingAdapter {
        calls: AtomicUsize::new(0),
        sent_messages: Mutex::new(Vec::new()),
    };
    run_turn(
        db.clone(),
        Some(&adapter),
        task_id,
        "2回目".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await
    .unwrap();

    let rounds = adapter.sent_messages.into_inner().unwrap();
    let first_round = &rounds[0];
    let has_error_content = first_round.iter().any(|m| match m {
        ChatMessage::User { text: content, .. }
        | ChatMessage::Assistant {
            content: Some(content),
            ..
        } => content.contains("APIキーが正しくない"),
        _ => false,
    });
    assert!(!has_error_content);
}

/// 編集(Issue #41): 対象のユーザー発言以降(自身を含む)が論理削除され、編集後の内容から
/// 会話が再生成される。旧アシスタント応答は履歴から消え、新しい応答だけが残る。
#[tokio::test]
async fn edit_user_message_truncates_and_regenerates() {
    let conn = db::open_in_memory().unwrap();
    let task_id = seed_task(&conn);
    let db = Arc::new(Mutex::new(conn));

    run_turn(
        db.clone(),
        Some(&TextAdapter::one("応答A")),
        task_id,
        "元の質問".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await
    .unwrap();

    let user_message_id = {
        let conn = db.lock().unwrap();
        let messages = db::messages::list_for_task(&conn, task_id).unwrap();
        messages.iter().find(|m| m.role == "user").unwrap().id
    };

    edit_user_message(
        db.clone(),
        Some(&TextAdapter::one("応答B")),
        task_id,
        user_message_id,
        "編集後の質問".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await
    .unwrap();

    let conn = db.lock().unwrap();
    let messages = db::messages::list_for_task(&conn, task_id).unwrap();
    let contents: Vec<_> = messages.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(contents, vec!["編集後の質問", "応答B"]);

    // 旧ユーザー発言は物理削除ではなく論理削除(deleted_atが立つだけ)。
    let deleted_at: Option<String> = conn
        .query_row(
            "SELECT deleted_at FROM messages WHERE id = ?1",
            [user_message_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(deleted_at.is_some());
}

/// 編集(Issue #95): ツールを実行したターンを編集で破棄しても、編集後の発言は**元の位置**に
/// 現れる。編集後の本文は新しい行として挿入されるが、生き残る通常発言はすべて対象より前の
/// idなので、破棄されたターンのツール実行記録さえ会話から外れれば順序は元のままになる。
/// 記録はDBに残す(`data-model.md`「ツール実行記録は……対象に含めない」)。
#[tokio::test]
async fn editing_a_turn_that_ran_tools_keeps_the_message_in_place() {
    let conn = db::open_in_memory().unwrap();
    let task_id = seed_task(&conn);
    let db = Arc::new(Mutex::new(conn));

    // 1ターン目: 編集対象より前に来る、生き残る会話。
    run_turn(
        db.clone(),
        Some(&TextAdapter::one("応答A")),
        task_id,
        "最初の質問".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await
    .unwrap();

    // 2ターン目: ツールを実行するターン。これを編集で破棄する。
    run_turn(
        db.clone(),
        Some(&FakeAdapter {
            calls: AtomicUsize::new(0),
        }),
        task_id,
        "タイトル決めて".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await
    .unwrap();

    let target_id = {
        let conn = db.lock().unwrap();
        let messages = db::messages::list_for_task(&conn, task_id).unwrap();
        messages
            .iter()
            .find(|m| m.content == "タイトル決めて")
            .unwrap()
            .id
    };

    edit_user_message(
        db.clone(),
        Some(&TextAdapter::one("応答B")),
        task_id,
        target_id,
        "編集後の質問".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await
    .unwrap();

    let conn = db.lock().unwrap();
    let messages = db::messages::list_for_task(&conn, task_id).unwrap();
    let contents: Vec<_> = messages.iter().map(|m| m.content.as_str()).collect();

    // 編集後の発言は「応答A」の直後、つまり編集前と同じ位置。破棄されたターンの
    // ツール実行記録が間に挟まらない(これが挟まると新規送信と見分けが付かなくなる)。
    assert_eq!(
        contents,
        vec!["最初の質問", "応答A", "編集後の質問", "応答B"]
    );

    // 記録そのものはDBに残っている(表示から外すだけで、消してはいない)。
    let tool_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages
             WHERE kind = 'tool_execution' AND deleted_at IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tool_rows, 1);
}

/// 思考(reasoning)は該当する行の`reasoning`列に保存され、モデルへの再送信には
/// 一切含まれないことを検証する(Issue #42、principles.md 3節「思考は履歴に送り返さない」)。
#[tokio::test]
async fn run_turn_persists_reasoning_per_row_without_sending_it_back() {
    let conn = db::open_in_memory().unwrap();
    let task_id = seed_task(&conn);
    let adapter = ReasoningAdapter {
        calls: AtomicUsize::new(0),
        sent_messages: Mutex::new(Vec::new()),
    };
    let db = Arc::new(Mutex::new(conn));

    run_turn(
        db.clone(),
        Some(&adapter),
        task_id,
        "工程を追加して".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await
    .unwrap();

    let conn = db.lock().unwrap();
    let messages = db::messages::list_for_task(&conn, task_id).unwrap();
    let by_kind: Vec<_> = messages
        .iter()
        .map(|m| (m.role.as_str(), m.kind.as_str(), m.reasoning.as_deref()))
        .collect();
    assert_eq!(
        by_kind,
        vec![
            ("user", "normal", None),
            (
                "assistant",
                "tool_execution",
                Some("工程を追加すべきか考える")
            ),
            ("assistant", "normal", Some("結果を報告する文面を考える")),
        ]
    );

    // モデルへ送り返す発言列(ChatMessage)には思考が現れる余地が無い
    // (`ChatMessage`に思考を運ぶ構成要素自体が無いため型で保証される)。
    // ここでは実際に送信された本文にも思考テキストが混入していないことを重ねて確認する。
    let rounds = adapter.sent_messages.into_inner().unwrap();
    for round in &rounds {
        for message in round {
            match message {
                ChatMessage::User { text: content, .. }
                | ChatMessage::Assistant {
                    content: Some(content),
                    ..
                } => {
                    assert!(!content.contains("考える"));
                }
                ChatMessage::Tool { content, .. } => {
                    assert!(!content.contains("考える"));
                }
                _ => {}
            }
        }
    }
}

/// 編集の対象はユーザー発言のみ。アシスタント発言を編集しようとするとエラーになる。
#[tokio::test]
async fn edit_user_message_rejects_assistant_target() {
    let conn = db::open_in_memory().unwrap();
    let task_id = seed_task(&conn);
    let db = Arc::new(Mutex::new(conn));

    run_turn(
        db.clone(),
        Some(&TextAdapter::one("応答A")),
        task_id,
        "質問".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await
    .unwrap();

    let assistant_message_id = {
        let conn = db.lock().unwrap();
        let messages = db::messages::list_for_task(&conn, task_id).unwrap();
        messages.iter().find(|m| m.role == "assistant").unwrap().id
    };

    let result = edit_user_message(
        db.clone(),
        Some(&TextAdapter::one("応答B")),
        task_id,
        assistant_message_id,
        "書き換え".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await;
    assert!(result.is_err());
}

/// 再試行(Issue #41): 同じ`turn_id`のまま`attempt_no`が増え、旧アシスタント応答は
/// 表示から外れて新しい応答に置き換わる。対応するユーザー発言はそのまま残る。
#[tokio::test]
async fn retry_assistant_message_keeps_turn_id_and_increments_attempt_no() {
    let conn = db::open_in_memory().unwrap();
    let task_id = seed_task(&conn);
    let db = Arc::new(Mutex::new(conn));

    run_turn(
        db.clone(),
        Some(&TextAdapter::one("応答A")),
        task_id,
        "質問".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await
    .unwrap();

    let (assistant_message_id, original_turn_id) = {
        let conn = db.lock().unwrap();
        let messages = db::messages::list_for_task(&conn, task_id).unwrap();
        let m = messages.iter().find(|m| m.role == "assistant").unwrap();
        (m.id, m.turn_id.clone().unwrap())
    };

    retry_assistant_message(
        db.clone(),
        Some(&TextAdapter::one("応答B")),
        task_id,
        assistant_message_id,
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await
    .unwrap();

    let conn = db.lock().unwrap();
    let messages = db::messages::list_for_task(&conn, task_id).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "user");
    assert_eq!(messages[1].content, "応答B");
    assert_eq!(
        messages[1].turn_id.as_deref(),
        Some(original_turn_id.as_str())
    );
    assert_eq!(messages[1].attempt_no, Some(2));
}

/// 再試行の対象はアシスタント発言のみ。ユーザー発言を再試行しようとするとエラーになる。
#[tokio::test]
async fn retry_assistant_message_rejects_user_target() {
    let conn = db::open_in_memory().unwrap();
    let task_id = seed_task(&conn);
    let db = Arc::new(Mutex::new(conn));

    run_turn(
        db.clone(),
        Some(&TextAdapter::one("応答A")),
        task_id,
        "質問".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await
    .unwrap();

    let user_message_id = {
        let conn = db.lock().unwrap();
        let messages = db::messages::list_for_task(&conn, task_id).unwrap();
        messages.iter().find(|m| m.role == "user").unwrap().id
    };

    let result = retry_assistant_message(
        db.clone(),
        Some(&TextAdapter::one("応答B")),
        task_id,
        user_message_id,
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await;
    assert!(result.is_err());
}

/// 削除(Issue #41): カスケードしない単発の論理削除。対象以外の発言はそのまま残る。
#[tokio::test]
async fn delete_message_removes_only_the_target_without_cascade() {
    let conn = db::open_in_memory().unwrap();
    let task_id = seed_task(&conn);
    let db = Arc::new(Mutex::new(conn));

    run_turn(
        db.clone(),
        Some(&TextAdapter::one("応答1")),
        task_id,
        "1回目".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await
    .unwrap();
    run_turn(
        db.clone(),
        Some(&TextAdapter::one("応答2")),
        task_id,
        "2回目".to_string(),
        &SystemPrompts::default(),
        &McpAccess::none(),
        ToolLimits::default(),
    )
    .await
    .unwrap();

    let first_user_id = {
        let conn = db.lock().unwrap();
        let messages = db::messages::list_for_task(&conn, task_id).unwrap();
        messages.iter().find(|m| m.role == "user").unwrap().id
    };

    delete_message(db.clone(), task_id, first_user_id)
        .await
        .unwrap();

    let conn = db.lock().unwrap();
    let messages = db::messages::list_for_task(&conn, task_id).unwrap();
    let contents: Vec<_> = messages.iter().map(|m| m.content.as_str()).collect();
    // カスケードしないため、1回目の応答・2回目のやり取りはそのまま残る。
    assert_eq!(contents, vec!["応答1", "2回目", "応答2"]);
}
