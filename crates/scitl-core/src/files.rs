//! アプリのデータとして残すファイルの書き込み。途中で落ちても、書きかけのファイルを
//! 完成したファイルの名前で残さない。

use std::io::{self, Write};
use std::path::Path;

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

/// I/Oの失敗を、画面に出すエラー文言にする。パスは載せない(利用者のホームディレクトリ等が
/// 画面とログに出るため)。
pub(crate) fn describe_io_error(action: &str, e: &io::Error) -> String {
    format!("failed to {action}: {:?}", e.kind())
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
}
