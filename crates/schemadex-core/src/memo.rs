//! LRU result cache. Keyed by `(fingerprint, sql)`. Bounded by capacity.
//!
//! The cache is intentionally process-local and opt-in via
//! [`crate::cache::CacheOptions::memoize_results`]. Two layers of invalidation
//! keep entries fresh:
//!
//! 1. Key includes the database fingerprint — when DDL moves, all old entries
//!    become unreachable (a fresh fingerprint produces a different key) and
//!    eventually fall out of the LRU.
//! 2. Refresh paths in [`crate::cache::SchemaCache`] call [`ResultCache::clear`]
//!    to be eager about it, so callers don't pay for stale entries that
//!    happen to share a fingerprint until eviction.
//!
//! The implementation is the simplest LRU that works: a `HashMap` for O(1)
//! lookup, plus a `VecDeque` recording access order. `put` removes the oldest
//! entry when the cache is full; `get` moves the touched key to the back of
//! the queue. Concurrent access is serialised through a single `Mutex` — the
//! critical sections are tiny.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

/// A rendered SQL result, ready to hand back to a [`crate::cache::SchemaCache::run_sql`]
/// caller. The fields mirror the return shape of `run_sql`: a markdown table
/// and its precise token count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedResult {
    pub rendered: String,
    pub tokens: usize,
}

/// Bounded LRU result cache keyed by `(fingerprint, sql)`. Cheap to construct
/// and cheap to clone via [`std::sync::Arc`].
pub struct ResultCache {
    capacity: usize,
    entries: Mutex<HashMap<String, CachedResult>>,
    order: Mutex<VecDeque<String>>,
}

impl ResultCache {
    /// Create a cache with at most `capacity` entries. A `capacity` of 0
    /// disables caching entirely (every `get` returns `None` and `put` is a
    /// no-op).
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: Mutex::new(HashMap::with_capacity(capacity.max(1))),
            order: Mutex::new(VecDeque::with_capacity(capacity.max(1))),
        }
    }

    fn key(fingerprint: &str, sql: &str) -> String {
        // The colon delimiter is fine because fingerprints are hex digests
        // and won't collide with SQL syntax.
        format!("{fingerprint}:{sql}")
    }

    /// Look up a cached result. Returns `None` on miss; on hit, the key is
    /// promoted to the most-recently-used position.
    pub fn get(&self, fingerprint: &str, sql: &str) -> Option<CachedResult> {
        if self.capacity == 0 {
            return None;
        }
        let key = Self::key(fingerprint, sql);
        let entries = self.entries.lock().expect("poisoned");
        let Some(value) = entries.get(&key).cloned() else {
            return None;
        };
        drop(entries);
        // Promote: move the key to the back of the order queue.
        let mut order = self.order.lock().expect("poisoned");
        if let Some(pos) = order.iter().position(|k| k == &key) {
            order.remove(pos);
        }
        order.push_back(key);
        Some(value)
    }

    /// Insert or replace a cached result. Evicts the oldest entry when the
    /// cache is at capacity.
    pub fn put(&self, fingerprint: &str, sql: &str, value: CachedResult) {
        if self.capacity == 0 {
            return;
        }
        let key = Self::key(fingerprint, sql);
        let mut entries = self.entries.lock().expect("poisoned");
        let mut order = self.order.lock().expect("poisoned");
        if entries.contains_key(&key) {
            // Replace existing value and refresh position.
            entries.insert(key.clone(), value);
            if let Some(pos) = order.iter().position(|k| k == &key) {
                order.remove(pos);
            }
            order.push_back(key);
            return;
        }
        // Evict if full.
        while entries.len() >= self.capacity {
            if let Some(oldest) = order.pop_front() {
                entries.remove(&oldest);
            } else {
                break;
            }
        }
        entries.insert(key.clone(), value);
        order.push_back(key);
    }

    /// Drop every entry. Used by refresh / invalidate paths.
    pub fn clear(&self) {
        self.entries.lock().expect("poisoned").clear();
        self.order.lock().expect("poisoned").clear();
    }

    /// Current number of cached entries.
    pub fn size(&self) -> usize {
        self.entries.lock().expect("poisoned").len()
    }
}

impl std::fmt::Debug for ResultCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResultCache")
            .field("capacity", &self.capacity)
            .field("size", &self.size())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(s: &str) -> CachedResult {
        CachedResult {
            rendered: s.to_string(),
            tokens: s.len(),
        }
    }

    #[test]
    fn put_then_get() {
        let c = ResultCache::new(4);
        c.put("fp", "SELECT 1", item("one"));
        assert_eq!(c.get("fp", "SELECT 1"), Some(item("one")));
        assert_eq!(c.get("fp", "SELECT 2"), None);
    }

    #[test]
    fn evicts_oldest_when_full() {
        let c = ResultCache::new(2);
        c.put("fp", "a", item("a"));
        c.put("fp", "b", item("b"));
        c.put("fp", "c", item("c"));
        assert!(c.get("fp", "a").is_none(), "oldest should have been evicted");
        assert_eq!(c.get("fp", "b"), Some(item("b")));
        assert_eq!(c.get("fp", "c"), Some(item("c")));
    }

    #[test]
    fn fingerprint_isolates_entries() {
        let c = ResultCache::new(4);
        c.put("fp1", "SELECT 1", item("one"));
        assert_eq!(c.get("fp2", "SELECT 1"), None);
        assert_eq!(c.get("fp1", "SELECT 1"), Some(item("one")));
    }

    #[test]
    fn clear_drops_everything() {
        let c = ResultCache::new(4);
        c.put("fp", "a", item("a"));
        c.clear();
        assert_eq!(c.size(), 0);
        assert!(c.get("fp", "a").is_none());
    }
}
