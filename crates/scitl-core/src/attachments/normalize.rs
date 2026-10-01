//! 添付画像の正規化。預かる時点で向きを補正し、上限の大きさへ縮め、形式を揃えたものだけを
//! 保存する。画面の表示とモデルへの送信が同じ画像を使い、送るたびに整形し直さずに
//! 済むようにするため。常に再エンコードするので、EXIF(撮影位置など)は保存する前に落ちる。

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageEncoder, ImageFormat, ImageReader, Limits};

use super::classify::image_mime_type;

/// 長辺の上限(px)。Anthropicの標準の上限に合わせる。これより大きい画像はプロバイダー側で
/// 縮められ、送った分がコンテキストと通信の無駄になる。拡大はしない。
const MAX_LONG_EDGE: u32 = 1568;

/// 画素数の上限。小さなファイルが巨大な寸法を宣言し、展開でメモリを使い切るのを防ぐ
/// (展開爆弾)。8K(約3300万)や高解像度のスマホ写真(〜5000万)は通る値にする。
/// ヘッダーの寸法で判定し、画素を展開する前に止める。
const MAX_PIXELS: u64 = 50_000_000;

/// デコーダーが一度に確保してよい大きさ。画素数の上限の画像を16ビットのRGBA(1画素8バイト)で
/// 展開できる大きさにする。GIFのフレームのように、判定した寸法とは別に大きさを宣言できる
/// 確保も、ここで止める。
const MAX_DECODE_BYTES: u64 = 512 * 1024 * 1024;

const JPEG_QUALITY: u8 = 90;

/// 正規化した画像1枚の大きさの上限(バイト)。Anthropicの1枚5MB(base64にした長さで数える)に
/// 収まるよう、base64で4/3倍になる分を見込む。5MBは小さいほう(10進)で数える。長辺を縮めても圧縮の効かないPNG(写真やノイズの
/// 多い画像)はこれを超えうるので、超えたらさらに縮める。
const MAX_ENCODED_BYTES: usize = 5_000_000 / 4 * 3;

/// 大きさの上限に収めるために縮めるときの、1回あたりの長辺の倍率(分子/分母)。
const SHRINK_STEP: (u32, u32) = (3, 4);

#[derive(Debug)]
pub(super) struct Normalized {
    pub(super) bytes: Vec<u8>,
    pub(super) mime_type: &'static str,
}

/// 画像として扱う形式の画像を正規化する。元がJPEGならJPEG、それ以外(アニメーションは
/// 先頭のフレーム)はPNGにする。写真以外をJPEGにすると文字が潰れやすく、WebPはローカルの
/// モデルに読めないものがあるため。デコードできない(壊れている・画素数が上限を超える)
/// ときは`None`。
pub(super) fn normalize_image(bytes: &[u8]) -> Option<Normalized> {
    // 形式は先頭バイトから決めたものを渡し、デコーダーに推測させない。
    let format = ImageFormat::from_mime_type(image_mime_type(bytes)?)?;
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().ok()?;
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return None;
    }
    // 向きの情報が壊れているだけなら、補正せずに画像として使う。
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let icc_profile = decoder.icc_profile().ok().flatten();
    let mut image = DynamicImage::from_decoder(decoder).ok()?;
    image.apply_orientation(orientation);
    if image.width().max(image.height()) > MAX_LONG_EDGE {
        image = image.resize(MAX_LONG_EDGE, MAX_LONG_EDGE, FilterType::Lanczos3);
    }
    let mut image = to_8bit(image);
    let as_jpeg = format == ImageFormat::Jpeg;
    loop {
        let normalized = encode(&image, as_jpeg, icc_profile.clone())?;
        let long_edge = image.width().max(image.height());
        if normalized.bytes.len() <= MAX_ENCODED_BYTES || long_edge <= 1 {
            return Some(normalized);
        }
        let edge = (long_edge * SHRINK_STEP.0 / SHRINK_STEP.1).max(1);
        image = image.resize(edge, edge, FilterType::Lanczos3);
    }
}

/// 16ビットの画像を8ビットにする。モデルにも画面にも8ビットで足り、PNGが倍の大きさになるのを避ける。
fn to_8bit(image: DynamicImage) -> DynamicImage {
    match (image.color().has_color(), image.color().has_alpha()) {
        (false, false) => image.into_luma8().into(),
        (false, true) => image.into_luma_alpha8().into(),
        (true, false) => image.into_rgb8().into(),
        (true, true) => image.into_rgba8().into(),
    }
}

/// 色の見え方を保つため、ICCプロファイルは引き継ぐ(撮影の情報は持たない)。書けなくても
/// 画像としては使えるので、失敗は無視する。
fn encode(image: &DynamicImage, as_jpeg: bool, icc_profile: Option<Vec<u8>>) -> Option<Normalized> {
    let mut bytes = Vec::new();
    let mime_type = if as_jpeg {
        let mut encoder = JpegEncoder::new_with_quality(&mut bytes, JPEG_QUALITY);
        if let Some(icc) = icc_profile {
            let _ = encoder.set_icc_profile(icc);
        }
        image.write_with_encoder(encoder).ok()?;
        "image/jpeg"
    } else {
        let mut encoder = PngEncoder::new(&mut bytes);
        if let Some(icc) = icc_profile {
            let _ = encoder.set_icc_profile(icc);
        }
        image.write_with_encoder(encoder).ok()?;
        "image/png"
    };
    Some(Normalized { bytes, mime_type })
}

#[cfg(test)]
mod tests {
    use image::codecs::gif::GifEncoder;
    use image::codecs::webp::WebPEncoder;
    use image::{Frame, GenericImageView, Rgb, RgbImage, Rgba, RgbaImage};

    use super::*;

