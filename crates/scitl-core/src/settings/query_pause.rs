//! 同期の間、設定からの問い合わせ(推論サーバー・MCPサーバー)を止める印(`docs/sync/index.md`「画面とCLI」)。
//! 乗っ取られた画面が、待ち受け中の数字をモデル名やヘッダーに入れ、LANの上の相手へ送らせるのを防ぐ。

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::error::{CoreError, Result};

#[derive(Default)]
pub struct QueryPause(AtomicUsize);

impl QueryPause {
    /// 返した印を落とすまで、問い合わせを断る。
    pub fn pause(&self) -> PausedQueries<'_> {
        self.0.fetch_add(1, Ordering::AcqRel);
        PausedQueries(self)
    }

    pub fn is_paused(&self) -> bool {
        self.0.load(Ordering::Acquire) > 0
    }

    /// 問い合わせを始める前に呼ぶ。止めている間は[`CoreError::Syncing`]。
    pub(super) fn check(&self) -> Result<()> {
        if self.is_paused() {
            return Err(CoreError::Syncing);
        }
        Ok(())
    }
}

/// [`QueryPause::pause`]の印。
pub struct PausedQueries<'a>(&'a QueryPause);

impl Drop for PausedQueries<'_> {
    fn drop(&mut self) {
        self.0 .0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_are_refused_until_every_pause_is_dropped() {
        let pause = QueryPause::default();
        assert!(pause.check().is_ok());
        let one = pause.pause();
        let two = pause.pause();
        assert!(matches!(pause.check(), Err(CoreError::Syncing)));
        drop(one);
        assert!(pause.check().is_err());
        drop(two);
        assert!(pause.check().is_ok());
    }
}
