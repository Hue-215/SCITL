//! 同じ対象への処理を同時に1本に絞る。画面側でボタンを無効にしても、画面以外の経路
//! (CLI等)や操作の行き違いがあるため、それだけでは保証にならない。

use std::collections::HashSet;
use std::hash::Hash;
use std::sync::Mutex;

/// 処理中の対象の集合。
pub struct InFlightSet<K> {
    keys: Mutex<HashSet<K>>,
}

impl<K: Eq + Hash + Clone> InFlightSet<K> {
    pub fn new() -> Self {
        Self {
            keys: Mutex::new(HashSet::new()),
        }
    }

    /// `key`が処理中でなければ処理中にし、ガードを返す。ガードを落とすと処理中が外れる。
    /// 既に処理中なら`None`。
    pub fn try_begin(&self, key: K) -> Option<InFlight<'_, K>> {
        if !self.lock().insert(key.clone()) {
            return None;
        }
        Some(InFlight { set: self, key })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashSet<K>> {
        self.keys.lock().expect("in-flight set mutex poisoned")
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
}

impl<K: Eq + Hash + Clone> Drop for InFlight<'_, K> {
    fn drop(&mut self) {
        self.set.lock().remove(&self.key);
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
}
