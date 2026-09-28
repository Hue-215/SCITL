pub mod attachments;
pub mod messages;
pub mod task_steps;
pub mod tasks;

pub use rusqlite::Connection;
use rusqlite::{Transaction, TransactionBehavior};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::error::{CoreError, Result};

const INIT_SQL: &str = include_str!("../../../../migrations/0001_init.sql");
const TOOL_EXECUTION_ROLE_SQL: &str =
    include_str!("../../../../migrations/0002_tool_execution_role.sql");
const ERROR_DETAIL_SQL: &str = include_str!("../../../../migrations/0003_error_detail.sql");
const MESSAGE_ORIGIN_SQL: &str = include_str!("../../../../migrations/0004_message_origin.sql");

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

/// 読んで判断してから書く操作を1つの単位にする。途中の文が失敗すれば何も残さず
/// (工程を一部だけ追加したのにツールは失敗を返す、といった食い違いを作らない)、
/// 別プロセスの書き込みは`f`が終わるまで待たせる(data-model.md 4節)。
///
/// 既にトランザクションの中で呼ばれたら、新しく始めずにその中で実行する(SQLiteは入れ子の
/// `BEGIN`を受け付けない)。開始方法・確定・巻き戻しは外側に従うので、トランザクションは
/// すべてこの関数で始める。
pub(crate) fn in_transaction<T>(
    conn: &Connection,
    f: impl FnOnce(&Connection) -> Result<T>,
) -> Result<T> {
    if !conn.is_autocommit() {
        return f(conn);
    }
    // 読むより前に書き込みの権利を取る(data-model.md 4節)。
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let out = f(&tx)?;
    tx.commit()?;
    Ok(out)
}

/// 番号順のマイグレーション。`user_version`は、この列の先頭から何個を適用済みかを表す
/// (data-model.md 5節)。
const MIGRATIONS: &[&str] = &[
    INIT_SQL,
    TOOL_EXECUTION_ROLE_SQL,
    ERROR_DETAIL_SQL,
    MESSAGE_ORIGIN_SQL,
];

/// 先頭から`target`個目までのマイグレーションを適用する。適用済みの版の読み取りから
/// 版の書き込みまでを1つのトランザクションに収め、GUIとCLIが同時に開いても、遅れた側は
/// 先に適用された版を読んでから判断する(data-model.md 5節)。
fn migrate_to(conn: &Connection, target: usize) -> Result<()> {
    debug_assert!(target <= MIGRATIONS.len());
    let applied_version =
        |conn: &Connection| conn.query_row("PRAGMA user_version", [], |row| row.get::<_, usize>(0));
    // 版は増える一方なので、既に届いていればロックを取らずに済ませる。他プロセスの書き込みを
    // 待たずに開ける。
    if applied_version(conn)? == target {
        return Ok(());
    }
    in_transaction(conn, |conn| {
        let applied = applied_version(conn)?;
        if applied > MIGRATIONS.len() {
            return Err(CoreError::Migration(format!(
                "database schema version {applied} is newer than this build supports ({})",
                MIGRATIONS.len()
            )));
        }
        if applied >= target {
            return Ok(());
        }
        for sql in &MIGRATIONS[applied..target] {
            conn.execute_batch(sql)?;
        }
        conn.pragma_update(None, "user_version", target)?;
        Ok(())
    })
}

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
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(Duration::from_secs(5))?;
    migrate_to(&conn, MIGRATIONS.len())?;
    Ok(conn)
}

pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    migrate_to(&conn, MIGRATIONS.len())?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CoreError;

    #[test]
    fn existing_tool_execution_records_move_to_the_tool_role() {
        let conn = Connection::open_in_memory().unwrap();
        migrate_to(&conn, 1).unwrap();
        conn.execute(
            "INSERT INTO messages (role, content, kind, turn_id, attempt_no, created_at)
             VALUES ('assistant', '{}', 'tool_execution', 't', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        migrate_to(&conn, MIGRATIONS.len()).unwrap();

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

    #[test]
    fn in_transaction_keeps_other_processes_out_from_the_first_read() {
        let dir = std::env::temp_dir().join(format!("scitl-db-test-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("scitl.sqlite3");
        let conn = open(&path).unwrap();
        let other_process = open(&path).unwrap();
        other_process
            .busy_timeout(Duration::from_millis(50))
            .unwrap();
        let task_id = tasks::create_task(&conn).unwrap().id;

        in_transaction(&conn, |conn| {
            tasks::get_task(conn, task_id)?;

            let interleaved = tasks::create_task(&other_process);
            assert!(
                matches!(
                    &interleaved,
                    Err(CoreError::Db(rusqlite::Error::SqliteFailure(e, _)))
                        if e.code == rusqlite::ErrorCode::DatabaseBusy
                ),
                "{interleaved:?}"
            );

            task_steps::add_steps(conn, task_id, &["買い出し".to_string()])
        })
        .unwrap();

        drop((conn, other_process));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn opening_during_another_process_migration_does_not_apply_it_twice() {
        let dir = std::env::temp_dir().join(format!("scitl-db-test-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("scitl.sqlite3");
        let migrating = Connection::open(&path).unwrap();
        migrating
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        let latest = MIGRATIONS.len();
        migrate_to(&migrating, latest - 1).unwrap();

        // もう一方のプロセスが最後のマイグレーションを適用している途中に開く。
        let tx = Transaction::new_unchecked(&migrating, TransactionBehavior::Immediate).unwrap();
        tx.execute_batch(MIGRATIONS[latest - 1]).unwrap();
        tx.pragma_update(None, "user_version", latest).unwrap();
        let late = std::thread::spawn({
            let path = path.clone();
            move || open(path).map(drop)
        });
        std::thread::sleep(Duration::from_millis(200));
        tx.commit().unwrap();

        let opened = late.join().unwrap();
        assert!(opened.is_ok(), "{opened:?}");

        drop(migrating);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_database_from_a_newer_build_is_not_opened() {
        let conn = open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", MIGRATIONS.len() + 1)
            .unwrap();

        let result = migrate_to(&conn, MIGRATIONS.len());

        assert!(matches!(result, Err(CoreError::Migration(_))), "{result:?}");
    }
}
