//! 添付の種別と形式の判定、受け付ける大きさの上限。拡張子や画面の申告は信用せず、
//! 中身だけから決める(拡張子を偽ったファイルを画像としてモデルや画面に渡さないため)。

use serde::Serialize;

use crate::db::attachments::AttachmentKind;

/// 受け付ける大きさの上限。設定からは変えられない仮の値(Issue #5で設定へ移す)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Limits {
    /// テキストは本文をDBに置き、毎ターン全文をモデルへ送るので、他より小さく絞る。
    pub text_bytes: u64,
    /// OpenAI互換APIが1枚に受け付ける大きさに合わせる。
    pub image_bytes: u64,
    pub other_bytes: u64,
    /// 1つの発言に付けられる数。
    pub per_message: usize,
}

pub const LIMITS: Limits = Limits {
    text_bytes: 256 * 1024,
    image_bytes: 20 * 1024 * 1024,
    other_bytes: 50 * 1024 * 1024,
    per_message: 10,
};

impl Limits {
    pub fn bytes_for(&self, kind: AttachmentKind) -> u64 {
        match kind {
            AttachmentKind::Text => self.text_bytes,
            AttachmentKind::Image => self.image_bytes,
            AttachmentKind::Other => self.other_bytes,
        }
    }

    /// どの種別でも受け付けない大きさ。画面が中身を読む前に弾くのに使う。
    pub fn largest_bytes(&self) -> u64 {
        self.text_bytes.max(self.image_bytes).max(self.other_bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Classified {
    pub kind: AttachmentKind,
    pub mime_type: &'static str,
}

/// 画像として扱う形式。OpenAI互換APIが画像入力として受け付ける形式に限る。SVGはスクリプトや
/// 外部参照を持てるうえAPIも受け付けないので、ここに入れない(テキストとして扱われる)。
/// これ以外の画像は、正規化(Issue #45)が入るまで「その他」になる。
const IMAGE_SIGNATURES: &[(&[u8], &str)] = &[
    (b"\x89PNG\r\n\x1a\n", "image/png"),
    (b"\xff\xd8\xff", "image/jpeg"),
    (b"GIF87a", "image/gif"),
    (b"GIF89a", "image/gif"),
];

const PDF_SIGNATURE: &[u8] = b"%PDF-";

pub fn classify(bytes: &[u8]) -> Classified {
    if let Some(mime_type) = image_mime_type(bytes) {
        return Classified {
            kind: AttachmentKind::Image,
            mime_type,
        };
    }
    // ASCIIだけで書かれたPDFもあるので、テキストの判定より先に見る。
    if bytes.starts_with(PDF_SIGNATURE) {
        return Classified {
            kind: AttachmentKind::Other,
            mime_type: "application/pdf",
        };
    }
    // UTF-8として読めても、NULを含むものはバイナリの形式とみなす。
    if !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok() {
        return Classified {
            kind: AttachmentKind::Text,
            mime_type: "text/plain",
        };
    }
    Classified {
        kind: AttachmentKind::Other,
        mime_type: "application/octet-stream",
    }
}

/// 画像として扱う形式なら、そのMIME。data URLに書くMIMEもここからしか来ない。
pub fn image_mime_type(bytes: &[u8]) -> Option<&'static str> {
    if let Some((_, mime_type)) = IMAGE_SIGNATURES
        .iter()
        .find(|(signature, _)| bytes.starts_with(signature))
    {
        return Some(mime_type);
    }
    // WebPはRIFFコンテナで、形式の印は8バイト目から。
    (bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP")
        .then_some("image/webp")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_images_by_their_signature_only() {
        assert_eq!(
            classify(b"\x89PNG\r\n\x1a\n rest"),
            Classified {
                kind: AttachmentKind::Image,
                mime_type: "image/png"
            }
        );
        assert_eq!(classify(b"\xff\xd8\xff\xe0").mime_type, "image/jpeg");
        assert_eq!(classify(b"GIF89a....").mime_type, "image/gif");
        assert_eq!(classify(b"RIFF\0\0\0\0WEBPVP8 ").mime_type, "image/webp");
        // RIFFでもWebPでなければ画像にしない(WAV等)。
        assert_eq!(
            classify(b"RIFF\0\0\0\0WAVEfmt ").kind,
            AttachmentKind::Other
        );
    }

    #[test]
    fn treats_svg_as_text_not_as_an_image() {
        assert_eq!(
            classify(b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>").kind,
            AttachmentKind::Text
        );
    }

    #[test]
    fn treats_utf8_without_nul_as_text() {
        assert_eq!(classify("締切は来週".as_bytes()).kind, AttachmentKind::Text);
        assert_eq!(classify(b"").kind, AttachmentKind::Text);
        assert_eq!(classify(b"a\0b").kind, AttachmentKind::Other);
        assert_eq!(classify(b"\xff\xfe").kind, AttachmentKind::Other);
    }

    #[test]
    fn names_pdf_and_leaves_the_rest_as_octet_stream() {
        let pdf = classify(b"%PDF-1.7\n\xe2\xe3\xcf\xd3");
        assert_eq!(pdf.kind, AttachmentKind::Other);
        assert_eq!(pdf.mime_type, "application/pdf");
        assert_eq!(classify(b"%PDF-1.4\n1 0 obj").mime_type, "application/pdf");
        assert_eq!(classify(b"\x00\x01").mime_type, "application/octet-stream");
    }
}
