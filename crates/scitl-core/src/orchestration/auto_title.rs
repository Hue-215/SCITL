//! 自動タイトル付け(Issue #46、`docs/spec/legacy/backend.md` 2節・4節手順8)。
//!
//! タイトルが未設定(`title IS NULL`)で、かつ会話の内容がある程度定まった時点
//! (説明・締切・工程のいずれかが決まっている、またはユーザー発言が一定数を超えた場合)に、
//! ツール無し・短い応答限定のLLM呼び出しでタイトルを生成する。手動でタイトルが変更済み
//! (`title IS NOT NULL`)なら上書きしない(`docs/spec/rebuild/data-model.md` 1節
//! 「自動命名の判定は`title IS NULL`」)。
//!
//! この処理は`run_turn`本体の応答生成に対して付随的なものであり、失敗してもターンの成否には
//! 影響させない(呼び出し元は本モジュールが返す`Err`をDB書き込み失敗としてログに残すのみで、
//! ユーザーへの応答はそのまま返す)。

use rusqlite::Connection;

use crate::db::error::Result;
use crate::db::{messages, now_iso8601, task_steps, tasks};
use crate::llm::{ChatMessage, LlmAdapter, ResponseEvent};
use crate::orchestration::turn::{db_call, SharedConnection};

/// ユーザー発言がこの件数を超えたら、説明・締切・工程が未設定でも生成条件を満たす
/// (`legacy/backend.md` 2節「ユーザー発言が一定数を超えた場合」の具体的な閾値は旧実装の
/// 記録に残っていないため、早すぎる生成(文脈不足で的外れなタイトルになる)と
/// 遅すぎる生成(ずっと無題のままになる)のバランスで判断した)。
const MIN_USER_MESSAGES: usize = 3;

/// 生成したタイトルの上限文字数。サイドバー等の表示崩れを避けるため、モデルの出力を
/// 信頼せずここで確実に切り詰める。
const MAX_TITLE_CHARS: usize = 40;

/// タイトル生成専用のシステムプロンプト。ツール定義を渡さないことに加え、出力形式を
/// 文言で強く縛ることで短い応答に限定する(`legacy/backend.md` 3節)。
const TITLE_SYSTEM_PROMPT: &str = "\
あなたはタスク管理アプリの内部処理です。これまでの会話から、このタスクのタイトルを\
1つ考えてください。出力は日本語で10〜20文字程度、長くても40文字以内の短い名詞句のみとし、\
説明・前置き・記号・引用符・改行を含めないでください。会話の内容がタスクとしてまだ\
十分に定まっていなくても、現時点で分かる範囲の短いタイトルを必ず1つだけ出力してください。";

/// `run_turn`の末尾(通常応答の保存後)から呼ぶ。DBへの書き込みが失敗した場合のみ`Err`を
/// 返す。生成条件を満たさない・モデル呼び出しが失敗した・応答が空だった、はいずれも
/// `Ok(())`で黙って終える(このターンの主目的である応答の生成・保存は既に完了しており、
/// 付随機能の失敗でターン全体をエラー扱いにしないため)。
pub async fn maybe_generate_title(
    db: SharedConnection,
    adapter: &dyn LlmAdapter,
    task_id: i64,
) -> Result<()> {
    let Some(transcript) =
        db_call(db.clone(), move |conn| load_if_eligible(conn, task_id)).await?
    else {
        return Ok(());
    };

    let request = vec![
        ChatMessage::System(TITLE_SYSTEM_PROMPT.to_string()),
        ChatMessage::User(transcript),
    ];
    // ツール無し(空のツール一覧)・短い応答限定(`legacy/backend.md` 3節)。
    let Ok(events) = adapter.send(&request, &[]).await else {
        return Ok(());
    };

    let mut text = String::new();
    for event in &events {
        if let ResponseEvent::TextDelta { text: delta } = event {
            text.push_str(delta);
        }
    }
    let title = sanitize_title(&text);
    if title.is_empty() {
        return Ok(());
    }

    db_call(db, move |conn| apply_title(conn, task_id, &title)).await
}

/// 生成条件を満たせば会話の書き起こしを返す。満たさなければ`None`
/// (タイトル設定済み、または説明・締切・工程がいずれも無く発言数も閾値未満)。
fn load_if_eligible(conn: &Connection, task_id: i64) -> Result<Option<String>> {
    let task = tasks::get_task(conn, task_id)?;
    if task.title.is_some() {
        return Ok(None);
    }

    let steps = task_steps::list_for_task(conn, task_id)?;
    let history = messages::list_for_task(conn, task_id)?;
    // 通常発言のみを会話の書き起こしに使う。ツール実行記録・エラー発言は除外する
    // (`turn.rs::build_history`と同じ絞り込み。タイトル生成にはJSON化されたツール結果や
    // 定型エラー文言ではなく、ユーザーとモデルの地の文だけがあれば十分なため)。
    let normal: Vec<&messages::Message> = history
        .iter()
        .filter(|m| m.kind == "normal" && (m.role == "user" || m.role == "assistant"))
        .collect();
    let user_count = normal.iter().filter(|m| m.role == "user").count();

    let eligible = task.description.is_some()
        || task.deadline.is_some()
        || !steps.is_empty()
        || user_count >= MIN_USER_MESSAGES;
    if !eligible {
        return Ok(None);
    }

    let transcript = normal
        .iter()
        .map(|m| {
            let speaker = if m.role == "user" { "User" } else { "Assistant" };
            format!("{speaker}: {}", m.content)
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Some(transcript))
}

/// モデルの出力を人間の目に触れるタイトルとして無害化する(`../principles.md` 4節
/// 「自由入力は地の文に混ぜる前にサニタイズする」)。制御文字(改行を含む)を空白に畳み込み、
/// 前後の空白・引用符を除き、連続空白を1つにまとめ、上限文字数で切り詰める。
fn sanitize_title(raw: &str) -> String {
    let collapsed: String = raw.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let trimmed = collapsed
        .trim()
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '「' | '」' | '『' | '』'));
    let squeezed = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    squeezed.chars().take(MAX_TITLE_CHARS).collect()
}

