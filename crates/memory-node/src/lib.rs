//! NAPI-RS binding for memcore.
//!
//! Exposes `MemoryStore` as a Node.js class with sync `search` and `upsert`.
//! All data is passed as JSON strings for maximum NAPI compatibility.
//! LLM calls (Voyage embedding, reranker, GLM-5 extractor) remain in JS/TS;
//! only the hot SQLite + scoring path lives here.

#![deny(clippy::all)]

use memcore::{
    is_noise_text, should_skip_query, MemoryEntry, MemoryStore as RustStore, SearchOptions,
};
use napi_derive::napi;
use std::sync::{Arc, Mutex};

/// Thread-safe wrapper around the Rust MemoryStore.
#[napi]
pub struct JsMemoryStore {
    inner: Arc<Mutex<RustStore>>,
}

#[napi]
impl JsMemoryStore {
    /// Open a store at `db_path`. Creates the file & schema if needed.
    #[napi(constructor)]
    pub fn new(db_path: String) -> napi::Result<Self> {
        let store =
            RustStore::open(&db_path).map_err(|e| napi::Error::from_reason(e.to_string()))?;
        Ok(Self {
            inner: Arc::new(Mutex::new(store)),
        })
    }

    /// Upsert a memory entry. `entry_json` is a JSON string of MemoryEntry.
    #[napi]
    pub fn upsert(&self, entry_json: String) -> napi::Result<()> {
        let e: MemoryEntry = serde_json::from_str(&entry_json)
            .map_err(|e| napi::Error::from_reason(format!("invalid entry JSON: {e}")))?;
        let mut store = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        store
            .upsert(&e)
            .map_err(|e| napi::Error::from_reason(e.to_string()))
    }

    /// Hybrid search. Returns a JSON string of SearchResult[].
    ///     "path_prefix": "/some/path"
    /// }
    #[napi]
    pub fn search(&self, query: String, options_json: Option<String>) -> napi::Result<String> {
        let mut opts = SearchOptions::default();
        if let Some(json_str) = options_json {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&json_str) {
                if let Some(k) = val.get("top_k").and_then(|v| v.as_u64()) {
                    opts.top_k = k as usize;
                }
                if let Some(c) = val.get("candidates").and_then(|v| v.as_u64()) {
                    opts.candidates_per_channel = c as usize;
                }
                if let Some(p) = val.get("path_prefix").and_then(|v| v.as_str()) {
                    if !p.is_empty() {
                        opts.path_prefix = Some(p.to_string());
                    }
                }
                if let Some(ra) = val.get("record_access").and_then(|v| v.as_bool()) {
                    opts.record_access = ra;
                }
                if val.get("mmr_threshold").is_some() {
                    opts.mmr_threshold = val.get("mmr_threshold").and_then(|v| v.as_f64());
                }
                if let Some(arr) = val.get("query_vec").and_then(|v| v.as_array()) {
                    let mut qv = Vec::with_capacity(arr.len());
                    for item in arr {
                        if let Some(num) = item.as_f64() {
                            qv.push(num as f32);
                        }
                    }
                    if !qv.is_empty() {
                        opts.query_vec = Some(qv);
                    }
                }
                if let Some(w) = val.get("weights").and_then(|v| v.as_object()) {
                    if let Some(s) = w.get("semantic").and_then(|v| v.as_f64()) {
                        opts.weights.semantic = s;
                    }
                    if let Some(f) = w.get("fts").and_then(|v| v.as_f64()) {
                        opts.weights.fts = f;
                    }
                    if let Some(sym) = w.get("symbolic").and_then(|v| v.as_f64()) {
                        opts.weights.symbolic = sym;
                    }
                    if let Some(d) = w.get("decay").and_then(|v| v.as_f64()) {
                        opts.weights.decay = d;
                    }
                }
            }
        }

        let store = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let results = store
            .search(&query, Some(opts))
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;

