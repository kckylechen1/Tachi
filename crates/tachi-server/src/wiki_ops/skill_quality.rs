use super::*;

const SKILL_QUALITY_PAIRWISE_CAP: usize = 500;

fn extract_skill_content(cap: &HubCapability) -> Option<String> {
    let def: Value = serde_json::from_str(&cap.definition).ok()?;
    def.get("content")
        .and_then(|v| v.as_str())
        .or_else(|| def.get("prompt").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
}

fn extract_skill_path(cap: &HubCapability) -> Option<String> {
    let def: Value = serde_json::from_str(&cap.definition).ok()?;
    def.get("skill_path")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn set_skill_quality_metadata(def: &mut Value, patch: Value) {
    if !def.is_object() {
        *def = json!({});
    }
    let Some(obj) = def.as_object_mut() else {
        return;
    };
    let quality = obj.entry("quality_guard").or_insert_with(|| json!({}));
    if !quality.is_object() {
        *quality = json!({});
    }
    if let (Some(target), Some(source)) = (quality.as_object_mut(), patch.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
}

fn latest_snapshot_for_skill(store: &mut MemoryStore, skill_path: &str) -> Option<MemoryEntry> {
    let root = format!("{}/distilled", skill_path.trim_end_matches('/'));
    store
        .list_by_path(&root, 50, false)
        .ok()?
        .into_iter()
        .max_by(|a, b| a.timestamp.cmp(&b.timestamp))
}

fn skill_activity_timestamp(cap: &HubCapability) -> Option<DateTime<Utc>> {
    cap.last_used
        .as_deref()
        .and_then(parse_rfc3339_utc)
        .or_else(|| parse_rfc3339_utc(&cap.updated_at))
}

#[derive(Clone)]
struct SkillQualitySnapshot {
    cap: HubCapability,
    def: Value,
    content: String,
    latest_snapshot: Option<MemoryEntry>,
}

fn run_skill_quality_guards_for_scope(
    server: &MemoryServer,
    scope: DbScope,
) -> Result<Value, String> {
    let mut snapshots: Vec<SkillQualitySnapshot> =
        server.with_store_for_scope_read(scope, |store| {
            let caps = store
                .hub_list(Some("skill"), false)
                .map_err(|e| format!("hub list skills: {e}"))?;
            let mut out = Vec::new();
            for cap in caps {
                if crate::builtins::is_retired_builtin_capability_id(&cap.id) {
                    continue;
                }
                let Some(content) = extract_skill_content(&cap) else {
                    continue;
                };
                let def: Value =
                    serde_json::from_str(&cap.definition).unwrap_or_else(|_| json!({}));
                let skill_path = extract_skill_path(&cap);
                let latest_snapshot = skill_path
                    .as_deref()
                    .and_then(|path| latest_snapshot_for_skill(store, path));
                out.push(SkillQualitySnapshot {
                    cap,
                    def,
                    content,
                    latest_snapshot,
                });
            }
            Ok(out)
        })?;
    snapshots.sort_by(|a, b| {
        skill_activity_timestamp(&b.cap)
            .cmp(&skill_activity_timestamp(&a.cap))
            .then_with(|| b.cap.uses.cmp(&a.cap.uses))
            .then_with(|| a.cap.id.cmp(&b.cap.id))
    });

    let now = Utc::now();
    let mut merge_map: HashMap<String, Vec<Value>> = HashMap::new();
    let mut graph_edges = Vec::<memcore::MemoryEdge>::new();
    let pairwise_evaluated_skills = snapshots.len().min(SKILL_QUALITY_PAIRWISE_CAP);
    let pairwise_skipped_skills = snapshots.len().saturating_sub(pairwise_evaluated_skills);
    let snapshots_for_pairwise = &snapshots[..pairwise_evaluated_skills];

    for i in 0..snapshots_for_pairwise.len() {
        for j in (i + 1)..snapshots_for_pairwise.len() {
            let similarity = token_cosine_similarity(
                &snapshots_for_pairwise[i].content,
                &snapshots_for_pairwise[j].content,
            );
            if similarity > 0.92 {
                merge_map
                    .entry(snapshots_for_pairwise[i].cap.id.clone())
                    .or_default()
                    .push(json!({
                        "skill_id": snapshots_for_pairwise[j].cap.id,
                        "similarity": similarity,
                    }));
                merge_map
                    .entry(snapshots_for_pairwise[j].cap.id.clone())
                    .or_default()
                    .push(json!({
                        "skill_id": snapshots_for_pairwise[i].cap.id,
                        "similarity": similarity,
                    }));

                if let (Some(left), Some(right)) = (
                    snapshots_for_pairwise[i].latest_snapshot.as_ref(),
                    snapshots_for_pairwise[j].latest_snapshot.as_ref(),
                ) {
                    graph_edges.push(memcore::MemoryEdge {
                        source_id: left.id.clone(),
                        target_id: right.id.clone(),
                        relation: "merge_hint".to_string(),
                        weight: similarity.clamp(0.0, 1.0),
                        metadata: json!({
                            "source": "skill_quality_guard",
                            "type": "merge_hint",
                            "similarity": similarity,
                        }),
                        created_at: now.to_rfc3339(),
                        valid_from: String::new(),
                        valid_to: None,
                    });
                }
            }
        }
    }

    if !graph_edges.is_empty() {
        let edges = graph_edges.clone();
        let _ = server.with_store_for_scope(scope, |store| {
            for edge in &edges {
                // tachi#1646: `merge_hint` edges are a token-similarity
                // heuristic Tachi computed itself.
                store
                    .add_edge_with_provenance(
                        edge,
                        &memcore::db::EdgeProvenance {
                            authority: Some(memcore::db::EdgeAuthority::DerivedHeuristic),
                            ..Default::default()
                        },
                    )
                    .map_err(|e| format!("skill graph edge: {e}"))?;
            }
            Ok(())
        });
    }

    let pagerank = local_pagerank(&graph_edges, 0.85);
    let mut archived_skills = Vec::<String>::new();
    let mut changed_caps = Vec::<HubCapability>::new();

    for snapshot in &mut snapshots {
        let merge_hints = merge_map.get(&snapshot.cap.id).cloned().unwrap_or_default();
        let pagerank_score = snapshot
            .latest_snapshot
            .as_ref()
            .and_then(|memory| pagerank.get(&memory.id).copied())
            .unwrap_or(0.0);
        let mut new_def = snapshot.def.clone();
        set_skill_quality_metadata(
            &mut new_def,
            json!({
                "merge_hints": merge_hints,
                "pagerank": pagerank_score,
                "updated_at": now.to_rfc3339(),
            }),
        );

        let stale_cutoff = now - ChronoDuration::days(30);
        let should_archive = snapshot.cap.avg_rating < 0.3
            && snapshot
                .cap
                .last_used
                .as_deref()
                .and_then(parse_rfc3339_utc)
                .map(|ts| ts < stale_cutoff)
                .unwrap_or(false);
        if should_archive {
            if !new_def.is_object() {
                new_def = json!({});
            }
            if let Some(obj) = new_def.as_object_mut() {
                let policy = obj.entry("policy").or_insert_with(|| json!({}));
                if !policy.is_object() {
                    *policy = json!({});
                }
                if let Some(policy_obj) = policy.as_object_mut() {
                    policy_obj.insert("visibility".to_string(), json!("hidden"));
                }
            }
            set_skill_quality_metadata(
                &mut new_def,
                json!({
                    "status": "archived",
                    "archived_reason": "stale_low_rating",
                    "archived_at": now.to_rfc3339(),
                }),
            );
            archived_skills.push(snapshot.cap.id.clone());
        }

        let serialized = serde_json::to_string(&new_def)
            .map_err(|e| format!("serialize skill quality def: {e}"))?;
        if serialized != snapshot.cap.definition {
            let mut updated = snapshot.cap.clone();
            updated.definition = serialized;
            changed_caps.push(updated);
        }
    }

    if !changed_caps.is_empty() {
        let caps_to_store = changed_caps.clone();
        server.with_store_for_scope(scope, |store| {
            for cap in &caps_to_store {
                store
                    .hub_register(cap)
                    .map_err(|e| format!("hub register skill quality update: {e}"))?;
            }
            Ok(())
        })?;

        for cap in &changed_caps {
            if capability_callable(cap) && should_expose_skill_tool(cap) {
                let _ = server.register_skill_tool(cap);
            } else {
                let _ = server.unregister_skill_tool(&cap.id);
            }
        }
    }

    Ok(json!({
        "scope": scope.as_str(),
        "archived_skills": archived_skills,
        "merge_hints": merge_map,
        "pairwise_cap": SKILL_QUALITY_PAIRWISE_CAP,
        "pairwise_evaluated_skills": pairwise_evaluated_skills,
        "pairwise_skipped_skills": pairwise_skipped_skills,
        "pagerank": pagerank,
        "updated_caps": changed_caps.iter().map(|cap| cap.id.clone()).collect::<Vec<_>>(),
    }))
}

pub(crate) fn refresh_skill_quality_guards(server: &MemoryServer) -> Result<Value, String> {
    let global = run_skill_quality_guards_for_scope(server, DbScope::Global)?;
    let project = if server.has_project_db() {
        Some(run_skill_quality_guards_for_scope(
            server,
            DbScope::Project,
        )?)
    } else {
        None
    };
    Ok(json!({"global": global, "project": project}))
}
