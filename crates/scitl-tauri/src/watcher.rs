use std::sync::{Arc, Mutex, MutexGuard};

use tauri::ipc::{Channel, IpcResponse};

/// 画面が起動時に渡す知らせ先。渡し直したら置き換え、画面を読み込み直すときに捨てる
/// (`AppState::forget_watchers`)。複製は同じ知らせ先を指す。
pub struct Watcher<T>(Arc<Mutex<State<T>>>);

struct State<T> {
    channel: Option<Channel<T>>,
    /// `send_or_keep`で送れなかったもの。知らせ先が渡されたら送る。読み込み直しでは捨てない。
    unsent: Option<T>,
}

impl<T: IpcResponse> Watcher<T> {
    pub fn set(&self, channel: Channel<T>) {
        let mut state = self.lock();
        if let Some(value) = state.unsent.take() {
            let _ = channel.send(value);
        }
        state.channel = Some(channel);
    }

    /// 知らせ先が無ければ、渡されるまで残す。前に残したものは捨てる。
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub fn send_or_keep(&self, value: T) {
        let mut state = self.lock();
        match &state.channel {
            Some(channel) => {
                let _ = channel.send(value);
            }
            None => state.unsent = Some(value),
        }
    }
}

impl<T> Watcher<T> {
    pub fn forget(&self) {
        self.lock().channel = None;
    }

    pub fn current(&self) -> Option<Channel<T>> {
        self.lock().channel.clone()
    }

    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.0.lock().expect("watcher mutex poisoned")
    }
}

impl<T> Default for Watcher<T> {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(State {
            channel: None,
            unsent: None,
        })))
    }
}

impl<T> Clone for Watcher<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

#[cfg(test)]
mod tests {
    use tauri::ipc::InvokeResponseBody;

    use super::*;

    fn channel() -> Channel<()> {
        Channel::new(|_| Ok(()))
    }

    fn recording() -> (Channel<u32>, Arc<Mutex<Vec<String>>>) {
        let received: Arc<Mutex<Vec<String>>> = Arc::default();
        let sink = Arc::clone(&received);
        let channel = Channel::new(move |body| {
            if let InvokeResponseBody::Json(json) = body {
                sink.lock().unwrap().push(json);
            }
            Ok(())
        });
        (channel, received)
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

    #[test]
    fn kept_value_is_sent_to_the_next_channel_only_once() {
        let watcher = Watcher::default();
        let (first, first_received) = recording();
        watcher.set(first);
        watcher.forget();
        watcher.send_or_keep(1);
        watcher.send_or_keep(2);
        assert!(first_received.lock().unwrap().is_empty());

        let (second, second_received) = recording();
        watcher.set(second);
        watcher.send_or_keep(3);
        assert_eq!(*second_received.lock().unwrap(), ["2", "3"]);

        let (third, third_received) = recording();
        watcher.set(third);
        assert!(third_received.lock().unwrap().is_empty());
    }
}
