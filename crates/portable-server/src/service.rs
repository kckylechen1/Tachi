//! The MCP service object: the minimal downstream keep-set of memory tools
//! (`save` / `search` / `get` / `status`) over `portable-kernel`.
//!
//! Deliberately excluded (the #924 "denied surface"): briefing/checkpoint
//! (entangled with dispatch/handoff), `tachi_gh`, dispatch/ship/merge, hub CLI,
//! foundry job queue, vault secrets, PR lifecycle. Those live in `tachi-server`
//! and cannot be reached from here — this crate does not depend on it.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::schemars::{self, JsonSchema};
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use serde::Deserialize;
use serde_json::json;

use portable_kernel::{DecayPolicy, MemoryEntry, MemoryStore, SearchOptions};

/// Params for the `save` tool. Only `text` is required; everything else mirrors
/// the kernel's `MemoryEntry` defaults so a caller can write a bare note.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SaveParams {
    /// Full text content of the memory (required).
    pub text: String,
    /// Optional stable id; when omitted a UUID v4 is generated. Reusing an id updates in place.
    #[serde(default)]
    pub id: Option<String>,
    /// Short summary (<=100 chars).
    #[serde(default)]
    pub summary: String,
    /// Hierarchical path, e.g. "/trading/notes".
    #[serde(default)]
    pub path: Option<String>,
    /// Category: "fact" | "decision" | "experience" | "preference" | "entity" | "other".
    #[serde(default)]
    pub category: Option<String>,
    /// Scope: "user" | "project" | "general".
    #[serde(default)]
    pub scope: Option<String>,
    /// Domain scoping key (e.g. "domain-pack"); None = unscoped.
    #[serde(default)]
    pub domain: Option<String>,
    /// Retention policy: "ephemeral" | "durable" | "permanent" | "pinned".
    #[serde(default)]
    pub retention_policy: Option<String>,
    /// 0.0-1.0 importance score.
    #[serde(default)]
    pub importance: Option<f64>,
    /// Keyword tags for recall/FTS.
    #[serde(default)]
    pub keywords: Vec<String>,
}

/// Default `top_k` when the caller omits it, mirroring `SearchOptions::default()`.
const DEFAULT_SEARCH_TOP_K: usize = 6;

/// Upper bound on `top_k`, mirroring `tachi-params::MAX_SEARCH_TOP_K` (100). Not
/// depended on directly: `tachi-params` pulls in full `memcore` (admin on),
/// which would reintroduce admin-feature unification into this crate's build
/// graph and defeat the portable isolation guarantee, so the cap is mirrored
/// as a plain constant instead of imported.
const MAX_SEARCH_TOP_K: usize = 100;

/// Clamp a caller-supplied `top_k` into `[1, MAX_SEARCH_TOP_K]`, defaulting to
/// `DEFAULT_SEARCH_TOP_K` when absent. An unbounded `top_k` would let a caller
/// force `hybrid_search` to rank/return an unbounded result set — a cheap DoS
/// lever over a store containing arbitrarily many rows.
fn normalized_top_k(requested: Option<usize>) -> usize {
    requested
        .unwrap_or(DEFAULT_SEARCH_TOP_K)
        .clamp(1, MAX_SEARCH_TOP_K)
}

/// Params for the `search` tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Query string (hybrid text + FTS + optional vector).
    pub query: String,
    /// Max results to return. Defaults to 6, clamped to at most 100.
    #[serde(default)]
    pub top_k: Option<usize>,
    /// Optional path-prefix filter, e.g. "/trading".
    #[serde(default)]
    pub path: Option<String>,
}

/// Params for the `get` tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GetParams {
    /// Entry id to fetch.
    pub id: String,
}

/// Params for the `status` tool (no arguments).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct StatusParams {}

/// Params for the downstream `hapi_memory` / `tachi_memory` aliases. The
/// portable profile deliberately supports only save and search actions.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct MemoryActionParams {
    pub action: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub retention_policy: Option<String>,
    #[serde(default)]
    pub importance: Option<f64>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub top_k: Option<usize>,
}

