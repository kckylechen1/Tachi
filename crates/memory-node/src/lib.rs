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

const DEFAULT_GET_ALL_LIMIT: usize = 200;
const MAX_GET_ALL_LIMIT: usize = 500;
const DEFAULT_HUB_DISCOVER_LIMIT: usize = 100;
const MAX_HUB_DISCOVER_LIMIT: usize = 500;
const DEFAULT_GET_EDGES_LIMIT: usize = 100;
const MAX_GET_EDGES_LIMIT: usize = 500;
const DEFAULT_GRAPH_EXPAND_EDGE_LIMIT: usize = 100;
const MAX_GRAPH_EXPAND_EDGE_LIMIT: usize = 500;
const DEFAULT_GRAPH_EXPAND_HOPS: usize = 2;
const MAX_GRAPH_EXPAND_HOPS: usize = 50;
const MIN_SEARCH_TOP_K: usize = 1;
const MAX_SEARCH_TOP_K: usize = 100;
const DEFAULT_SEARCH_CANDIDATES_PER_CHANNEL: usize = 20;
const MAX_SEARCH_CANDIDATES_PER_CHANNEL: usize = 500;

/// Mirrors the portable service limits before a JavaScript number can reach
/// memcore's result collection or SQLite limit binding.
fn clamp_u64_to_usize(value: u64, maximum: usize) -> usize {
    let maximum = u64::try_from(maximum).expect("usize must fit into u64");
    value.min(maximum) as usize
}

fn normalized_top_k(value: u64) -> usize {
    // A zero result limit still returns the smallest meaningful search page.
    clamp_u64_to_usize(value, MAX_SEARCH_TOP_K).max(MIN_SEARCH_TOP_K)
}

fn normalized_candidates_per_channel(value: Option<u64>, top_k: usize) -> usize {
    let requested = match value {
        Some(0) => top_k,
        None => DEFAULT_SEARCH_CANDIDATES_PER_CHANNEL,
        Some(value) => clamp_u64_to_usize(value, MAX_SEARCH_CANDIDATES_PER_CHANNEL),
    };
    requested.max(top_k).min(MAX_SEARCH_CANDIDATES_PER_CHANNEL)
}

fn normalize_js_limit(
    name: &str,
    value: Option<f64>,
    default: usize,
    maximum: usize,
) -> napi::Result<usize> {
    let Some(value) = value else {
        return Ok(default.min(maximum));
    };
    if !value.is_finite() {
        return Err(napi::Error::from_reason(format!(
            "{name} must be a finite number"
        )));
    }
    if value < 0.0 {
        return Err(napi::Error::from_reason(format!(
            "{name} must be non-negative"
        )));
    }
    if value.fract() != 0.0 {
        return Err(napi::Error::from_reason(format!(
            "{name} must be an integer"
        )));
    }

    Ok(value.min(maximum as f64) as usize)
}

