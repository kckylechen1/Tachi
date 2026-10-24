//! Process-local LRU for Voyage *query* embeddings (#2109).
//!
//! Only `input_type == "query"` is eligible. Document embeds bypass this
//! module entirely. Errors are never stored. No TTL in v1.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

const ENABLED_ENV: &str = "TACHI_QUERY_EMBEDDING_CACHE";
const CAPACITY_ENV: &str = "TACHI_QUERY_EMBEDDING_CACHE_CAPACITY";
const DEFAULT_CAPACITY: usize = 512;

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct CacheKey {
    model: String,
    input_type: String,
    text: String,
}

impl CacheKey {
    fn new(model: &str, input_type: &str, text: &str) -> Self {
        Self {
            model: model.to_string(),
            input_type: input_type.to_string(),
            text: text.to_string(),
        }
    }
}

struct LruState {
    map: HashMap<CacheKey, Vec<f32>>,
    order: VecDeque<CacheKey>,
    capacity: usize,
}

impl LruState {
    fn new(capacity: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    fn get(&mut self, key: &CacheKey) -> Option<Vec<f32>> {
        let value = self.map.get(key)?.clone();
        // Move to most-recently-used.
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
            self.order.push_back(key.clone());
        }
        Some(value)
    }

    fn insert(&mut self, key: CacheKey, value: Vec<f32>) {
        if self.map.contains_key(&key) {
            self.map.insert(key.clone(), value);
            if let Some(pos) = self.order.iter().position(|k| k == &key) {
                self.order.remove(pos);
            }
            self.order.push_back(key);
            return;
        }
        while self.map.len() >= self.capacity {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            } else {
                break;
            }
        }
        self.order.push_back(key.clone());
        self.map.insert(key, value);
    }
}

/// Shared across `LlmClient` clones.
pub(crate) struct QueryEmbeddingCache {
    enabled: bool,
    entries: Mutex<LruState>,
    /// In-flight singleflight slots. Leader inserts; completes by remove+notify.
    inflight: Mutex<HashMap<CacheKey, Arc<Notify>>>,
}

/// Outcome of trying to join a singleflight for one cache key.
pub(crate) enum FlightRole {
    /// This caller must fetch the embedding for `key`.
    Leader,
    /// Another caller is fetching; wait on `notify` then re-read the cache.
    Waiter { notify: Arc<Notify> },
}

impl QueryEmbeddingCache {
    pub(crate) fn from_env() -> Self {
        let enabled = match std::env::var(ENABLED_ENV) {
            Ok(v) => {
                let t = v.trim().to_ascii_lowercase();
                !matches!(t.as_str(), "0" | "false" | "off" | "no")
            }
            Err(_) => true,
        };
        let capacity = std::env::var(CAPACITY_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(DEFAULT_CAPACITY);
        Self {
            enabled,
            entries: Mutex::new(LruState::new(capacity)),
            inflight: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn get(&self, model: &str, input_type: &str, text: &str) -> Option<Vec<f32>> {
        if !self.enabled {
            return None;
        }
        let key = CacheKey::new(model, input_type, text);
        let mut guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        guard.get(&key)
    }

    pub(crate) fn insert(&self, model: &str, input_type: &str, text: &str, embedding: Vec<f32>) {
        if !self.enabled {
            return;
        }
        let key = CacheKey::new(model, input_type, text);
        let mut guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        guard.insert(key, embedding);
    }

    /// Register as leader or waiter for `key`. Must not be called while
    /// holding any other flight; callers register all keys, fetch leaders,
    /// then await waiters (avoids cross-key deadlock).
    pub(crate) fn begin_flight(&self, model: &str, input_type: &str, text: &str) -> FlightRole {
        let key = CacheKey::new(model, input_type, text);
        let mut guard = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = guard.get(&key) {
            FlightRole::Waiter {
                notify: Arc::clone(existing),
            }
        } else {
            let notify = Arc::new(Notify::new());
            guard.insert(key, Arc::clone(&notify));
            FlightRole::Leader
        }
    }

    /// Finish a leader flight. On `Ok`, stores the embedding; on `Err`, does
    /// not cache. Always removes the inflight slot and wakes waiters.
    pub(crate) fn complete_flight(
        &self,
        model: &str,
        input_type: &str,
        text: &str,
        result: Result<Vec<f32>, String>,
    ) {
        let key = CacheKey::new(model, input_type, text);
        let notify = {
            let mut guard = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
            guard.remove(&key)
        };
        if let Ok(embedding) = result {
            self.insert(model, input_type, text, embedding);
        }
        if let Some(notify) = notify {
            notify.notify_waiters();
        }
    }

    /// Drop a leader reservation without writing (e.g. abandoned before fetch).
    pub(crate) fn abandon_flight(&self, model: &str, input_type: &str, text: &str) {
        let key = CacheKey::new(model, input_type, text);
        let notify = {
            let mut guard = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
            guard.remove(&key)
        };
        if let Some(notify) = notify {
            notify.notify_waiters();
        }
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn lru_evicts_oldest_at_capacity() {
        let cache = QueryEmbeddingCache {
            enabled: true,
            entries: Mutex::new(LruState::new(2)),
            inflight: Mutex::new(HashMap::new()),
        };
        cache.insert("m", "query", "a", vec![1.0]);
        cache.insert("m", "query", "b", vec![2.0]);
        cache.insert("m", "query", "c", vec![3.0]);
        assert!(cache.get("m", "query", "a").is_none());
        assert_eq!(cache.get("m", "query", "b"), Some(vec![2.0]));
        assert_eq!(cache.get("m", "query", "c"), Some(vec![3.0]));
    }

    #[test]
    fn disabled_cache_never_stores() {
        let cache = QueryEmbeddingCache {
            enabled: false,
            entries: Mutex::new(LruState::new(8)),
            inflight: Mutex::new(HashMap::new()),
        };
        cache.insert("m", "query", "a", vec![1.0]);
        assert!(cache.get("m", "query", "a").is_none());
    }
}