/// `title IS NULL`のときだけ書き込む。読み取り([`load_if_eligible`])からモデル呼び出しの
/// 完了までの間にユーザーが手動でタイトルを設定していた場合、この`WHERE`句が競合を防ぐ
/// (`load_if_eligible`の事前判定だけでは、その間の変更を捕捉できないため)。
fn apply_title(conn: &Connection, task_id: i64, title: &str) -> Result<()> {
    conn.execute(
        "UPDATE tasks SET title = ?1, updated_at = ?2 WHERE id = ?3 AND title IS NULL",
        rusqlite::params![title, now_iso8601(), task_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::llm::{FinishReason, Readiness};
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    struct StubAdapter {
        reply: &'static str,
    }

    #[async_trait]
    impl LlmAdapter for StubAdapter {
        fn readiness(&self) -> Readiness {
            Readiness::Ready
        }

        async fn send(
            &self,
            _messages: &[ChatMessage],
            tools: &[crate::llm::ToolSchema],
        ) -> std::result::Result<Vec<ResponseEvent>, crate::db::error::CoreError> {
            // タイトル生成はツール無しで呼ぶこと自体を検証する。
            assert!(tools.is_empty());
            Ok(vec![
                ResponseEvent::TextDelta { text: self.reply.to_string() },
                ResponseEvent::Done { finish_reason: FinishReason::Stop },
            ])
        }
    }

    fn seed_task_with_user_messages(conn: &Connection, count: usize) -> i64 {
        let task_id = tasks::create_task(conn).unwrap().id;
        for i in 0..count {
            messages::insert_message(
                conn,
                messages::NewMessage {
                    task_id: Some(task_id),
                    role: messages::Role::User,
                    content: &format!("message {i}"),
                    kind: messages::Kind::Normal,
                    source: None,
                    turn: None,
                    error_kind: None,
                },
            )
            .unwrap();
        }
        task_id
    }

    #[test]
    fn not_eligible_below_threshold_with_no_other_signal() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task_with_user_messages(&conn, MIN_USER_MESSAGES - 1);
        assert!(load_if_eligible(&conn, task_id).unwrap().is_none());
    }

    #[test]
    fn eligible_once_user_message_threshold_reached() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task_with_user_messages(&conn, MIN_USER_MESSAGES);
        let transcript = load_if_eligible(&conn, task_id).unwrap();
        assert!(transcript.is_some());
        assert!(transcript.unwrap().contains("message 0"));
    }

    #[test]
    fn eligible_when_deadline_set_even_below_message_threshold() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task_with_user_messages(&conn, 1);
        tasks::update_task(
            &conn,
            task_id,
            tasks::TaskUpdate {
                deadline: Some("2026-01-01".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(load_if_eligible(&conn, task_id).unwrap().is_some());
    }

    #[test]
    fn not_eligible_when_title_already_set() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task_with_user_messages(&conn, MIN_USER_MESSAGES);
        tasks::update_task(
            &conn,
            task_id,
            tasks::TaskUpdate {
                title: Some("手動タイトル".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(load_if_eligible(&conn, task_id).unwrap().is_none());
    }

    #[test]
    fn sanitize_title_strips_control_chars_quotes_and_truncates() {
        let raw = "\"買い物リストの作成\n\n\"";
        assert_eq!(sanitize_title(raw), "買い物リストの作成");

        let long = "あ".repeat(MAX_TITLE_CHARS + 10);
        assert_eq!(sanitize_title(&long).chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn apply_title_does_not_overwrite_manually_set_title() {
        let conn = db::open_in_memory().unwrap();
        let task_id = tasks::create_task(&conn).unwrap().id;
        tasks::update_task(
            &conn,
            task_id,
            tasks::TaskUpdate {
                title: Some("手動".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        apply_title(&conn, task_id, "自動生成タイトル").unwrap();

        assert_eq!(
            tasks::get_task(&conn, task_id).unwrap().title.as_deref(),
            Some("手動")
        );
    }

    #[tokio::test]
    async fn maybe_generate_title_sets_title_when_eligible() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task_with_user_messages(&conn, MIN_USER_MESSAGES);
        let db: SharedConnection = Arc::new(Mutex::new(conn));
        let adapter = StubAdapter { reply: "買い物リストの作成" };

        maybe_generate_title(db.clone(), &adapter, task_id).await.unwrap();

        let conn = db.lock().unwrap();
        assert_eq!(
            tasks::get_task(&conn, task_id).unwrap().title.as_deref(),
            Some("買い物リストの作成")
        );
    }

    #[tokio::test]
    async fn maybe_generate_title_is_noop_when_not_eligible() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task_with_user_messages(&conn, MIN_USER_MESSAGES - 1);
        let db: SharedConnection = Arc::new(Mutex::new(conn));
        let adapter = StubAdapter { reply: "呼ばれないはず" };

        maybe_generate_title(db.clone(), &adapter, task_id).await.unwrap();

        let conn = db.lock().unwrap();
        assert!(tasks::get_task(&conn, task_id).unwrap().title.is_none());
    }
}
