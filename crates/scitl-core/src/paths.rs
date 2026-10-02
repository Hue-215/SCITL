//! アプリのデータの置き場所。GUIとCLIが同じファイルを開くため、ディレクトリの解決と
//! その中の並びをここに閉じる。

use std::path::{Path, PathBuf};

use crate::APP_IDENTIFIER;

/// OSがアプリのデータ・キャッシュの置き場所を持たない。
#[derive(Debug, thiserror::Error)]
#[error("this OS has no directory for application data")]
pub struct NoAppDir;

/// 既定のデータディレクトリ。Tauriの`app_data_dir`と同じく、OSごとの場所(`dirs::data_dir`)に
/// 識別子を繋ぐ。
pub fn default_data_dir() -> Result<PathBuf, NoAppDir> {
    dirs::data_dir()
        .map(|dir| dir.join(APP_IDENTIFIER))
        .ok_or(NoAppDir)
}

/// 既定のキャッシュディレクトリ。Tauriの`app_cache_dir`と同じ決め方。
pub fn default_cache_dir() -> Result<PathBuf, NoAppDir> {
    dirs::cache_dir()
        .map(|dir| dir.join(APP_IDENTIFIER))
        .ok_or(NoAppDir)
}

/// データディレクトリの中の並び。
#[derive(Debug, Clone)]
pub struct DataLayout {
    root: PathBuf,
}

impl DataLayout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn database(&self) -> PathBuf {
        self.root.join("scitl.sqlite3")
    }

    pub fn config(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    /// 添付の実体の置き場所。
    pub fn attachments(&self) -> PathBuf {
        self.root.join("attachments")
    }

    /// Markdownエクスポートの書き出し先。
    pub fn export(&self) -> PathBuf {
        self.root.join("export")
    }
}

/// アプリのデータを置くディレクトリを作る。Unixでは持ち主だけが入れる権限(0700)にし、
/// 既にあれば権限をそれに揃える(会話や添付を、同じマシンの他のアカウントに読ませないため)。
/// 親のディレクトリは通常の権限で作り、変えない。Windowsはユーザーごとの`AppData`が初めから
/// 本人だけのものなので、作るだけにする。
///
/// 同じ名前のディレクトリでないものがあれば失敗にする。既にあるディレクトリの権限を変えられない
/// (権限を持たないファイルシステム、持ち主が別のディレクトリ等)ときは、診断に書いて先へ進む。
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    {
        use std::io::ErrorKind;
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        match std::fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() != ErrorKind::AlreadyExists => return Err(e),
            Err(_) => {}
        }
        if !std::fs::metadata(dir)?.is_dir() {
            return Err(ErrorKind::NotADirectory.into());
        }
        if let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)) {
            crate::diagnostics::report(format_args!(
                "could not restrict a data directory to its owner: {e}"
            ));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)
}

/// キャッシュディレクトリの中の、添付を開くために書き出す場所。
pub fn revealed_attachments(cache_dir: &Path) -> PathBuf {
    cache_dir.join("revealed-attachments")
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn private_dir_is_created_or_narrowed_to_its_owner() {
        let temp = tempfile::tempdir().unwrap();
        let created = temp.path().join("parent").join("data");
        create_private_dir(&created).unwrap();
        assert_eq!(mode(&created), 0o700);

        let existing = temp.path().join("existing");
        std::fs::create_dir(&existing).unwrap();
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o755)).unwrap();
        create_private_dir(&existing).unwrap();
        assert_eq!(mode(&existing), 0o700);
    }

    #[test]
    fn a_file_in_place_of_the_dir_is_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("data");
        std::fs::write(&file, b"").unwrap();
        let mode_before = mode(&file);

        assert!(create_private_dir(&file).is_err());
        assert_eq!(mode(&file), mode_before);
    }
}