    fn encoded(image: RgbImage, format: ImageFormat) -> Vec<u8> {
        let mut bytes = Vec::new();
        DynamicImage::from(image)
            .write_to(&mut Cursor::new(&mut bytes), format)
            .unwrap();
        bytes
    }

    fn decoded(normalized: &Normalized) -> DynamicImage {
        image::load_from_memory(&normalized.bytes).unwrap()
    }

    fn exif_of(bytes: &[u8]) -> Option<Vec<u8>> {
        let format = ImageFormat::from_mime_type(image_mime_type(bytes).unwrap()).unwrap();
        ImageReader::with_format(Cursor::new(bytes), format)
            .into_decoder()
            .unwrap()
            .exif_metadata()
            .unwrap()
    }

    /// 向き(Orientation)の項目だけを持つEXIF。6は「表示するとき右に90度回す」。
    const EXIF_ROTATE_90: &[u8] =
        b"MM\0\x2a\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01\0\x06\0\0\0\0\0\0";

    #[test]
    fn shrinks_the_long_edge_keeping_the_aspect_ratio() {
        let png = encoded(RgbImage::new(3000, 1000), ImageFormat::Png);
        let normalized = normalize_image(&png).unwrap();
        assert_eq!(normalized.mime_type, "image/png");
        assert_eq!(decoded(&normalized).dimensions(), (1568, 523));
    }

    /// 長辺を縮めても大きさの上限を超えるPNG(ノイズの多い画像)は、収まるまでさらに縮める。
    #[test]
    fn shrinks_further_until_the_encoded_size_fits() {
        let mut state: u32 = 1;
        let noise = RgbaImage::from_fn(MAX_LONG_EDGE, MAX_LONG_EDGE, |_, _| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            Rgba(state.to_le_bytes())
        });
        let mut png = Vec::new();
        DynamicImage::from(noise)
            .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
            .unwrap();
        assert!(
            png.len() > MAX_ENCODED_BYTES,
            "前提: 縮めないと上限を超える"
        );

        let normalized = normalize_image(&png).unwrap();
        assert!(normalized.bytes.len() <= MAX_ENCODED_BYTES);
        assert_eq!(normalized.mime_type, "image/png");
        let (width, height) = decoded(&normalized).dimensions();
        assert_eq!(width, height);
        assert!(width < MAX_LONG_EDGE);
    }

    #[test]
    fn does_not_enlarge_small_images() {
        let png = encoded(RgbImage::new(40, 30), ImageFormat::Png);
        assert_eq!(
            decoded(&normalize_image(&png).unwrap()).dimensions(),
            (40, 30)
        );
    }

    #[test]
    fn applies_the_exif_orientation_and_drops_the_exif() {
        let mut jpeg = Vec::new();
        let mut encoder = JpegEncoder::new_with_quality(&mut jpeg, 90);
        encoder.set_exif_metadata(EXIF_ROTATE_90.to_vec()).unwrap();
        DynamicImage::from(RgbImage::new(40, 20))
            .write_with_encoder(encoder)
            .unwrap();
        assert!(exif_of(&jpeg).is_some());

        let normalized = normalize_image(&jpeg).unwrap();
        assert_eq!(normalized.mime_type, "image/jpeg");
        assert_eq!(decoded(&normalized).dimensions(), (20, 40));
        assert_eq!(exif_of(&normalized.bytes), None);
    }

    #[test]
    fn turns_webp_into_png() {
        let mut webp = Vec::new();
        DynamicImage::from(RgbaImage::new(8, 8))
            .write_with_encoder(WebPEncoder::new_lossless(&mut webp))
            .unwrap();
        let normalized = normalize_image(&webp).unwrap();
        assert_eq!(normalized.mime_type, "image/png");
        assert_eq!(decoded(&normalized).dimensions(), (8, 8));
    }

    #[test]
    fn keeps_only_the_first_frame_of_an_animated_gif() {
        let frame = |color| Frame::new(RgbaImage::from_pixel(4, 4, Rgba(color)));
        let mut gif = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut gif);
            encoder.encode_frame(frame([255, 0, 0, 255])).unwrap();
            encoder.encode_frame(frame([0, 0, 255, 255])).unwrap();
        }
        let normalized = normalize_image(&gif).unwrap();
        assert_eq!(normalized.mime_type, "image/png");
        assert_eq!(decoded(&normalized).get_pixel(0, 0), Rgba([255, 0, 0, 255]));
    }

    #[test]
    fn refuses_images_that_declare_too_many_pixels() {
        let mut gif = encoded(RgbImage::from_pixel(1, 1, Rgb([1, 2, 3])), ImageFormat::Gif);
        assert!(normalize_image(&gif).is_some());
        // 論理画面の幅と高さ(6〜9バイト目)を65535×65535に書き換える。中身は1画素のまま。
        gif[6..10].copy_from_slice(&[0xff; 4]);
        assert!(normalize_image(&gif).is_none());
    }

    #[test]
    fn refuses_frames_larger_than_the_declared_screen() {
        // 論理画面は1×1、2色のパレット。フレームだけが65535×65535を宣言する。
        let gif = [
            b"GIF89a\x01\x00\x01\x00\x80\x00\x00".as_slice(),
            b"\x00\x00\x00\xff\xff\xff",
            b"\x2c\x00\x00\x00\x00\xff\xff\xff\xff\x00",
            b"\x02\x02\x44\x01\x00\x3b",
        ]
        .concat();
        assert!(normalize_image(&gif).is_none());
    }

    #[test]
    fn refuses_broken_images() {
        assert!(normalize_image(b"\x89PNG\r\n\x1a\nbody").is_none());
        assert!(normalize_image(b"not an image").is_none());
    }
}
