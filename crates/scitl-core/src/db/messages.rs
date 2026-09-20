use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use super::{now_iso8601, CoreError, Result};

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

/// `docs/spec/rebuild/data-model.md`「ターン境界」の3分類。
/// ユーザー発言・外部経由の記録は`None`、SCITL自身の応答生成に属する行は`Some`。
pub struct NewMessage<'a> {
    pub task_id: Option<i64>,
    pub role: Role,
    pub content: &'a str,
    pub kind: Kind,
    pub source: Option<&'a str>,
    pub turn: Option<(&'a str, i64)>,
    /// `role`が`Error`のときのみ`Some`(`CHECK ((role = 'error') = (error_kind IS NOT NULL))`)。
    pub error_kind: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub id: i64,
    pub task_id: Option<i64>,
    pub role: String,
    pub content: String,
    pub kind: String,
    pub source: Option<String>,
    pub error_kind: Option<String>,
    pub turn_id: Option<String>,
    pub attempt_no: Option<i64>,
    pub created_at: String,
}

pub fn insert_message(conn: &Connection, msg: NewMessage) -> Result<i64> {
    let (turn_id, attempt_no) = match msg.turn {
        Some((id, attempt)) => (Some(id), Some(attempt)),
        None => (None, None),
    };
    conn.execute(
        "INSERT INTO messages
            (task_id, role, content, kind, source, error_kind, turn_id, attempt_no, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            msg.task_id,
            msg.role.as_str(),
            msg.content,
            msg.kind.as_str(),
            msg.source,
            msg.error_kind,
            turn_id,
            attempt_no,
            now_iso8601(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// タスクチャンネル分の発言取得(支配的クエリ)。
/// ターンを持つ行は`turn_id`ごとの最新試行のみに絞る
/// (data-model.md「ターン境界」— 外部経由の記録はturn_idを持たないため常に残る)。
pub fn list_for_task(conn: &Connection, task_id: i64) -> Result<Vec<Message>> {
    let mut stmt = conn.prepare(
        "SELECT id, task_id, role, content, kind, source, error_kind, turn_id, attempt_no, created_at
         FROM messages
         WHERE task_id = ?1
           AND deleted_at IS NULL
           AND (
             turn_id IS NULL
             OR attempt_no = (
               SELECT MAX(attempt_no) FROM messages m2
               WHERE m2.turn_id = messages.turn_id AND m2.deleted_at IS NULL
             )
           )
         ORDER BY created_at ASC, id ASC",
    )?;
    let rows = stmt
        .query_map([task_id], |row| {
            Ok(Message {
                id: row.get(0)?,
                task_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                kind: row.get(4)?,
                source: row.get(5)?,
                error_kind: row.get(6)?,
                turn_id: row.get(7)?,
                attempt_no: row.get(8)?,
                created_at: row.get(9)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// idで1件取得する(論理削除済みは対象外)。編集・再試行・削除いずれも、操作対象の
/// 現在の役割・種別を確認するためにまずこれを通る。
pub fn find_message(conn: &Connection, id: i64) -> Result<Option<Message>> {
    conn.query_row(
        "SELECT id, task_id, role, content, kind, source, error_kind, turn_id, attempt_no, created_at
         FROM messages
         WHERE id = ?1 AND deleted_at IS NULL",
        [id],
        |row| {
            Ok(Message {
                id: row.get(0)?,
                task_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                kind: row.get(4)?,
                source: row.get(5)?,
                error_kind: row.get(6)?,
                turn_id: row.get(7)?,
                attempt_no: row.get(8)?,
                created_at: row.get(9)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

/// 削除(共通)の唯一の入口。対象はユーザー/アシスタントの通常発言のみ
/// (`data-model.md`「ツール実行記録は通常発言の編集・削除・再試行の対象に含めない」)。
/// 確認ダイアログを挟まない即時の論理削除で、`deleted_at`を立てるだけの取り消し可能な
/// 操作にする(`deleted_at`をNULLに戻せば復元できる。復元UIは本Issueの範囲外)。
pub fn soft_delete_message(conn: &Connection, id: i64) -> Result<()> {
    let msg = find_message(conn, id)?.ok_or(CoreError::MessageNotFound(id))?;
    if msg.kind != "normal" || (msg.role != "user" && msg.role != "assistant") {
        return Err(CoreError::InvalidMessageOperation(
            "delete is only allowed for normal user/assistant messages".to_string(),
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
/// アシスタント発言から、それぞれ以降をすべて削除してから会話を再生成する)。
///
/// ツール実行記録(`kind='tool_execution'`)は対象に含めない
/// (`data-model.md`「ツール実行記録は通常発言の編集・削除・再試行の対象に含めない
/// (会話の整合性より実行記録の保全を優先する)」)。
///
/// この結果、経路によって表示上の見え方が異なる点に注意。**再試行**は同一`turn_id`のまま
/// `attempt_no`を増やすため、`list_for_task`の「`turn_id`ごとの最新`attempt_no`」絞り込みで
/// 旧試行のツール実行記録は自動的に表示から外れる。一方**編集**は新しい`turn_id`を振って
/// 会話を再生成するため、旧ターンの`turn_id`自体はもう他のどの行にも使われず「最新」のまま
/// 残り続け、対応する通常発言が消えた後も旧ターンのツール実行記録だけが単独で表示に残る
/// (`soft_delete_normal_from_cascades_but_spares_tool_execution_rows`で確認済み)。
pub fn soft_delete_normal_from(conn: &Connection, task_id: i64, from_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE messages SET deleted_at = ?1
         WHERE task_id = ?2 AND id >= ?3 AND kind = 'normal' AND deleted_at IS NULL",
        rusqlite::params![now_iso8601(), task_id, from_id],
    )?;
    Ok(())
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
                source: None,
                turn: None,
                error_kind: None,
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
                source: None,
                turn: Some(("turn-1", 1)),
                error_kind: Some("empty_response"),
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
                source: None,
                turn: Some(("turn-1", 2)),
                error_kind: None,
            },
        )
        .unwrap();

        let messages = list_for_task(&conn, task_id).unwrap();
        let contents: Vec<_> = messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, vec!["こんにちは", "再試行後の応答"]);
    }

    #[test]
    fn external_records_without_turn_are_always_shown() {
        let conn = db::open_in_memory().unwrap();
        let task_id = seed_task(&conn);

        insert_message(
            &conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::Tool,
                content: "{}",
                kind: Kind::ToolExecution,
                source: Some("mcp:external-client"),
                turn: None,
                error_kind: None,
            },
        )
        .unwrap();

        let messages = list_for_task(&conn, task_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].source.as_deref(), Some("mcp:external-client"));
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
                source: None,
                turn: Some(("turn-1", 1)),
                error_kind: Some("no_api_key"),
            },
        )
        .unwrap();

        let messages = list_for_task(&conn, task_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "error");
        assert_eq!(messages[0].error_kind.as_deref(), Some("no_api_key"));
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
                source: None,
                turn: None,
                error_kind: None,
            },
        );
        assert!(result.is_err());
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
                source: None,
                turn: None,
                error_kind: None,
            },
        )
        .unwrap();

        soft_delete_message(&conn, id).unwrap();

        assert!(list_for_task(&conn, task_id).unwrap().is_empty());
        // 物理削除ではないことを確認する(deleted_atを無視すれば行は残っている)。
        let deleted_at: Option<String> = conn
            .query_row("SELECT deleted_at FROM messages WHERE id = ?1", [id], |r| r.get(0))
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
                role: Role::Assistant,
                content: r#"{"tool":"add_steps"}"#,
                kind: Kind::ToolExecution,
                source: None,
                turn: Some(("turn-1", 1)),
                error_kind: None,
            },
        )
        .unwrap();

        let result = soft_delete_message(&conn, id);
        assert!(matches!(
            result,
            Err(CoreError::InvalidMessageOperation(_))
        ));
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
                source: None,
                turn: None,
                error_kind: None,
            },
        )
        .unwrap();

        insert_message(
            &conn,
            NewMessage {
                task_id: Some(task_id),
                role: Role::Assistant,
                content: r#"{"tool":"add_steps"}"#,
                kind: Kind::ToolExecution,
                source: None,
                turn: Some(("turn-1", 1)),
                error_kind: None,
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
                source: None,
                turn: Some(("turn-1", 1)),
                error_kind: None,
            },
        )
        .unwrap();

        // ユーザー発言以降(自身を含む)をすべて論理削除する = 編集操作のカスケードと同じ形。
        soft_delete_normal_from(&conn, task_id, user_id).unwrap();

        // kind='normal'の行(ユーザー発言・アシスタント発言)はすべて消えるが、
        // ツール実行記録は`soft_delete_normal_from`の対象外のため`deleted_at`が立たず、
        // `list_for_task`の「turn_idごとの最新attempt_no」判定になお該当し続ける結果、
        // 表示にはこのツール実行記録だけが残る。これは`data-model.md`が明言する
        // 「会話の整合性より実行記録の保全を優先する」というトレードオフの帰結であり、
        // 見た目の孤立したツール実行行が残る点は既知の許容範囲とする(#42の折りたたみ表示で
        // 改善されうるが、本Issueの範囲外)。
        let remaining = list_for_task(&conn, task_id).unwrap();
        assert_eq!(remaining.len(), 1, "unexpected remaining rows: {remaining:?}");
        assert_eq!(remaining[0].kind, "tool_execution");

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
                source: None,
                turn: Some(("turn-1", 1)),
                error_kind: None,
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
                source: None,
                turn: None,
                error_kind: None,
            },
        )
        .unwrap();

        assert!(find_message(&conn, id).unwrap().is_some());
        soft_delete_message(&conn, id).unwrap();
        assert!(find_message(&conn, id).unwrap().is_none());
    }
}
