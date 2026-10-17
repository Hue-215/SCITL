use std::sync::{Arc, Mutex};

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

    /// 送る前に、送るものを用意するか決めるために使う(渡される前の頼みを引き取らないなど)。
    pub fn current(&self) -> Option<Channel<T>> {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Channel<T>>> {
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
