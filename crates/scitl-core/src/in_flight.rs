//! 同じ対象への処理を同時に1本に絞る。画面側でボタンを無効にしても、画面以外の経路
//! (CLI等)や操作の行き違いがあるため、それだけでは保証にならない。
//!
//! 処理中の対象ごとに、止める指示の印も持つ。印を見て止まるかどうか、どこで止まるかは
//! 処理の側が決める。
//!
//! 処理のうち長く掛かる部分に入っているものがあるかも数え、有無が変わったら知らせる
//! ([`InFlightSet::watching_long_running`])。どこが長く掛かる部分かは処理の側が決める。

use std::collections::HashMap;
use std::future::Future;
use std::hash::Hash;
use std::sync::Mutex;

use tokio::sync::watch;

/// 処理中の対象と、それぞれへの止める指示。
pub struct InFlightSet<K> {
    entries: Mutex<HashMap<K, watch::Sender<bool>>>,
    long_running: LongRunningCount,
}

impl<K: Eq + Hash + Clone> InFlightSet<K> {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            long_running: LongRunningCount::new(None),
        }
    }

    /// 長く掛かる部分([`InFlight::long_running`])に入っている処理が、無い状態から在る状態に
    /// なったら`on_change(true)`を、在る状態から無い状態になったら`on_change(false)`を呼ぶ集合。
    /// 呼び出しは起きた順に1つずつ行う。次の出入りを待たせるので、`on_change`に待つ処理を置かない。
    /// `on_change`がpanicしても数は狂わないが、知らせた状態と食い違ったままになる。
    pub fn watching_long_running(on_change: impl Fn(bool) + Send + Sync + 'static) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            long_running: LongRunningCount::new(Some(Box::new(on_change))),
        }
    }

    /// `key`が処理中でなければ処理中にし、ガードを返す。ガードを落とすと処理中が外れる。
    /// 既に処理中なら`None`。
    pub fn try_begin(&self, key: K) -> Option<InFlight<'_, K>> {
        let mut entries = self.lock();
        if entries.contains_key(&key) {
            return None;
        }
        let (sender, receiver) = watch::channel(false);
        entries.insert(key.clone(), sender);
        Some(InFlight {
            set: self,
            key,
            stop: StopSignal(receiver),
        })
    }

    /// 処理中の`key`に止める指示を出す。処理中でなければ何もせず`false`。`true`は指示を
    /// 出したことを表すだけで、止まったことは表さない。指示は今の処理だけに効き、次に
    /// `try_begin`した処理には持ち越さない。
    pub fn request_stop(&self, key: &K) -> bool {
        match self.lock().get(key) {
            Some(sender) => {
                sender.send_replace(true);
                true
            }
            None => false,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<K, watch::Sender<bool>>> {
        self.entries.lock().expect("in-flight set mutex poisoned")
    }
}

impl<K: Eq + Hash + Clone> Default for InFlightSet<K> {
    fn default() -> Self {
        Self::new()
    }
}

pub struct InFlight<'a, K: Eq + Hash + Clone> {
    set: &'a InFlightSet<K>,
    key: K,
    stop: StopSignal,
}

impl<K: Eq + Hash + Clone> InFlight<'_, K> {
    /// この処理への止める指示。
    pub fn stop_signal(&self) -> &StopSignal {
        &self.stop
    }

    /// この処理が長く掛かる部分に入ったことにする。返ったガードを落とすと出たことになる。
    pub fn long_running(&self) -> LongRunning<'_> {
        self.set.long_running.enter()
    }
}

impl<K: Eq + Hash + Clone> Drop for InFlight<'_, K> {
    fn drop(&mut self) {
        self.set.lock().remove(&self.key);
    }
}

/// 長く掛かる部分に入っている処理の数と、その有無が変わったことの知らせ先。
struct LongRunningCount {
    count: Mutex<usize>,
    on_change: Option<Box<dyn Fn(bool) + Send + Sync>>,
}

impl LongRunningCount {
    fn new(on_change: Option<Box<dyn Fn(bool) + Send + Sync>>) -> Self {
        Self {
            count: Mutex::new(0),
            on_change,
        }
    }

