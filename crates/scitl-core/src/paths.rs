//! アプリのデータの置き場所。GUIとCLIが同じファイルを開くため、ディレクトリの解決と
//! その中の並びをここに閉じる。OSがアプリに与える場所(Android)はTauriのパス解決で決まるので、
//! GUIが決めたパスを受け取る(このモジュールはTauriに依存しない)。

use std::path::{Path, PathBuf};

use crate::{CoreError, APP_IDENTIFIER};

/// OSがアプリのキャッシュの置き場所を持たない。
#[derive(Debug, thiserror::Error)]
#[error("this OS has no directory for application cache")]
pub struct NoAppDir;

/// データディレクトリの名前。実行ファイルのフォルダ(デスクトップ)か、OSがアプリに与えた場所(Android)の中に置く。
const DATA_DIR_NAME: &str = "data";

/// データディレクトリを決められない・使えない理由。GUIは起動時に開けなかった理由として
/// 画面へ渡し、画面は種類で文言を選ぶ。パスは利用者が置き場所を直すのに要るので載せる。
#[derive(Debug, Clone, serde::Serialize, thiserror::Error)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DataDirError {
    /// 実行ファイルの場所が分からない。
    #[error("could not locate this executable: {reason}")]
    NoExecutable { reason: String },
    /// 実行ファイルが一時ディレクトリの中にある(アーカイブを展開せずに中から起動した等)。
    /// そこに書いたデータは消されうるので使わない。
    #[error(
        "this executable is in a temporary directory ({dir}); extract it to a permanent location"
    )]
    TemporaryDir { dir: String },
    /// OSがアプリのデータ・キャッシュの置き場所を示さない。
    #[error("the OS did not give a directory for this application: {reason}")]
    NoAppDir { reason: String },
    /// データディレクトリを作れない・書き込めない(書き込めない場所に置いた等)。
    #[error("could not open the data directory {dir}: {reason}")]
    Unusable { dir: String, reason: String },
    /// OSがアプリに与えた場所(Android)のデータディレクトリを作れない・書き込めない。
    /// 利用者は置き場所を変えられないので、[`Self::Unusable`]と分けて文言を選ばせる。
    #[error("could not open the data directory {dir}: {reason}")]
    AppDirUnusable { dir: String, reason: String },
    /// データディレクトリには書けるが、DBを開けない(新しい版で使ったDB、壊れたDB等)。
    #[error("could not open the database in {dir}: {reason}")]
    Database { dir: String, reason: String },
}

impl DataDirError {
    /// `dir`の中のDBを開けなかった失敗。書き込めないための失敗は置き場所の問題として
    /// [`Self::Unusable`]、それ以外は[`Self::Database`]にする。
    pub fn from_database(dir: &Path, e: &CoreError) -> Self {
        let dir = dir.display().to_string();
        let reason = e.to_string();
        if crate::db::is_read_only_error(e) || crate::db::is_cannot_open_error(e) {
            Self::Unusable { dir, reason }
        } else {
            Self::Database { dir, reason }
        }
    }

    /// OSがアプリに与えた場所で起きた失敗として読み替える。置き場所を移すよう促す
    /// [`Self::Unusable`]を[`Self::AppDirUnusable`]にし、ほかはそのまま返す。
    pub fn in_app_dir(self) -> Self {
        match self {
            Self::Unusable { dir, reason } => Self::AppDirUnusable { dir, reason },
            other => other,
        }
    }
}

/// デスクトップのデータディレクトリ。実行ファイルと同じフォルダの`data`で、フォルダごと持ち運べる。
/// GUIとCLIは同じフォルダに置くので、同じデータを開く。
pub fn data_dir_beside_executable() -> Result<PathBuf, DataDirError> {
    let exe = std::env::current_exe().map_err(|e| DataDirError::NoExecutable {
        reason: e.to_string(),
    })?;
    data_dir_beside(&exe, &std::env::temp_dir())
}

/// OSがアプリに与えた場所(Androidのアプリの内部ストレージ)の中のデータディレクトリ。
/// その場所の直下にはWebViewのプロファイル等も置かれるので、混ざらないよう`data`の下に置く。
pub fn data_dir_within(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join(DATA_DIR_NAME)
}

