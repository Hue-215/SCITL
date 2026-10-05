pub mod attachments;
pub mod messages;
pub mod task_steps;
pub mod tasks;
pub mod transcripts;

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
const TURN_TRANSCRIPTS_SQL: &str = include_str!("../../../../migrations/0005_turn_transcripts.sql");
const TRANSCRIPT_SERVER_SQL: &str =
    include_str!("../../../../migrations/0006_transcript_server.sql");
const PARTIAL_REPLY_SQL: &str = include_str!("../../../../migrations/0007_partial_reply.sql");

/// DBの列に文字列で持つ列挙。列の値との対応をここに1度だけ書き、書き込み(`ToSql`)と
/// 読み出し(`FromSql`)を同じ対応から作る。値は列のCHECK制約と揃える。
macro_rules! text_column_enum {
    ($ty:ty { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl $ty {
            /// DBの列の値。
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)+
                }
            }
        }

        impl rusqlite::types::ToSql for $ty {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                Ok(self.as_str().into())
            }
        }

        impl rusqlite::types::FromSql for $ty {
            fn column_result(
                value: rusqlite::types::ValueRef<'_>,
            ) -> rusqlite::types::FromSqlResult<Self> {
                match value.as_str()? {
                    $($text => Ok(Self::$variant),)+
                    other => Err(rusqlite::types::FromSqlError::Other(
                        format!("unknown {}: {other}", stringify!($ty)).into(),
                    )),
                }
            }
        }
    };
}
pub(crate) use text_column_enum;

/// 非同期層から使うDBハンドル。`rusqlite::Connection`は`Sync`ではないため`&Connection`を
/// 非同期関数のawaitをまたいで持たせられない。触るときは[`with_conn`]を通す。
pub type SharedConnection = Arc<Mutex<Connection>>;

/// 非同期層からリポジトリ層(同期の`fn`)を呼ぶ唯一の入口。ロックの取得からドロップまでを
/// [`crate::blocking::run`]のクロージャ内に閉じ込め、ロックガードがawaitを
/// またがないようにする。
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

/// 読んで判断してから書く操作を1つの単位にする。途中の文が失敗すれば何も残さず(工程を
/// 一部だけ追加したのにツールは失敗を返す、といった食い違いを作らない)、別プロセスの
/// 書き込みは`f`が終わるまで待たせる。
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
    // 読むより前に書き込みの権利を取る。
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let out = f(&tx)?;
    tx.commit()?;
    Ok(out)
}

/// 書き込みの権利を取ったトランザクションの中で`f`を実行し、結果によらず巻き戻す。書いた
/// 状態を読んだ結果だけが要り、書いたもの自体は残さない場合に使う(送信内容のプレビュー)。
pub(crate) fn in_rolled_back_transaction<T>(
    conn: &Connection,
    f: impl FnOnce(&Connection) -> Result<T>,
) -> Result<T> {
    // SQLiteは入れ子の`BEGIN`を受け付けず、外側の一部だけを巻き戻す手段も無い。
    if !conn.is_autocommit() {
        return Err(CoreError::Internal(
            "cannot roll back part of an outer transaction".to_string(),
        ));
    }
    // 捨てる書き込みでも、読んだ時点から他プロセスの書き込みを止める点は`in_transaction`と同じ。
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    f(&tx)
    // `tx`は確定せずに落とすので、ここで巻き戻る。
}

/// 番号順のマイグレーション。`user_version`は、この列の先頭から何個を適用済みかを表す。
const MIGRATIONS: &[&str] = &[
    INIT_SQL,
    TOOL_EXECUTION_ROLE_SQL,
    ERROR_DETAIL_SQL,
    MESSAGE_ORIGIN_SQL,
    TURN_TRANSCRIPTS_SQL,
    TRANSCRIPT_SERVER_SQL,
    PARTIAL_REPLY_SQL,
];

/// 先頭から`target`個目までのマイグレーションを適用する。適用済みの版の読み取りから
/// 版の書き込みまでを1つのトランザクションに収め、GUIとCLIが同時に開いても、遅れた側は
/// 先に適用された版を読んでから判断する。
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

/// ISO8601 UTC(`YYYY-MM-DDTHH:MM:SSZ`)。生成箇所をここに集約する。
pub fn now_iso8601() -> String {
    iso8601(chrono::Utc::now())
}

