use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use super::{check_max_chars, now_iso8601};
use crate::error::{CoreError, Result};
use crate::text::{drop_stacked_variation_selectors, visible_line};

/// 会話をまたいで共有する、利用者についての事実1件。どの会話にも属さない。
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct Memory {
    pub id: i64,
    pub content: String,
    pub created_at: String,
    pub updated_at: String,
}

/// [`row_to_memory`]の並び。
const MEMORY_COLUMNS: &str = "id, content, created_at, updated_at";

/// 1件の本文の上限文字数。
pub const MAX_MEMORY_CHARS: usize = 200;

/// 持てるメモリ(未削除)の上限件数。読み取りと書き込み系のツールは毎回全体を返すので、
/// 件数に上限が無いと1回の結果が際限なく大きくなる。
pub const MAX_MEMORIES: usize = 100;

/// 削除済みを除くメモリを、書いた順で返す。
pub fn list(conn: &Connection) -> Result<Vec<Memory>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {MEMORY_COLUMNS} FROM memories
         WHERE deleted_at IS NULL
         ORDER BY created_at ASC, id ASC"
    ))?;
    let rows = stmt
        .query_map([], row_to_memory)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// 本文の正規化。描かれない文字と重ねた異体字セレクタを除き、制御文字(改行を含む)を空白に
/// して1行に畳み、空ならエラーにする。見えない文字を保存の時点で除くのは、全会話のモデルに
/// 渡る本文に、設定画面で見えない文字列を残さないため。追加と更新で同じ規則を通す。
fn normalize_content(raw: &str, arg_name: &str) -> Result<String> {
    let content = visible_line(&drop_stacked_variation_selectors(raw));
    if content.is_empty() {
        return Err(CoreError::InvalidArgument {
            name: arg_name.to_string(),
            reason: "must not be empty".to_string(),
        });
    }
    check_max_chars(arg_name, &content, MAX_MEMORY_CHARS)?;
    Ok(content)
}

/// メモリの追加。`contents`内の重複と、既存の未削除のメモリと同じ本文は除く。空・長すぎる
/// 本文が1つでもあるか、追加すると[`MAX_MEMORIES`]を超えるなら、1件も追加せずにエラーを返す。
/// 戻り値は新しく追加したメモリだけ。
pub fn add(conn: &Connection, contents: &[String]) -> Result<Vec<Memory>> {
    super::in_transaction(conn, |conn| {
        let contents = contents
            .iter()
            .map(|c| normalize_content(c, "contents"))
            .collect::<Result<Vec<_>>>()?;

        let existing = list(conn)?;
        let existing_count = existing.len();
        let mut seen: HashSet<String> = existing.into_iter().map(|m| m.content).collect();
        let to_add: Vec<String> = contents
            .into_iter()
            .filter(|c| seen.insert(c.clone()))
            .collect();
        if existing_count + to_add.len() > MAX_MEMORIES {
            return Err(CoreError::InvalidArgument {
                name: "contents".to_string(),
                reason: format!(
                    "at most {MAX_MEMORIES} memories can be kept \
                     ({existing_count} exist, {} would be added); \
                     update or delete memories that are no longer needed",
                    to_add.len()
                ),
            });
        }

        let now = now_iso8601();
        let mut created = Vec::new();
        for content in &to_add {
            conn.execute(
                "INSERT INTO memories (content, created_at, updated_at) VALUES (?1, ?2, ?2)",
                rusqlite::params![content, now],
            )?;
            created.push(get(conn, conn.last_insert_rowid())?);
        }
        Ok(created)
    })
}

/// メモリの本文の書き換え。`updated_at`は値が変わったかを比べずに進める。他のメモリと同じ
/// 本文にする書き換えは断る(追加と同じく、同じ本文を2件持たない)。
pub fn update(conn: &Connection, memory_id: i64, content: &str) -> Result<Memory> {
    super::in_transaction(conn, |conn| {
        get(conn, memory_id)?;
        let content = normalize_content(content, "content")?;
        if list(conn)?
            .iter()
            .any(|m| m.id != memory_id && m.content == content)
        {
            return Err(CoreError::InvalidArgument {
                name: "content".to_string(),
                reason: "another memory already has this content; delete one of them instead"
                    .to_string(),
            });
        }
        conn.execute(
            "UPDATE memories SET content = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![content, now_iso8601(), memory_id],
        )?;
        get(conn, memory_id)
    })
}

pub fn delete(conn: &Connection, memory_id: i64) -> Result<()> {
    super::in_transaction(conn, |conn| {
        get(conn, memory_id)?;
        conn.execute(
            "UPDATE memories SET deleted_at = ?1 WHERE id = ?2",
            rusqlite::params![now_iso8601(), memory_id],
        )?;
        Ok(())
    })
}

/// 削除済みを除いて1件を引く。無ければ[`CoreError::MemoryNotFound`]。
fn get(conn: &Connection, memory_id: i64) -> Result<Memory> {
    conn.query_row(
        &format!("SELECT {MEMORY_COLUMNS} FROM memories WHERE id = ?1 AND deleted_at IS NULL"),
        [memory_id],
        row_to_memory,
    )
    .optional()?
    .ok_or(CoreError::MemoryNotFound(memory_id))
}

