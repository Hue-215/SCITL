//! 待ち受けのたびに作る数字(`docs/sync/index.md` 3節)。利用者が目で運び、PAKEの合言葉にする。

use std::fmt;

use crate::SyncError;

/// 数字の桁数。待ち受ける側は1回の待ち受けで相手を1回しか試させないので、当て推量が通るのは
/// 1回の待ち受けにつき10^-6。
const DIGITS: usize = 6;

/// 使い捨てで短命なので、捨てるときにメモリを消去しない(`spake2`の状態も、合言葉を消去せずに持つ)。
#[derive(Clone, PartialEq, Eq)]
pub struct PairingCode(String);

impl PairingCode {
    pub(crate) fn generate() -> Self {
        // 2^32を10^6で割った余りの偏りを避けるため、範囲に収まる値が出るまで引き直す。
        const BOUND: u32 = 1_000_000;
        const LIMIT: u32 = u32::MAX - u32::MAX % BOUND;
        loop {
            let mut bytes = [0u8; 4];
            aws_lc_rs::rand::fill(&mut bytes).expect("OS random source");
            let n = u32::from_be_bytes(bytes);
            if n < LIMIT {
                return Self(format!("{:0width$}", n % BOUND, width = DIGITS));
            }
        }
    }

    /// 利用者が打った数字を読む。空白・ハイフン・長音(日本語の入力で`-`を打つと出る)は無視し、
    /// 全角の数字は半角に揃える。
    pub fn parse(input: &str) -> Result<Self, SyncError> {
        let digits: String = input
            .chars()
            .filter(|c| !c.is_whitespace() && !matches!(c, '-' | '\u{FF0D}' | '\u{30FC}'))
            .map(|c| match c {
                '\u{FF10}'..='\u{FF19}' => char::from(b'0' + (c as u32 - 0xFF10) as u8),
                _ => c,
            })
            .collect();
        if digits.len() != DIGITS || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(SyncError::InvalidCode);
        }
        Ok(Self(digits))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// ログやエラーに数字が出ないよう、`Debug`では伏せる。
impl fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingCode(******)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_codes_are_six_digits() {
        for _ in 0..100 {
            let code = PairingCode::generate();
            assert_eq!(PairingCode::parse(code.as_str()).unwrap(), code);
        }
    }

    #[test]
    fn typed_codes_are_normalized() {
        assert_eq!(PairingCode::parse(" 012-345 ").unwrap().as_str(), "012345");
        assert_eq!(PairingCode::parse("012ー345").unwrap().as_str(), "012345");
        assert_eq!(
            PairingCode::parse("０１２ ３４５").unwrap().as_str(),
            "012345"
        );
        for bad in ["12345", "1234567", "12345a", ""] {
            assert!(PairingCode::parse(bad).is_err(), "{bad}");
        }
    }
}