impl MemoryActionParams {
    fn save_params(self) -> Result<SaveParams, String> {
        let text = self
            .text
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| "hapi_memory action=save requires text".to_string())?;
        Ok(SaveParams {
            text,
            id: self.id,
            summary: self.summary,
            path: self.path,
            category: self.category,
            scope: self.scope,
            domain: self.domain,
            retention_policy: self.retention_policy,
            importance: self.importance,
            keywords: self.keywords,
        })
    }

    fn search_params(self) -> Result<SearchParams, String> {
        let query = self
            .query
            .filter(|query| !query.trim().is_empty())
            .ok_or_else(|| "hapi_memory action=search requires query".to_string())?;
        Ok(SearchParams {
            query,
            top_k: self.top_k,
            path: self.path,
        })
    }
}

#[derive(Clone)]
struct StoreHandle {
    name: String,
    path: String,
    store: Arc<Mutex<MemoryStore>>,
}

#[derive(Clone)]
struct StoreSet {
    global: StoreHandle,
    attached: Vec<StoreHandle>,
}

impl StoreSet {
    fn new(
        global: MemoryStore,
        global_path: String,
        attached: Vec<(String, String, MemoryStore)>,
    ) -> Self {
        Self {
            global: StoreHandle {
                name: "global".to_string(),
                path: global_path,
                store: Arc::new(Mutex::new(global)),
            },
            attached: attached
                .into_iter()
                .map(|(name, path, store)| StoreHandle {
                    name,
                    path,
                    store: Arc::new(Mutex::new(store)),
                })
                .collect(),
        }
    }

    fn all(&self) -> impl Iterator<Item = &StoreHandle> {
        std::iter::once(&self.global).chain(self.attached.iter())
    }

    fn write_target(&self, scope: Option<&str>) -> &StoreHandle {
        if matches!(scope, Some(scope) if scope.eq_ignore_ascii_case("project")) {
            if let Some(project) = self.attached.iter().find(|store| store.name == "project") {
                return project;
            }
        }
        &self.global
    }

    fn project(&self) -> Option<&StoreHandle> {
        self.attached.iter().find(|store| store.name == "project")
    }
}

/// The rmcp service. Owns the kernel store behind a mutex (rusqlite `Connection`
/// is `Send` but not `Sync`; all handler work is synchronous and never awaits
/// while the lock is held) plus the injected #791 decay policy.
#[derive(Clone)]
pub struct PortableServer {
    stores: StoreSet,
    decay_policy: Option<Arc<dyn DecayPolicy>>,
    decay_policy_name: String,
    db_path: String,
}

// A single #[tool_router] block covers this whole tool surface (no
// multi-router combination like tachi-server's per-facade routers), so
// #[tool_handler] below uses its default `Self::tool_router()` expression
// instead of a stored field.
#[tool_router]
impl PortableServer {
    pub fn new(
        store: MemoryStore,
        attached_stores: Vec<(String, String, MemoryStore)>,
        decay_policy: Option<Arc<dyn DecayPolicy>>,
        decay_policy_name: String,
        db_path: String,
    ) -> Self {
        Self {
            stores: StoreSet::new(store, db_path.clone(), attached_stores),
            decay_policy,
            decay_policy_name,
            db_path,
        }
    }

