//! 画面の外から届くファイルの受け取り(`docs/spec/architecture/attachments.md`「受け取り方」)。
//! 窓に落としたファイル・選択画面で選んだファイル・クリップボードの画像は、どれもGUIのシェルが
//! OSから受け取り、WebViewを通らない。画面へは受け取りの番号と名前だけを知らせ、画面が受け付けると
//! 決めたものだけを、番号と位置で指して読む。

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::Serialize;

use super::classify::{classify_prefix, LIMITS};
use super::staging::Rejection;
use crate::error::{CoreError, Result};

/// 受け取ったファイル1つと、その読み方。
pub enum ReceivedFile {
    /// 窓に落とした・選択画面で選んだファイルのパス(デスクトップ)。
    Path(PathBuf),
    /// OSが開いて渡すもの(Androidの選択画面が返す`content://`のURI)。`open`はGUIのシェルが
    /// 渡す開き方で、読むときに1回だけ呼ぶ。
    Opened {
        name: String,
        open: Box<dyn FnOnce() -> io::Result<File> + Send>,
    },
    /// 受け取った時点で中身まで手元にあるもの(クリップボードの画像)。
    Bytes { name: String, bytes: Vec<u8> },
}

impl ReceivedFile {
    /// 画面とDBに残す名前。パスは残さない。
    fn name(&self) -> String {
        match self {
            Self::Path(path) => name_of(path),
            Self::Opened { name, .. } | Self::Bytes { name, .. } => name.clone(),
        }
    }
}

/// ファイルを受け取ったことを画面へ知らせる形。パスは渡さない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct ReceivedFiles {
    pub batch_id: u64,
    /// 受け取った順のファイル名。読むときは、この並びの位置で指す。
    pub names: Vec<String>,
}

/// 最後に受け取ったファイルのうち、まだ読んでいないもの。次に受け取ると置き換わり、読まれ
/// なかった分は捨てる。
#[derive(Default)]
pub(super) struct Received {
    last: Mutex<Option<(u64, Vec<Option<ReceivedFile>>)>>,
    next_id: AtomicU64,
}

impl Received {
    /// 受け取る。パスは絶対パスでないものを捨てる。ファイル以外(ブラウザのリンク・画像の
    /// data URL等)を窓に落とすと、`file://`を外せなかったURIが相対パスとして届くため。何も
    /// 残らなければ`None`で、前に受け取った分はそのまま残す。
    pub(super) fn receive(&self, files: Vec<ReceivedFile>) -> Option<ReceivedFiles> {
        let files: Vec<ReceivedFile> = files
            .into_iter()
            .filter(|f| !matches!(f, ReceivedFile::Path(p) if !p.is_absolute()))
            .collect();
        if files.is_empty() {
            return None;
        }
        let batch_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let names = files.iter().map(ReceivedFile::name).collect();
        *self.lock() = Some((batch_id, files.into_iter().map(Some).collect()));
        Some(ReceivedFiles { batch_id, names })
    }

