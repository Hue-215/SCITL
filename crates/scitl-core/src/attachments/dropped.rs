//! 窓に落としたファイルの受け取り(`docs/spec/architecture/attachments.md`「受け取り方」)。
//! パスはOSのドロップからGUIのシェルへ直接届き、WebViewを通らない。画面へはドロップの番号と
//! 名前だけを知らせ、画面が受け付けると決めたものだけを、番号と位置で指して読む。

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::Serialize;

use super::classify::{classify, LIMITS};
use super::staging::Rejection;
use crate::error::{CoreError, Result};

/// 窓にファイルが落とされたことを画面へ知らせる形。パスは渡さない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct DropNotice {
    pub drop_id: u64,
    /// 落とした順のファイル名。読むときは、この並びの位置で指す。
    pub names: Vec<String>,
}

/// 最後に落とされたファイルのうち、まだ読んでいないもの。次のドロップで置き換わり、読まれ
/// なかった分は捨てる。
#[derive(Default)]
pub(super) struct Dropped {
    last: Mutex<Option<(u64, Vec<Option<PathBuf>>)>>,
    next_id: AtomicU64,
}

impl Dropped {
    /// 落とされたものを受け取る。ファイルのパス(絶対パス)でないものは捨てる。ファイル以外
    /// (ブラウザのリンク・画像のdata URL等)を落とすと、`file://`を外せなかったURIが相対パスとして
    /// 届くため。何も残らなければ`None`で、前のドロップの分はそのまま残す。
    pub(super) fn receive(&self, paths: Vec<PathBuf>) -> Option<DropNotice> {
        let paths: Vec<PathBuf> = paths.into_iter().filter(|p| p.is_absolute()).collect();
        if paths.is_empty() {
            return None;
        }
        let drop_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let names = paths.iter().map(|path| name_of(path)).collect();
        *self.lock() = Some((drop_id, paths.into_iter().map(Some).collect()));
        Some(DropNotice { drop_id, names })
    }

    /// 落としたファイルのパスを取り出す。同じ位置は1回だけ取り出せ、置き換わった前のドロップは
    /// 取り出せない。
    pub(super) fn take(&self, drop_id: u64, index: usize) -> Option<PathBuf> {
        match &mut *self.lock() {
            Some((id, paths)) if *id == drop_id => paths.get_mut(index)?.take(),
            _ => None,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<(u64, Vec<Option<PathBuf>>)>> {
        self.last.lock().expect("dropped files mutex poisoned")
    }
}

/// 元のファイル名。画面とDBに残すのは名前だけで、パスは残さない。
pub(super) fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// 落としたファイルの中身を読む。通常のファイルだけを読み(フォルダ・デバイス・名前付きパイプ等は
/// 断る)、どの種別の上限も超えるものは読まずに断る。読めなければ`Err`で、その表示文はパスを
/// 含まない。
pub(super) fn read(path: &Path) -> Result<std::result::Result<Vec<u8>, Rejection>> {
    let failed =
        |e: std::io::Error| CoreError::Attachment(format!("could not read the dropped file: {e}"));
    // 開く前に確かめる。名前付きパイプは、開くだけで書き手が来るまで止まる。
    let metadata = fs::metadata(path).map_err(failed)?;
    if !metadata.is_file() {
        return Ok(Err(Rejection::NotAFile));
    }
    let largest = LIMITS.largest_bytes();
    let too_large = metadata.len() > largest;
    let file = File::open(path).map_err(failed)?;
    // 大きさは調べた時点のもので、読む間に伸びうるので、読む量も上限で止める。
    let limit = if too_large {
        TOO_LARGE_PREFIX
    } else {
        largest + 1
    };
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes).map_err(failed)?;
    if too_large || bytes.len() as u64 > largest {
        // 種別は読んだ分だけで見る(断る理由の文言に、その種別の上限を添えるため)。
        let kind = classify(&bytes).kind;
        return Ok(Err(Rejection::TooLarge {
            kind,
            limit_bytes: LIMITS.bytes_for(kind),
        }));
    }
    Ok(Ok(bytes))
}

/// 上限を超えるファイルの、種別を見るために読む先頭の長さ。画像の形式は先頭の数バイトで決まる。
const TOO_LARGE_PREFIX: u64 = 4096;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::attachments::AttachmentKind;

    #[test]
    fn reads_regular_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memo.txt");
        fs::write(&path, "hello").unwrap();
        assert_eq!(read(&path).unwrap(), Ok(b"hello".to_vec()));
    }

    #[test]
    fn refuses_folders_without_reading_them() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("secret.txt"), "key").unwrap();
        assert_eq!(read(dir.path()).unwrap(), Err(Rejection::NotAFile));
    }

    #[test]
    fn stops_reading_past_the_largest_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.bin");
        let file = File::create(&path).unwrap();
        file.set_len(LIMITS.largest_bytes() + 1024).unwrap();
        assert_eq!(
            read(&path).unwrap(),
            Err(Rejection::TooLarge {
                kind: AttachmentKind::Other,
                limit_bytes: LIMITS.bytes_for(AttachmentKind::Other),
            })
        );
    }

    #[test]
    fn reports_missing_files_without_their_path() {
        let dir = tempfile::tempdir().unwrap();
        let error = read(&dir.path().join("gone.txt")).unwrap_err().to_string();
        assert!(!error.contains("gone.txt"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_named_pipes_without_opening_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pipe");
        let status = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(read(&path).unwrap(), Err(Rejection::NotAFile));
    }

    #[test]
    fn hands_out_each_dropped_path_once_and_only_for_the_last_drop() {
        let dropped = Dropped::default();
        let first = dropped.receive(vec![absolute("one.txt")]).unwrap();
        // ファイル以外を落としたもの(相対パスで届く)は受け取らず、前のドロップを残す。
        assert_eq!(dropped.receive(vec![PathBuf::from("data:image/png")]), None);
        assert_eq!(dropped.take(first.drop_id, 0), Some(absolute("one.txt")));
        let second = dropped
            .receive(vec![
                absolute("two.txt"),
                PathBuf::from("https:/example.com/x"),
                absolute("three.png"),
            ])
            .unwrap();
        assert_eq!(second.names, ["two.txt", "three.png"]);
        assert_eq!(dropped.take(first.drop_id, 0), None);
        assert_eq!(dropped.take(second.drop_id, 1), Some(absolute("three.png")));
        assert_eq!(dropped.take(second.drop_id, 1), None);
        assert_eq!(dropped.take(second.drop_id, 2), None);
    }

    /// どのOSでも絶対パスになるパス。
    fn absolute(name: &str) -> PathBuf {
        std::env::temp_dir().join(name)
    }
}