fn data_dir_beside(exe: &Path, temp_dir: &Path) -> Result<PathBuf, DataDirError> {
    let dir = exe.parent().ok_or_else(|| DataDirError::NoExecutable {
        reason: "the executable has no parent directory".to_string(),
    })?;
    if is_within(dir, temp_dir) {
        return Err(DataDirError::TemporaryDir {
            dir: dir.display().to_string(),
        });
    }
    Ok(data_dir_within(dir))
}

/// `dir`が`temp_dir`の中にあるか。どちらもリンクと短い形の名前(Windowsの8.3形式)を
/// 解決してから比べる。解決できなければ中に無いものとする。一時ディレクトリがルートなら
/// すべてが中に入るので、中に無いものとする。
fn is_within(dir: &Path, temp_dir: &Path) -> bool {
    match (dir.canonicalize(), temp_dir.canonicalize()) {
        (Ok(dir), Ok(temp_dir)) => temp_dir.parent().is_some() && dir.starts_with(temp_dir),
        _ => false,
    }
}

/// 既定のキャッシュディレクトリ。GUIはTauriの`app_cache_dir`で決めるので、CLIがデスクトップで
/// 同じ場所を使うために、それと同じ決め方をする。
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
/// 親のディレクトリは通常の権限で作り、変えない。Windowsでは作るだけにし、権限は置いた
/// 場所のものを引き継ぐ。
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

#[cfg(test)]
mod data_dir_tests {
    use super::*;

    #[test]
    fn data_dir_is_beside_the_executable() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("app");
        let elsewhere = temp.path().join("tmp");
        std::fs::create_dir(&app).unwrap();
        std::fs::create_dir(&elsewhere).unwrap();

        let dir = data_dir_beside(&app.join("scitl"), &elsewhere).unwrap();
        assert_eq!(dir, app.join("data"));
    }

    #[test]
    fn an_executable_in_the_temporary_directory_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let extracted = temp.path().join("Temp1_scitl.zip").join("scitl");
        std::fs::create_dir_all(&extracted).unwrap();

        let result = data_dir_beside(&extracted.join("scitl.exe"), temp.path());
        assert!(matches!(result, Err(DataDirError::TemporaryDir { .. })));
    }

    #[test]
    fn a_failure_in_the_app_dir_does_not_ask_to_move_the_executable() {
        let unusable = DataDirError::Unusable {
            dir: "d".to_string(),
            reason: "r".to_string(),
        };
        assert!(matches!(
            unusable.in_app_dir(),
            DataDirError::AppDirUnusable { .. }
        ));
        let database = DataDirError::Database {
            dir: "d".to_string(),
            reason: "r".to_string(),
        };
        assert!(matches!(
            database.in_app_dir(),
            DataDirError::Database { .. }
        ));
    }

    #[test]
    fn a_database_that_cannot_be_read_is_not_blamed_on_the_location() {
        let temp = tempfile::tempdir().unwrap();
        let data = DataLayout::new(temp.path());
        std::fs::write(
            data.database(),
            b"not a database, but long enough to have a header",
        )
        .unwrap();

        let e = crate::db::open(data.database()).unwrap_err();
        let failure = DataDirError::from_database(data.root(), &e);
        assert!(matches!(failure, DataDirError::Database { .. }));
    }

    /// 書き込めないディレクトリでは、DBを作れずに失敗する。root権限では書けてしまうので確かめない。
    #[cfg(unix)]
    #[test]
    fn a_directory_that_cannot_be_written_is_blamed_on_the_location() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let data = DataLayout::new(temp.path().join("data"));
        std::fs::create_dir(data.root()).unwrap();
        std::fs::set_permissions(data.root(), std::fs::Permissions::from_mode(0o500)).unwrap();
        if std::fs::write(data.root().join("probe"), b"").is_ok() {
            return;
        }

        let e = crate::db::open(data.database()).unwrap_err();
        let failure = DataDirError::from_database(data.root(), &e);
        std::fs::set_permissions(data.root(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            matches!(failure, DataDirError::Unusable { .. }),
            "{failure:?}"
        );
    }

    /// 一時ディレクトリがルートを指していても、すべてを断らない。
    #[test]
    fn a_temporary_directory_at_the_root_refuses_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().ancestors().last().unwrap();

        assert!(data_dir_beside(&temp.path().join("scitl"), root).is_ok());
    }
}
