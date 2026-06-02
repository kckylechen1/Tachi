use super::*;

fn load_memory_entry(
    server: &MemoryServer,
    id: &str,
    project: Option<&str>,
    include_archived: bool,
) -> Result<Option<(MemoryEntry, DbScope)>, String> {
    if let Some(project_name) = project {
        let project_entry = server.with_named_project_store_read(project_name, |store| {
            store.get_with_options(id, include_archived).map_err(|e| {
                format!(
                    "Failed to get memory from project '{}': {}",
                    project_name, e
                )
            })
        })?;
        if let Some(entry) = project_entry {
            return Ok(Some((entry, DbScope::Project)));
        }
    } else if server.has_project_db() {
        let project_entry = server.with_project_store_read(|store| {
            store
                .get_with_options(id, include_archived)
                .map_err(|e| format!("Failed to get memory from project DB: {}", e))
        })?;
        if let Some(entry) = project_entry {
            return Ok(Some((entry, DbScope::Project)));
        }
    }

    let global_entry = server.with_global_store_read(|store| {
        store
            .get_with_options(id, include_archived)
            .map_err(|e| format!("Failed to get memory from global DB: {}", e))
    })?;
    Ok(global_entry.map(|entry| (entry, DbScope::Global)))
}

fn load_edges_for_memory(
    server: &MemoryServer,
    id: &str,
    db_scope: DbScope,
    project: Option<&str>,
) -> Result<Vec<memory_core::MemoryEdge>, String> {
    let load = |store: &mut MemoryStore| {
        store
            .get_edges(id, "both", None)
            .map_err(|e| format!("Failed to get edges: {e}"))
    };
    if db_scope == DbScope::Project {
        if let Some(project_name) = project {
            server.with_named_project_store_read(project_name, load)
        } else {
            server.with_project_store_read(load)
        }
    } else {
        server.with_global_store_read(load)
    }
}

pub(crate) async fn handle_add_edge(
    server: &MemoryServer,
    params: AddEdgeParams,
) -> Result<String, String> {
    let requested_scope = params.scope.clone();
    let (target_db, warning) = server.resolve_write_scope(&requested_scope);

    let edge = memory_core::MemoryEdge {
        source_id: params.source_id.clone(),
        target_id: params.target_id.clone(),
        relation: params.relation.clone(),
        weight: params.weight,
        metadata: params.metadata.unwrap_or(json!({})),
        created_at: Utc::now().to_rfc3339(),
        valid_from: String::new(),
        valid_to: None,
    };

    server.with_store_for_scope(target_db, |store| {
        store
            .add_edge(&edge)
            .map_err(|e| format!("Failed to add edge: {}", e))
    })?;

    let mut resp = serde_json::Map::new();
    resp.insert("ok".into(), json!(true));
    resp.insert("db".into(), json!(target_db.as_str()));
    resp.insert("source_id".into(), json!(params.source_id));
    resp.insert("target_id".into(), json!(params.target_id));
    resp.insert("relation".into(), json!(params.relation));
    if let Some(w) = warning {
        resp.insert("warning".into(), json!(w));
    }

    serde_json::to_string(&resp).map_err(|e| format!("Failed to serialize: {}", e))
}

pub(crate) async fn handle_get_edges(
    server: &MemoryServer,
    params: GetEdgesParams,
) -> Result<String, String> {
    let (target_db, _warning) = server.resolve_write_scope(&params.scope);

    let edges = server.with_store_for_scope(target_db, |store| {
        store
            .get_edges(
                &params.memory_id,
                &params.direction,
                params.relation_filter.as_deref(),
            )
            .map_err(|e| format!("Failed to get edges: {}", e))
    })?;

    let output: Vec<serde_json::Value> = edges
        .iter()
        .map(|e| {
            json!({
                "db": target_db.as_str(),
                "source_id": e.source_id,
                "target_id": e.target_id,
                "relation": e.relation,
                "weight": e.weight,
                "metadata": e.metadata,
                "created_at": e.created_at,
            })
        })
        .collect();

    serde_json::to_string(&output).map_err(|e| format!("Failed to serialize: {}", e))
}