fn row_to_memory(row: &rusqlite::Row) -> rusqlite::Result<Memory> {
    Ok(Memory {
        id: row.get(0)?,
        content: row.get(1)?,
        created_at: row.get(2)?,
        updated_at: row.get(3)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn add_lists_in_the_order_written() {
        let conn = db::open_in_memory().unwrap();
        add(&conn, &strings(&["平日は9時から18時まで働く"])).unwrap();
        add(&conn, &strings(&["締切は2日前に置く"])).unwrap();

        let contents: Vec<String> = list(&conn)
            .unwrap()
            .into_iter()
            .map(|m| m.content)
            .collect();
        assert_eq!(
            contents,
            strings(&["平日は9時から18時まで働く", "締切は2日前に置く"])
        );
    }

    #[test]
    fn add_folds_to_one_visible_line_and_dedupes() {
        let conn = db::open_in_memory().unwrap();
        add(&conn, &strings(&["朝型"])).unwrap();

        let added = add(
            &conn,
            &strings(&[
                "  夜は\n作業しない ",
                "夜は 作業しない",
                "朝\u{200B}型",
                "見え\u{E0041}ない",
            ]),
        )
        .unwrap();

        let contents: Vec<String> = added.into_iter().map(|m| m.content).collect();
        assert_eq!(contents, strings(&["夜は 作業しない", "見えない"]));
    }

    #[test]
    fn add_rejects_blank_or_too_long_content_without_adding_any() {
        let conn = db::open_in_memory().unwrap();
        for bad in ["  \u{200B} ", &"あ".repeat(MAX_MEMORY_CHARS + 1)] {
            let err = add(&conn, &strings(&["残らない", bad])).unwrap_err();
            assert!(matches!(err, CoreError::InvalidArgument { .. }), "{bad:?}");
        }
        assert!(list(&conn).unwrap().is_empty());

        add(&conn, &strings(&[&"あ".repeat(MAX_MEMORY_CHARS)])).unwrap();
    }

    #[test]
    fn add_rejects_going_over_the_limit_without_adding_any() {
        let conn = db::open_in_memory().unwrap();
        let first: Vec<String> = (0..MAX_MEMORIES - 1).map(|i| format!("事実{i}")).collect();
        add(&conn, &first).unwrap();

        let err = add(&conn, &strings(&["追加1", "追加2"])).unwrap_err();
        assert!(matches!(err, CoreError::InvalidArgument { .. }));
        assert_eq!(list(&conn).unwrap().len(), MAX_MEMORIES - 1);

        // 既にある本文は数えないので、上限に届いていても重複だけなら通る。
        add(&conn, &strings(&["追加1", "事実0"])).unwrap();
        assert_eq!(list(&conn).unwrap().len(), MAX_MEMORIES);
    }

    #[test]
    fn update_rewrites_content_with_the_same_rules() {
        let conn = db::open_in_memory().unwrap();
        let id = add(&conn, &strings(&["朝型"])).unwrap()[0].id;

        let updated = update(&conn, id, " 夜型\t").unwrap();
        assert_eq!(updated.content, "夜型");

        let err = update(&conn, id, "\n").unwrap_err();
        assert!(matches!(err, CoreError::InvalidArgument { .. }));
        assert_eq!(list(&conn).unwrap()[0].content, "夜型");

        // 同じ本文への書き換えは、自分自身なら通り、他のメモリと重なるなら断る。
        update(&conn, id, "夜型").unwrap();
        add(&conn, &strings(&["猫が好き"])).unwrap();
        let err = update(&conn, id, "猫が好き").unwrap_err();
        assert!(matches!(err, CoreError::InvalidArgument { .. }));
    }

    /// 字形を選ぶ1個は残し、重ねて文字列を隠す並びは除く。
    #[test]
    fn stacked_variation_selectors_are_dropped() {
        let conn = db::open_in_memory().unwrap();
        let hidden: String = "😀\u{FE0F}\u{E0101}\u{E0102}\u{FE01}".to_string();

        let added = add(&conn, &[hidden, "葛\u{E0100}飾".to_string()]).unwrap();

        assert_eq!(added[0].content, "😀\u{FE0F}");
        assert_eq!(added[1].content, "葛\u{E0100}飾");
    }

    #[test]
    fn deleted_memories_are_gone_from_listing_and_updates() {
        let conn = db::open_in_memory().unwrap();
        let id = add(&conn, &strings(&["朝型"])).unwrap()[0].id;

        delete(&conn, id).unwrap();

        assert!(list(&conn).unwrap().is_empty());
        assert!(matches!(
            update(&conn, id, "夜型"),
            Err(CoreError::MemoryNotFound(_))
        ));
        assert!(matches!(
            delete(&conn, id),
            Err(CoreError::MemoryNotFound(_))
        ));
        // 消した本文は重複の判定に入らない。
        assert_eq!(add(&conn, &strings(&["朝型"])).unwrap().len(), 1);
    }
}
