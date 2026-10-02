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

/// キャッシュディレクトリの中の、添付を開くために書き出す場所。
pub fn revealed_attachments(cache_dir: &Path) -> PathBuf {
    cache_dir.join("revealed-attachments")
}