    #[tool(
        description = "Save a memory entry to the kernel store. Only `text` is required; omit `id` to create, pass an existing `id` to update in place. Returns the stored entry id."
    )]
    pub async fn save(&self, Parameters(params): Parameters<SaveParams>) -> Result<String, String> {
        let id = params
            .id
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let now = chrono::Utc::now().to_rfc3339();
        let scope = params.scope.unwrap_or_else(|| "project".to_string());
        let target = self.stores.write_target(Some(&scope));

        let mut entry = MemoryEntry {
            id: id.clone(),
            path: params.path.unwrap_or_else(|| "/notes".to_string()),
            summary: params.summary,
            text: params.text,
            importance: params.importance.unwrap_or(0.6).clamp(0.0, 1.0),
            timestamp: now.clone(),
            valid_from: now,
            valid_until: None,
            category: params.category.unwrap_or_else(|| "fact".to_string()),
            topic: String::new(),
            keywords: params.keywords,
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "portable-server".to_string(),
            scope,
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            vector: None,
            retention_policy: params.retention_policy,
            domain: params.domain,
            metadata: serde_json::Value::Object(Default::default()),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        };
        entry.fold_persons_into_entities();

        let mut store = target
            .store
            .lock()
            .map_err(|_| "store lock poisoned".to_string())?;
        store.upsert(&entry).map_err(|e| e.to_string())?;
        Ok(json!({ "id": id, "saved": true }).to_string())
    }

    #[tool(
        description = "Hybrid search over the kernel store (text + FTS + optional vector). Returns ranked results with scores. The configured decay policy (#791 hook) is applied."
    )]
    pub async fn search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<String, String> {
        let top_k = normalized_top_k(params.top_k);
        let mut results = Vec::new();
        for handle in self.stores.all() {
            let opts = SearchOptions {
                top_k,
                path_prefix: params.path.clone(),
                decay_policy: self.decay_policy.clone(),
                ..Default::default()
            };
            let store = handle
                .store
                .lock()
                .map_err(|_| "store lock poisoned".to_string())?;
            let mut store_results = store
                .search(&params.query, Some(opts))
                .map_err(|e| e.to_string())?;
            results.append(&mut store_results);
        }
        results.sort_by(|left, right| right.score.final_score.total_cmp(&left.score.final_score));
        let mut seen = HashSet::new();
        results.retain(|result| seen.insert(result.entry.id.clone()));
        results.truncate(top_k);
        serde_json::to_string(&results).map_err(|e| e.to_string())
    }

    #[tool(description = "Fetch a single memory entry by id. Returns null when not found.")]
    pub async fn get(&self, Parameters(params): Parameters<GetParams>) -> Result<String, String> {
        for handle in self.stores.all() {
            let store = handle
                .store
                .lock()
                .map_err(|_| "store lock poisoned".to_string())?;
            if let Some(entry) = store.get(&params.id).map_err(|e| e.to_string())? {
                return serde_json::to_string(&Some(entry)).map_err(|e| e.to_string());
            }
        }
        serde_json::to_string(&Option::<MemoryEntry>::None).map_err(|e| e.to_string())
    }

    /// Cheap DB-reachability probe used by `--daemon` mode's `/health` route
    /// (`http.rs`): `true` when the store answers a stats query without
    /// error. Kept transport-agnostic here (returns a plain `bool`, not an
    /// HTTP response) so this file stays free of any HTTP-framework
    /// dependency — `http.rs` owns turning this into a status code + JSON.
    pub fn health_ok(&self) -> bool {
        self.stores.all().all(|handle| {
            handle
                .store
                .lock()
                .map(|store| store.stats(false).is_ok())
                .unwrap_or(false)
        })
    }

    #[tool(
        description = "Report portable-server runtime status: db path, entry count, vector availability, active decay policy, and the exposed tool set."
    )]
    pub async fn status(
        &self,
        Parameters(_params): Parameters<StatusParams>,
    ) -> Result<String, String> {
        let database_status = |handle: &StoreHandle| -> Result<serde_json::Value, String> {
            let store = handle
                .store
                .lock()
                .map_err(|_| "store lock poisoned".to_string())?;
            let stats = store.stats(true).map_err(|e| e.to_string())?;
            Ok(json!({
                "name": handle.name,
                "path": handle.path,
                "entry_count": stats.total,
                "vec_available": store.vec_available,
            }))
        };
        let global = database_status(&self.stores.global)?;
        let project = self.stores.project().map(database_status).transpose()?;
        let attached = self
            .stores
            .attached
            .iter()
            .map(database_status)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(json!({
            "profile": "portable",
            "runtime": { "name": "tachi", "profile": "portable" },
            "db_path": self.db_path,
            "entry_count": global["entry_count"],
            "vec_available": global["vec_available"],
            "decay_policy": self.decay_policy_name,
            "databases": { "global": global, "project": project, "attached": attached },
            "tools": ["save", "search", "get", "status", "hapi_memory", "hapi_save", "hapi_search", "hapi_runtime", "tachi_memory", "tachi_save", "tachi_search", "runtime_info"],
        })
        .to_string())
    }

    #[tool(description = "Quant-compatible alias for save.")]
    pub async fn hapi_save(
        &self,
        Parameters(params): Parameters<SaveParams>,
    ) -> Result<String, String> {
        self.save(Parameters(params)).await
    }

    #[tool(description = "Quant-compatible alias for search.")]
    pub async fn hapi_search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<String, String> {
        self.search(Parameters(params)).await
    }

    #[tool(
        description = "Quant-compatible memory facade. Supports action=save and action=search in the portable profile."
    )]
    pub async fn hapi_memory(
        &self,
        Parameters(params): Parameters<MemoryActionParams>,
    ) -> Result<String, String> {
        match params.action.trim().to_ascii_lowercase().as_str() {
            "save" => self.save(Parameters(params.save_params()?)).await,
            "search" => self.search(Parameters(params.search_params()?)).await,
            action => Err(format!(
                "action '{action}' is not available in this profile"
            )),
        }
    }

    #[tool(description = "Quant-compatible runtime status alias.")]
    pub async fn hapi_runtime(
        &self,
        Parameters(params): Parameters<StatusParams>,
    ) -> Result<String, String> {
        self.status(Parameters(params)).await
    }

    #[tool(description = "Legacy Tachi alias for the portable memory facade.")]
    pub async fn tachi_memory(
        &self,
        Parameters(params): Parameters<MemoryActionParams>,
    ) -> Result<String, String> {
        self.hapi_memory(Parameters(params)).await
    }

    #[tool(description = "Legacy Tachi alias for save.")]
    pub async fn tachi_save(
        &self,
        Parameters(params): Parameters<SaveParams>,
    ) -> Result<String, String> {
        self.save(Parameters(params)).await
    }

    #[tool(description = "Legacy Tachi alias for search.")]
    pub async fn tachi_search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<String, String> {
        self.search(Parameters(params)).await
    }

    #[tool(description = "Legacy runtime status alias used by downstream session validation.")]
    pub async fn runtime_info(
        &self,
        Parameters(params): Parameters<StatusParams>,
    ) -> Result<String, String> {
        self.status(Parameters(params)).await
    }
}

