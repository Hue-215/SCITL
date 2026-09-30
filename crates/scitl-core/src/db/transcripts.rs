//! モデルに送った形の保存(`turn_transcripts`・`transcript_blobs`)。形(`input`・`rounds`の
//! JSON)の意味は`orchestration::transcript`が持ち、ここは行の読み書きだけを行う。

use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::db::messages::Chat;
use crate::db::now_iso8601;
use crate::error::Result;

/// 本文の指紋(SHA-256の小文字16進)。
pub fn digest(body: &str) -> String {
    format!("{:x}", Sha256::digest(body.as_bytes()))
}

/// 書き込む1試行分。本文(システムプロンプト・ツール定義)は指紋ではなく本文で渡し、
/// `transcript_blobs`への置き場所はここで決める。
pub struct NewTranscript<'a> {
    pub chat: Chat,
    pub turn_id: &'a str,
    pub attempt_no: i64,
    pub api_format: &'a str,
    pub model: &'a str,
    /// 先頭に置いたシステムプロンプト。
    pub system: &'a str,
    /// そのとき設定から作ったシステムプロンプト。
    pub settings_system: &'a str,
    /// 渡したツール定義の一覧(JSON)。
    pub tools: &'a str,
    pub prefix_digest: &'a str,
    pub history_start: Option<i64>,
    pub input: &'a str,
    pub rounds: &'a str,
}

/// 1試行分を書く。本文は`transcript_blobs`に無ければ足す。返信の行と同じトランザクションの
/// 中で呼ぶ(`docs/spec/rebuild/data-model.md` turn_transcripts)。
pub fn insert(conn: &Connection, new: &NewTranscript) -> Result<()> {
    let system = put_blob(conn, new.system)?;
    let settings_system = put_blob(conn, new.settings_system)?;
    let tools = put_blob(conn, new.tools)?;
    conn.execute(
        "INSERT INTO turn_transcripts (
             task_id, turn_id, attempt_no, api_format, model,
             system_digest, settings_system_digest, tools_digest,
             prefix_digest, history_start, input, rounds, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            new.chat.task_id(),
            new.turn_id,
            new.attempt_no,
            new.api_format,
            new.model,
            system,
            settings_system,
            tools,
            new.prefix_digest,
            new.history_start,
            new.input,
            new.rounds,
            now_iso8601(),
        ],
    )?;
    Ok(())
}

fn put_blob(conn: &Connection, body: &str) -> Result<String> {
    let digest = digest(body);
    conn.execute(
        "INSERT OR IGNORE INTO transcript_blobs (digest, body) VALUES (?1, ?2)",
        params![digest, body],
    )?;
    Ok(digest)
}

/// 保存した1試行分。
#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    pub turn_id: String,
    pub attempt_no: i64,
    pub api_format: String,
    pub model: String,
    pub system_digest: String,
    pub settings_system_digest: String,
    pub tools_digest: String,
    pub prefix_digest: String,
    pub history_start: Option<i64>,
    pub input: String,
    pub rounds: String,
}

const TRANSCRIPT_COLUMNS: &str = "turn_id, attempt_no, api_format, model, system_digest,
     settings_system_digest, tools_digest, prefix_digest, history_start, input, rounds";

fn transcript_from_row(row: &rusqlite::Row) -> rusqlite::Result<Transcript> {
    Ok(Transcript {
        turn_id: row.get(0)?,
        attempt_no: row.get(1)?,
        api_format: row.get(2)?,
        model: row.get(3)?,
        system_digest: row.get(4)?,
        settings_system_digest: row.get(5)?,
        tools_digest: row.get(6)?,
        prefix_digest: row.get(7)?,
        history_start: row.get(8)?,
        input: row.get(9)?,
        rounds: row.get(10)?,
    })
}

/// 試行1つ分の保存。無ければ`None`。
pub fn find(conn: &Connection, turn_id: &str, attempt_no: i64) -> Result<Option<Transcript>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {TRANSCRIPT_COLUMNS} FROM turn_transcripts
                 WHERE turn_id = ?1 AND attempt_no = ?2"
            ),
            params![turn_id, attempt_no],
            transcript_from_row,
        )
        .optional()?)
}

/// 会話の保存すべて。どの試行の保存を使うかは呼び出し側が`messages`から決める。
pub fn list_for_chat(conn: &Connection, chat: Chat) -> Result<Vec<Transcript>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {TRANSCRIPT_COLUMNS} FROM turn_transcripts WHERE task_id IS ?1 ORDER BY id"
    ))?;
    let rows = stmt
        .query_map([chat.task_id()], transcript_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 指紋が指す本文。無ければ`None`。
pub fn blob(conn: &Connection, digest: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT body FROM transcript_blobs WHERE digest = ?1",
            [digest],
            |row| row.get(0),
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn new<'a>(turn_id: &'a str, attempt_no: i64, system: &'a str) -> NewTranscript<'a> {
        NewTranscript {
            chat: Chat::General,
            turn_id,
            attempt_no,
            api_format: "anthropic",
            model: "claude-test",
            system,
            settings_system: system,
            tools: "[]",
            prefix_digest: "p",
            history_start: None,
            input: r#"{"rows":[],"messages":[]}"#,
            rounds: "[]",
        }
    }

    #[test]
    fn writes_a_transcript_and_puts_each_body_once() {
        let conn = db::open_in_memory().unwrap();
        insert(&conn, &new("t1", 1, "system")).unwrap();
        insert(&conn, &new("t2", 1, "system")).unwrap();

        let found = find(&conn, "t1", 1).unwrap().unwrap();
        assert_eq!(found.system_digest, digest("system"));
        assert_eq!(found.model, "claude-test");
        assert_eq!(
            blob(&conn, &found.system_digest).unwrap().as_deref(),
            Some("system")
        );
        let blobs: i64 = conn
            .query_row("SELECT COUNT(*) FROM transcript_blobs", [], |row| {
                row.get(0)
            })
            .unwrap();
        // システムプロンプト(設定から作ったものと同じ)とツール定義の2つ。
        assert_eq!(blobs, 2);
        assert!(find(&conn, "t1", 2).unwrap().is_none());
        let listed: Vec<_> = list_for_chat(&conn, Chat::General)
            .unwrap()
            .into_iter()
            .map(|t| t.turn_id)
            .collect();
        assert_eq!(listed, ["t1", "t2"]);
        assert!(list_for_chat(&conn, Chat::Task(1)).unwrap().is_empty());
    }

    #[test]
    fn refuses_a_second_transcript_for_the_same_attempt() {
        let conn = db::open_in_memory().unwrap();
        insert(&conn, &new("t1", 1, "system")).unwrap();
        assert!(insert(&conn, &new("t1", 1, "system")).is_err());
    }
}
