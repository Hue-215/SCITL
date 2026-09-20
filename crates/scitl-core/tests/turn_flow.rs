use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use scitl_core::db;
use scitl_core::db::error::CoreError;
use scitl_core::llm::{ChatMessage, FinishReason, LlmAdapter, Readiness, ResponseEvent, ToolSchema};
use scitl_core::orchestration::{run_turn, SystemPrompts};
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
                    arguments: json!({ "descriptions": ["買い出し"] }),
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
                    arguments: json!({ "title": "買い物" }),
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
                    arguments: json!({ "descriptions": ["買い出し"] }),
                },
                ResponseEvent::ToolCall {
                    id: Some("call_2".to_string()),
                    name: "update_task".to_string(),
                    arguments: json!({ "title": "買い物" }),
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
                arguments: json!({ "descriptions": ["買い出し"] }),
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
                    arguments: json!({ "descriptions": ["買い出し"] }),
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
        ChatMessage::Assistant { content, tool_calls } => {
            assert_eq!(tool_calls.len(), 1);
            assert_eq!(tool_calls[0].name, "add_steps");
            assert_eq!(tool_calls[0].id.as_deref(), Some("call_1"));
            assert!(content.is_none());
        }
        other => panic!("expected Assistant with tool_calls, got {other:?}"),
    }
    match &round2_tail[1] {
        ChatMessage::Tool { tool_call_id, content } => {
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
    )
    .await
    .unwrap();
    assert!(events
        .iter()
        .any(|e| matches!(e, ResponseEvent::Done { finish_reason: FinishReason::Error })));

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
    )
    .await
    .unwrap();

    let conn = db.lock().unwrap();
    let messages = db::messages::list_for_task(&conn, task_id).unwrap();
    let error_message = messages.iter().find(|m| m.role == "error").unwrap();
    assert_eq!(error_message.error_kind.as_deref(), Some("tool_round_limit"));
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
    )
    .await
    .unwrap();

    let rounds = adapter.sent_messages.into_inner().unwrap();
    let first_round = &rounds[0];
    let has_error_content = first_round.iter().any(|m| match m {
        ChatMessage::User(content) | ChatMessage::Assistant { content: Some(content), .. } => {
            content.contains("APIキーが正しくない")
        }
        _ => false,
    });
    assert!(!has_error_content);
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
            ("assistant", "tool_execution", Some("工程を追加すべきか考える")),
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
                ChatMessage::User(content) | ChatMessage::Assistant { content: Some(content), .. } => {
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
