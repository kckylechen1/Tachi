//! The MCP service object: the minimal downstream keep-set of memory tools
//! (`save` / `search` / `get` / `status`) over `portable-kernel`.
//!
//! Deliberately excluded (the #924 "denied surface"): briefing/checkpoint
//! (entangled with dispatch/handoff), `tachi_gh`, dispatch/ship/merge, hub CLI,
//! foundry job queue, vault secrets, PR lifecycle. Those live in `tachi-server`
//! and cannot be reached from here — this crate does not depend on it.

use std::sync::{Arc, Mutex};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::schemars::{self, JsonSchema};
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use serde::Deserialize;
use serde_json::json;

use portable_kernel::{
    DecayPolicy, MemoryEntry, MemoryStore, SearchOptions,
};

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

/// Params for the `search` tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Query string (hybrid text + FTS + optional vector).
    pub query: String,
    /// Max results to return. Defaults to 6.
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

/// The rmcp service. Owns the kernel store behind a mutex (rusqlite `Connection`
/// is `Send` but not `Sync`; all handler work is synchronous and never awaits
/// while the lock is held) plus the injected #791 decay policy.
#[derive(Clone)]
pub struct PortableServer {
    store: Arc<Mutex<MemoryStore>>,
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
        decay_policy: Option<Arc<dyn DecayPolicy>>,
        decay_policy_name: String,
        db_path: String,
    ) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
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
            scope: params.scope.unwrap_or_else(|| "project".to_string()),
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

        let mut store = self.store.lock().map_err(|_| "store lock poisoned".to_string())?;
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
        let opts = SearchOptions {
            top_k: params.top_k.unwrap_or(6),
            path_prefix: params.path,
            decay_policy: self.decay_policy.clone(),
            ..Default::default()
        };
        let store = self.store.lock().map_err(|_| "store lock poisoned".to_string())?;
        let results = store
            .search(&params.query, Some(opts))
            .map_err(|e| e.to_string())?;
        serde_json::to_string(&results).map_err(|e| e.to_string())
    }

    #[tool(description = "Fetch a single memory entry by id. Returns null when not found.")]
    pub async fn get(&self, Parameters(params): Parameters<GetParams>) -> Result<String, String> {
        let store = self.store.lock().map_err(|_| "store lock poisoned".to_string())?;
        let entry = store.get(&params.id).map_err(|e| e.to_string())?;
        serde_json::to_string(&entry).map_err(|e| e.to_string())
    }

    #[tool(
        description = "Report portable-server runtime status: db path, entry count, vector availability, active decay policy, and the exposed tool set."
    )]
    pub async fn status(
        &self,
        Parameters(_params): Parameters<StatusParams>,
    ) -> Result<String, String> {
        let store = self.store.lock().map_err(|_| "store lock poisoned".to_string())?;
        let stats = store.stats(true).map_err(|e| e.to_string())?;
        let vec_available = store.vec_available;
        Ok(json!({
            "profile": "portable",
            "db_path": self.db_path,
            "entry_count": stats.total,
            "vec_available": vec_available,
            "decay_policy": self.decay_policy_name,
            "tools": ["save", "search", "get", "status"],
        })
        .to_string())
    }
}

#[tool_handler]
impl ServerHandler for PortableServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Portable memory kernel (tachi #924): save/search/get/status only. \
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
        PortableServer::new(store, policy, name.to_string(), ":memory:".to_string())
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
}
