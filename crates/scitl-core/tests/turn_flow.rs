use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use scitl_core::db;
use scitl_core::db::error::CoreError;
use scitl_core::llm::{ChatMessage, FinishReason, LlmAdapter, ResponseEvent, ToolSchema};
use scitl_core::orchestration::run_turn;
use serde_json::json;

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
        None,
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
