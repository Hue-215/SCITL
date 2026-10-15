//! 待ち受ける側が出すQR(`docs/sync/index.md` 3節)。中身は手で打つアドレス(候補のすべて)と数字で、
//! 相手の確かめ方は打ったときと変わらない。読んだ中身は外から来た入力なので、打ったものと同じ
//! 規則で確かめる。
//!
//! 形は`scitl-sync;1;012345;192.168.1.5:50123;[fd00::1]:50123`(前置き・形の版・数字・アドレス)。

use std::net::SocketAddr;

use qrcode::{Color, EcLevel, QrCode};
use serde::Serialize;

use crate::{parse_peer_address, PairingCode, SyncError};

const PREFIX: &str = "scitl-sync";
/// 中身の形の版。形を変えたら上げる(古い版の読み取りは断る)。
const FORMAT: &str = "1";
/// 読む中身の上限。出す側のアドレスは数個なので、これを超えるものは別のQRとみなす。
const MAX_LEN: usize = 512;
const MAX_ADDRESSES: usize = 8;

/// QRから読んだ宛先の候補と数字。
pub struct Invitation {
    pub addresses: Vec<SocketAddr>,
    pub code: PairingCode,
}

/// 画面が描くQRの升目。`dark`は左上から行ごとに`size * size`個。中身を含むので`Debug`を持たない。
#[derive(Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct QrModules {
    pub size: usize,
    pub dark: Vec<bool>,
}

pub(crate) fn encode(addresses: &[SocketAddr], code: &PairingCode) -> String {
    let mut text = format!("{PREFIX};{FORMAT};{}", code.as_str());
    for addr in addresses {
        text.push(';');
        text.push_str(&addr.to_string());
    }
    text
}

/// 中身をQRの升目にする。中身は[`MAX_LEN`]に収まる長さなので、QRに入らないことは無い。
pub(crate) fn modules(text: &str) -> QrModules {
    // 画面から読むので、汚れ・反射への強さより升目の粗さを取る(Mは約15%の欠けまで読める)。
    let code = QrCode::with_error_correction_level(text, EcLevel::M)
        .expect("an invitation fits in a QR code");
    QrModules {
        size: code.width(),
        dark: code
            .to_colors()
            .into_iter()
            .map(|c| c == Color::Dark)
            .collect(),
    }
}

/// 読んだQRの中身を確かめる。SCITLの同期のQRでなければ[`SyncError::InvalidInvitation`]、宛先・数字が
/// 規則に合わなければ、打ったときと同じ失敗にする。
pub fn parse_invitation(text: &str) -> Result<Invitation, SyncError> {
    if text.len() > MAX_LEN {
        return Err(SyncError::InvalidInvitation);
    }
    let mut fields = text.split(';');
    if fields.next() != Some(PREFIX) || fields.next() != Some(FORMAT) {
        return Err(SyncError::InvalidInvitation);
    }
    let code = fields.next().ok_or(SyncError::InvalidInvitation)?;
    let addresses: Vec<&str> = fields.collect();
    if addresses.is_empty() || addresses.len() > MAX_ADDRESSES {
        return Err(SyncError::InvalidInvitation);
    }
    // 出す側は6桁の数字だけを書く。打ったときの読みかえ(空白・全角)はここでは要らないので、形が
    // 違えば別のQRとみなす。
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(SyncError::InvalidInvitation);
    }
    Ok(Invitation {
        addresses: addresses
            .into_iter()
            .map(parse_peer_address)
            .collect::<Result<_, _>>()?,
        code: PairingCode::parse(code)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addrs(list: &[&str]) -> Vec<SocketAddr> {
        list.iter().map(|a| a.parse().unwrap()).collect()
    }

    #[test]
    fn what_is_shown_is_read_back() {
        let addresses = addrs(&["192.168.1.5:50123", "[fd00::1]:50123"]);
        let code = PairingCode::parse("012345").unwrap();
        let text = encode(&addresses, &code);
        assert_eq!(
            text,
            "scitl-sync;1;012345;192.168.1.5:50123;[fd00::1]:50123"
        );
        let read = parse_invitation(&text).unwrap();
        assert_eq!(read.addresses, addresses);
        assert_eq!(read.code, code);
    }

    #[test]
    fn modules_form_a_square() {
        let code = PairingCode::parse("012345").unwrap();
        let qr = modules(&encode(
            &addrs(&["192.168.1.5:50123"; MAX_ADDRESSES]),
            &code,
        ));
        assert_eq!(qr.dark.len(), qr.size * qr.size);
        // 位置合わせの印(左上の角)は黒。
        assert!(qr.dark[0]);
    }

    #[test]
    fn other_qr_codes_are_refused() {
        for text in [
            "",
            "https://example.com/",
            "scitl-sync;2;012345;192.168.1.5:50123",
            "scitl-sync;1;012345",
            "scitl-sync;1;012 345;192.168.1.5:50123",
            "scitl-sync;1;０１２３４５;192.168.1.5:50123",
            "scitl-sync;1;12345;192.168.1.5:50123",
            &format!(
                "scitl-sync;1;012345{}",
                ";192.168.1.5:50123".repeat(MAX_ADDRESSES + 1)
            ),
            &format!(
                "scitl-sync;1;012345;192.168.1.5:50123{}",
                " ".repeat(MAX_LEN)
            ),
        ] {
            assert!(
                matches!(parse_invitation(text), Err(SyncError::InvalidInvitation)),
                "{text}"
            );
        }
    }

    #[test]
    fn destinations_follow_the_typed_rules() {
        for text in [
            "scitl-sync;1;012345;8.8.8.8:443",
            "scitl-sync;1;012345;192.168.1.5:50123;example.com:443",
        ] {
            assert!(
                matches!(parse_invitation(text), Err(SyncError::AddressNotAllowed(_))),
                "{text}"
            );
        }
    }
}
