//! Fragment caching: `cache record do ... end` in ERB and `json.cache! record do ... end` in
//! Jbuilder. Production Rails keeps fragments in `redis_cache_store`, so the first rendering of a
//! record version is what every later render reuses, whoever renders it: a broadcast renders
//! without a request (no CSRF tokens in `button_to` forms, the renderer's host in URLs), and the
//! pages that show the same message afterwards repeat that rendering byte for byte.
//!
//! [`FragmentCache`] is the process's store (bounded, least recently used entries go first, like
//! Redis' `allkeys-lru` once it's full). Templates reach the store that's current on this thread:
//! the app enters it for every request ([`Scoped`]) and for renders outside one ([`with`]).
//! Without a current store, fragments render uncached (`perform_caching = false`).
//!
//! Keys follow `ActionView::Helpers::CacheHelper#fragment_name_with_digest`:
//! `views/<template>:<digest>/<record cache_key_with_version>[/<extra>]`, where the digest covers
//! the template and the partials it renders (see [`digest`]).

use std::any::Any;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

/// How many fragments the store keeps by default.
pub const DEFAULT_CAPACITY: usize = 50_000;

type Value = Arc<dyn Any + Send + Sync>;

/// A bounded in-process fragment store.
pub struct FragmentCache {
    capacity: usize,
    entries: Mutex<Entries>,
}

#[derive(Default)]
struct Entries {
    values: HashMap<String, (Value, u64)>,
    /// Last use → key, oldest first.
    recency: BTreeMap<u64, String>,
    clock: u64,
}

impl FragmentCache {
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self { capacity: capacity.max(1), entries: Mutex::default() })
    }

    /// `Rails.cache.fetch(key) { render }` for a rendered fragment.
    pub fn fetch(&self, key: &str, render: impl FnOnce() -> String) -> String {
        self.fetch_value(key, render)
    }

    /// `Rails.cache.fetch(key) { value }` for any cloneable value (Jbuilder caches the hash it
    /// built, not its JSON).
    pub fn fetch_value<T: Clone + Send + Sync + 'static>(&self, key: &str, compute: impl FnOnce() -> T) -> T {
        match self.try_fetch_value(key, || Ok::<T, std::convert::Infallible>(compute())) {
            Ok(value) => value,
            Err(never) => match never {},
        }
    }

    /// [`Self::fetch_value`] where computing can fail: nothing is stored then.
    pub fn try_fetch_value<T: Clone + Send + Sync + 'static, E>(&self, key: &str, compute: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
        if let Some(value) = self.read::<T>(key) {
            return Ok(value);
        }
        // Rendered unlocked: a fragment renders the fragments nested in it through this store.
        let value = compute()?;
        self.write(key, Arc::new(value.clone()));
        Ok(value)
    }

    pub fn len(&self) -> usize {
        self.lock().values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        *self.lock() = Entries::default();
    }

    fn read<T: Clone + 'static>(&self, key: &str) -> Option<T> {
        let mut entries = self.lock();
        let Entries { values, recency, clock } = &mut *entries;
        let (value, used) = values.get_mut(key)?;
        let value = value.downcast_ref::<T>()?.clone();
        *clock += 1;
        recency.remove(used);
        *used = *clock;
        recency.insert(*clock, key.to_string());
        Some(value)
    }

    fn write(&self, key: &str, value: Value) {
        let mut entries = self.lock();
        let Entries { values, recency, clock } = &mut *entries;
        *clock += 1;
        if let Some((_, used)) = values.insert(key.to_string(), (value, *clock)) {
            recency.remove(&used);
        }
        recency.insert(*clock, key.to_string());
        while values.len() > self.capacity {
            let Some((_, oldest)) = recency.pop_first() else { break };
            values.remove(&oldest);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Entries> {
        self.entries.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<FragmentCache>>> = const { RefCell::new(None) };
}

/// Runs `f` with `cache` as this thread's current store.
pub fn with<R>(cache: &Arc<FragmentCache>, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Arc<FragmentCache>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            CURRENT.with(|current| *current.borrow_mut() = previous);
        }
    }
    let _restore = Restore(CURRENT.with(|current| current.borrow_mut().replace(cache.clone())));
    f()
}

