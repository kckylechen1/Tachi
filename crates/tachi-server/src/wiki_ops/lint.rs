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

fn store_db_label(store_ref: &StoreRef) -> &'static str {
    match store_ref {
        StoreRef::LegacyGlobal => "global",
        StoreRef::BoundProject | StoreRef::NamedProject { .. } => "project",
    }
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
            // Hot path (every briefing/alerts call) — never a surprise
            // writer. See `WikiLintParams::persist_stale` doc.
            persist_stale: false,
            project: None,
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
    let plan = WikiReadPlan::from_project(params.project.as_deref())?;
    let nodes = list_wiki_entries_for_plan(server, &plan, path_prefix, limit)?;

    let mut orphans = Vec::new();
    let mut stale_nodes = Vec::new();
    let mut contradiction_candidates = Vec::new();
    let mut missing_edge_hints = Vec::new();
    let mut dirty_data = Vec::new();
    let mut duplicates = Vec::new();

    let mut all_edges = Vec::<memcore::MemoryEdge>::new();
    for node in &nodes {
        let entry = &node.entry;
        let store_ref = &node.store;
        let edges = with_wiki_store_read(server, store_ref, |store| {
            store
                .get_edges(&entry.id, "both", None)
                .map_err(|e| format!("wiki_lint get edges: {e}"))
        })?;
        if checks.iter().any(|check| check == "orphans") && edges.is_empty() {
            orphans.push(json!({
                "id": entry.id,
                "path": entry.path,
                "db": store_db_label(store_ref),
                "store": store_ref,
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
                        "db": store_db_label(store_ref),
                        "store": store_ref,
                        "reason": "retention_age",
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
                "db": store_db_label(store_ref),
                "store": store_ref,
            }));
        }
    }

    // #1072 RED case 4: "permanent/pinned does not exempt [content] from
    // semantic staleness" — canon doc §7. This pass is deliberately
    // independent of `retention_policy` (unlike the retention-age pass
    // above, which explicitly exempts permanent/pinned): any node targeted
    // by a `contradicts` or `supersedes` edge from another node is flagged
    // regardless of retention policy. `all_edges` is fully populated by now
    // (accumulated across every node's "both"-direction query above).
    //
    // Scoping note (documented, not hidden — see `knowledge_artifact`
    // module doc): this covers the "supersedes/contradicts edges" trigger
    // from canon doc §7's required-behavior list. Full external trusted-doc
    // blob-SHA drift detection ("source revision drift, trusted-ref
    // changes") is a separate leaf's worth of work (needs #1002's
    // `CanonicalDocRefV1` resolver wired into wiki writes).
    let mut semantic_stale_to_persist: Vec<(MemoryEntry, StoreRef)> = Vec::new();
    if checks.iter().any(|check| check == "stale") {
        let already_stale: HashSet<String> = stale_nodes
            .iter()
            .filter_map(|node| node.get("id").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        for node in &nodes {
            let entry = &node.entry;
            let store_ref = &node.store;
            if already_stale.contains(entry.id.as_str()) {
                continue;
            }
            let contradicted_or_superseded = all_edges.iter().any(|edge| {
                edge.target_id == entry.id
                    && matches!(edge.relation.as_str(), "contradicts" | "supersedes")
            });
            if contradicted_or_superseded {
                stale_nodes.push(json!({
                    "id": entry.id,
                    "path": entry.path,
                    "timestamp": entry.timestamp,
                    "db": store_db_label(store_ref),
                    "store": store_ref,
                    "reason": "semantic_stale_contradicted_or_superseded",
                }));
                // #1072 fix-round (#1215 BUG 6): "lint appends a diagnostic
                // row only; persisted lifecycle stays active and
                // retrievable." Canon doc §7's required behavior is
                // retrieval EXCLUSION, not just a report. Queue this node
                // for a real `metadata.lifecycle = "stale"` write-back — see
                // `WikiLintParams::persist_stale` doc for why this is
                // opt-in, and only when the entry isn't already gated
                // (`pending_review`/etc — downgrading FROM active TO stale
                // is this pass's job; it must not clobber a stronger
                // existing gate like `rejected`).
                if params.persist_stale
                    && derive_wiki_lifecycle(&entry.metadata, &entry.path)
                        == WikiLifecycleV1::Active
                {
                    semantic_stale_to_persist.push((entry.clone(), store_ref.clone()));
                }
            }
        }
    }
    let mut stale_persist_errors: Vec<String> = Vec::new();
    for (mut entry, store_ref) in semantic_stale_to_persist {
        let Some(obj) = entry.metadata.as_object_mut() else {
            continue;
        };
        obj.insert(
            "lifecycle".to_string(),
            json!(WikiLifecycleV1::Stale.as_str()),
        );
        obj.insert(
            "stale_reason".to_string(),
            json!("semantic_stale_contradicted_or_superseded"),
        );
        let write_result = with_wiki_store(server, &store_ref, |store| {
                store
                    .upsert(&entry)
                    .map_err(|e| format!("wiki_lint stale persist: {e}"))
            });
        if let Err(err) = write_result {
            tracing::warn!(
                "wiki_lint persist_stale write failed for {}: {err}",
                entry.id
            );
            stale_persist_errors.push(err);
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
                let left = &nodes_for_pairwise[i].entry;
                let right = &nodes_for_pairwise[j].entry;
                let store_ref = &nodes_for_pairwise[i].store;
                if store_ref != &nodes_for_pairwise[j].store {
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
                        "db": store_db_label(store_ref),
                        "store": store_ref,
                    }));
                }
                if checks.iter().any(|check| check == "duplicates") && similarity > 0.95 {
                    duplicates.push(json!({
                        "left_id": left.id,
                        "right_id": right.id,
                        "left_path": left.path,
                        "right_path": right.path,
                        "similarity": similarity,
                        "db": store_db_label(store_ref),
                        "store": store_ref,
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
                            "db": store_db_label(store_ref),
                            "store": store_ref,
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
        "project": params.project,
        "stores": stores_for_wiki_plan(server, &plan),
        "checks": checks,
        "orphans": orphans,
        "stale_nodes": stale_nodes,
        "contradiction_candidates": contradiction_candidates,
        "missing_edge_hints": missing_edge_hints,
        "dirty_data": dirty_data,
        "duplicates": duplicates,
        "skill_quality": skill_quality,
        "stale_persisted": params.persist_stale,
        "stale_persist_errors": stale_persist_errors,
    }))
    .map_err(|e| format!("serialize wiki_lint: {e}"))
}
