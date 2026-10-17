use std::sync::{Arc, Mutex, MutexGuard};

use tauri::ipc::Channel;

/// 画面が起動時に渡す知らせ先。渡し直したら置き換え、画面を読み込み直すときに捨てる
/// (`AppState::forget_watchers`)。複製は同じ知らせ先を指す。
pub struct Watcher<T>(Arc<Mutex<Option<Channel<T>>>>);

impl<T> Watcher<T> {
    pub fn set(&self, channel: Channel<T>) {
        *self.lock() = Some(channel);
    }

    pub fn forget(&self) {
        *self.lock() = None;
    }

    pub fn current(&self) -> Option<Channel<T>> {
        self.lock().clone()
    }

    fn lock(&self) -> MutexGuard<'_, Option<Channel<T>>> {
        self.0.lock().expect("watcher mutex poisoned")
    }
}

impl<T> Default for Watcher<T> {
    fn default() -> Self {
        Self(Arc::default())
    }
}

impl<T> Clone for Watcher<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel() -> Channel<()> {
        Channel::new(|_| Ok(()))
    }

    #[test]
    fn set_replaces_and_forget_clears() {
        let watcher = Watcher::default();
        let first = channel();
        let second = channel();
        watcher.set(first);
        watcher.set(second.clone());
        assert_eq!(watcher.current().map(|c| c.id()), Some(second.id()));
        watcher.forget();
        assert!(watcher.current().is_none());
    }

    /// `finished_sink`が持つ複製にも、読み込み直したときに捨てたことが届く。
    #[test]
    fn clones_share_the_channel() {
        let watcher = Watcher::default();
        let held = watcher.clone();
        watcher.set(channel());
        assert!(held.current().is_some());
        watcher.forget();
        assert!(held.current().is_none());
    }
}