    /// 受け取ったファイルを取り出す。同じ位置は1回だけ取り出せ、置き換わった前の分は
    /// 取り出せない。
    pub(super) fn take(&self, batch_id: u64, index: usize) -> Option<ReceivedFile> {
        match &mut *self.lock() {
            Some((id, files)) if *id == batch_id => files.get_mut(index)?.take(),
            _ => None,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<(u64, Vec<Option<ReceivedFile>>)>> {
        self.last.lock().expect("received files mutex poisoned")
    }
}

/// OSが渡すURI(Androidの選択画面の`content://`)から、名前に使う最後の区間を取り出す。表示名を
/// 引く手段(Androidの`OpenableColumns.DISPLAY_NAME`)をRust側から持たないので、その代わり。提供元に
/// よっては`image:1000000034`のように元のファイル名にならない。
pub fn name_from_uri(uri: &str) -> String {
    let path = uri.split(['?', '#']).next().unwrap_or_default();
    let last = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let decoded = percent_encoding::percent_decode_str(last).decode_utf8_lossy();
    // 文書の提供元は`primary:Download/memo.txt`のように、区切りを符号化した道筋を最後の区間に置く。
    let name = decoded
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    // 名前が取れない提供元でも読めるよう、書き出しのときの既定の名前(`safe_file_name`)と揃える。
    if name.trim().is_empty() {
        "attachment".to_string()
    } else {
        name.to_string()
    }
}

/// 元のファイル名。
pub(super) fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// 受け取ったファイルの名前と中身を読む。フォルダ・デバイス・名前付きパイプ等は断り、どの種別の
/// 上限も超えるものは読み切らずに断る。読めなければ`Err`で、その表示文はパスを含まない。
pub(super) fn read(
    file: ReceivedFile,
) -> Result<std::result::Result<(String, Vec<u8>), Rejection>> {
    let name = file.name();
    let bytes = match file {
        ReceivedFile::Path(path) => read_path(&path)?,
        ReceivedFile::Opened { open, .. } => {
            let file = open().map_err(failed)?;
            let metadata = file.metadata().map_err(failed)?;
            if metadata.is_dir() {
                return Ok(Err(Rejection::NotAFile));
            }
            // OSが開いて渡すものは、通常のファイルでないこと(パイプで流す提供元)もあるので、
            // 大きさが分かるときだけ先に見て、あとは読む量で止める。
            read_limited(file, metadata.is_file().then_some(metadata.len()))?
        }
        ReceivedFile::Bytes { bytes, .. } => Ok(bytes),
    };
    Ok(bytes.map(|bytes| (name, bytes)))
}

fn failed(e: io::Error) -> CoreError {
    CoreError::Attachment(format!("could not read the received file: {e}"))
}

/// 通常のファイルだけを読む。
fn read_path(path: &Path) -> Result<std::result::Result<Vec<u8>, Rejection>> {
    // 開く前に確かめる。名前付きパイプは、開くだけで書き手が来るまで止まる。
    let metadata = fs::metadata(path).map_err(failed)?;
    if !metadata.is_file() {
        return Ok(Err(Rejection::NotAFile));
    }
    let file = File::open(path).map_err(failed)?;
    read_limited(file, Some(metadata.len()))
}

/// どの種別の上限も超えるものは、読み切らずに断る。`len`は調べた時点の大きさで、読む間に
/// 伸びうるので、読む量も上限で止める。
fn read_limited(file: File, len: Option<u64>) -> Result<std::result::Result<Vec<u8>, Rejection>> {
    let largest = LIMITS.largest_bytes();
    let too_large = len.is_some_and(|len| len > largest);
    let limit = if too_large {
        TOO_LARGE_PREFIX
    } else {
        largest + 1
    };
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes).map_err(failed)?;
    if too_large || bytes.len() as u64 > largest {
        // 種別は読んだ分だけで見る(断る理由の文言に、その種別の上限を添えるため。受け付けない
        // 種別なら、大きさではなくそちらを理由にする)。
        let kind = classify_prefix(&bytes).kind;
        return Ok(Err(match LIMITS.bytes_for(kind) {
            Some(limit_bytes) => Rejection::TooLarge { kind, limit_bytes },
            None => Rejection::Unsupported,
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

    fn read_path_of(path: &Path) -> std::result::Result<Vec<u8>, Rejection> {
        read(ReceivedFile::Path(path.to_path_buf()))
            .unwrap()
            .map(|(_, bytes)| bytes)
    }

    #[test]
    fn reads_regular_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memo.txt");
        fs::write(&path, "hello").unwrap();
        assert_eq!(
            read(ReceivedFile::Path(path)).unwrap(),
            Ok(("memo.txt".to_string(), b"hello".to_vec()))
        );
    }

    /// OSが開いて渡すものは、名前を受け取ったものにし、開いたファイルを上限まで読む。
    #[test]
    fn reads_opened_files_up_to_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("content");
        fs::write(&path, "hello").unwrap();
        let opened = |path: PathBuf| ReceivedFile::Opened {
            name: "memo.txt".to_string(),
            open: Box::new(move || File::open(path)),
        };
        assert_eq!(
            read(opened(path.clone())).unwrap(),
            Ok(("memo.txt".to_string(), b"hello".to_vec()))
        );
        File::create(&path)
            .unwrap()
            .set_len(LIMITS.largest_bytes() + 1)
            .unwrap();
        // 中身はNULだけなので、大きさではなく受け付けない種別を理由に断る。
        assert_eq!(read(opened(path)).unwrap(), Err(Rejection::Unsupported));
        // フォルダを開けるOS(Unix)では、開いたあとでもフォルダは断る。
        #[cfg(unix)]
        assert_eq!(
            read(opened(dir.path().to_path_buf())).unwrap(),
            Err(Rejection::NotAFile)
        );
    }

    #[test]
    fn refuses_folders_without_reading_them() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("secret.txt"), "key").unwrap();
        assert_eq!(read_path_of(dir.path()), Err(Rejection::NotAFile));
    }

    /// 上限を超えるものは読み切らずに断る。理由は先頭から見た種別で決め、受け付けない種別なら
    /// 大きさではなくそれを理由にする。
    #[test]
    fn stops_reading_past_the_largest_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.bin");
        let file = File::create(&path).unwrap();
        file.set_len(LIMITS.largest_bytes() + 1024).unwrap();
        assert_eq!(read_path_of(&path), Err(Rejection::Unsupported));

        // 先頭で切った位置が文字の途中でも、テキストの上限を理由にする。
        let path = dir.path().join("big.txt");
        let text = "締".repeat((LIMITS.largest_bytes() / 3 + 1) as usize);
        fs::write(&path, text).unwrap();
        assert_eq!(
            read_path_of(&path),
            Err(Rejection::TooLarge {
                kind: AttachmentKind::Text,
                limit_bytes: LIMITS.text_bytes,
            })
        );
    }

