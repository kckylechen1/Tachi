use super::*;

fn default_checks() -> Vec<String> {
    vec![
        "orphans".to_string(),
        "contradictions".to_string(),
        "stale".to_string(),
        "missing_edges".to_string(),
        "dirty_data".to_string(),
        "duplicates".to_string(),
    ]
}

// ─── Wiki Lint ──────────────────────────────────────────────────────────────

/// Count-only wiki hygiene for agent alerts/briefing — never runs skill-quality guards.
pub(crate) async fn wiki_hygiene_counts(
    server: &MemoryServer,
) -> Result<serde_json::Value, String> {
    let lint = handle_wiki_lint(
        server,
        WikiLintParams {
            path_prefix: Some("/wiki".to_string()),
            checks: vec![
                "orphans".to_string(),
                "stale".to_string(),
                "duplicates".to_string(),
            ],
            limit: 25,
            stale_days: 90,
            missing_edge_threshold: 0.72,
            contradiction_threshold: 0.75,
            include_skill_quality: false,
        },
    )
    .await?;
    let parsed: serde_json::Value = serde_json::from_str(&lint).unwrap_or_else(|_| json!({}));
    Ok(json!({
        "orphans": parsed.get("orphans").and_then(|v| v.as_array()).map(|rows| rows.len()).unwrap_or(0),
        "stale_nodes": parsed.get("stale_nodes").and_then(|v| v.as_array()).map(|rows| rows.len()).unwrap_or(0),
        "duplicates": parsed.get("duplicates").and_then(|v| v.as_array()).map(|rows| rows.len()).unwrap_or(0),
    }))
}

