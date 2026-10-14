//! 宛先と待ち受けのアドレスの絞り込み(`docs/sync/index.md` 3節)。分類は`net::classify_host`と同じ。

use std::net::{IpAddr, SocketAddr};

use scitl_core::net::{classify_ip, HostClass};

use crate::SyncError;

/// 宛先・接続元として受け付けるか。ホスト名は形の上で入ってこない(IPのリテラルだけを読む)。
pub(crate) fn is_allowed_ip(ip: IpAddr) -> bool {
    matches!(
        classify_ip(ip),
        HostClass::Loopback | HostClass::PrivateLiteral
    )
}

/// 利用者が打った宛先(`192.168.1.5:50123`・`[fd00::1]:50123`)を読む。全角で打たれた分は半角に
/// 揃える(日本語の入力のまま打たれうるため)。
pub fn parse_peer_address(input: &str) -> Result<SocketAddr, SyncError> {
    let normalized: String = input.trim().chars().map(to_halfwidth).collect();
    let addr: SocketAddr = normalized
        .parse()
        .map_err(|_| SyncError::AddressNotAllowed(normalized.clone()))?;
    if addr.port() == 0 || !is_allowed_ip(addr.ip()) {
        return Err(SyncError::AddressNotAllowed(normalized));
    }
    Ok(addr)
}

/// 全角の英数字・記号(U+FF01〜U+FF5E)を半角に、句点を`.`に。ほかの文字はそのまま。
fn to_halfwidth(c: char) -> char {
    match c {
        '\u{3002}' => '.',
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        _ => c,
    }
}

/// 待ち受ける側が出すアドレスの候補。インターフェースのアドレスのうちプライベートIPだけを、
/// IPv4を先にして返す。ループバックは含めない(ほかの端末からは届かない)。
pub fn local_addresses() -> Result<Vec<IpAddr>, SyncError> {
    let interfaces = if_addrs::get_if_addrs().map_err(SyncError::Bind)?;
    let mut ips: Vec<IpAddr> = interfaces
        .iter()
        .map(|i| i.ip())
        .filter(|ip| matches!(classify_ip(*ip), HostClass::PrivateLiteral))
        .collect();
    ips.sort_by_key(|ip| (ip.is_ipv6(), *ip));
    ips.dedup();
    Ok(ips)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_and_loopback_literals_are_accepted() {
        for input in [
            "192.168.1.5:50123",
            "10.0.0.1:1",
            "127.0.0.1:9",
            "[fd00::1]:443",
            "[::1]:80",
        ] {
            assert!(parse_peer_address(input).is_ok(), "{input}");
        }
    }

    #[test]
    fn fullwidth_input_is_read_as_halfwidth() {
        assert_eq!(
            parse_peer_address(" １９２．１６８。１．５：５０１２３ ").unwrap(),
            "192.168.1.5:50123".parse().unwrap()
        );
    }

    #[test]
    fn other_destinations_are_refused() {
        for input in [
            "8.8.8.8:443",
            "169.254.169.254:80",
            "[fd00:ec2::254]:80",
            "[fe80::1]:80",
            "localhost:80",
            "example.com:443",
            "192.168.1.5",
            "192.168.1.5:0",
            "",
        ] {
            assert!(
                matches!(
                    parse_peer_address(input),
                    Err(SyncError::AddressNotAllowed(_))
                ),
                "{input}"
            );
        }
    }
}