/// This thread's current store, if any.
pub fn current() -> Option<Arc<FragmentCache>> {
    CURRENT.with(|current| current.borrow().clone())
}

/// `cache key do render end` against the current store (uncached without one).
pub fn fetch(key: impl FnOnce() -> String, render: impl FnOnce() -> String) -> String {
    match current() {
        Some(cache) => cache.fetch(&key(), render),
        None => render(),
    }
}

/// `json.cache! key do ... end` against the current store (uncached without one).
pub fn try_fetch_value<T: Clone + Send + Sync + 'static, E>(
    key: impl FnOnce() -> String,
    compute: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    match current() {
        Some(cache) => cache.try_fetch_value(&key(), compute),
        None => compute(),
    }
}

/// A future that has `cache` as the current store whenever it's polled: a request's handler,
/// whose synchronous renders (on whichever worker thread polls it) then see the store.
pub struct Scoped<F> {
    cache: Arc<FragmentCache>,
    future: Pin<Box<F>>,
}

impl<F: Future> Scoped<F> {
    pub fn new(cache: Arc<FragmentCache>, future: F) -> Self {
        Self { cache, future: Box::pin(future) }
    }
}

impl<F: Future> Future for Scoped<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let this = &mut *self;
        with(&this.cache, || this.future.as_mut().poll(cx))
    }
}

/// The template digest part of a key: a stable hash of the template sources a fragment renders
/// (`ActionView::Digestor` digests the template and its dependency tree). Only its stability
/// within the process matters: the store doesn't outlive it.
pub fn digest(sources: &[&str]) -> String {
    #[allow(deprecated)]
    let mut hasher = std::hash::SipHasher::new();
    sources.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// `Time#to_fs(:usec)` of a record's `updated_at`: its `cache_version`.
pub fn cache_version(updated_at: jiff::Timestamp) -> String {
    format!("{}{:06}", updated_at.strftime("%Y%m%d%H%M%S"), updated_at.subsec_microsecond())
}

/// `record.cache_key_with_version`: `"messages/1-20240601120000000000"`.
pub fn cache_key_with_version(table: &str, id: i64, updated_at: jiff::Timestamp) -> String {
    format!("{table}/{id}-{}", cache_version(updated_at))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_rendering_is_reused() {
        let cache = FragmentCache::new(10);
        assert_eq!(cache.fetch("a", || "first".into()), "first");
        assert_eq!(cache.fetch("a", || "second".into()), "first");
        assert_eq!(cache.fetch("b", || "other".into()), "other");
    }

    #[test]
    fn least_recently_used_entries_are_evicted() {
        let cache = FragmentCache::new(2);
        cache.fetch("a", || "a".into());
        cache.fetch("b", || "b".into());
        cache.fetch("a", || unreachable!());
        cache.fetch("c", || "c".into());
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.fetch("a", || "a again".into()), "a");
        assert_eq!(cache.fetch("b", || "b again".into()), "b again");
    }

    #[test]
    fn nested_fragments_use_the_same_store() {
        let cache = FragmentCache::new(10);
        let outer = with(&cache, || fetch(|| "outer".into(), || format!("[{}]", fetch(|| "inner".into(), || "x".into()))));
        assert_eq!(outer, "[x]");
        assert_eq!(cache.len(), 2);
        assert!(current().is_none(), "the store is only current inside `with`");
        assert_eq!(fetch(|| "outer".into(), || "uncached".into()), "uncached");
    }

    #[test]
    fn failures_are_not_stored() {
        let cache = FragmentCache::new(10);
        assert!(cache.try_fetch_value::<i32, _>("k", || Err("boom")).is_err());
        assert_eq!(cache.try_fetch_value::<i32, &str>("k", || Ok(1)), Ok(1));
        assert_eq!(cache.try_fetch_value::<i32, &str>("k", || Ok(2)), Ok(1));
    }

    #[test]
    fn keys_use_usec_versions() {
        let time: jiff::Timestamp = "2024-06-01T12:00:00.000123Z".parse().unwrap();
        assert_eq!(cache_key_with_version("messages", 1, time), "messages/1-20240601120000000123");
    }
}
