use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use scitl_core::db;
use scitl_core::db::error::CoreError;
use scitl_core::llm::{ChatMessage, FinishReason, LlmAdapter, Readiness, ResponseEvent, ToolSchema};
use scitl_core::orchestration::{
    delete_message, edit_user_message, retry_assistant_message, run_turn, SystemPrompts,
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
    )
    .await
    .unwrap();

    let conn = db.lock().unwrap();
    let messages = db::messages::list_for_task(&conn, task_id).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "user");
    assert_eq!(messages[1].content, "応答B");
    assert_eq!(messages[1].turn_id.as_deref(), Some(original_turn_id.as_str()));
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
    )
    .await
    .unwrap();
    run_turn(
        db.clone(),
        Some(&TextAdapter::one("応答2")),
        task_id,
        "2回目".to_string(),
        &SystemPrompts::default(),
    )
    .await
    .unwrap();

    let first_user_id = {
        let conn = db.lock().unwrap();
        let messages = db::messages::list_for_task(&conn, task_id).unwrap();
        messages.iter().find(|m| m.role == "user").unwrap().id
    };

    delete_message(db.clone(), task_id, first_user_id).await.unwrap();

    let conn = db.lock().unwrap();
    let messages = db::messages::list_for_task(&conn, task_id).unwrap();
    let contents: Vec<_> = messages.iter().map(|m| m.content.as_str()).collect();
    // カスケードしないため、1回目の応答・2回目のやり取りはそのまま残る。
    assert_eq!(contents, vec!["応答1", "2回目", "応答2"]);
}