#[tool_handler]
impl ServerHandler for PortableServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Portable memory kernel (tachi #924): save/search/get/status plus Quant-compatible aliases. \
             Operator surfaces (dispatch, hub, foundry, vault, PR lifecycle) are \
             not available in this profile.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boot(policy: Option<Arc<dyn DecayPolicy>>, name: &str) -> PortableServer {
        let store = MemoryStore::open_in_memory().expect("open_in_memory");
        PortableServer::new(
            store,
            Vec::new(),
            policy,
            name.to_string(),
            ":memory:".to_string(),
        )
    }

    fn save_params(text: &str, path: &str) -> SaveParams {
        SaveParams {
            text: text.to_string(),
            id: None,
            summary: String::new(),
            path: Some(path.to_string()),
            category: None,
            scope: None,
            domain: None,
            retention_policy: None,
            importance: Some(0.7),
            keywords: vec!["portable".to_string()],
        }
    }

    /// Boot the rmcp service object in-process, save an entry, then prove
    /// search + get round-trip it back. This is the #924 smoke: the stripped
    /// server serves the memory kernel with no operator surface present.
    #[tokio::test]
    async fn boot_save_search_round_trip() {
        let server = boot(None, "default");

        let saved = server
            .save(Parameters(save_params(
                "portable kernel round trip fact about trading decay",
                "/trading/notes",
            )))
            .await
            .expect("save");
        let saved: serde_json::Value = serde_json::from_str(&saved).expect("save json");
        assert_eq!(saved["saved"], serde_json::json!(true));
        let id = saved["id"].as_str().expect("id").to_string();

        // search finds it
        let hits = server
            .search(Parameters(SearchParams {
                query: "trading decay fact".to_string(),
                top_k: Some(5),
                path: None,
            }))
            .await
            .expect("search");
        let hits: serde_json::Value = serde_json::from_str(&hits).expect("search json");
        let arr = hits.as_array().expect("search returns array");
        assert!(
            !arr.is_empty(),
            "search should return the saved entry, got {hits}"
        );

        // get by id round-trips the exact entry
        let got = server
            .get(Parameters(GetParams { id: id.clone() }))
            .await
            .expect("get");
        let got: serde_json::Value = serde_json::from_str(&got).expect("get json");
        assert_eq!(got["id"], serde_json::json!(id));
        assert_eq!(got["path"], serde_json::json!("/trading/notes"));

        // status reflects the write and the exposed tool set
        let status = server
            .status(Parameters(StatusParams {}))
            .await
            .expect("status");
        let status: serde_json::Value = serde_json::from_str(&status).expect("status json");
        assert_eq!(status["profile"], serde_json::json!("portable"));
        assert!(status["entry_count"].as_i64().unwrap_or(0) >= 1);
        assert_eq!(status["decay_policy"], serde_json::json!("default"));
    }

    #[tokio::test]
    async fn status_has_runtime_info_database_shape_for_downstream_clients() {
        let server = boot(None, "default");
        let status = server
            .status(Parameters(StatusParams {}))
            .await
            .expect("status");
        let status: serde_json::Value = serde_json::from_str(&status).expect("status json");

        assert_eq!(status["runtime"]["name"], serde_json::json!("tachi"));
        assert_eq!(
            status["databases"]["global"]["path"],
            serde_json::json!(":memory:")
        );
        assert!(
            status["databases"]["project"].is_null(),
            "the single-store profile must explicitly report that no project store is attached"
        );
    }

    #[tokio::test]
    async fn hapi_and_legacy_aliases_preserve_portable_memory_contract() {
        let server = boot(None, "default");
        let action = MemoryActionParams {
            action: "save".to_string(),
            text: Some("alias contract fact".to_string()),
            id: None,
            summary: String::new(),
            path: Some("/compat".to_string()),
            category: None,
            scope: None,
            domain: None,
            retention_policy: None,
            importance: None,
            keywords: Vec::new(),
            query: None,
            top_k: None,
        };
        server
            .hapi_memory(Parameters(action))
            .await
            .expect("hapi save");

        let hits = server
            .tachi_search(Parameters(SearchParams {
                query: "alias contract".to_string(),
                top_k: Some(3),
                path: None,
            }))
            .await
            .expect("legacy search");
        assert!(!serde_json::from_str::<Vec<serde_json::Value>>(&hits)
            .expect("hits")
            .is_empty());

        let runtime = server
            .hapi_runtime(Parameters(StatusParams {}))
            .await
            .expect("hapi runtime");
        let runtime: serde_json::Value = serde_json::from_str(&runtime).expect("runtime json");
        assert_eq!(runtime["runtime"]["name"], serde_json::json!("tachi"));

        let unsupported = server
            .tachi_memory(Parameters(MemoryActionParams {
                action: "wiki".to_string(),
                text: None,
                id: None,
                summary: String::new(),
                path: None,
                category: None,
                scope: None,
                domain: None,
                retention_policy: None,
                importance: None,
                keywords: Vec::new(),
                query: None,
                top_k: None,
            }))
            .await
            .expect_err("unsupported action must fail clearly");
        assert!(unsupported.contains("not available in this profile"));
    }

    #[tokio::test]
    async fn project_store_receives_project_writes_and_reads_merge_across_store_set() {
        let global = MemoryStore::open_in_memory().expect("global");
        let project = MemoryStore::open_in_memory().expect("project");
        let server = PortableServer::new(
            global,
            vec![(
                "project".to_string(),
                "/tmp/trading.db".to_string(),
                project,
            )],
            None,
            "default".to_string(),
            "/tmp/global.db".to_string(),
        );

        let mut global_params = save_params("global store fact", "/global");
        global_params.scope = Some("global".to_string());
        server
            .save(Parameters(global_params))
            .await
            .expect("global save");

        server
            .save(Parameters(save_params("project store fact", "/project")))
            .await
            .expect("default project save");

        let hits = server
            .search(Parameters(SearchParams {
                query: "store fact".to_string(),
                top_k: Some(10),
                path: None,
            }))
            .await
            .expect("merged search");
        let hits: Vec<serde_json::Value> = serde_json::from_str(&hits).expect("hits json");
        assert_eq!(
            hits.len(),
            2,
            "search must merge global and attached project stores"
        );

        let status = server
            .status(Parameters(StatusParams {}))
            .await
            .expect("status");
        let status: serde_json::Value = serde_json::from_str(&status).expect("status json");
        assert_eq!(
            status["databases"]["global"]["path"],
            serde_json::json!("/tmp/global.db")
        );
        assert_eq!(
            status["databases"]["project"]["path"],
            serde_json::json!("/tmp/trading.db")
        );
        assert_eq!(status["databases"]["global"]["entry_count"], 1);
        assert_eq!(status["databases"]["project"]["entry_count"], 1);
    }

    /// The #791 hook is reachable from this profile: a `flat` policy injected
    /// via config flows into `SearchOptions::decay_policy` and is reported by
    /// status. Default (`None`) keeps the kernel's current behavior.
    #[tokio::test]
    async fn injected_decay_policy_is_wired() {
        let server = boot(Some(Arc::new(crate::decay::FlatDecayPolicy)), "flat");
        server
            .save(Parameters(save_params("flat policy fact", "/scratch")))
            .await
            .expect("save");
        let hits = server
            .search(Parameters(SearchParams {
                query: "flat policy fact".to_string(),
                top_k: Some(3),
                path: None,
            }))
            .await
            .expect("search");
        let hits: serde_json::Value = serde_json::from_str(&hits).expect("search json");
        assert!(!hits.as_array().expect("array").is_empty());

        let status = server
            .status(Parameters(StatusParams {}))
            .await
            .expect("status");
        let status: serde_json::Value = serde_json::from_str(&status).expect("status json");
        assert_eq!(status["decay_policy"], serde_json::json!("flat"));
    }

    /// Pure clamp logic: absent -> default; in-range passes through; huge or
    /// zero values clamp into `[1, MAX_SEARCH_TOP_K]`.
    #[test]
    fn normalized_top_k_clamps_range() {
        assert_eq!(normalized_top_k(None), DEFAULT_SEARCH_TOP_K);
        assert_eq!(normalized_top_k(Some(3)), 3);
        assert_eq!(normalized_top_k(Some(0)), 1);
        assert_eq!(normalized_top_k(Some(999_999)), MAX_SEARCH_TOP_K);
        assert_eq!(normalized_top_k(Some(MAX_SEARCH_TOP_K)), MAX_SEARCH_TOP_K);
    }

    /// A caller-forced `top_k=999999` (the DoS lever an uncapped value would
    /// open) must not error and must not be forwarded raw into
    /// `SearchOptions`: the search tool call succeeds and the result set is
    /// bounded by `MAX_SEARCH_TOP_K`, not by the requested value.
    #[tokio::test]
    async fn search_clamps_huge_top_k_without_error() {
        let server = boot(None, "default");
        server
            .save(Parameters(save_params(
                "uncapped top_k dos regression fact",
                "/scratch/dos",
            )))
            .await
            .expect("save");

        let hits = server
            .search(Parameters(SearchParams {
                query: "uncapped top_k dos regression fact".to_string(),
                top_k: Some(999_999),
                path: None,
            }))
            .await
            .expect("search must not error on an oversized top_k");
        let hits: serde_json::Value = serde_json::from_str(&hits).expect("search json");
        let arr = hits.as_array().expect("search returns array");
        assert!(
            arr.len() <= MAX_SEARCH_TOP_K,
            "expected at most {MAX_SEARCH_TOP_K} results, got {}",
            arr.len()
        );
    }
}