fn search_options_from_json(options_json: Option<&str>) -> SearchOptions {
    let mut opts = SearchOptions::default();
    if let Some(json_str) = options_json {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
            let requested_top_k = val.get("top_k").and_then(|v| v.as_u64());
            let requested_candidates = val.get("candidates").and_then(|v| v.as_u64());
            if requested_top_k.is_some() || requested_candidates.is_some() {
                opts.top_k = requested_top_k
                    .map(normalized_top_k)
                    .unwrap_or_else(|| opts.top_k.clamp(MIN_SEARCH_TOP_K, MAX_SEARCH_TOP_K));
                opts.candidates_per_channel =
                    normalized_candidates_per_channel(requested_candidates, opts.top_k);
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
    opts
}

/// Thread-safe wrapper around the Rust MemoryStore.
#[napi]
pub struct JsMemoryStore {
    inner: Arc<Mutex<RustStore>>,
}

#[napi]
impl JsMemoryStore {
    /// Open a store at `db_path`. Creates the file & schema if needed.
    ///
    /// # This binding is an UNFILTERED raw store handle — contract, not trivia
    ///
    /// It opens with `RustStore::open`, so the handle carries **no manifest
    /// label**. Path-routing validation is off, and every identity-keyed read
    /// predicate (`is_wiki_corpus_store`, and with it tachi#1569's Wiki
    /// internal-row gate on `search` / `get` / `get_all` / `graph_expand`)
    /// answers "not the Wiki corpus" no matter which database this is.
    ///
    /// Consequences a caller must assume:
    ///
    /// * Pointed at the Wiki database, every read here returns the raw table,
    ///   including rows the facade never exposes — `wiki-rem:` review drafts,
    ///   `/wiki/_log` operation-log rows, recall-cache rows, anchor plumbing.
    /// * None of `tachi-server`'s server-side filters (`readable_entry`,
    ///   `is_listable_row`) run either; those live above this layer.
    ///
    /// **Do not use this binding to serve reads that reach an end user.** It
    /// exists for tooling that wants the store verbatim. Anything user-facing
    /// goes through the facade, which owns the internal-row contract.
    /// Giving this surface a real identity is tracked separately (tachi#1569
    /// acceptance item 5) and is deliberately not done here: an unlabelled
    /// handle has nothing to derive one from.
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
        let opts = search_options_from_json(options_json.as_deref());

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

    /// Get most recent entries up to `limit`. Zero returns an empty list; positive
    /// values are capped at this binding's named ceiling. Returns JSON string of MemoryEntry[].
    #[napi]
    pub fn get_all(&self, limit: Option<f64>) -> napi::Result<String> {
        let lim = normalize_js_limit(
            "getAll limit",
            limit,
            DEFAULT_GET_ALL_LIMIT,
            MAX_GET_ALL_LIMIT,
        )?;
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
        // tachi#1646: this N-API surface accepts an arbitrary caller-supplied
        // edge (relation, weight, endpoints all from `edge_json`) — the
        // Rust-side caller is not asserting anything Tachi computed or
        // verified, so it is classified `CallerAsserted` regardless of who
        // is embedding this binding.
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .add_edge_with_provenance(
                &edge,
                &memcore::db::EdgeProvenance {
                    authority: Some(memcore::db::EdgeAuthority::CallerAsserted),
                    ..Default::default()
                },
            )
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
        limit: Option<f64>,
    ) -> napi::Result<String> {
        let dir = direction.as_deref().unwrap_or("both");
        let rel = relation_filter.as_deref();
        let limit = normalize_js_limit(
            "getEdges limit",
            limit,
            DEFAULT_GET_EDGES_LIMIT,
            MAX_GET_EDGES_LIMIT,
        )?;
        let edges = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_edges_limited(&memory_id, dir, rel, limit)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;

        serde_json::to_string(&edges).map_err(|e| napi::Error::from_reason(e.to_string()))
    }

    /// BFS graph expansion from seed IDs. Returns JSON string of GraphExpandResult.
    #[napi]
    pub fn graph_expand(
        &self,
        seed_ids_json: String,
        max_hops: Option<f64>,
        relation_filter: Option<String>,
        edge_limit: Option<f64>,
    ) -> napi::Result<String> {
        let hops = normalize_js_limit(
            "graphExpand maxHops",
            max_hops,
            DEFAULT_GRAPH_EXPAND_HOPS,
            MAX_GRAPH_EXPAND_HOPS,
        )? as u32;
        let edge_limit = normalize_js_limit(
            "graphExpand edgeLimit",
            edge_limit,
            DEFAULT_GRAPH_EXPAND_EDGE_LIMIT,
            MAX_GRAPH_EXPAND_EDGE_LIMIT,
        )?;
        let seeds: Vec<String> = serde_json::from_str(&seed_ids_json)
            .map_err(|e| napi::Error::from_reason(format!("invalid seed_ids JSON: {e}")))?;
        let rel = relation_filter.as_deref();

        let result = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .graph_expand_limited(&seeds, hops, rel, edge_limit)
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
        limit: Option<f64>,
    ) -> napi::Result<String> {
        let limit = normalize_js_limit(
            "hubDiscover limit",
            limit,
            DEFAULT_HUB_DISCOVER_LIMIT,
            MAX_HUB_DISCOVER_LIMIT,
        )?;
        let store = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let caps = if let Some(ref q) = query {
            store
                .hub_search_limited(q, cap_type.as_deref(), limit)
                .map_err(|e| napi::Error::from_reason(e.to_string()))?
        } else {
            store
                .hub_list_limited(cap_type.as_deref(), true, limit)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_STORE: AtomicU64 = AtomicU64::new(0);

    struct TempJsStore {
        store: Option<JsMemoryStore>,
        dir: PathBuf,
    }

    impl TempJsStore {
        fn new() -> Self {
            let sequence = NEXT_TEST_STORE.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "memory-node-bounds-{}-{sequence}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("create temporary memory-node test directory");
            let db_path = dir.join("memory.db");
            let store = JsMemoryStore::new(db_path.to_string_lossy().into_owned())
                .expect("open temporary JsMemoryStore");
            Self {
                store: Some(store),
                dir,
            }
        }

        fn store(&self) -> &JsMemoryStore {
            self.store.as_ref().expect("test store must be open")
        }
    }

    impl Drop for TempJsStore {
        fn drop(&mut self) {
            self.store.take();
            std::fs::remove_dir_all(&self.dir)
                .expect("remove temporary memory-node test directory");
        }
    }

    fn upsert_test_memory(store: &JsMemoryStore, id: &str, text: &str) {
        store
            .upsert(
                json!({
                    "id": id,
                    "path": "/test/napi-bounds",
                    "summary": text,
                    "text": text,
                    "timestamp": "2026-07-25T00:00:00Z"
                })
                .to_string(),
            )
            .expect("upsert test memory through N-API method");
    }

    fn register_test_capability(store: &JsMemoryStore, index: usize) {
        store
            .hub_register(
                json!({
                    "id": format!("skill:napi-bound-{index:03}"),
                    "cap_type": "skill",
                    "name": format!("NAPI bounded capability {index:03}"),
                    "version": 1,
                    "description": "NAPI bounded capability",
                    "definition": "{}",
                    "enabled": true,
                    "uses": index,
                    "successes": 0,
                    "failures": 0,
                    "avg_rating": 0.0,
                    "last_used": null,
                    "created_at": "",
                    "updated_at": ""
                })
                .to_string(),
            )
            .expect("register test capability through N-API method");
    }

    fn seed_star_graph(store: &JsMemoryStore, edge_count: usize) {
        upsert_test_memory(store, "graph-root", "graph root");
        for index in 0..edge_count {
            let target = format!("graph-target-{index:03}");
            upsert_test_memory(store, &target, "graph target");
            store
                .add_edge(
                    json!({
                        "source_id": "graph-root",
                        "target_id": target,
                        "relation": "supports",
                        "weight": 1.0,
                        "metadata": {},
                        "created_at": "",
                        "valid_from": "",
                        "valid_to": null
                    })
                    .to_string(),
                )
                .expect("add test edge through N-API method");
        }
    }

    fn parse_json_array(json: String) -> Vec<Value> {
        serde_json::from_str(&json).expect("parse N-API JSON array")
    }

    #[test]
    fn search_options_cap_u64_bounds_before_the_core_search() {
        let options = search_options_from_json(Some(
            r#"{"top_k":18446744073709551615,"candidates":18446744073709551615}"#,
        ));

        assert_eq!(options.top_k, MAX_SEARCH_TOP_K);
        assert_eq!(
            options.candidates_per_channel,
            MAX_SEARCH_CANDIDATES_PER_CHANNEL
        );
    }

    #[test]
    fn search_options_define_zero_bounds() {
        let options = search_options_from_json(Some(r#"{"top_k":0,"candidates":0}"#));

        assert_eq!(options.top_k, MIN_SEARCH_TOP_K);
        assert_eq!(
            options.candidates_per_channel, options.top_k,
            "explicit candidates=0 must floor to normalized top_k"
        );
    }

    #[test]
    fn search_options_cover_omission_and_candidates_below_top_k_through_store() {
        let default_options = SearchOptions::default();
        let only_top_k = search_options_from_json(Some(r#"{"top_k":1}"#));
        assert_eq!(
            only_top_k.candidates_per_channel,
            DEFAULT_SEARCH_CANDIDATES_PER_CHANNEL
        );

        let only_candidates = search_options_from_json(Some(r#"{"candidates":1}"#));
        assert_eq!(only_candidates.top_k, default_options.top_k);
        assert_eq!(
            only_candidates.candidates_per_channel,
            default_options.top_k
        );

        let temp = TempJsStore::new();
        for index in 0..3 {
            upsert_test_memory(
                temp.store(),
                &format!("search-bound-{index}"),
                &format!("production boundary needle row {index}"),
            );
        }
        let results = parse_json_array(
            temp.store()
                .search(
                    "production boundary needle".to_string(),
                    Some(r#"{"top_k":3,"candidates":1,"mmr_threshold":null}"#.to_string()),
                )
                .expect("search through exported production method"),
        );
        assert_eq!(results.len(), 3, "candidates below top_k must be raised");

        let top_only = parse_json_array(
            temp.store()
                .search(
                    "production boundary needle".to_string(),
                    Some(r#"{"top_k":1,"mmr_threshold":null}"#.to_string()),
                )
                .expect("search with candidates omitted through exported production method"),
        );
        assert_eq!(top_only.len(), 1);

        let candidates_only = parse_json_array(
            temp.store()
                .search(
                    "production boundary needle".to_string(),
                    Some(r#"{"candidates":1,"mmr_threshold":null}"#.to_string()),
                )
                .expect("search with top_k omitted through exported production method"),
        );
        assert_eq!(candidates_only.len(), 3);

        let zero_candidates = parse_json_array(
            temp.store()
                .search(
                    "production boundary needle".to_string(),
                    Some(r#"{"top_k":3,"candidates":0,"mmr_threshold":null}"#.to_string()),
                )
                .expect("search with zero candidates through exported production method"),
        );
        assert_eq!(zero_candidates.len(), 3);
    }

    #[test]
    fn js_limit_normalization_rejects_invalid_numbers_before_narrowing() {
        assert_eq!(
            normalize_js_limit("limit", None, DEFAULT_GET_ALL_LIMIT, MAX_GET_ALL_LIMIT)
                .expect("default limit"),
            DEFAULT_GET_ALL_LIMIT
        );
        assert_eq!(
            normalize_js_limit(
                "limit",
                Some(f64::MAX),
                DEFAULT_GET_ALL_LIMIT,
                MAX_GET_ALL_LIMIT
            )
            .expect("huge finite limit"),
            MAX_GET_ALL_LIMIT
        );
        for invalid in [-1.0, 1.5, f64::NAN, f64::INFINITY] {
            assert!(
                normalize_js_limit(
                    "limit",
                    Some(invalid),
                    DEFAULT_GET_ALL_LIMIT,
                    MAX_GET_ALL_LIMIT
                )
                .is_err(),
                "invalid JavaScript number must be rejected: {invalid}"
            );
        }
    }

    #[test]
    fn napi_get_all_caps_huge_limit_before_sqlite() {
        let temp = TempJsStore::new();
        for index in 0..=MAX_GET_ALL_LIMIT {
            upsert_test_memory(
                temp.store(),
                &format!("get-all-bound-{index:03}"),
                "get all bounded row",
            );
        }

        let rows = parse_json_array(
            temp.store()
                .get_all(Some(f64::MAX))
                .expect("getAll through exported production method"),
        );
        assert_eq!(rows.len(), MAX_GET_ALL_LIMIT);

        assert!(
            parse_json_array(temp.store().get_all(Some(0.0)).expect("zero getAll limit"))
                .is_empty()
        );
        for invalid in [-1.0, 1.5, f64::NAN, f64::INFINITY] {
            assert!(
                temp.store().get_all(Some(invalid)).is_err(),
                "getAll must reject invalid JavaScript number: {invalid}"
            );
        }
    }

    #[test]
    fn napi_hub_discover_is_bounded_by_default() {
        let temp = TempJsStore::new();
        for index in 0..=DEFAULT_HUB_DISCOVER_LIMIT {
            register_test_capability(temp.store(), index);
        }

        let rows = parse_json_array(
            temp.store()
                .hub_discover(None, None, None)
                .expect("hubDiscover through exported production method"),
        );
        assert_eq!(rows.len(), DEFAULT_HUB_DISCOVER_LIMIT);

        let searched = parse_json_array(
            temp.store()
                .hub_discover(Some("bounded capability".to_string()), None, Some(2.0))
                .expect("limited hub search through exported production method"),
        );
        assert_eq!(searched.len(), 2);
        assert!(parse_json_array(
            temp.store()
                .hub_discover(None, None, Some(0.0))
                .expect("zero hub list limit")
        )
        .is_empty());
    }

    #[test]
    fn napi_get_edges_is_bounded_by_default() {
        let temp = TempJsStore::new();
        seed_star_graph(temp.store(), DEFAULT_GET_EDGES_LIMIT + 1);

        let edges = parse_json_array(
            temp.store()
                .get_edges(
                    "graph-root".to_string(),
                    Some("outgoing".to_string()),
                    None,
                    None,
                )
                .expect("getEdges through exported production method"),
        );
        assert_eq!(edges.len(), DEFAULT_GET_EDGES_LIMIT);

        let limited = parse_json_array(
            temp.store()
                .get_edges(
                    "graph-root".to_string(),
                    Some("outgoing".to_string()),
                    None,
                    Some(3.0),
                )
                .expect("limited getEdges through exported production method"),
        );
        assert_eq!(limited.len(), 3);
        assert!(parse_json_array(
            temp.store()
                .get_edges(
                    "graph-root".to_string(),
                    Some("outgoing".to_string()),
                    None,
                    Some(0.0),
                )
                .expect("zero getEdges limit")
        )
        .is_empty());
    }

    #[test]
    fn napi_graph_expand_bounds_edge_accumulation_by_default() {
        let temp = TempJsStore::new();
        seed_star_graph(temp.store(), DEFAULT_GRAPH_EXPAND_EDGE_LIMIT + 1);

        let result: Value = serde_json::from_str(
            &temp
                .store()
                .graph_expand("[\"graph-root\"]".to_string(), Some(1.0), None, None)
                .expect("graphExpand through exported production method"),
        )
        .expect("parse graph expansion result");
        assert_eq!(
            result["edges"].as_array().expect("graph edges").len(),
            DEFAULT_GRAPH_EXPAND_EDGE_LIMIT
        );

        let limited: Value = serde_json::from_str(
            &temp
                .store()
                .graph_expand("[\"graph-root\"]".to_string(), Some(1.0), None, Some(4.0))
                .expect("limited graphExpand through exported production method"),
        )
        .expect("parse limited graph expansion result");
        assert_eq!(limited["edges"].as_array().expect("graph edges").len(), 4);

        let zero: Value = serde_json::from_str(
            &temp
                .store()
                .graph_expand("[\"graph-root\"]".to_string(), Some(1.0), None, Some(0.0))
                .expect("zero graph edge limit"),
        )
        .expect("parse zero graph expansion result");
        assert!(zero["edges"].as_array().expect("graph edges").is_empty());
        assert!(zero["entries"]
            .as_array()
            .expect("graph entries")
            .is_empty());

        assert!(temp
            .store()
            .graph_expand("[\"graph-root\"]".to_string(), Some(1.5), None, Some(1.0),)
            .is_err());
    }
}