pub(crate) async fn handle_wiki_lint(
    server: &MemoryServer,
    params: WikiLintParams,
) -> Result<String, String> {
    let checks: Vec<String> = if params.checks.is_empty() {
        default_checks()
    } else {
        params.checks.clone()
    };
    let path_prefix = params.path_prefix.as_deref().unwrap_or("/wiki");
    let limit = params.limit.max(1).min(500);
    let stale_cutoff = Utc::now() - ChronoDuration::days(params.stale_days as i64);

    let mut nodes: Vec<(MemoryEntry, DbScope)> = Vec::new();
    let global_entries = server.with_global_store_read(|store| {
        store
            .list_by_path(path_prefix, limit, false)
            .map_err(|e| format!("wiki_lint global list: {e}"))
    })?;
    nodes.extend(
        global_entries
            .into_iter()
            .map(|entry| (entry, DbScope::Global)),
    );
    if server.has_project_db() {
        let project_entries = server.with_project_store_read(|store| {
            store
                .list_by_path(path_prefix, limit, false)
                .map_err(|e| format!("wiki_lint project list: {e}"))
        })?;
        nodes.extend(
            project_entries
                .into_iter()
                .map(|entry| (entry, DbScope::Project)),
        );
    }
    nodes.retain(|(entry, _)| is_user_facing_wiki_entry(entry));

    let mut orphans = Vec::new();
    let mut stale_nodes = Vec::new();
    let mut contradiction_candidates = Vec::new();
    let mut missing_edge_hints = Vec::new();
    let mut dirty_data = Vec::new();
    let mut duplicates = Vec::new();

    let mut all_edges = Vec::<memcore::MemoryEdge>::new();
    for (entry, scope) in &nodes {
        let edges = if *scope == DbScope::Global {
            server.with_global_store_read(|store| {
                store
                    .get_edges(&entry.id, "both", None)
                    .map_err(|e| format!("wiki_lint get edges: {e}"))
            })?
        } else {
            server.with_project_store_read(|store| {
                store
                    .get_edges(&entry.id, "both", None)
                    .map_err(|e| format!("wiki_lint get edges: {e}"))
            })?
        };
        if checks.iter().any(|check| check == "orphans") && edges.is_empty() {
            orphans.push(json!({
                "id": entry.id,
                "path": entry.path,
                "db": scope.as_str(),
            }));
        }
        all_edges.extend(edges);
        if checks.iter().any(|check| check == "stale") {
            if let Some(ts) = parse_rfc3339_utc(&entry.timestamp) {
                if ts < stale_cutoff
                    && !matches!(
                        entry.retention_policy.as_deref(),
                        Some("permanent" | "pinned")
                    )
                {
                    stale_nodes.push(json!({
                        "id": entry.id,
                        "path": entry.path,
                        "timestamp": entry.timestamp,
                        "db": scope.as_str(),
                    }));
                }
            }
        }
        if checks.iter().any(|check| check == "dirty_data")
            && (entry.text.contains("<think）")
                || entry.summary.contains("<think）")
                || entry.text.contains("<think>")
                || entry.summary.contains("<think>"))
        {
            dirty_data.push(json!({
                "id": entry.id,
                "path": entry.path,
                "issue": "think_tag_leak",
                "db": scope.as_str(),
            }));
        }
    }

    if checks
        .iter()
        .any(|check| check == "contradictions" || check == "missing_edges" || check == "duplicates")
    {
        // Cap pairwise comparison to avoid O(n²) blowup on large wikis.
        // At 500 nodes the nested loop produces ≤124,750 pairs, which is
        // fast enough for an interactive lint call.
        const PAIRWISE_NODE_CAP: usize = 500;
        let nodes_for_pairwise = &nodes[..nodes.len().min(PAIRWISE_NODE_CAP)];
        for i in 0..nodes_for_pairwise.len() {
            for j in (i + 1)..nodes_for_pairwise.len() {
                let left = &nodes_for_pairwise[i].0;
                let right = &nodes_for_pairwise[j].0;
                if nodes_for_pairwise[i].1 != nodes_for_pairwise[j].1 {
                    continue;
                }
                let similarity = token_cosine_similarity(&left.text, &right.text);
                if checks.iter().any(|check| check == "missing_edges")
                    && similarity > params.missing_edge_threshold
                    && !relation_exists(&all_edges, &left.id, &right.id, None)
                {
                    missing_edge_hints.push(json!({
                        "left_id": left.id,
                        "right_id": right.id,
                        "left_path": left.path,
                        "right_path": right.path,
                        "similarity": similarity,
                        "db": nodes_for_pairwise[i].1.as_str(),
                    }));
                }
                if checks.iter().any(|check| check == "duplicates") && similarity > 0.95 {
                    duplicates.push(json!({
                        "left_id": left.id,
                        "right_id": right.id,
                        "left_path": left.path,
                        "right_path": right.path,
                        "similarity": similarity,
                        "db": nodes_for_pairwise[i].1.as_str(),
                    }));
                }
                if checks.iter().any(|check| check == "contradictions") {
                    let contradiction = contradiction_score(&left.text, &right.text);
                    if contradiction > params.contradiction_threshold {
                        contradiction_candidates.push(json!({
                            "left_id": left.id,
                            "right_id": right.id,
                            "left_path": left.path,
                            "right_path": right.path,
                            "score": contradiction,
                            "db": nodes_for_pairwise[i].1.as_str(),
                        }));
                    }
                }
            }
        }
    }

    let skill_quality = if params.include_skill_quality {
        refresh_skill_quality_guards(server)?
    } else {
        json!({ "skipped": true })
    };
    append_wiki_log(
        server,
        "lint",
        &format!("{} | {} node(s)", path_prefix, nodes.len()),
    );

    serde_json::to_string(&json!({
        "path_prefix": path_prefix,
        "checks": checks,
        "orphans": orphans,
        "stale_nodes": stale_nodes,
        "contradiction_candidates": contradiction_candidates,
        "missing_edge_hints": missing_edge_hints,
        "dirty_data": dirty_data,
        "duplicates": duplicates,
        "skill_quality": skill_quality,
    }))
    .map_err(|e| format!("serialize wiki_lint: {e}"))
}
