use std::fmt;

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::attachments::{self, AttachmentView};
use super::{now_iso8601, CoreError, Result};

/// 発言が属する会話。`messages.task_id`がNULLなら総合チャット(data-model.md messages)。
/// `Option<i64>`で持たないのは、渡し忘れの`None`が総合チャットへの書き込みに化けるのを
/// 型で防ぐため(tools.md 1節が修正した「対象の取り違え」と同種の事故)。
/// 画面とは`{"kind":"general"}`・`{"kind":"task","task_id":1}`の形でやり取りする。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "task_id", rename_all = "snake_case")]
pub enum Chat {
    General,
    Task(i64),
}

impl Chat {
    /// `messages.task_id`の値。
    pub fn task_id(self) -> Option<i64> {
        match self {
            Self::General => None,
            Self::Task(id) => Some(id),
        }
    }
}

impl fmt::Display for Chat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::General => f.write_str("the general chat"),
            Self::Task(id) => write!(f, "task {id}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    Tool,
    Error,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
            Role::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Normal,
    ToolExecution,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Normal => "normal",
            Kind::ToolExecution => "tool_execution",
        }
    }
}

/// 行の出どころ(`docs/spec/rebuild/data-model.md`「ターン境界」の3分類)。`source`と
/// `turn_id`/`attempt_no`の組み合わせはこれだけから決まり、取り違えた組み合わせは書けない。
/// 操作の記録が実行記録であることまでは型で縛らず、DBのトリガー(`0004_message_origin.sql`)が
/// 止める。
#[derive(Debug, Clone, Copy)]
pub enum Origin<'a> {
    User,
    /// SCITLの応答生成の1試行に属する行。応答生成の途中で外部のツールサーバーを呼んだ記録も
    /// ここに入る(`source`は「外部と通信した」印ではない)。
    Turn {
        turn_id: &'a str,
        attempt_no: i64,
    },
    /// 応答生成以外の経路での操作の記録。
    Operation(OperationSource),
}

/// 応答生成以外の経路の印(`messages.source`)。CLI(#23)・MCP(#73)を実装したら値を足す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationSource {
    Ui,
}

impl OperationSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ui => "ui",
        }
    }
}

pub struct NewMessage<'a> {
    pub task_id: Option<i64>,
    pub role: Role,
    pub content: &'a str,
    pub kind: Kind,
    pub origin: Origin<'a>,
    /// `role`が`Error`のときのみ`Some`(`CHECK ((role = 'error') = (error_kind IS NOT NULL))`)。
    pub error_kind: Option<&'a str>,
    /// エラー発言の詳細(`orchestration::TurnFailure::detail`)。画面の「詳細を表示」専用で、
    /// モデル入力・エクスポートには使わない(data-model.md messages「error_detail」)。
    pub error_detail: Option<&'a str>,
    /// モデルの思考(reasoning)。表示・エクスポート専用で、APIへの入力には使わない
    /// (`docs/spec/rebuild/data-model.md` messagesテーブル、Issue #42)。
    pub reasoning: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub id: i64,
    pub task_id: Option<i64>,
    pub role: String,
    pub content: String,
    pub kind: String,
    pub source: Option<String>,
    pub reasoning: Option<String>,
    pub error_kind: Option<String>,
    pub error_detail: Option<String>,
    pub turn_id: Option<String>,
    pub attempt_no: Option<i64>,
    pub created_at: String,
    /// 発言に付いた添付。付けた順。
    pub attachments: Vec<AttachmentView>,
}

