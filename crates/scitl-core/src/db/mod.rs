pub mod error;
pub mod messages;
pub mod task_steps;
pub mod tasks;

pub use rusqlite::Connection;
use rusqlite_migration::{Migrations, M};
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

pub use error::{CoreError, Result};

const INIT_SQL: &str = include_str!("../../../../migrations/0001_init.sql");
const TOOL_EXECUTION_ROLE_SQL: &str =
    include_str!("../../../../migrations/0002_tool_execution_role.sql");

/// 非同期層から使うDBハンドル。`rusqlite::Connection`は`Sync`ではないため`&Connection`を
/// 非同期関数のawaitをまたいで持たせられない(architecture.md 4節)。触るときは[`with_conn`]を通す。
pub type SharedConnection = Arc<Mutex<Connection>>;

/// 非同期層からリポジトリ層(同期の`fn`)を呼ぶ唯一の入口。ロックの取得からドロップまでを
/// [`crate::blocking::run`]のクロージャ内に閉じ込め、ロックガードがawaitをまたがないようにする
/// (architecture.md 4節)。
pub async fn with_conn<F, T>(db: SharedConnection, f: F) -> Result<T>
where
    F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    crate::blocking::run(move || {
        let conn = db.lock().expect("db mutex poisoned");
        f(&conn)
    })
    .await
}

/// 複数文にわたる書き込みを1つの単位にする。途中の文が失敗すれば何も残さない
/// (工程を一部だけ追加したのにツールは失敗を返す、といった食い違いを作らない)。
/// 読んでから書く操作の直列化は、今はプロセス内で接続を包む`Mutex`が担っている。
/// 複数プロセスを跨いだ排他の方式(`BEGIN IMMEDIATE`等)はIssue #74で決める(data-model.md 4節)。
///
/// 既にトランザクションの中で呼ばれたら、新しく始めずにその中で実行する(SQLiteは入れ子の
/// `BEGIN`を受け付けない)。確定と巻き戻しは外側に任せる。
pub(crate) fn in_transaction<T>(
    conn: &Connection,
    f: impl FnOnce(&Connection) -> Result<T>,
) -> Result<T> {
    if !conn.is_autocommit() {
        return f(conn);
    }
    let tx = conn.unchecked_transaction()?;
    let out = f(&tx)?;
    tx.commit()?;
    Ok(out)
}

static MIGRATIONS: LazyLock<Migrations<'static>> =
    LazyLock::new(|| Migrations::new(vec![M::up(INIT_SQL), M::up(TOOL_EXECUTION_ROLE_SQL)]));

/// ISO8601 UTC(`YYYY-MM-DDTHH:MM:SSZ`)。生成箇所をここに集約する
/// (docs/spec/rebuild/data-model.md 1節)。
pub fn now_iso8601() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch");
    format_unix_utc(now.as_secs())
}

fn format_unix_utc(secs: u64) -> String {
    // 外部クレート無しでUTCの日時文字列を組み立てる(civil_from_days, Howard Hinnant方式)。
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m_num = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m_num <= 2 { y + 1 } else { y };

    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, m_num, d, h, m, s)
}

/// 接続を開き、PRAGMAとマイグレーションを適用する
/// (docs/spec/rebuild/data-model.md 4節・5節、architecture.md 4節)。
pub fn open<P: AsRef<Path>>(path: P) -> Result<Connection> {
    let mut conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(Duration::from_secs(5))?;
    MIGRATIONS.to_latest(&mut conn)?;
    Ok(conn)
}

pub fn open_in_memory() -> Result<Connection> {
    let mut conn = Connection::open_in_memory()?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    MIGRATIONS.to_latest(&mut conn)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_valid() {
        MIGRATIONS.validate().unwrap();
    }

    #[test]
    fn existing_tool_execution_records_move_to_the_tool_role() {
        let mut conn = Connection::open_in_memory().unwrap();
        MIGRATIONS.to_version(&mut conn, 1).unwrap();
        conn.execute(
            "INSERT INTO messages (role, content, kind, turn_id, attempt_no, created_at)
             VALUES ('assistant', '{}', 'tool_execution', 't', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        MIGRATIONS.to_latest(&mut conn).unwrap();

        let role: String = conn
            .query_row("SELECT role FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(role, "tool");
    }

    #[test]
    fn in_transaction_leaves_nothing_when_a_later_statement_fails() {
        let conn = open_in_memory().unwrap();
        let task_id = tasks::create_task(&conn).unwrap().id;

        let result: Result<()> = in_transaction(&conn, |conn| {
            task_steps::add_steps(conn, task_id, &["買い出し".to_string()])?;
            Err(CoreError::Internal(
                "fail after the first write".to_string(),
            ))
        });

        assert!(matches!(result, Err(CoreError::Internal(_))), "{result:?}");
        assert!(task_steps::list_for_task(&conn, task_id)
            .unwrap()
            .is_empty());
    }
}
