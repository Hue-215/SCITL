//! 添付の実体が、それを指す行と合うか(`docs/sync/index.md`「同期するもの」)。送る側は送る前に自分の実体を、
//! 受け取る側は届いた実体と既に持っていた実体を、同じ規則で確かめる。

use std::collections::HashMap;

use sha2::{Digest, Sha256};

use scitl_core::attachments::classify;

use crate::transport::AttachmentRow;
use crate::InvalidRow;

/// 同期する実体1つの上限。実体はメモリに組み立ててから確かめて置く。預かる画像の上限より
/// 大きいのは、受け付けていた頃の「その他」があるため。
pub(crate) const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;

/// 実体ごとに、それを指す行。
pub(crate) fn rows_by_file(attachments: &[AttachmentRow]) -> HashMap<&str, Vec<&AttachmentRow>> {
    let mut files: HashMap<&str, Vec<&AttachmentRow>> = HashMap::new();
    for row in attachments {
        if let Some(hash) = row.file_hash.as_deref() {
            files.entry(hash).or_default().push(row);
        }
    }
    files
}

/// 中身がハッシュと合い、指す行それぞれの大きさと、画像の行なら形式とも合うか。外れた行を返す。
pub(crate) fn check(hash: &str, bytes: &[u8], rows: &[&AttachmentRow]) -> Vec<InvalidRow> {
    let fail = |row: &AttachmentRow, reason: &str| InvalidRow {
        table: "attachments",
        id: None,
        uid: row.uid.clone(),
        reason: reason.to_string(),
    };
    if format!("{:x}", Sha256::digest(bytes)) != hash {
        return rows
            .iter()
            .map(|r| fail(r, "the file does not match its hash"))
            .collect();
    }
    let classified = classify(bytes);
    rows.iter()
        .filter_map(|row| {
            if usize::try_from(row.size_bytes) != Ok(bytes.len()) {
                return Some(fail(row, "the size does not match the file"));
            }
            // 画像は預かる時点で正規化してあり、形式は中身から決めた値。受け付けていた頃の
            // 「その他」は画像のMIMEを持ちうるので、種別が画像の行だけを見る。
            if row.kind == "image"
                && (classified.kind.as_str() != "image" || classified.mime_type != row.mime_type)
            {
                return Some(fail(row, "the file is not the image the row says"));
            }
            None
        })
        .collect()
}