pub(crate) async fn handle_memory_graph(
    server: &MemoryServer,
    params: MemoryGraphParams,
) -> Result<String, String> {
    let depth = params.depth.max(1).min(2);
    let mut seed_ids = Vec::<String>::new();

    if let Some(memory_id) = params
        .memory_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        seed_ids.push(memory_id.to_string());
    } else {
        let query = params
            .query
            .as_deref()
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .ok_or_else(|| "memory_graph requires memory_id or query".to_string())?;
        let rows = search_memory_rows(
            server,
            SearchMemoryParams {
                query: query.to_string(),
                query_vec: None,
                top_k: params.top_k.max(1),
                path_prefix: params.path_prefix.clone(),
                include_archived: false,
                candidates_per_channel: params.top_k.max(1).max(20),
                mmr_threshold: None,
                graph_expand_hops: 0,
                graph_relation_filter: None,
                weights: None,
                agent_role: None,
                project: params.project.clone(),
                domain: None,
                file_context: None,
                error_context: None,
                enable_rerank: false,
                as_of: None,
                include_metadata: false,
            },
        false,
        )
        .await?;
        seed_ids.extend(rows.into_iter().filter_map(|row| {
            row.get("id")
                .and_then(|value| value.as_str())
                .map(ToOwned::to_owned)
        }));
    }

    seed_ids.sort();
    seed_ids.dedup();
    if seed_ids.is_empty() {
        return serde_json::to_string(&json!({
            "status": "skipped",
            "reason": "no_seed_memories",
            "nodes": [],
            "edges": [],
        }))
        .map_err(|e| format!("Failed to serialize memory_graph response: {e}"));
    }

    let mut nodes = serde_json::Map::<String, serde_json::Value>::new();
    let mut edges = Vec::<serde_json::Value>::new();
    let mut seen_edge_keys = HashSet::<String>::new();
    let mut queue = std::collections::VecDeque::<(String, DbScope, usize)>::new();
    let mut visited = HashSet::<String>::new();

    for seed_id in &seed_ids {
        let Some((entry, scope)) =
            load_memory_entry(server, seed_id, params.project.as_deref(), false)?
        else {
            continue;
        };
        nodes.insert(seed_id.clone(), slim_entry(&entry, scope));
        queue.push_back((seed_id.clone(), scope, 0));
    }

    while let Some((current_id, scope, current_depth)) = queue.pop_front() {
        if !visited.insert(format!("{}::{}", scope.as_str(), current_id)) {
            continue;
        }
        let current_edges =
            load_edges_for_memory(server, &current_id, scope, params.project.as_deref())?;
        for edge in current_edges {
            let key = format!(
                "{}|{}|{}|{}",
                edge.source_id,
                edge.target_id,
                edge.relation,
                scope.as_str()
            );
            if seen_edge_keys.insert(key) {
                edges.push(json!({
                    "db": scope.as_str(),
                    "source_id": edge.source_id,
                    "target_id": edge.target_id,
                    "relation": edge.relation,
                    "weight": edge.weight,
                    "metadata": edge.metadata,
                    "created_at": edge.created_at,
                }));
            }

            let neighbor_id = if edge.source_id == current_id {
                edge.target_id.clone()
            } else {
                edge.source_id.clone()
            };
            if nodes.contains_key(&neighbor_id) {
                continue;
            }
            if let Some((entry, neighbor_scope)) =
                load_memory_entry(server, &neighbor_id, params.project.as_deref(), false)?
            {
                nodes.insert(neighbor_id.clone(), slim_entry(&entry, neighbor_scope));
                if current_depth + 1 < depth {
                    queue.push_back((neighbor_id, neighbor_scope, current_depth + 1));
                }
            }
        }
    }

    serde_json::to_string(&json!({
        "status": "completed",
        "seed_ids": seed_ids,
        "depth": depth,
        "node_count": nodes.len(),
        "edge_count": edges.len(),
        "nodes": nodes.into_values().collect::<Vec<_>>(),
        "edges": edges,
    }))
    .map_err(|e| format!("Failed to serialize memory_graph response: {e}"))
}

pub(crate) async fn handle_set_state(
    server: &MemoryServer,
    params: SetStateParams,
) -> Result<String, String> {
    let value_json = serde_json::to_string(&params.value)
        .map_err(|e| format!("Failed to serialize value: {}", e))?;

    server.with_global_store(|store| {
        let version = store
            .set_state("mcp", &params.key, &value_json)
            .map_err(|e| format!("Failed to set state: {}", e))?;

        serde_json::to_string(&json!({
            "key": params.key,
            "value": params.value,
            "version": version
        }))
        .map_err(|e| format!("Failed to serialize response: {}", e))
    })
}

pub(crate) async fn handle_get_state(
    server: &MemoryServer,
    params: GetStateParams,
) -> Result<String, String> {
    server.with_global_store(|store| match store.get_state_kv("mcp", &params.key) {
        Ok(Some((value, version))) => {
            let parsed_value: serde_json::Value =
                serde_json::from_str(&value).unwrap_or_else(|_| serde_json::json!(value));

            serde_json::to_string(&json!({
                "key": params.key,
                "value": parsed_value,
                "version": version
            }))
            .map_err(|e| format!("Failed to serialize: {}", e))
        }
        Ok(None) => serde_json::to_string(&json!({
            "key": params.key,
            "error": "not found"
        }))
        .map_err(|e| format!("Failed to serialize: {}", e)),
        Err(e) => Err(format!("Failed to get state: {}", e)),
    })
}