fn iso8601(at: chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// 自由入力の値の長さの上限を確かめる。超えた値は切らずに断り、モデルに短く書き直させる
/// (黙って切ると、モデルは自分の指定が変わったことに気付けない)。数えるのはUnicodeの
/// スカラー値(`char`)の数。
pub(super) fn check_max_chars(name: &str, value: &str, max: usize) -> Result<()> {
    let count = value.chars().count();
    if count > max {
        return Err(CoreError::InvalidArgument {
            name: name.to_string(),
            reason: format!("must be at most {max} characters (got {count}); shorten it"),
        });
    }
    Ok(())
}

/// 他プロセスのロックを待つ上限。
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// 接続を開き、PRAGMAとマイグレーションを適用する。
pub fn open<P: AsRef<Path>>(path: P) -> Result<Connection> {
    let conn = Connection::open(path)?;
    // `enable_wal`の待ちを自前の再試行だけにするため、`busy_timeout`はその後に設定する。
    enable_wal(&conn)?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    migrate_to(&conn, MIGRATIONS.len())?;
    Ok(conn)
}

/// WALへ切り替える。作成直後のDBを別プロセスが同時に開いていると、この文は`busy_timeout`を
/// 待たずに`SQLITE_BUSY`を返すので、[`BUSY_TIMEOUT`]まで間を置いて送り直す。
fn enable_wal(conn: &Connection) -> Result<()> {
    const RETRY_INTERVAL: Duration = Duration::from_millis(10);
    let started = std::time::Instant::now();
    loop {
        match conn.pragma_update(None, "journal_mode", "WAL") {
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::DatabaseBusy
                    && started.elapsed() < BUSY_TIMEOUT =>
            {
                std::thread::sleep(RETRY_INTERVAL);
            }
            result => return Ok(result?),
        }
    }
}

/// DBのファイルか、それを置くディレクトリに書き込めないための失敗か。
pub fn is_read_only_error(e: &CoreError) -> bool {
    has_error_code(e, rusqlite::ErrorCode::ReadOnly)
}

/// DBのファイルを開けも作れもしなかった失敗か(置いたディレクトリに書き込めない等)。
pub fn is_cannot_open_error(e: &CoreError) -> bool {
    has_error_code(e, rusqlite::ErrorCode::CannotOpen)
}

fn has_error_code(e: &CoreError, code: rusqlite::ErrorCode) -> bool {
    matches!(
        e,
        CoreError::Db(rusqlite::Error::SqliteFailure(failure, _)) if failure.code == code
    )
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
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scitl.sqlite3");
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
    }

    #[test]
    fn opening_during_another_process_migration_does_not_apply_it_twice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scitl.sqlite3");
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
    }

    /// 開く処理が重なるかは実行のたびに違うので、そろえて開く8本を10回繰り返す。
    #[test]
    fn many_connections_can_create_the_same_database_at_once() {
        for _ in 0..10 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("scitl.sqlite3");
            let start = Arc::new(std::sync::Barrier::new(8));
            let openers: Vec<_> = (0..8)
                .map(|_| {
                    let path = path.clone();
                    let start = Arc::clone(&start);
                    std::thread::spawn(move || {
                        start.wait();
                        open(path).map(drop)
                    })
                })
                .collect();

            for opener in openers {
                let opened = opener.join().unwrap();
                assert!(opened.is_ok(), "{opened:?}");
            }
            let mode: String = open(&path)
                .unwrap()
                .query_row("PRAGMA journal_mode", [], |row| row.get(0))
                .unwrap();
            assert_eq!(mode, "wal");
        }
    }

    /// 辞書順が時系列順になる固定幅の形。秒未満は書かず、UTCは`Z`で書く。
    #[test]
    fn timestamps_are_fixed_width_utc_seconds() {
        let at = chrono::DateTime::from_timestamp(1_790_000_000, 999_999_999).unwrap();
        assert_eq!(iso8601(at), "2026-09-21T14:13:20Z");
        let epoch = chrono::DateTime::from_timestamp(0, 0).unwrap();
        assert_eq!(iso8601(epoch), "1970-01-01T00:00:00Z");
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