    #[test]
    fn reports_missing_files_without_their_path() {
        let dir = tempfile::tempdir().unwrap();
        let error = read(ReceivedFile::Path(dir.path().join("gone.txt")))
            .unwrap_err()
            .to_string();
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
        assert_eq!(read_path_of(&path), Err(Rejection::NotAFile));
    }

    #[test]
    fn hands_out_each_received_file_once_and_only_for_the_last_batch() {
        let received = Received::default();
        let paths = |names: Vec<PathBuf>| names.into_iter().map(ReceivedFile::Path).collect();
        let first = received.receive(paths(vec![absolute("one.txt")])).unwrap();
        // ファイル以外を落としたもの(相対パスで届く)は受け取らず、前の分を残す。
        assert_eq!(
            received.receive(paths(vec![PathBuf::from("data:image/png")])),
            None
        );
        assert!(matches!(
            received.take(first.batch_id, 0),
            Some(ReceivedFile::Path(p)) if p == absolute("one.txt")
        ));
        let second = received
            .receive(vec![
                ReceivedFile::Path(absolute("two.txt")),
                ReceivedFile::Path(PathBuf::from("https:/example.com/x")),
                ReceivedFile::Bytes {
                    name: "image.png".to_string(),
                    bytes: Vec::new(),
                },
            ])
            .unwrap();
        assert_eq!(second.names, ["two.txt", "image.png"]);
        assert!(received.take(first.batch_id, 0).is_none());
        assert!(matches!(
            received.take(second.batch_id, 1),
            Some(ReceivedFile::Bytes { .. })
        ));
        assert!(received.take(second.batch_id, 1).is_none());
        assert!(received.take(second.batch_id, 2).is_none());
    }

    #[test]
    fn names_come_from_the_last_segment_of_a_content_uri() {
        assert_eq!(
            name_from_uri(
                "content://com.android.externalstorage.documents/document/primary%3ADownload%2Fmemo.txt"
            ),
            "memo.txt"
        );
        assert_eq!(
            name_from_uri("content://media/external/images/media/image%3A1000000034?x=1"),
            "image:1000000034"
        );
        assert_eq!(
            name_from_uri("content://x/document/primary%3ADownload%2F"),
            "primary:Download"
        );
        assert_eq!(name_from_uri("content://x/document/%2F"), "attachment");
    }

    /// どのOSでも絶対パスになるパス。
    fn absolute(name: &str) -> PathBuf {
        std::env::temp_dir().join(name)
    }
}