        serde_json::to_string(&results).map_err(|e| napi::Error::from_reason(e.to_string()))
    }

    /// Delete a memory by ID.
    #[napi]
    pub fn delete(&self, id: String) -> napi::Result<bool> {
        let mut store = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        store
            .delete(&id)
            .map_err(|e| napi::Error::from_reason(e.to_string()))
    }

    /// Get aggregate stats. Returns JSON string.
    #[napi]
    pub fn stats(&self, include_archived: Option<bool>) -> napi::Result<String> {
        let stats = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stats(include_archived.unwrap_or(false))
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;

        serde_json::to_string(&stats).map_err(|e| napi::Error::from_reason(e.to_string()))
    }

    /// Fetch a single memory by ID. Returns JSON string or null.
    #[napi]
    pub fn get(&self, id: String) -> napi::Result<Option<String>> {
        let entry = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;

        match entry {
            Some(e) => serde_json::to_string(&e)
                .map(Some)
                .map_err(|e| napi::Error::from_reason(e.to_string())),
            None => Ok(None),
        }
    }

    /// Get most recent entries up to `limit`. Returns JSON string of MemoryEntry[].
    #[napi]
    pub fn get_all(&self, limit: Option<u32>) -> napi::Result<String> {
        let lim = limit.unwrap_or(200) as usize;
        let entries = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_all(lim)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;

        serde_json::to_string(&entries).map_err(|e| napi::Error::from_reason(e.to_string()))
    }

    /// Returns true if the sqlite-vec extension was loaded successfully.
    #[napi(getter)]
    pub fn vec_available(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .vec_available
    }

    // ─── Graph Operations ────────────────────────────────────────────────────

    /// Add or update an edge in the memory graph. `edge_json` is a JSON string of MemoryEdge.
    #[napi]
    pub fn add_edge(&self, edge_json: String) -> napi::Result<()> {
        let edge: memcore::MemoryEdge = serde_json::from_str(&edge_json)
            .map_err(|e| napi::Error::from_reason(format!("invalid edge JSON: {e}")))?;
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .add_edge(&edge)
            .map_err(|e| napi::Error::from_reason(e.to_string()))
    }

    /// Remove an edge. Returns true if found and deleted.
    #[napi]
    pub fn remove_edge(
        &self,
        source_id: String,
        target_id: String,
        relation: String,
    ) -> napi::Result<bool> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove_edge(&source_id, &target_id, &relation)
            .map_err(|e| napi::Error::from_reason(e.to_string()))
    }

    /// Get edges for a memory ID. Returns JSON string of MemoryEdge[].
    /// direction: "outgoing", "incoming", or "both"
    #[napi]
    pub fn get_edges(
        &self,
        memory_id: String,
        direction: Option<String>,
        relation_filter: Option<String>,
    ) -> napi::Result<String> {
        let dir = direction.as_deref().unwrap_or("both");
        let rel = relation_filter.as_deref();
        let edges = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_edges(&memory_id, dir, rel)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;

        serde_json::to_string(&edges).map_err(|e| napi::Error::from_reason(e.to_string()))
    }

    /// BFS graph expansion from seed IDs. Returns JSON string of GraphExpandResult.
    #[napi]
    pub fn graph_expand(
        &self,
        seed_ids_json: String,
        max_hops: Option<u32>,
        relation_filter: Option<String>,
    ) -> napi::Result<String> {
        let seeds: Vec<String> = serde_json::from_str(&seed_ids_json)
            .map_err(|e| napi::Error::from_reason(format!("invalid seed_ids JSON: {e}")))?;
        let hops = max_hops.unwrap_or(2);
        let rel = relation_filter.as_deref();

        let result = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .graph_expand(&seeds, hops, rel)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;

        serde_json::to_string(&result).map_err(|e| napi::Error::from_reason(e.to_string()))
    }

    // ─── Hub Operations ──────────────────────────────────────────────────────

    /// Register a hub capability. `cap_json` is a JSON string of HubCapability.
    #[napi]
    pub fn hub_register(&self, cap_json: String) -> napi::Result<()> {
        let cap: memcore::HubCapability = serde_json::from_str(&cap_json)
            .map_err(|e| napi::Error::from_reason(format!("invalid capability JSON: {e}")))?;
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .hub_register(&cap)
            .map_err(|e| napi::Error::from_reason(e.to_string()))
    }

    /// Discover hub capabilities. Returns JSON string of HubCapability[].
    /// Optional query for search, optional cap_type filter.
    #[napi]
    pub fn hub_discover(
        &self,
        query: Option<String>,
        cap_type: Option<String>,
    ) -> napi::Result<String> {
        let store = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let caps = if let Some(ref q) = query {
            store
                .hub_search(q, cap_type.as_deref())
                .map_err(|e| napi::Error::from_reason(e.to_string()))?
        } else {
            store
                .hub_list(cap_type.as_deref(), true)
                .map_err(|e| napi::Error::from_reason(e.to_string()))?
        };
        serde_json::to_string(&caps).map_err(|e| napi::Error::from_reason(e.to_string()))
    }

    /// Get a single hub capability by ID. Returns JSON string or null.
    #[napi]
    pub fn hub_get(&self, id: String) -> napi::Result<Option<String>> {
        let cap = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .hub_get(&id)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        match cap {
            Some(c) => serde_json::to_string(&c)
                .map(Some)
                .map_err(|e| napi::Error::from_reason(e.to_string())),
            None => Ok(None),
        }
    }

    /// Record feedback for a hub capability invocation.
    #[napi]
    pub fn hub_feedback(
        &self,
        id: String,
        success: bool,
        rating: Option<f64>,
    ) -> napi::Result<bool> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .hub_record_feedback(&id, success, rating)
            .map_err(|e| napi::Error::from_reason(e.to_string()))
    }
}

/// Check if text is noise.
#[napi]
pub fn is_noise(text: String) -> bool {
    is_noise_text(&text)
}

/// Check if query should skip retrieval.
#[napi]
pub fn should_skip(query: String) -> bool {
    should_skip_query(&query)
}