pub fn insert_message(conn: &Connection, msg: NewMessage) -> Result<i64> {
    let (source, turn_id, attempt_no) = match msg.origin {
        Origin::User => (None, None, None),
        Origin::Turn {
            turn_id,
            attempt_no,
        } => (None, Some(turn_id), Some(attempt_no)),
        Origin::Operation(source) => (Some(source.as_str()), None, None),
    };
    conn.execute(
        "INSERT INTO messages
            (task_id, role, content, kind, source, reasoning, error_kind, error_detail, turn_id, attempt_no, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        rusqlite::params![
            msg.task_id,
            msg.role.as_str(),
            msg.content,
            msg.kind.as_str(),
            source,
            msg.reasoning,
            msg.error_kind,
            msg.error_detail,
            turn_id,
            attempt_no,
            now_iso8601(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 1つの会話の発言取得(支配的クエリ)。
/// ターンを持つ行は`turn_id`ごとの最新試行のみに絞り、さらに**通常発言が1行も生き残って
/// いないターン(破棄されたターン)を丸ごと除く**(data-model.md「ターン境界」—
/// 応答生成以外の経路での操作の記録はturn_idを持たないため常に残る)。
///
/// 後者はIssue #95。編集・再試行のカスケードは`kind='normal'`しか論理削除しないため
/// (ツール実行記録は保全する。data-model.md)、破棄されたターンのツール実行記録だけが
/// 残る。これを会話に並べると、直後に挿入される編集後の発言がその下に来て新規送信と
/// 見分けが付かなくなる。記録はDBに残したまま、この支配的クエリの時点で会話から外す。
pub fn list_for_chat(conn: &Connection, chat: Chat) -> Result<Vec<Message>> {
    let mut stmt = conn.prepare(
        "SELECT id, task_id, role, content, kind, source, reasoning, error_kind, error_detail, turn_id, attempt_no, created_at
         FROM messages
         WHERE task_id IS ?1
           AND deleted_at IS NULL
           AND (
             turn_id IS NULL
             OR (
               attempt_no = (
                 SELECT MAX(attempt_no) FROM messages m2
                 WHERE m2.turn_id = messages.turn_id AND m2.deleted_at IS NULL
               )
               AND EXISTS (
                 SELECT 1 FROM messages m3
                 WHERE m3.turn_id = messages.turn_id
                   AND m3.attempt_no = messages.attempt_no
                   AND m3.kind = 'normal'
                   AND m3.deleted_at IS NULL
               )
             )
           )
         ORDER BY created_at ASC, id ASC",
    )?;
    let mut rows = stmt
        .query_map([chat.task_id()], message_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut attached = attachments::views_for_chat(conn, chat)?;
    for row in &mut rows {
        row.attachments = attached.remove(&row.id).unwrap_or_default();
    }
    Ok(rows)
}

/// idで1件取得する(論理削除済みは対象外)。編集・再試行・削除いずれも、操作対象の
/// 現在の役割・種別を確認するためにまずこれを通る。
pub fn find_message(conn: &Connection, id: i64) -> Result<Option<Message>> {
    let found = conn.query_row(
        "SELECT id, task_id, role, content, kind, source, reasoning, error_kind, error_detail, turn_id, attempt_no, created_at
         FROM messages
         WHERE id = ?1 AND deleted_at IS NULL",
        [id],
        message_from_row,
    )
    .optional()?;
    found
        .map(|mut m| {
            m.attachments = attachments::views_for_message(conn, m.id)?;
            Ok(m)
        })
        .transpose()
}

/// `SELECT`の列の並びは`list_for_chat`・`find_message`で共通。添付は呼び出し側が埋める。
fn message_from_row(row: &rusqlite::Row) -> rusqlite::Result<Message> {
    Ok(Message {
        id: row.get(0)?,
        task_id: row.get(1)?,
        role: row.get(2)?,
        content: row.get(3)?,
        kind: row.get(4)?,
        source: row.get(5)?,
        reasoning: row.get(6)?,
        error_kind: row.get(7)?,
        error_detail: row.get(8)?,
        turn_id: row.get(9)?,
        attempt_no: row.get(10)?,
        created_at: row.get(11)?,
        attachments: Vec::new(),
    })
}

/// 削除(共通)の唯一の入口。対象はユーザー発言とターンの返信(アシスタント発言・
/// エラー発言)の通常発言のみ
/// (`data-model.md`「ツール実行記録は通常発言の編集・削除・再試行の対象に含めない」)。
/// 返信を消したターンは通常発言が残らないため、`list_for_chat`がターンごと会話から外す。
/// 確認ダイアログを挟まない即時の論理削除で、`deleted_at`を立てるだけの取り消し可能な
/// 操作にする(`deleted_at`をNULLに戻せば復元できる。復元UIは本Issueの範囲外)。
pub fn soft_delete_message(conn: &Connection, id: i64) -> Result<()> {
    let msg = find_message(conn, id)?.ok_or(CoreError::MessageNotFound(id))?;
    if msg.kind != "normal" || !matches!(msg.role.as_str(), "user" | "assistant" | "error") {
        return Err(CoreError::InvalidMessageOperation(
            "delete is only allowed for normal user/assistant/error messages".to_string(),
        ));
    }
    let updated = conn.execute(
        "UPDATE messages SET deleted_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
        rusqlite::params![now_iso8601(), id],
    )?;
    if updated == 0 {
        return Err(CoreError::MessageNotFound(id));
    }
    Ok(())
}

/// `from_id`以降(自身を含む)の通常発言(`kind='normal'`)を一括で論理削除する。
/// 編集・再試行のカスケード用の共通入口(編集は対象のユーザー発言から、再試行は対象の
/// ターンの返信から、それぞれ以降をすべて削除してから会話を再生成する)。
///
/// ツール実行記録(`kind='tool_execution'`)は対象に含めない
/// (`data-model.md`「ツール実行記録は通常発言の編集・削除・再試行の対象に含めない
/// (会話の整合性より実行記録の保全を優先する)」)。
///
/// この呼び出しの後、対象のターンには通常発言が1行も残らず、ツール実行記録だけが浮く。
/// 会話としては破棄されたターンなので、`list_for_chat`が表示から外す(Issue #95。
/// `soft_delete_normal_from_cascades_but_spares_tool_execution_rows`で、DBには残り
/// 会話には出ないことを確認している)。**保全と表示を切り離すのがここの要点**で、
/// 記録の側を消して辻褄を合わせてはならない。
pub fn soft_delete_normal_from(conn: &Connection, chat: Chat, from_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE messages SET deleted_at = ?1
         WHERE task_id IS ?2 AND id >= ?3 AND kind = 'normal' AND deleted_at IS NULL",
        rusqlite::params![now_iso8601(), chat.task_id(), from_id],
    )?;
    Ok(())
}

/// タスクの会話を始めた側(Issue #76)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opener {
    User,
    /// ユーザー発言より先に、SCITLの応答生成が始まった(聞き取りの開始)。
    Reply,
}

/// タスクの会話を始めた側。まだ1行も無ければ`None`。
///
/// 論理削除した行も含めた最初の行で決める。最初のユーザー発言を消しても編集で置き換えても
/// 行は残るので、答えが変わらない。応答生成以外の経路での操作の記録はこの会話の発言ではないので
/// 見ない。
pub fn opener(conn: &Connection, task_id: i64) -> Result<Option<Opener>> {
    let role: Option<String> = conn
        .query_row(
            "SELECT role FROM messages
             WHERE task_id = ?1 AND (role = 'user' OR turn_id IS NOT NULL)
             ORDER BY created_at ASC, id ASC
             LIMIT 1",
            [task_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(role.map(|role| {
        if role == Role::User.as_str() {
            Opener::User
        } else {
            Opener::Reply
        }
    }))
}

/// 指定`turn_id`の次の試行番号を採番する。論理削除済みの試行も`MAX`の対象に含める
/// (物理削除しない方針と同様、番号を使い回さず単調増加させることで、削除された古い
/// 試行の記録と新しい試行が`attempt_no`の面でも混同されないようにするため)。
pub fn next_attempt_no(conn: &Connection, turn_id: &str) -> Result<i64> {
    let max: Option<i64> = conn.query_row(
        "SELECT MAX(attempt_no) FROM messages WHERE turn_id = ?1",
        [turn_id],
        |row| row.get(0),
    )?;
    Ok(max.unwrap_or(0) + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn seed_task(conn: &Connection) -> i64 {
        let now = now_iso8601();
        conn.execute(
            "INSERT INTO tasks (created_at, updated_at) VALUES (?1, ?1)",
            [&now],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn retry_only_shows_latest_attempt() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);

        insert_message(
            &conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::User,
                content: "こんにちは",
                kind: Kind::Normal,
                origin: Origin::User,
                error_kind: None,
                error_detail: None,
                reasoning: None,
            },
        )
        .unwrap();

        insert_message(
            &conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::Error,
                content: "モデルからの応答が空でした",
                kind: Kind::Normal,
                origin: Origin::Turn {
                    turn_id: "turn-1",
                    attempt_no: 1,
                },
                error_kind: Some("empty_response"),
                error_detail: None,
                reasoning: None,
            },
        )
        .unwrap();

        insert_message(
            &conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::Assistant,
                content: "再試行後の応答",
                kind: Kind::Normal,
                origin: Origin::Turn {
                    turn_id: "turn-1",
                    attempt_no: 2,
                },
                error_kind: None,
                error_detail: None,
                reasoning: None,
            },
        )
        .unwrap();

        let messages = list_for_chat(&conn, Chat::Task(task_id)).unwrap();
        let contents: Vec<_> = messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, vec!["こんにちは", "再試行後の応答"]);
    }

    #[test]
    fn operation_records_without_turn_are_always_shown() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);

        insert_message(
            &conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::Tool,
                content: "{}",
                kind: Kind::ToolExecution,
                origin: Origin::Operation(OperationSource::Ui),
                error_kind: None,
                error_detail: None,
                reasoning: None,
            },
        )
        .unwrap();

        let messages = list_for_chat(&conn, Chat::Task(task_id)).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].source.as_deref(), Some("ui"));
    }

    #[test]
    fn only_operation_records_outside_a_turn_carry_a_source() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let insert = |role, kind, origin| {
            insert_message(
                &conn,
                NewMessage {
                    task_id: Some(task_id),
                    role,
                    content: "{}",
                    kind,
                    origin,
                    error_kind: None,
                    error_detail: None,
                    reasoning: None,
                },
            )
        };
        let raw = |source: Option<&str>, turn_id: Option<&str>| {
            conn.execute(
                "INSERT INTO messages (task_id, role, content, kind, source, turn_id, attempt_no, created_at)
                 VALUES (?1, 'tool', '{}', 'tool_execution', ?2, ?3, ?4, '2026-01-01T00:00:00Z')",
                rusqlite::params![task_id, source, turn_id, turn_id.map(|_| 1)],
            )
        };

        let record = insert(
            Role::Tool,
            Kind::ToolExecution,
            Origin::Operation(OperationSource::Ui),
        )
        .unwrap();
        // 経路の印を発言に付けることはできない(CLIからの発言もただのユーザー発言)。
        assert!(insert(
            Role::User,
            Kind::Normal,
            Origin::Operation(OperationSource::Ui)
        )
        .is_err());
        // ターンの記録に印を付けること、印の無い記録をターンの外に書くことはできない。
        assert!(raw(Some("ui"), Some("turn-1")).is_err());
        assert!(raw(None, None).is_err());
        assert!(conn
            .execute(
                "UPDATE messages SET turn_id = 'turn-1', attempt_no = 1 WHERE id = ?1",
                [record]
            )
            .is_err());
        // 論理削除は印に関わらない。
        conn.execute(
            "UPDATE messages SET deleted_at = '2026-01-01T00:00:00Z' WHERE id = ?1",
            [record],
        )
        .unwrap();
    }

    #[test]
    fn tool_role_is_reserved_for_tool_execution_records() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let insert = |role, kind, content| {
            insert_message(
                &conn,
                NewMessage {
                    task_id: Some(task_id),
                    role,
                    content,
                    kind,
                    origin: Origin::Turn {
                        turn_id: "turn-1",
                        attempt_no: 1,
                    },
                    error_kind: None,
                    error_detail: None,
                    reasoning: None,
                },
            )
        };

        assert!(insert(Role::Assistant, Kind::ToolExecution, "{}").is_err());
        assert!(insert(Role::Tool, Kind::Normal, "結果").is_err());
        let id = insert(Role::Tool, Kind::ToolExecution, "{}").unwrap();
        assert!(conn
            .execute("UPDATE messages SET kind = 'normal' WHERE id = ?1", [id])
            .is_err());
    }

    #[test]
    fn error_role_round_trips_with_error_kind() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);

        insert_message(
            &conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::Error,
                content: "APIキーが設定されていません",
                kind: Kind::Normal,
                origin: Origin::Turn {
                    turn_id: "turn-1",
                    attempt_no: 1,
                },
                error_kind: Some("no_api_key"),
                error_detail: Some("HTTP 401: invalid key"),
                reasoning: None,
            },
        )
        .unwrap();

        let messages = list_for_chat(&conn, Chat::Task(task_id)).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "error");
        assert_eq!(messages[0].error_kind.as_deref(), Some("no_api_key"));
        assert_eq!(
            messages[0].error_detail.as_deref(),
            Some("HTTP 401: invalid key")
        );
    }

    /// 詳細を持てるのはエラー発言だけで、空文字は未設定(NULL)と区別させない
    /// (`0003_error_detail.sql`のトリガー)。
    #[test]
    fn error_detail_is_rejected_outside_error_messages_and_when_empty() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);

        let on_user = insert_message(
            &conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::User,
                content: "こんにちは",
                kind: Kind::Normal,
                origin: Origin::User,
                error_kind: None,
                error_detail: Some("HTTP 500: boom"),
                reasoning: None,
            },
        );
        assert!(on_user.is_err());

        let empty = insert_message(
            &conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::Error,
                content: "LLMプロバイダーとの通信に失敗しました。",
                kind: Kind::Normal,
                origin: Origin::User,
                error_kind: Some("provider"),
                error_detail: Some(""),
                reasoning: None,
            },
        );
        assert!(empty.is_err());

        let id = insert_message(
            &conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::Error,
                content: "LLMプロバイダーとの通信に失敗しました。",
                kind: Kind::Normal,
                origin: Origin::User,
                error_kind: Some("provider"),
                error_detail: Some("HTTP 500: boom"),
                reasoning: None,
            },
        )
        .unwrap();
        assert!(conn
            .execute(
                "UPDATE messages SET role = 'assistant', error_kind = NULL WHERE id = ?1",
                [id]
            )
            .is_err());
    }

    #[test]
    fn error_role_without_error_kind_is_rejected_by_check_constraint() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);

        let result = insert_message(
            &conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::Error,
                content: "壊れた呼び出し",
                kind: Kind::Normal,
                origin: Origin::User,
                error_kind: None,
                error_detail: None,
                reasoning: None,
            },
        );
        assert!(result.is_err());
    }

    /// 複数回再試行したターンを、さらに前方の発言の編集で丸ごと破棄した場合
    /// (Issue #95)。旧試行の行は`MAX(attempt_no)`で、最新試行の行は「通常発言が
    /// 生き残っていない」判定で、それぞれ別の条件で外れる。両方が同時に効くことを固定する。
    #[test]
    fn a_retried_turn_discarded_by_a_later_edit_disappears_from_every_attempt() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);

        let user_id = insert_message(
            &conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::User,
                content: "工程を作って",
                kind: Kind::Normal,
                origin: Origin::User,
                error_kind: None,
                error_detail: None,
                reasoning: None,
            },
        )
        .unwrap();

        // 同じturn_idのまま2回試行し、どちらもツール実行記録と通常応答を残す。
        for attempt in 1..=2 {
            insert_message(
                &conn,
                NewMessage {
                    task_id: Some(task_id),
                    role: Role::Tool,
                    content: r#"{"tool":"add_steps"}"#,
                    kind: Kind::ToolExecution,
                    origin: Origin::Turn {
                        turn_id: "turn-1",
                        attempt_no: attempt,
                    },
                    error_kind: None,
                    error_detail: None,
                    reasoning: None,
                },
            )
            .unwrap();
            insert_message(
                &conn,
                NewMessage {
                    task_id: Some(task_id),
                    role: Role::Assistant,
                    content: "追加しました",
                    kind: Kind::Normal,
                    origin: Origin::Turn {
                        turn_id: "turn-1",
                        attempt_no: attempt,
                    },
                    error_kind: None,
                    error_detail: None,
                    reasoning: None,
                },
            )
            .unwrap();
        }

        // 前方のユーザー発言を編集した場合のカスケード。通常発言は全試行分が消える。
        soft_delete_normal_from(&conn, Chat::Task(task_id), user_id).unwrap();

        // 旧試行・最新試行のどちらのツール実行記録も会話には出ない。
        let remaining = list_for_chat(&conn, Chat::Task(task_id)).unwrap();
        assert!(
            remaining.is_empty(),
            "unexpected remaining rows: {remaining:?}"
        );

        // 記録自体は全試行分がDBに残る(保全優先)。
        let kept: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages
                 WHERE kind = 'tool_execution' AND deleted_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, 2);
    }

    #[test]
    fn soft_delete_message_hides_it_but_keeps_the_row() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let id = insert_message(
            &conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::User,
                content: "こんにちは",
                kind: Kind::Normal,
                origin: Origin::User,
                error_kind: None,
                error_detail: None,
                reasoning: None,
            },
        )
        .unwrap();

        soft_delete_message(&conn, id).unwrap();

        assert!(list_for_chat(&conn, Chat::Task(task_id))
            .unwrap()
            .is_empty());
        // 物理削除ではないことを確認する(deleted_atを無視すれば行は残っている)。
        let deleted_at: Option<String> = conn
            .query_row("SELECT deleted_at FROM messages WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(deleted_at.is_some());
    }

    #[test]
    fn soft_delete_message_rejects_tool_execution_rows() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let id = insert_message(
            &conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::Tool,
                content: r#"{"tool":"add_steps"}"#,
                kind: Kind::ToolExecution,
                origin: Origin::Turn {
                    turn_id: "turn-1",
                    attempt_no: 1,
                },
                error_kind: None,
                error_detail: None,
                reasoning: None,
            },
        )
        .unwrap();

        let result = soft_delete_message(&conn, id);
        assert!(matches!(result, Err(CoreError::InvalidMessageOperation(_))));
    }

    #[test]
    fn soft_delete_message_on_missing_id_is_not_found() {
        let conn = db::open_in_memory().unwrap();
        let result = soft_delete_message(&conn, 999);
        assert!(matches!(result, Err(CoreError::MessageNotFound(999))));
    }

    #[test]
    fn soft_delete_normal_from_cascades_but_spares_tool_execution_rows() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);

        let user_id = insert_message(
            &conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::User,
                content: "工程を追加して",
                kind: Kind::Normal,
                origin: Origin::User,
                error_kind: None,
                error_detail: None,
                reasoning: None,
            },
        )
        .unwrap();

        insert_message(
            &conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::Tool,
                content: r#"{"tool":"add_steps"}"#,
                kind: Kind::ToolExecution,
                origin: Origin::Turn {
                    turn_id: "turn-1",
                    attempt_no: 1,
                },
                error_kind: None,
                error_detail: None,
                reasoning: None,
            },
        )
        .unwrap();

        let assistant_id = insert_message(
            &conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::Assistant,
                content: "追加しました",
                kind: Kind::Normal,
                origin: Origin::Turn {
                    turn_id: "turn-1",
                    attempt_no: 1,
                },
                error_kind: None,
                error_detail: None,
                reasoning: None,
            },
        )
        .unwrap();

        // ユーザー発言以降(自身を含む)をすべて論理削除する = 編集操作のカスケードと同じ形。
        soft_delete_normal_from(&conn, Chat::Task(task_id), user_id).unwrap();

        // kind='normal'の行(ユーザー発言・アシスタント発言)はすべて消える。ツール実行記録は
        // `soft_delete_normal_from`の対象外なので`deleted_at`が立たないが、通常発言が1行も
        // 残らないターンは会話としては破棄されているため、`list_for_chat`は丸ごと外す
        // (Issue #95)。保全(DBに残る)と表示(会話に出ない)を切り離すのがこのテストの要点。
        let remaining = list_for_chat(&conn, Chat::Task(task_id)).unwrap();
        assert!(
            remaining.is_empty(),
            "unexpected remaining rows: {remaining:?}"
        );

        // ツール実行記録の行自体は監査記録として物理的には残る(保全優先)。
        let tool_deleted_at: Option<String> = conn
            .query_row(
                "SELECT deleted_at FROM messages WHERE kind = 'tool_execution'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(tool_deleted_at.is_none());

        let assistant_deleted_at: Option<String> = conn
            .query_row(
                "SELECT deleted_at FROM messages WHERE id = ?1",
                [assistant_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(assistant_deleted_at.is_some());
    }

    #[test]
    fn next_attempt_no_increments_and_ignores_deleted_rows() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);

        assert_eq!(next_attempt_no(&conn, "turn-1").unwrap(), 1);

        let id = insert_message(
            &conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::Assistant,
                content: "1回目の応答",
                kind: Kind::Normal,
                origin: Origin::Turn {
                    turn_id: "turn-1",
                    attempt_no: 1,
                },
                error_kind: None,
                error_detail: None,
                reasoning: None,
            },
        )
        .unwrap();
        assert_eq!(next_attempt_no(&conn, "turn-1").unwrap(), 2);

        soft_delete_message(&conn, id).unwrap();
        // 削除済みでも採番は巻き戻らない(番号を使い回さないことで、削除された旧試行と
        // 新しい試行が混同されないようにする)。
        assert_eq!(next_attempt_no(&conn, "turn-1").unwrap(), 2);
    }

    #[test]
    fn find_message_ignores_deleted_rows() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let id = insert_message(
            &conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::User,
                content: "こんにちは",
                kind: Kind::Normal,
                origin: Origin::User,
                error_kind: None,
                error_detail: None,
                reasoning: None,
            },
        )
        .unwrap();

        assert!(find_message(&conn, id).unwrap().is_some());
        soft_delete_message(&conn, id).unwrap();
        assert!(find_message(&conn, id).unwrap().is_none());
    }

    #[test]
    fn opener_is_decided_by_the_first_row_even_after_it_is_deleted() {
        let conn = db::open_in_memory().unwrap();
        let insert = |task_id, role, origin| {
            insert_message(
                &conn,
                NewMessage {
                    task_id: Some(task_id),
                    role,
                    content: if role == Role::Tool { "{}" } else { "本文" },
                    kind: if role == Role::Tool {
                        Kind::ToolExecution
                    } else {
                        Kind::Normal
                    },
                    origin,
                    error_kind: None,
                    error_detail: None,
                    reasoning: None,
                },
            )
            .unwrap()
        };

        let by_user = seed_task(&conn);
        assert_eq!(opener(&conn, by_user).unwrap(), None);
        let turn = |turn_id| Origin::Turn {
            turn_id,
            attempt_no: 1,
        };
        let first = insert(by_user, Role::User, Origin::User);
        insert(by_user, Role::Assistant, turn("turn-1"));
        soft_delete_message(&conn, first).unwrap();
        assert_eq!(opener(&conn, by_user).unwrap(), Some(Opener::User));

        let by_reply = seed_task(&conn);
        // 応答生成以外の経路での操作の記録は会話の発言ではない。
        insert(by_reply, Role::Tool, Origin::Operation(OperationSource::Ui));
        let reply = insert(by_reply, Role::Assistant, turn("turn-2"));
        insert(by_reply, Role::User, Origin::User);
        soft_delete_normal_from(&conn, Chat::Task(by_reply), reply).unwrap();
        assert_eq!(opener(&conn, by_reply).unwrap(), Some(Opener::Reply));
    }
    #[test]
    fn general_and_task_chats_do_not_see_each_others_messages() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);
        let insert = |task_id: Option<i64>, content| {
            insert_message(
                &conn,
                NewMessage {
                    task_id,
                    role: Role::User,
                    content,
                    kind: Kind::Normal,
                    origin: Origin::User,
                    error_kind: None,
                    error_detail: None,
                    reasoning: None,
                },
            )
            .unwrap()
        };
        let general_first = insert(None, "総合1");
        insert(Some(task_id), "タスク");
        insert(None, "総合2");

        let contents = |chat| -> Vec<String> {
            list_for_chat(&conn, chat)
                .unwrap()
                .into_iter()
                .map(|m| m.content)
                .collect()
        };
        assert_eq!(contents(Chat::General), vec!["総合1", "総合2"]);
        assert_eq!(contents(Chat::Task(task_id)), vec!["タスク"]);

        // 編集のカスケードも会話の中に留まる。
        soft_delete_normal_from(&conn, Chat::General, general_first).unwrap();
        assert!(contents(Chat::General).is_empty());
        assert_eq!(contents(Chat::Task(task_id)), vec!["タスク"]);
    }

    #[test]
    fn chat_is_exchanged_as_a_tagged_object() {
        assert_eq!(
            serde_json::to_value(Chat::General).unwrap(),
            serde_json::json!({ "kind": "general" })
        );
        assert_eq!(
            serde_json::to_value(Chat::Task(3)).unwrap(),
            serde_json::json!({ "kind": "task", "task_id": 3 })
        );
        let general: Chat =
            serde_json::from_value(serde_json::json!({ "kind": "general" })).unwrap();
        assert_eq!(general, Chat::General);
        // nullや数値だけでは総合チャットにもタスクにもならない。
        assert!(serde_json::from_value::<Chat>(serde_json::json!(null)).is_err());
        assert!(serde_json::from_value::<Chat>(serde_json::json!(3)).is_err());
        assert!(serde_json::from_value::<Chat>(serde_json::json!({ "kind": "task" })).is_err());
    }
}
