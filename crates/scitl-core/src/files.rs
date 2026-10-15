//! アプリのデータとして残すファイルの書き込み。途中で落ちても、書きかけのファイルを
//! 完成したファイルの名前で残さない。

use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, SystemTime};

use ulid::Ulid;

/// `path`へ`bytes`を書く。同じディレクトリの一時ファイルに書き切って同期してから置き換え、
/// 置き換え自体も同期する。
///
/// 直接書くと、書き込み途中で落ちたときに壊れたファイルが残る。同期せずに置き換えると、
/// 電源断で置き換えだけが残り、中身の無いファイルが完成した名前で残りうる。置き換えの同期に
/// 失敗しても書き込みは済んでいるので、エラーにはしない。
pub(crate) fn write_durably(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(path.file_name().unwrap_or_default());
    temp_name.push(format!(".{}.tmp", Ulid::new()));
    let temp = path.with_file_name(temp_name);

    let written = std::fs::File::create(&temp).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    if let Err(e) = written.and_then(|()| std::fs::rename(&temp, path)) {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    sync_parent_dir(path);
    Ok(())
}

#[cfg(unix)]
fn sync_parent_dir(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::File::open(parent).and_then(|dir| dir.sync_all()) {
            crate::diagnostics::report(format_args!("failed to sync a data directory: {e}"));
        }
    }
}

/// Windowsではディレクトリを開いて同期できない(置き換えは`MoveFileEx`に任せる)。
#[cfg(not(unix))]
fn sync_parent_dir(_path: &Path) {}

/// [`write_durably`]の一時ファイルのうち、作ってからこれだけ経ったものは書きかけの残りとみなす。
/// 別のプロセス(CLI・同期)が書いている最中のものを消さないため、書き切るのにかかる時間より十分長くとる。
const STALE_TEMP_AGE: Duration = Duration::from_secs(60 * 60);

/// `dir`に残った[`write_durably`]の一時ファイルを消す。書いてから置き換えるまでの間に
/// プロセスが終わると残り、ほかに消す手段が無い。作った時刻は名前のULIDから読む。
/// 失敗は診断に書くだけにする(残っても動作には障らない)。
pub(crate) fn remove_stale_temp_files(dir: &Path) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return,
        Err(e) => {
            crate::diagnostics::report(format_args!("failed to list a data directory: {e}"));
            return;
        }
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let created = entry.file_name().to_str().and_then(temp_file_created_at);
        let stale = created.is_some_and(|created| {
            now.duration_since(created)
                .is_ok_and(|age| age >= STALE_TEMP_AGE)
        });
        if !stale || !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        if let Err(e) = std::fs::remove_file(entry.path()) {
            if e.kind() != io::ErrorKind::NotFound {
                crate::diagnostics::report(format_args!("failed to remove a temporary file: {e}"));
            }
        }
    }
}

/// `.<名前>.<ULID>.tmp`の形の名前なら、ULIDに入っている作った時刻。
fn temp_file_created_at(name: &str) -> Option<SystemTime> {
    let (rest, ulid) = name
        .strip_prefix('.')?
        .strip_suffix(".tmp")?
        .rsplit_once('.')?;
    if rest.is_empty() {
        return None;
    }
    Ulid::from_string(ulid).ok().map(|ulid| ulid.datetime())
}

/// I/Oの失敗を、画面に出すエラー文言にする。理由はOSのエラー文で書く。パスは載せない
/// (利用者のホームディレクトリ等が画面とログに出るため)。OSが返したエラーの文にパスは
/// 入らないので、パスを含む文で自前に組み立てた`io::Error`は渡さない。
pub(crate) fn describe_io_error(action: &str, e: &io::Error) -> String {
    format!("failed to {action}: {e}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_the_file_without_leaving_a_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data");
        write_durably(&path, b"first").unwrap();
        write_durably(&path, b"second").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("data")]);
    }

    #[test]
    fn a_failed_write_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        // 置き換え先がディレクトリなので、改名に失敗する。
        let path = dir.path().join("occupied");
        std::fs::create_dir_all(path.join("inner")).unwrap();

        assert!(write_durably(&path, b"data").is_err());
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("occupied")]);
    }

    #[test]
    fn removes_only_temporary_files_old_enough_to_be_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let temp_named = |created: SystemTime| {
            let ulid = Ulid::from_datetime(created);
            dir.path().join(format!(".config.toml.{ulid}.tmp"))
        };
        let now = SystemTime::now();
        let stale = temp_named(now - STALE_TEMP_AGE - Duration::from_secs(1));
        let kept = [
            temp_named(now),
            // 時計が進んでいるプロセスが書いている最中のもの。
            temp_named(now + STALE_TEMP_AGE),
            dir.path().join("config.toml"),
            dir.path().join(".config.toml.not-a-ulid.tmp"),
            dir.path().join(format!(
                ".{}.tmp",
                Ulid::from_datetime(SystemTime::UNIX_EPOCH)
            )),
        ];
        for path in kept.iter().chain([&stale]) {
            std::fs::write(path, b"data").unwrap();
        }
        let stale_dir = temp_named(now - STALE_TEMP_AGE * 2);
        std::fs::create_dir(&stale_dir).unwrap();

        remove_stale_temp_files(dir.path());

        assert!(!stale.exists());
        assert!(stale_dir.is_dir());
        for path in &kept {
            assert!(path.exists(), "{}", path.display());
        }
    }

    #[test]
    fn a_missing_directory_has_no_temporary_files() {
        let dir = tempfile::tempdir().unwrap();
        remove_stale_temp_files(&dir.path().join("missing"));
    }

    #[test]
    fn an_io_error_is_described_with_the_message_of_the_os() {
        let dir = tempfile::tempdir().unwrap();
        let e = std::fs::read(dir.path().join("missing")).unwrap_err();

        let described = describe_io_error("read the file", &e);

        assert_eq!(described, format!("failed to read the file: {e}"));
        assert!(!described.contains("NotFound"), "{described}");
        assert!(
            !described.contains(&dir.path().display().to_string()),
            "{described}"
        );
    }
}
