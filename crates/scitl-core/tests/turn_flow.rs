use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use scitl_core::db;
use scitl_core::db::error::CoreError;
use scitl_core::llm::{ChatMessage, FinishReason, LlmAdapter, ResponseEvent, ToolSchema};
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
        &adapter,
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
        &adapter,
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
        &adapter,
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
