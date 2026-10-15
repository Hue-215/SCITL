//! 数字から鍵を合わせ、合ったことを確かめる(`docs/sync/index.md` 3節)。
//!
//! SPAKE2で合わせた鍵に、そのTLS接続のexporterの値を混ぜて確認用の鍵を作る。間に入った相手は
//! AともBとも別々のTLS接続を張るので、両側の値が食い違い、確認が通らない。確認はつなぐ側(B)が
//! 先に示し、待ち受ける側(A)はそれを確かめてから自分の分を返す。

use aws_lc_rs::{hkdf, hmac};
use spake2::{Ed25519Group, Identity, Password, Spake2};

use crate::{PairingCode, SyncError};

const ID_CONNECTOR: &[u8] = b"scitl-sync connector";
const ID_LISTENER: &[u8] = b"scitl-sync listener";
const INFO_CONNECTOR: &[u8] = b"scitl-sync confirm connector";
const INFO_LISTENER: &[u8] = b"scitl-sync confirm listener";
const CONFIRM_MESSAGE: &[u8] = b"scitl-sync key confirmation";

/// SPAKE2の1回分のメッセージの長さ(側の印1バイトと、Ed25519の点32バイト)。
pub(crate) const MESSAGE_LEN: usize = 33;
pub(crate) const CONFIRM_LEN: usize = 32;

#[derive(Debug, Clone, Copy)]
pub(crate) enum Role {
    Connector,
    Listener,
}

pub(crate) struct Started {
    role: Role,
    state: Spake2<Ed25519Group>,
}

pub(crate) fn start(role: Role, code: &PairingCode) -> (Started, Vec<u8>) {
    let password = Password::new(code.as_str());
    let (a, b) = (Identity::new(ID_CONNECTOR), Identity::new(ID_LISTENER));
    let (state, msg) = match role {
        Role::Connector => Spake2::<Ed25519Group>::start_a_with_rng(&password, &a, &b, OsRng),
        Role::Listener => Spake2::<Ed25519Group>::start_b_with_rng(&password, &a, &b, OsRng),
    };
    (Started { role, state }, msg)
}

/// 相手のメッセージを受け取って鍵を合わせる。数字が違っても、ここでは失敗しない(鍵が違う
/// だけ)。失敗するのは、メッセージの形が正しくないとき。
pub(crate) fn finish(
    started: Started,
    peer_msg: &[u8],
    exporter: &[u8],
) -> Result<Confirmation, SyncError> {
    let key = started
        .state
        .finish(peer_msg)
        .map_err(|e| SyncError::Malformed(format!("PAKE message: {e:?}")))?;
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, exporter);
    let prk = salt.extract(&key);
    let derive = |info: &[u8]| -> hmac::Key {
        let info = [info];
        let okm = prk
            .expand(&info, hmac::HMAC_SHA256)
            .expect("HMAC key length");
        hmac::Key::from(okm)
    };
    let (ours, theirs) = match started.role {
        Role::Connector => (derive(INFO_CONNECTOR), derive(INFO_LISTENER)),
        Role::Listener => (derive(INFO_LISTENER), derive(INFO_CONNECTOR)),
    };
    Ok(Confirmation { ours, theirs })
}

pub(crate) struct Confirmation {
    ours: hmac::Key,
    theirs: hmac::Key,
}

impl Confirmation {
    pub(crate) fn ours(&self) -> Vec<u8> {
        hmac::sign(&self.ours, CONFIRM_MESSAGE).as_ref().to_vec()
    }

    /// 相手の確認を、時間の差が出ない比較で確かめる。
    pub(crate) fn check_theirs(&self, tag: &[u8]) -> Result<(), SyncError> {
        hmac::verify(&self.theirs, CONFIRM_MESSAGE, tag).map_err(|_| SyncError::CodeMismatch)
    }
}

/// `spake2`へ渡す乱数。OSの乱数を`aws-lc-rs`から取る。
struct OsRng;

impl rand_core::RngCore for OsRng {
    fn next_u32(&mut self) -> u32 {
        rand_core::impls::next_u32_via_fill(self)
    }

    fn next_u64(&mut self) -> u64 {
        rand_core::impls::next_u64_via_fill(self)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        aws_lc_rs::rand::fill(dest).expect("OS random source");
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

impl rand_core::CryptoRng for OsRng {}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(
        code_b: &str,
        code_a: &str,
        exporter_b: &[u8],
        exporter_a: &[u8],
    ) -> (Confirmation, Confirmation) {
        let (b, msg_b) = start(Role::Connector, &PairingCode::parse(code_b).unwrap());
        let (a, msg_a) = start(Role::Listener, &PairingCode::parse(code_a).unwrap());
        assert_eq!((msg_b.len(), msg_a.len()), (MESSAGE_LEN, MESSAGE_LEN));
        (
            finish(b, &msg_a, exporter_b).unwrap(),
            finish(a, &msg_b, exporter_a).unwrap(),
        )
    }

    #[test]
    fn the_same_code_on_the_same_connection_confirms_both_ways() {
        let (b, a) = run("123456", "123456", b"e", b"e");
        a.check_theirs(&b.ours()).unwrap();
        b.check_theirs(&a.ours()).unwrap();
        assert_eq!(b.ours().len(), CONFIRM_LEN);
    }

    #[test]
    fn a_different_code_does_not_confirm() {
        let (b, a) = run("123456", "123457", b"e", b"e");
        assert!(matches!(
            a.check_theirs(&b.ours()),
            Err(SyncError::CodeMismatch)
        ));
    }

    #[test]
    fn different_connections_do_not_confirm_even_with_the_same_code() {
        let (b, a) = run("123456", "123456", b"connection 1", b"connection 2");
        assert!(matches!(
            a.check_theirs(&b.ours()),
            Err(SyncError::CodeMismatch)
        ));
    }

    #[test]
    fn a_reflected_confirmation_is_refused() {
        let (b, _a) = run("123456", "123456", b"e", b"e");
        assert!(matches!(
            b.check_theirs(&b.ours()),
            Err(SyncError::CodeMismatch)
        ));
    }
}
