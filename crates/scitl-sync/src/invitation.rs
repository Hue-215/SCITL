//! 待ち受ける側が出すQR(`docs/sync/index.md`「接続」)。中身は手で打つアドレス(候補のうち先頭から
//! [`MAX_ADDRESSES`]個)と数字で、相手の確かめ方は打ったときと変わらない。読んだ中身は外から来た
//! 入力なので、出す側が書く形だけを受け付ける。
//!
//! 形は`scitl-sync;1;012345;192.168.1.5:50123;[fd00::1]:50123`(前置き・形の版・数字・アドレス)。

use std::net::SocketAddr;

use qrcode::{Color, EcLevel, QrCode};
use serde::Serialize;

use scitl_core::net::{classify_ip, HostClass};

use crate::code::DIGITS;
use crate::{PairingCode, SyncError};

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

/// 候補が多いときは、先頭(IPv4が先)から読む側の上限までを入れる。打つときは画面のアドレスの
/// どれでも打てる。
pub(crate) fn encode(addresses: &[SocketAddr], code: &PairingCode) -> String {
    let mut text = format!("{PREFIX};{FORMAT};{}", code.as_str());
    for addr in addresses.iter().take(MAX_ADDRESSES) {
        let field = format!(";{addr}");
        if text.len() + field.len() > MAX_LEN {
            break;
        }
        text.push_str(&field);
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

/// 読んだQRの中身を確かめる。出す側([`encode`])が書く形でなければ、どこが違っても
/// [`SyncError::InvalidInvitation`]にする。中身を失敗の詳細に載せない(画面へ返さない)。
///
/// 打ったときの読みかえ(空白・全角)はせず、宛先はプライベートIPに限る(出す側はループバックを
/// 出さない)。
pub fn parse_invitation(text: &str) -> Result<Invitation, SyncError> {
    if text.len() > MAX_LEN {
        return Err(SyncError::InvalidInvitation);
    }
    let mut fields = text.split(';');
    if fields.next() != Some(PREFIX) || fields.next() != Some(FORMAT) {
        return Err(SyncError::InvalidInvitation);
    }
    let code = fields.next().ok_or(SyncError::InvalidInvitation)?;
    if code.len() != DIGITS || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(SyncError::InvalidInvitation);
    }
    let addresses = fields
        .map(|field| {
            field
                .parse::<SocketAddr>()
                .ok()
                .filter(|a| a.port() != 0 && classify_ip(a.ip()) == HostClass::PrivateLiteral)
                .ok_or(SyncError::InvalidInvitation)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if addresses.is_empty() || addresses.len() > MAX_ADDRESSES {
        return Err(SyncError::InvalidInvitation);
    }
    Ok(Invitation {
        addresses,
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

    /// 候補がいくつあっても、出したQRは読める(多ければ上限まで入る)。
    #[test]
    fn what_is_shown_is_always_readable() {
        let code = PairingCode::parse("012345").unwrap();
        let longest: SocketAddr = "[fdff:ffff:ffff:ffff:ffff:ffff:ffff:ffff]:65535"
            .parse()
            .unwrap();
        for count in [1, MAX_ADDRESSES, MAX_ADDRESSES + 1, 40] {
            let addresses = vec![longest; count];
            let text = encode(&addresses, &code);
            let read = parse_invitation(&text).unwrap();
            assert_eq!(read.addresses.len(), count.min(MAX_ADDRESSES));
            let qr = modules(&text);
            assert_eq!(qr.dark.len(), qr.size * qr.size);
            // 位置合わせの印(左上の角)は黒。
            assert!(qr.dark[0]);
        }
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
            // 宛先も出す側が書く形だけを受け付ける(打ったときの読みかえはしない)。
            "scitl-sync;1;012345; 192.168.1.5:50123",
            "scitl-sync;1;012345;１９２.168.1.5:50123",
            "scitl-sync;1;012345;192.168.1.5",
            "scitl-sync;1;012345;192.168.1.5:0",
            "scitl-sync;1;012345;8.8.8.8:443",
            "scitl-sync;1;012345;127.0.0.1:50123",
            "scitl-sync;1;012345;[::1]:50123",
            "scitl-sync;1;012345;192.168.1.5:50123;example.com:443",
        ] {
            assert!(
                matches!(parse_invitation(text), Err(SyncError::InvalidInvitation)),
                "{text}"
            );
        }
    }
}
