//! 会話ごとのセッションID。プロバイダーのカスタムヘッダーの値に`{session_id}`と書くと、
//! 送るときにこの値へ置き換わる(`providers::CustomHeaders`)。会話ごとに変わらないIDで
//! 振り分けとプロンプトキャッシュを効かせる送り先(OpenCode Go等)のため。

use std::sync::OnceLock;

use sha2::{Digest, Sha256};

/// 1つの会話を表すID。値は、起動ごとのシードと会話のキーを合わせたSHA-256の先頭128ビットを
/// 16進で書いたもの。会話のキー(タスクのID・作成日時)もシードも外へ出さず、ハッシュだけを送る。
///
/// - 同じ起動中は、同じ会話なら同じ値になる。ターン・試行をまたいでも変わらない
/// - 起動し直すとシードが変わり、全会話の値が変わる。保存しないのは、プロンプトキャッシュの
///   寿命が短く、再起動をまたいで保つ意味が薄いため
/// - シードを知らない送り先は、値からタスクの番号を総当たりで割り出せない
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionId(String);

/// ハッシュの先頭から使うバイト数。
const ID_BYTES: usize = 16;

impl SessionId {
    /// 会話のキーからIDを作る。会話のキーをどう組むかは呼び出し側(`orchestration::turn`)が
    /// 決める。OSの乱数を読めなければ`None`で、そのときは`{session_id}`を含むヘッダーを
    /// 送らない(固定の値で代えると、別々の会話が同じIDになる)。
    pub fn for_conversation(conversation: &str) -> Option<Self> {
        let seed = seed()?;
        let digest = Sha256::new()
            .chain_update(seed)
            .chain_update(conversation.as_bytes())
            .finalize();
        Some(Self(
            digest[..ID_BYTES]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        ))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 起動ごとのシード。最初に使うときにOSの乱数から1度だけ作る。読めなかったら覚えず、
/// 次に使うときに読み直す。
fn seed() -> Option<[u8; 32]> {
    static SEED: OnceLock<[u8; 32]> = OnceLock::new();
    if let Some(seed) = SEED.get() {
        return Some(*seed);
    }
    let mut seed = [0u8; 32];
    if let Err(e) = getrandom::fill(&mut seed) {
        crate::diagnostics::report(format_args!(
            "failed to read OS randomness for the session ID seed: {e}"
        ));
        return None;
    }
    // 同時に作った別のスレッドの値が先に入っていたら、そちらを使う。
    Some(*SEED.get_or_init(|| seed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_stable_per_conversation_and_differs_between_them() {
        let a = SessionId::for_conversation("task:1:2026-10-07T00:00:00Z").unwrap();
        assert_eq!(
            a,
            SessionId::for_conversation("task:1:2026-10-07T00:00:00Z").unwrap()
        );
        assert_ne!(
            a,
            SessionId::for_conversation("task:1:2026-10-08T00:00:00Z").unwrap()
        );
        assert_ne!(a, SessionId::for_conversation("general").unwrap());
    }

    #[test]
    fn is_32_lowercase_hex_digits() {
        let id = SessionId::for_conversation("general").unwrap();
        assert_eq!(id.as_str().len(), ID_BYTES * 2);
        assert!(id
            .as_str()
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)));
    }
}
