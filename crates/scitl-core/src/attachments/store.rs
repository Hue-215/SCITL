//! 画像・その他の添付の実体の置き場所(data-model.md attachments)。実体は内容のハッシュを
//! 名前にして置き、同じ内容を何度添付しても1つで済ませる(legacy/data-model.md 5節)。

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use ulid::Ulid;

use crate::db::error::{CoreError, Result};
use crate::llm::InlineImage;
use crate::text::is_invisible_format;

#[derive(Debug, Clone)]
pub struct AttachmentStore {
    blobs: PathBuf,
    revealed: PathBuf,
}

impl AttachmentStore {
    /// `blobs`は実体の置き場所(アプリのデータの一部)。`revealed`は「フォルダを開く」ために
    /// 元の名前で書き出す場所で、消えても実体から作り直せる(キャッシュに置いてよい)。
    pub fn new(blobs: impl Into<PathBuf>, revealed: impl Into<PathBuf>) -> Self {
        Self {
            blobs: blobs.into(),
            revealed: revealed.into(),
        }
    }

    /// 実体を置いてハッシュ(SHA-256の小文字16進)を返す。同じ内容が既にあれば書かない。
    ///
    /// 同じディレクトリの一時ファイルに書いてからハッシュの名前へ移す。途中まで書いたファイルが
    /// ハッシュの名前で残ると「既に置いてある」とみなされ、以降の同じ内容の添付がすべて
    /// 壊れた実体を共有してしまうため。
    pub fn put(&self, bytes: &[u8]) -> Result<String> {
        let hash = format!("{:x}", Sha256::digest(bytes));
        let path = self.blobs.join(&hash);
        if path.exists() {
            return Ok(hash);
        }
        fs::create_dir_all(&self.blobs).map_err(io_error("create the attachment directory"))?;
        let temp = self.blobs.join(format!(".{hash}.{}.tmp", Ulid::new()));
        fs::write(&temp, bytes).map_err(io_error("write an attachment"))?;
        match fs::rename(&temp, &path) {
            Ok(()) => Ok(hash),
            // Windowsは移し先が既にあると失敗する。同時に同じ内容を置いた相手が先に済ませた。
            Err(_) if path.exists() => {
                let _ = fs::remove_file(&temp);
                Ok(hash)
            }
            Err(e) => {
                let _ = fs::remove_file(&temp);
                Err(io_error("store an attachment")(e))
            }
        }
    }

    pub fn read(&self, hash: &str) -> Result<Vec<u8>> {
        fs::read(self.blob_path(hash)?).map_err(io_error("read an attachment"))
    }

    /// 実体を`dest`へ複製する(元の名前での書き出し)。メモリに読み込まずにファイルのまま写す。
    /// `dest`の親ディレクトリは呼び出し側が作る。
    pub fn copy_to(&self, hash: &str, dest: &Path) -> Result<()> {
        fs::copy(self.blob_path(hash)?, dest)
            .map(drop)
            .map_err(io_error("copy an attachment"))
    }

    /// 実体を画像として読む。形式は保存した値ではなく実体の先頭バイトから決め直す
    /// ([`InlineImage::from_bytes`])ので、画像として扱う形式でなければ失敗にする。
    pub fn read_image(&self, hash: &str) -> Result<InlineImage> {
        InlineImage::from_bytes(&self.read(hash)?).ok_or_else(|| {
            CoreError::Attachment("stored file is not an image of a supported format".to_string())
        })
    }

    /// 添付を元の名前(安全にした形)で書き出し、入っているフォルダをOSで開く。
    /// ファイルそのものは開かない。実行形式の添付が確認なしに起動されないようにするため。
    pub fn reveal(&self, attachment_id: i64, original_name: &str, hash: &str) -> Result<()> {
        let dir = self.revealed.join(attachment_id.to_string());
        let path = dir.join(safe_file_name(original_name));
        if !path.exists() {
            fs::create_dir_all(&dir).map_err(io_error("create the reveal directory"))?;
            self.copy_to(hash, &path)?;
        }
        open::that_detached(&dir).map_err(io_error("open the folder"))
    }

    /// DBから読んだハッシュをパスに繋ぐ前に形を確かめる。DBが書き換えられていても、置き場所の
    /// 外を指せないようにするため。
    fn blob_path(&self, hash: &str) -> Result<PathBuf> {
        if hash.len() != 64 || !hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(CoreError::Attachment(
                "stored hash is not a SHA-256 hex digest".to_string(),
            ));
        }
        Ok(self.blobs.join(hash))
    }
}

