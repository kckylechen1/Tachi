//! Process-local LRU for Voyage *query* embeddings (#2109).
//!
//! Only `input_type == "query"` is eligible. Document embeds bypass this
//! module entirely. Errors are never stored in the LRU. No TTL in v1.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use tokio::sync::watch;

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
    /// Each flight retains its result independently of LRU eviction.
    inflight: Mutex<HashMap<CacheKey, watch::Sender<FlightResult>>>,
}

type FlightResult = Option<Result<Vec<f32>, String>>;

/// A reservation owns its completion, including when its request is cancelled.
pub(crate) struct FlightLeader<'a> {
    cache: &'a QueryEmbeddingCache,
    key: CacheKey,
    completion: Option<watch::Sender<FlightResult>>,
}

impl FlightLeader<'_> {
    pub(crate) fn complete(mut self, result: Result<Vec<f32>, String>) {
        self.finish(result);
    }

    fn finish(&mut self, result: Result<Vec<f32>, String>) {
        let Some(completion) = self.completion.take() else {
            return;
        };
        let mut inflight = self
            .cache
            .inflight
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if inflight
            .get(&self.key)
            .is_some_and(|slot| slot.same_channel(&completion))
        {
            if let Ok(embedding) = &result {
                self.cache.insert(
                    &self.key.model,
                    &self.key.input_type,
                    &self.key.text,
                    embedding.clone(),
                );
            }
            inflight.remove(&self.key);
        }
        drop(inflight);
        completion.send_replace(Some(result));
    }
}

impl Drop for FlightLeader<'_> {
    fn drop(&mut self) {
        self.finish(Err("query embedding leader cancelled".to_string()));
    }
}

pub(crate) struct FlightWaiter {
    completion: watch::Receiver<FlightResult>,
}

impl FlightWaiter {
    pub(crate) async fn wait(mut self) -> Result<Vec<f32>, String> {
        loop {
            if let Some(result) = self.completion.borrow_and_update().clone() {
                return result;
            }
            self.completion
                .changed()
                .await
                .map_err(|_| "query embedding flight closed without a result".to_string())?;
        }
    }
}

pub(crate) enum FlightRole<'a> {
    Leader(FlightLeader<'a>),
    Waiter(FlightWaiter),
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

    /// Reserve every miss before awaiting HTTP, then finish leaders before
    /// waiting on other flights. A waiter can observe completion even if its
    /// first poll happens after the leader finishes or the LRU evicts the key.
    pub(crate) fn begin_flight(&self, model: &str, input_type: &str, text: &str) -> FlightRole<'_> {
        let key = CacheKey::new(model, input_type, text);
        let mut inflight = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = inflight.get(&key) {
            FlightRole::Waiter(FlightWaiter {
                completion: existing.subscribe(),
            })
        } else {
            let (completion, _) = watch::channel(None);
            inflight.insert(key.clone(), completion.clone());
            FlightRole::Leader(FlightLeader {
                cache: self,
                key,
                completion: Some(completion),
            })
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

    fn test_cache() -> QueryEmbeddingCache {
        QueryEmbeddingCache {
            enabled: true,
            entries: Mutex::new(LruState::new(1)),
            inflight: Mutex::new(HashMap::new()),
        }
    }

    #[tokio::test]
    async fn completion_before_first_wait_preserves_errors_without_caching_them() {
        let cache = test_cache();
        let FlightRole::Leader(leader) = cache.begin_flight("m", "query", "a") else {
            panic!("leader");
        };
        let FlightRole::Waiter(waiter) = cache.begin_flight("m", "query", "a") else {
            panic!("waiter");
        };
        leader.complete(Err("provider failed".to_string()));
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), waiter.wait())
                .await
                .expect("late waiter"),
            Err("provider failed".to_string())
        );
        assert!(cache.get("m", "query", "a").is_none());
        assert!(matches!(
            cache.begin_flight("m", "query", "a"),
            FlightRole::Leader(_)
        ));
    }

    #[tokio::test]
    async fn dropped_leader_completes_existing_waiters_and_allows_a_successor() {
        let cache = test_cache();
        let FlightRole::Leader(leader) = cache.begin_flight("m", "query", "a") else {
            panic!("leader");
        };
        let FlightRole::Waiter(waiter) = cache.begin_flight("m", "query", "a") else {
            panic!("waiter");
        };
        drop(leader);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), waiter.wait())
                .await
                .expect("cancelled flight finishes"),
            Err("query embedding leader cancelled".to_string())
        );
        assert!(cache.get("m", "query", "a").is_none());
        let FlightRole::Leader(successor) = cache.begin_flight("m", "query", "a") else {
            panic!("successor owns flight");
        };
        successor.complete(Ok(vec![3.0]));
        assert_eq!(cache.get("m", "query", "a"), Some(vec![3.0]));
    }
}