    fn enter(&self) -> LongRunning<'_> {
        let mut count = self.lock();
        *count += 1;
        if *count == 1 {
            self.notify(true);
        }
        LongRunning { count: self }
    }

    /// 知らせる順が出入りの順と入れ替わらないよう、数のロックを持ったまま呼ぶ。
    fn notify(&self, any: bool) {
        if let Some(on_change) = &self.on_change {
            on_change(any);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, usize> {
        // 数は整数1つなので、`on_change`のpanicで毒されても壊れていない。以後の処理を止めない。
        self.count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// 長く掛かる部分に入っている間だけ持つガード([`InFlight::long_running`])。
pub struct LongRunning<'a> {
    count: &'a LongRunningCount,
}

impl Drop for LongRunning<'_> {
    fn drop(&mut self) {
        let mut count = self.count.lock();
        *count -= 1;
        if *count == 0 {
            self.count.notify(false);
        }
    }
}

/// 1つの処理に出された止める指示([`InFlightSet::request_stop`])の受け口。
#[derive(Clone)]
pub struct StopSignal(watch::Receiver<bool>);

impl StopSignal {
    pub fn is_requested(&self) -> bool {
        *self.0.borrow()
    }

    /// `future`を、止める指示が来るまで待つ。指示が先に来たら`future`を落として`None`を返す
    /// (既に来ていれば`future`を始めない)。
    pub async fn unless_requested<F: Future>(&self, future: F) -> Option<F::Output> {
        let mut receiver = self.0.clone();
        tokio::select! {
            biased;
            // 送り手は処理中の間ずっと残るので、ここが`Err`になるのはガードを落としたあとだけ。
            Ok(_) = receiver.wait_for(|requested| *requested) => None,
            output = future => Some(output),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_can_be_taken_again_only_after_the_guard_is_dropped() {
        let set = InFlightSet::new();
        let guard = set.try_begin(1).unwrap();
        assert!(set.try_begin(1).is_none());
        assert!(set.try_begin(2).is_some(), "別の対象は妨げない");
        drop(guard);
        assert!(set.try_begin(1).is_some());
    }

    #[test]
    fn a_stop_reaches_only_the_running_work_on_that_key() {
        let set = InFlightSet::new();
        let first = set.try_begin(1).unwrap();
        let other = set.try_begin(2).unwrap();
        assert!(set.request_stop(&1));
        assert!(first.stop_signal().is_requested());
        assert!(!other.stop_signal().is_requested());

        drop(first);
        assert!(!set.request_stop(&1), "処理中でなければ何もしない");
        let next = set.try_begin(1).unwrap();
        assert!(!next.stop_signal().is_requested(), "次の処理に持ち越さない");
    }

    #[test]
    fn long_running_work_is_reported_when_the_first_enters_and_the_last_leaves() {
        let reported = std::sync::Arc::new(Mutex::new(Vec::new()));
        let set = InFlightSet::watching_long_running({
            let reported = reported.clone();
            move |any| reported.lock().unwrap().push(any)
        });
        let first = set.try_begin(1).unwrap();
        let second = set.try_begin(2).unwrap();
        assert!(
            reported.lock().unwrap().is_empty(),
            "処理中になっただけでは知らせない"
        );

        let first_long = first.long_running();
        let second_long = second.long_running();
        assert_eq!(*reported.lock().unwrap(), [true]);
        drop(first_long);
        assert_eq!(*reported.lock().unwrap(), [true], "まだ1つ残っている");
        drop(second_long);
        assert_eq!(*reported.lock().unwrap(), [true, false]);

        let _again = first.long_running();
        assert_eq!(*reported.lock().unwrap(), [true, false, true]);
    }

    #[tokio::test]
    async fn a_stop_drops_the_awaited_future() {
        let set = InFlightSet::new();
        let guard = set.try_begin(1).unwrap();
        let waiting = guard
            .stop_signal()
            .unless_requested(std::future::pending::<()>());
        let stop = async {
            tokio::task::yield_now().await;
            set.request_stop(&1);
        };
        let (result, ()) = tokio::join!(waiting, stop);
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn a_stop_already_requested_keeps_the_future_from_starting() {
        let set = InFlightSet::new();
        let guard = set.try_begin(1).unwrap();
        set.request_stop(&1);
        let started = std::sync::atomic::AtomicBool::new(false);
        let result = guard
            .stop_signal()
            .unless_requested(async { started.store(true, std::sync::atomic::Ordering::SeqCst) })
            .await;
        assert_eq!(result, None);
        assert!(!started.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn without_a_stop_the_future_runs_to_completion() {
        let set = InFlightSet::new();
        let guard = set.try_begin(1).unwrap();
        let result = guard.stop_signal().unless_requested(async { 7 }).await;
        assert_eq!(result, Some(7));
    }
}