/// 失敗の文言にパスを載せない(エラーは画面にそのまま出る)。
fn io_error(action: &'static str) -> impl Fn(std::io::Error) -> CoreError {
    move |e| CoreError::Attachment(format!("failed to {action}: {:?}", e.kind()))
}

/// どのOSでも1つのファイル名として置ける形にする。元の名前(利用者が付けたもの)は
/// DBにそのまま残り、これは書き出すときだけに使う。
pub(crate) fn safe_file_name(name: &str) -> String {
    const MAX_BYTES: usize = 200;
    const FALLBACK: &str = "attachment";
    // 双方向制御文字は、名前の見た目を並べ替える(`report\u{202E}fdp.exe`が`reportexe.pdf`に見える)。
    let replaced: String = name
        .chars()
        .filter(|&c| !is_invisible_format(c))
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    // Windowsは末尾の点と空白を黙って落とすので、書いた名前と開く名前が食い違う。
    let trimmed = replaced.trim().trim_end_matches(['.', ' ']);
    let mut out = if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        FALLBACK.to_string()
    } else {
        trimmed.to_string()
    };
    if is_windows_reserved(&out) {
        out.insert(0, '_');
    }
    if out.len() > MAX_BYTES {
        out = truncate_keeping_extension(&out, MAX_BYTES);
    }
    out
}

/// `CON`・`NUL`・`COM1`等は、拡張子を付けてもWindowsではファイル名にできない。
fn is_windows_reserved(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim_end()
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit())
}

/// 拡張子を残して`max`バイトに収める。フォルダを開いたあとで、利用者が形式を見分けられるように。
fn truncate_keeping_extension(name: &str, max: usize) -> String {
    let extension = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| e.len() <= 16)
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let budget = max - extension.len();
    let stem = &name[..name.len() - extension.len()];
    let mut end = budget.min(stem.len());
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{extension}", &stem[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempStore {
        root: PathBuf,
        store: AttachmentStore,
    }

    impl TempStore {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("scitl-attachments-{}", Ulid::new()));
            let store = AttachmentStore::new(root.join("blobs"), root.join("revealed"));
            Self { root, store }
        }
    }

    impl Drop for TempStore {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn names_contents_by_their_full_sha256_and_stores_them_once() {
        let t = TempStore::new();
        let hash = t.store.put(b"abc").unwrap();
        assert_eq!(
            hash,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(t.store.put(b"abc").unwrap(), hash);
        assert_eq!(t.store.read(&hash).unwrap(), b"abc");

        // 一時ファイルは残らず、実体は1つだけ。
        let entries: Vec<_> = fs::read_dir(t.root.join("blobs")).unwrap().collect();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn refuses_hashes_that_could_point_outside_the_store() {
        let t = TempStore::new();
        for bad in ["../secret", &"A".repeat(64), &"a".repeat(63), ""] {
            assert!(
                matches!(t.store.read(bad), Err(CoreError::Attachment(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn makes_names_that_every_os_accepts() {
        assert_eq!(safe_file_name("report.pdf"), "report.pdf");
        assert_eq!(safe_file_name("../../etc/passwd"), ".._.._etc_passwd");
        assert_eq!(safe_file_name("a\\b:c*d?.txt"), "a_b_c_d_.txt");
        assert_eq!(safe_file_name("line\nbreak"), "line_break");
        assert_eq!(safe_file_name("trailing. . "), "trailing");
        assert_eq!(safe_file_name(".."), "attachment");
        assert_eq!(safe_file_name("   "), "attachment");
        assert_eq!(safe_file_name("con.txt"), "_con.txt");
        assert_eq!(safe_file_name("COM1"), "_COM1");
        assert_eq!(safe_file_name("COM10.txt"), "COM10.txt");
        assert_eq!(safe_file_name("資料.docx"), "資料.docx");
        assert_eq!(safe_file_name("report\u{202E}fdp.exe"), "reportfdp.exe");
    }

    #[test]
    fn keeps_the_extension_when_shortening_long_names() {
        let long = format!("{}.pdf", "あ".repeat(100));
        let safe = safe_file_name(&long);
        assert!(safe.len() <= 200);
        assert!(safe.ends_with("あ.pdf"), "{safe}");
    }
}
