use crate::tool_params::{GetMemoryParams, RememberParams, SearchMemoryParams};
use crate::MemoryServer;
use serde_json::{json, Value};
use std::path::Path;

pub(in crate::bootstrap::poke_cli) async fn probe_memory_basic(
    server: &MemoryServer,
    run_dir: &Path,
) -> Result<Value, String> {
    let marker = format!("poke_{}", uuid::Uuid::new_v4().as_simple());
    let fact = format!("{marker} isolated memory probe fact");
    let saved_raw = crate::memory_search_ops::handle_remember(
        server,
        RememberParams {
            text: fact.clone(),
            summary: "Poke isolated memory probe".to_string(),
            tags: vec!["poke".to_string(), "smoke".to_string()],
            topic: "poke-memory-basic".to_string(),
            importance: Some(0.2),
            scope: Some("project".to_string()),
            project: None,
            path: Some("/scratch/poke/memory-basic".to_string()),
            category: Some("fact".to_string()),
            domain: Some("engineering".to_string()),
            retention_policy: None,
            valid_from: None,
            valid_until: None,
            force: true,
        },
    )
    .await?;
    let saved: Value = serde_json::from_str(&saved_raw).map_err(|e| format!("parse save: {e}"))?;
    let id = saved
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("remember response lacks id: {saved}"))?;
    let search_raw = crate::memory_search_ops::handle_search_memory(
        server,
        SearchMemoryParams {
            query: marker.clone(),
            query_vec: None,
            top_k: 5,
            path_prefix: Some("/scratch/poke".to_string()),
            include_training: false,
            include_archived: false,
            candidates_per_channel: 10,
            mmr_threshold: None,
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            context_symbols: Vec::new(),
            agent_role: None,
            project: None,
            domain: Some("engineering".to_string()),
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
        },
        false,
    )
    .await?;
    let search: Value =
        serde_json::from_str(&search_raw).map_err(|e| format!("parse search: {e}"))?;
    let hits = search
        .as_array()
        .cloned()
        .or_else(|| {
            search
                .get("results")
                .or_else(|| search.get("memories"))
                .and_then(Value::as_array)
                .cloned()
        })
        .unwrap_or_default();
    let found = hits.iter().any(|hit| {
        hit.get("id").and_then(Value::as_str) == Some(id)
            || hit
                .get("text")
                .and_then(Value::as_str)
                .is_some_and(|text| text.contains(&marker))
    });
    let get_raw = crate::memory_ops::handle_get_memory(
        server,
        GetMemoryParams {
            id: id.to_string(),
            include_archived: false,
            project: None,
        },
    )
    .await?;
    let get: Value = serde_json::from_str(&get_raw).map_err(|e| format!("parse get: {e}"))?;
    let exact = get
        .get("text")
        .and_then(Value::as_str)
        .is_some_and(|text| text == fact);
    if !found || !exact {
        return Err(format!(
            "memory probe failed: found={found} exact={exact} saved={saved} search={search} get={get}"
        ));
    }
    Ok(json!({
        "name": "memory_basic",
        "status": "passed",
        "expected": "save/search/get exact isolated poke_ fact",
        "observed": {
            "id": id,
            "search_hit": found,
            "exact_get": exact,
        },
        "cleanup": {
            "mode": "isolated_sandbox_retained_for_audit",
            "run_dir": run_dir.to_string_lossy(),
        },
        "repro_steps": [
            "remember poke_ fact in isolated project DB",
            "search /scratch/poke for marker",
            "get saved id and compare exact text"
        ],
    }))
}
