use super::*;
use crate::utils::stable_hash;
use std::collections::hash_map::Entry;

const REDACTED_SECRET: &str = "[REDACTED]";
const REINFORCEMENT_MIN_SIMILARITY: f64 = 0.75;
const REINFORCEMENT_DUPLICATE_SIMILARITY: f64 = 0.95;

fn should_enqueue_enrichment(entry: &MemoryEntry) -> bool {
    entry.importance >= 0.5 || entry.vector.is_some()
}

fn path_root(path: &str) -> &str {
    path.trim_matches('/').split('/').next().unwrap_or("")
}

fn is_newer_than(new_ts: &str, old_ts: &str) -> bool {
    let new = chrono::DateTime::parse_from_rfc3339(new_ts);
    let old = chrono::DateTime::parse_from_rfc3339(old_ts);
    match (new, old) {
        (Ok(new), Ok(old)) => new > old,
        _ => new_ts > old_ts,
    }
}

fn should_supersede(
    new_entry: &MemoryEntry,
    old_entry: &MemoryEntry,
    shared_count: usize,
    symbolic_score: f64,
) -> bool {
    matches!(new_entry.category.as_str(), "fact" | "preference")
        && matches!(old_entry.category.as_str(), "fact" | "preference")
        && is_newer_than(&new_entry.timestamp, &old_entry.timestamp)
        && shared_count >= 2
        && (new_entry.topic == old_entry.topic || symbolic_score > 0.3)
        && path_root(&new_entry.path) == path_root(&old_entry.path)
}

fn vector_similarity_between(new_entry: &MemoryEntry, old_entry: &MemoryEntry) -> Option<f64> {
    let new_vec = new_entry.vector.as_deref()?;
    let old_vec = old_entry.vector.as_deref()?;
    if new_vec.is_empty() || new_vec.len() != old_vec.len() {
        return None;
    }
    Some(memory_core::scorer::cosine_similarity(new_vec, old_vec).clamp(0.0, 1.0))
}

fn should_reinforce(
    new_entry: &MemoryEntry,
    old_entry: &MemoryEntry,
    shared_count: usize,
    similarity: f64,
    supersedes: bool,
) -> bool {
    !supersedes
        && shared_count > 0
        && matches!(new_entry.category.as_str(), "fact" | "preference")
        && matches!(old_entry.category.as_str(), "fact" | "preference")
        && path_root(&new_entry.path) == path_root(&old_entry.path)
        && (REINFORCEMENT_MIN_SIMILARITY..REINFORCEMENT_DUPLICATE_SIMILARITY)
            .contains(&similarity)
}

fn confidence_increment(similarity: f64) -> f64 {
    (0.1 * similarity).clamp(0.0, 0.1)
}

fn apply_confidence_reinforcement(
    store: &mut MemoryStore,
    reinforced_id: &str,
    increment: f64,
    reinforced_at: &str,
) -> Result<(), String> {
    store
        .connection()
        .execute(
            r#"UPDATE memories
               SET metadata = json_set(
                   CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                   '$.confidence',
                   min(
                       1.0,
                       coalesce(
                           CAST(json_extract(CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END, '$.confidence') AS REAL),
                           importance
                       ) + ?1
                   ),
                   '$.confidence_reinforced_at', ?2
               ),
               updated_at = ?2
               WHERE id = ?3"#,
            rusqlite::params![increment, reinforced_at, reinforced_id],
        )
        .map_err(|e| format!("update confidence reinforcement: {e}"))?;
    Ok(())
}

pub(crate) fn scrub_secrets(text: &str) -> (String, usize) {
    static REGEXES: std::sync::OnceLock<Vec<regex::Regex>> = std::sync::OnceLock::new();
    let regexes = REGEXES.get_or_init(|| {
        [
            r#"(?i)(Authorization\s*:\s*Bearer\s+)([^\s`'\"]+)"#,
            r#"(?i)((?:api[_-]?key|token|secret|password)\s*[:=]\s*)([^\s`'\"]{8,})"#,
            r"(?i)\b(sk-[A-Za-z0-9_-]{20,})\b",
            r"(?i)\b(voy-[A-Za-z0-9_-]{20,})\b",
            r"(?i)\b(xox[baprs]-[A-Za-z0-9-]{20,})\b",
            r"(?i)\b(gh[pousr]_[A-Za-z0-9_]{20,})\b",
            r"(?i)\b(AKIA[0-9A-Z]{16})\b",
        ]
        .iter()
        .filter_map(|pattern| regex::Regex::new(pattern).ok())
        .collect()
    });

    let mut redactions = 0usize;
    let mut out = text.to_string();
    for re in regexes {
        let matches = re.find_iter(&out).count();
        if matches == 0 {
            continue;
        }
        redactions += matches;
        out = re
            .replace_all(&out, |caps: &regex::Captures<'_>| {
                if caps.len() > 2 {
                    format!("{}{}", &caps[1], REDACTED_SECRET)
                } else {
                    REDACTED_SECRET.to_string()
                }
            })
            .to_string();
    }
    (out, redactions)
}

pub(crate) async fn handle_save_memory(
    server: &MemoryServer,
    params: SaveMemoryParams,
) -> Result<String, String> {
    let original_text = params.text;
    let (safe_text, secret_redactions) = scrub_secrets(&original_text);
    if !params.force && memory_core::is_noise_text(&safe_text) {
        return serde_json::to_string(&json!({
            "saved": false,
            "noise": true,
            "reason": "Text detected as noise (greeting, denial, or meta-question). Not saved.",
            "hint": "Retry with force=true if this is intentional content.",
        }))
        .map_err(|e| format!("Failed to serialize: {}", e));
    }

    // Capture gate (Branch #4): validate domain, path bucket, min-chars, and
    // markdown-dump heuristic. Default mode = Warn (annotate response, write
    // proceeds). TACHI_CAPTURE_GATE=enforce switches to hard rejection.
    let gate_mode = crate::capture_gate::GateMode::from_env();
    let gate_decision = crate::capture_gate::evaluate(
        &crate::capture_gate::GateInput::new(
            &safe_text,
            &params.path,
            params.domain.as_deref(),
            params.force,
        ),
        gate_mode,
    );
    if !gate_decision.accept {
        return serde_json::to_string(&json!({
            "saved": false,
            "rejected_by": "capture_gate",
            "mode": gate_decision.mode,
            "violations": gate_decision.violations,
            "hint": "Set TACHI_CAPTURE_GATE=warn to downgrade these to warnings, or pass force=true on save.",
        }))
        .map_err(|e| format!("Failed to serialize: {}", e));
    }
    let gate_warnings = if gate_decision.violations.is_empty() {
        None
    } else {
        Some(gate_decision.violations.clone())
    };

    let requested_id = params.id.clone();
    let id = requested_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let timestamp = params
        .timestamp
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    let requested_scope = params.scope.clone();
    let named_project = params.project.clone();
    let (target_db, warning) = if named_project.is_some() {
        (DbScope::Project, None) // Will use named project below
    } else {
        server.resolve_write_scope(&requested_scope)
    };
    let existing_revision = if requested_id.is_some() {
        let lookup = |store: &mut MemoryStore| {
            store
                .get(&id)
                .map(|entry| entry.map(|entry| entry.revision))
                .map_err(|e| format_save_error(server, target_db, named_project.as_deref(), &e))
        };
        if let Some(ref project_name) = named_project {
            server.with_named_project_store_read(project_name, lookup)?
        } else {
            server.with_store_for_scope_read(target_db, lookup)?
        }
    } else {
        None
    };
    let enrichment_revision = existing_revision.unwrap_or(0) + 1;

    let summary = params.summary;
    let needs_summary = summary.is_empty();
    let needs_embedding = params.vector.is_none();
    let path = params.path;
    let category = params.category;
    let topic = params.topic;
    let metadata = crate::provenance::inject_provenance(
        server,
        params.metadata.unwrap_or_else(|| json!({})),
        "save_memory",
        "memory_write",
        Some(requested_scope.as_str()),
        target_db,
        json!({
            "path": path.clone(),
            "category": category.clone(),
            "topic": topic.clone(),
        }),
    );

    let importance = params.importance.clamp(0.0, 1.0);
    let keywords = params.keywords;

    let entry = MemoryEntry {
        id: id.clone(),
        path,
        summary,
        text: safe_text,
        importance,
        timestamp: timestamp.clone(),
        category,
        topic,
        keywords,
        persons: params.persons,
        entities: params.entities,
        location: params.location,
        source: "mcp".to_string(),
        scope: requested_scope,
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata,
        vector: params.vector,
        retention_policy: params.retention_policy,
        domain: params.domain,
    };

    if let Some(ref project_name) = named_project {
        server.with_named_project_store(project_name, |store| {
            store
                .upsert(&entry)
                .map_err(|e| format_save_error(server, target_db, Some(project_name), &e))
        })?;
    } else {
        server.with_store_for_scope(target_db, |store| {
            store
                .upsert(&entry)
                .map_err(|e| format_save_error(server, target_db, None, &e))
        })?;
    }

    // Queue enrichment (embedding + summary) via the batcher instead of
    // spawning a per-item task. The batcher accumulates items and calls
    // the Voyage API in batch (up to 128 per request), dramatically
    // reducing API calls when the agent saves multiple memories in sequence.
    if (needs_embedding || needs_summary) && should_enqueue_enrichment(&entry) {
        server.enqueue_enrichment(super::EnrichmentItem {
            id: id.clone(),
            text: entry.text.clone(),
            summary: entry.summary.clone(),
            keywords: entry.keywords.clone(),
            needs_embedding,
            needs_summary,
            target_db,
            named_project: params.project.clone(),
            db_path: None,
            foundry_agent_id: None,
            foundry_path_prefix: None,
            revision: enrichment_revision,
        });
    }

    let mut response = serde_json::Map::new();
    response.insert("id".into(), json!(id));
    response.insert("timestamp".into(), json!(timestamp));
    response.insert("db".into(), json!(target_db.as_str()));
    let status = if (needs_embedding || needs_summary) && should_enqueue_enrichment(&entry) {
        "saved (enrichment pending)"
    } else {
        "saved (enrichment skipped by policy)"
    };
    response.insert("status".into(), json!(status));
    if let Some(warning) = warning {
        response.insert("warning".into(), json!(warning));
    }
    if let Some(violations) = gate_warnings {
        response.insert("capture_gate_warnings".into(), json!(violations));
    }
    if secret_redactions > 0 {
        response.insert("secret_redactions".into(), json!(secret_redactions));
        response.insert(
            "secret_redaction_warning".into(),
            json!("Potential secrets were redacted before persistence."),
        );
    }

    if params.auto_link && !entry.entities.is_empty() {
        let auto_link_server = server.clone();
        let auto_link_id = id.clone();
        let auto_link_entry = entry.clone();
        let auto_link_entities = entry.entities.clone();
        let auto_link_named_project = params.project.clone();
        let auto_link_target_db = target_db;

        tokio::spawn(async move {
            for entity in &auto_link_entities {
                let query = entity.clone();
                let search_action = |store: &mut MemoryStore| {
                    store
                        .search(
                            &query,
                            Some(memory_core::SearchOptions {
                                top_k: 5,
                                // Auto-link is a write-side side effect that
                                // probes related memories by entity. It must
                                // NOT bump access stats: doing so inflates
                                // ACT-R frequency / blocks `access_count = 0`
                                // prune / biases promotion ranking on entries
                                // the user never read.
                                record_access: false,
                                ..Default::default()
                            }),
                        )
                        .map_err(|e| format!("{}", e))
                };

                let search_res = if let Some(ref p) = auto_link_named_project {
                    auto_link_server.with_named_project_store_read(p, search_action)
                } else {
                    auto_link_server.with_store_for_scope_read(auto_link_target_db, search_action)
                };

                if let Ok(results) = search_res {
                    for result in results {
                        if result.entry.id == auto_link_id {
                            continue;
                        }
                        let shared: Vec<String> = result
                            .entry
                            .entities
                            .iter()
                            .filter(|e| auto_link_entities.contains(e))
                            .cloned()
                            .collect();
                        if !shared.is_empty() {
                            let now = chrono::Utc::now().to_rfc3339();
                            let vector_similarity =
                                vector_similarity_between(&auto_link_entry, &result.entry);
                            let supersedes = should_supersede(
                                &auto_link_entry,
                                &result.entry,
                                shared.len(),
                                result.score.symbolic,
                            );
                            let reinforces = vector_similarity.is_some_and(|similarity| {
                                should_reinforce(
                                    &auto_link_entry,
                                    &result.entry,
                                    shared.len(),
                                    similarity,
                                    supersedes,
                                )
                            });
                            let relation = if supersedes {
                                "supersedes"
                            } else if reinforces {
                                "reinforces"
                            } else {
                                "related_to"
                            };
                            let weight = if supersedes {
                                0.9
                            } else if reinforces {
                                vector_similarity.unwrap_or(0.0)
                            } else {
                                0.5
                            };
                            let edge = memory_core::MemoryEdge {
                                source_id: auto_link_id.clone(),
                                target_id: result.entry.id.clone(),
                                relation: relation.to_string(),
                                weight,
                                metadata: json!({
                                    "auto_link": true,
                                    "shared_entities": shared,
                                    "similarity": vector_similarity,
                                    "confidence_increment": reinforces.then(|| confidence_increment(weight)),
                                }),
                                created_at: now.clone(),
                                valid_from: String::new(),
                                // New supersedes/related_to edges are open-ended.
                                // `get_edges` filters with `valid_to IS NULL OR valid_to > now`,
                                // so setting valid_to = Some(now) at creation time would
                                // immediately expire the edge and hide the supersession
                                // from graph readers, repair, and explainability. Edges
                                // are only closed/expired when the supersession is
                                // explicitly reversed.
                                valid_to: None,
                            };
                            let save_edge_action = |store: &mut MemoryStore| {
                                store.add_edge(&edge).map_err(|e| format!("{}", e))?;
                                if supersedes {
                                    store
                                        .connection()
                                        .execute(
                                            "UPDATE memories SET superseded_by = ?1, updated_at = ?2 WHERE id = ?3",
                                            rusqlite::params![auto_link_id, now, result.entry.id],
                                        )
                                        .map_err(|e| format!("{e}"))?;
                                } else if reinforces {
                                    apply_confidence_reinforcement(
                                        store,
                                        &result.entry.id,
                                        confidence_increment(weight),
                                        &now,
                                    )?;
                                }
                                Ok(())
                            };
                            let _ = if let Some(ref p) = auto_link_named_project {
                                auto_link_server.with_named_project_store(p, save_edge_action)
                            } else {
                                auto_link_server
                                    .with_store_for_scope(auto_link_target_db, save_edge_action)
                            };
                        }
                    }
                }
            }
        });
        response.insert("auto_link".into(), json!("pending"));
    }

    serde_json::to_string(&serde_json::Value::Object(response))
        .map_err(|e| format!("Failed to serialize response: {}", e))
}

/// Low-friction shortcut over `handle_save_memory`. Infers `path`, `category`,
/// and `importance` so callers only need to pass `text` (and optionally
/// `tags`). Internally constructs `SaveMemoryParams` and delegates, so noise
/// filter, capture gate, provenance injection, auto-link, and the enrichment
/// batcher all run identically to a direct save_memory call.
pub(crate) async fn handle_remember(
    server: &MemoryServer,
    params: RememberParams,
) -> Result<String, String> {
    // Default path = /notes/{YYYY-MM-DD} so quick captures land in a
    // predictable, browsable bucket without forcing the caller to choose one.
    let inferred_path = params.path.unwrap_or_else(|| {
        let date = Utc::now().format("%Y-%m-%d");
        format!("/notes/{date}")
    });

    let save_params = SaveMemoryParams {
        text: params.text,
        summary: params.summary,
        path: inferred_path,
        importance: params.importance.unwrap_or(0.6).clamp(0.0, 1.0),
        category: params.category.unwrap_or_else(|| "fact".to_string()),
        topic: params.topic,
        keywords: params.tags,
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        scope: params.scope.unwrap_or_else(|| "project".to_string()),
        vector: None,
        id: None,
        force: params.force,
        auto_link: true,
        project: params.project,
        retention_policy: params.retention_policy,
        domain: params.domain,
        timestamp: None,
        metadata: Some(json!({ "shortcut": "remember" })),
    };

    handle_save_memory(server, save_params).await
}

pub(super) async fn search_memory_rows(
    server: &MemoryServer,
    mut params: SearchMemoryParams,
) -> Result<Vec<serde_json::Value>, String> {
    if !params
        .path_prefix
        .as_deref()
        .is_some_and(|prefix| prefix == "/wiki" || prefix.starts_with("/wiki/"))
        && memory_core::should_skip_query(&params.query)
    {
        return Ok(vec![]);
    }
    let top_k = params.top_k.max(1);
    params.top_k = top_k;

    let named_project_vec_available = if let Some(ref project_name) = params.project {
        server
            .with_named_project_store_read(project_name, |store| Ok(store.vec_available))
            .unwrap_or(false)
    } else {
        false
    };

    if params.query_vec.is_none()
        && (server.global_vec_available
            || server.project_vec_available
            || named_project_vec_available)
    {
        match server.llm.embed_voyage(&params.query, "query").await {
            Ok(query_vec) => {
                params.query_vec = Some(query_vec);
            }
            Err(e) => {
                eprintln!(
                    "[search_memory] query embedding failed, falling back to lexical-only search: {e}"
                );
            }
        }
    }

    let pipeline_enabled = server.pipeline_enabled;

    let mut combined_results: Vec<(memory_core::SearchResult, DbScope)> = Vec::new();

    if let Some(ref project_name) = params.project {
        let project_results = server.with_named_project_store_read(project_name, |store| {
            let vec_avail = store.vec_available;
            let project_opts = params.to_search_options(vec_avail);
            store
                .search(&params.query, Some(project_opts))
                .map_err(|e| format!("Search failed in project DB '{}': {}", project_name, e))
        })?;
        combined_results.extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
    } else {
        let global_opts = params.to_search_options(server.global_vec_available);
        let global_results = server.with_global_store_read(|store| {
            store
                .search(&params.query, Some(global_opts))
                .map_err(|e| format!("Search failed in global DB: {}", e))
        })?;
        combined_results.extend(global_results.into_iter().map(|r| (r, DbScope::Global)));

        if server.has_project_db() {
            let project_opts = params.to_search_options(server.project_vec_available);
            let project_results = server.with_project_store_read(|store| {
                store
                    .search(&params.query, Some(project_opts))
                    .map_err(|e| format!("Search failed in project DB: {}", e))
            })?;
            combined_results.extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
        }
    }

    apply_guide_context_boosts(
        &mut combined_results,
        params.file_context.as_deref(),
        params.error_context.as_deref(),
    );

    combined_results.sort_by(|a, b| {
        b.0.score
            .final_score
            .partial_cmp(&a.0.score.final_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    combined_results = dedup_search_results(combined_results, top_k);

    let mut seen_ids = HashSet::new();
    let mut deduped_results: Vec<(memory_core::SearchResult, DbScope)> = Vec::new();
    for (result, db_scope) in combined_results {
        if seen_ids.insert(result.entry.id.clone()) {
            deduped_results.push((result, db_scope));
        }
        if deduped_results.len() >= top_k {
            break;
        }
    }

    // Sandbox filtering: if agent_role is specified, filter out denied entries
    if let Some(ref role) = params.agent_role {
        deduped_results.retain(|(result, db_scope)| {
            let allowed = match db_scope {
                DbScope::Global => server.with_global_store_read(|store| {
                    store
                        .check_sandbox_access(role, &result.entry.path, "read")
                        .map(|(allowed, _)| allowed)
                        .map_err(|e| format!("{e}"))
                }),
                DbScope::Project => {
                    if let Some(ref p) = params.project {
                        server.with_named_project_store_read(p, |store| {
                            store
                                .check_sandbox_access(role, &result.entry.path, "read")
                                .map(|(allowed, _)| allowed)
                                .map_err(|e| format!("{e}"))
                        })
                    } else {
                        server.with_project_store_read(|store| {
                            store
                                .check_sandbox_access(role, &result.entry.path, "read")
                                .map(|(allowed, _)| allowed)
                                .map_err(|e| format!("{e}"))
                        })
                    }
                }
            };
            allowed.unwrap_or(true)
        });
    }

    let mut output: Vec<serde_json::Value> = deduped_results
        .iter()
        .map(|(r, db_scope)| slim_search_result(r, *db_scope))
        .collect();

    if pipeline_enabled {
        let mut existing_ids: HashSet<String> = deduped_results
            .iter()
            .map(|(r, _)| r.entry.id.clone())
            .collect();

        if server.has_project_db() {
            let project_rules = server.with_project_store_read(|store| {
                Ok(store
                    .list_by_path("/behavior/global_rules", 50, false)
                    .unwrap_or_default())
            })?;
            for rule in project_rules {
                if !is_active_global_rule(&rule) {
                    continue;
                }
                if !existing_ids.insert(rule.id.clone()) {
                    continue;
                }
                output.push(slim_l0_rule(&rule, DbScope::Project));
            }
        }

        let global_rules = server.with_global_store_read(|store| {
            Ok(store
                .list_by_path("/behavior/global_rules", 50, false)
                .unwrap_or_default())
        })?;
        for rule in global_rules {
            if !is_active_global_rule(&rule) {
                continue;
            }
            if !existing_ids.insert(rule.id.clone()) {
                continue;
            }
            output.push(slim_l0_rule(&rule, DbScope::Global));
        }
    }

    Ok(output)
}

fn dedup_search_results(
    results: Vec<(memory_core::SearchResult, DbScope)>,
    top_k: usize,
) -> Vec<(memory_core::SearchResult, DbScope)> {
    let mut by_subject: HashMap<String, (memory_core::SearchResult, DbScope)> = HashMap::new();
    let mut passthrough = Vec::new();

    for (result, db_scope) in results {
        let Some(key) = dedup_subject_key(&result.entry) else {
            passthrough.push((result, db_scope));
            continue;
        };
        match by_subject.entry(key) {
            Entry::Vacant(slot) => {
                slot.insert((result, db_scope));
            }
            Entry::Occupied(mut slot) => {
                if should_replace_dedup_result(&result, &slot.get().0) {
                    slot.insert((result, db_scope));
                }
            }
        }
    }

    let mut out = by_subject
        .into_values()
        .chain(passthrough)
        .collect::<Vec<_>>();
    out.sort_by(|a, b| {
        b.0.score
            .final_score
            .partial_cmp(&a.0.score.final_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(top_k.saturating_mul(3).max(top_k).max(1));
    out
}

fn should_replace_dedup_result(
    candidate: &memory_core::SearchResult,
    current: &memory_core::SearchResult,
) -> bool {
    canonical_rank(&candidate.entry)
        .cmp(&canonical_rank(&current.entry))
        .then_with(|| candidate.entry.timestamp.cmp(&current.entry.timestamp))
        .then_with(|| {
            candidate
                .score
                .final_score
                .partial_cmp(&current.score.final_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .is_gt()
}

fn canonical_rank(entry: &MemoryEntry) -> u8 {
    if entry.source.eq_ignore_ascii_case("foundry_distill") {
        return 2;
    }
    if entry
        .metadata
        .get("wiki")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || entry.domain.as_deref() == Some("wiki")
        || entry.category.eq_ignore_ascii_case("wiki")
    {
        4
    } else if entry.is_guide() {
        3
    } else {
        1
    }
}

fn dedup_subject_key(entry: &MemoryEntry) -> Option<String> {
    let path = entry.path.trim();
    if path == "/wiki/_log" {
        return Some("wiki-log".to_string());
    }
    if path.starts_with("/wiki/") {
        return Some(format!("wiki:path:{}", path));
    }
    let topic = entry.topic.trim().to_ascii_lowercase();
    if !topic.is_empty()
        && (entry.source.eq_ignore_ascii_case("foundry_distill") || entry.is_guide())
    {
        return Some(format!("distill:{topic}:{}", entry.entities.join("|")));
    }
    None
}

fn search_score(row: &serde_json::Value) -> f64 {
    row.get("score")
        .and_then(|score| score.get("final"))
        .and_then(serde_json::Value::as_f64)
        .or_else(|| row.get("relevance").and_then(serde_json::Value::as_f64))
        .unwrap_or(0.0)
}

fn apply_guide_context_boosts(
    results: &mut [(memory_core::SearchResult, DbScope)],
    file_context: Option<&str>,
    error_context: Option<&str>,
) {
    if file_context.is_none() && error_context.is_none() {
        return;
    }
    for (result, _) in results.iter_mut() {
        let boost = guide_context_boost(&result.entry, file_context, error_context);
        if boost > 0.0 {
            result.score.final_score = (result.score.final_score + boost).min(1.0);
        }
    }
}

fn guide_context_boost(
    entry: &MemoryEntry,
    file_context: Option<&str>,
    error_context: Option<&str>,
) -> f64 {
    if !entry.is_guide() {
        return 0.0;
    }
    let mut boost = 0.0;
    if let Some(context) = file_context {
        if context_matches_patterns(context, &entry.file_patterns()) {
            boost += 0.25;
        }
    }
    if let Some(context) = error_context {
        if context_matches_patterns(context, &entry.error_patterns()) {
            boost += 0.35;
        }
    }
    boost
}

fn context_matches_patterns(context: &str, patterns: &[String]) -> bool {
    let context = context.trim().to_ascii_lowercase();
    if context.is_empty() {
        return false;
    }
    patterns
        .iter()
        .map(|pattern| pattern.trim().to_ascii_lowercase())
        .filter(|pattern| !pattern.is_empty())
        .any(|pattern| pattern_matches_context(&pattern, &context))
}

fn pattern_matches_context(pattern: &str, context: &str) -> bool {
    if pattern.contains('*') {
        let mut cursor = 0usize;
        for segment in pattern.split('*').filter(|segment| !segment.is_empty()) {
            let Some(pos) = context[cursor..].find(segment) else {
                return false;
            };
            cursor += pos + segment.len();
        }
        true
    } else {
        context.contains(pattern) || pattern.contains(context)
    }
}

pub(crate) async fn handle_search_memory(
    server: &MemoryServer,
    params: SearchMemoryParams,
) -> Result<String, String> {
    let top_k = params.top_k.max(1);
    let mut search_params = params.clone();
    if params.enable_rerank {
        search_params.top_k = top_k.saturating_mul(3).max(top_k + 1);
        search_params.candidates_per_channel = search_params
            .candidates_per_channel
            .max(search_params.top_k);
    }
    let mut rows = search_memory_rows(server, search_params).await?;
    if params.enable_rerank && rows.len() > top_k {
        if rows.len() >= 3 && search_score(&rows[0]) - search_score(&rows[2]) < 0.15 {
            let (reranked, outcome) = crate::foundry_runtime_ops::rerank_rows_with_outcome(
                server,
                &params.query,
                rows,
                top_k,
            )
            .await;
            if outcome == crate::foundry_runtime_ops::RerankOutcome::Fallback {
                eprintln!(
                    "[search_memory] rerank fail-open: query_hash={} top_k={}",
                    stable_hash(&params.query), top_k
                );
            }
            rows = reranked;
        } else {
            rows.truncate(top_k);
        }
    } else {
        rows.truncate(top_k);
    }
    serde_json::to_string(&rows).map_err(|e| format!("Failed to serialize response: {}", e))
}

pub(crate) async fn handle_find_similar_memory(
    server: &MemoryServer,
    params: FindSimilarMemoryParams,
) -> Result<String, String> {
    if params.query_vec.is_empty() {
        return serde_json::to_string(&json!([]))
            .map_err(|e| format!("Failed to serialize response: {}", e));
    }

    if params.query_vec.iter().any(|v| !v.is_finite()) {
        return Err("query_vec contains non-finite values".to_string());
    }

    let mut combined_results: Vec<(memory_core::SearchResult, DbScope)> = Vec::new();
    let common_weights = memory_core::HybridWeights {
        semantic: 1.0,
        fts: 0.0,
        symbolic: 0.0,
        decay: 0.0,
        use_rrf: false,
    };

    let global_opts = SearchOptions {
        candidates_per_channel: params.candidates_per_channel.max(params.top_k),
        top_k: params.top_k,
        weights: common_weights.clone(),
        path_prefix: params.path_prefix.clone(),
        query_vec: Some(params.query_vec.clone()),
        vec_available: server.global_vec_available,
        record_access: false,
        include_archived: params.include_archived,
        include_superseded: false,
        mmr_threshold: None,
        graph_expand_hops: 0,
        graph_relation_filter: None,
        domain: None,
    };

    let global_results = server.with_global_store_read(|store| {
        store
            .search("", Some(global_opts))
            .map_err(|e| format!("Vector search failed in global DB: {}", e))
    })?;
    combined_results.extend(global_results.into_iter().map(|r| (r, DbScope::Global)));

    if server.has_project_db() {
        let project_opts = SearchOptions {
            candidates_per_channel: params.candidates_per_channel.max(params.top_k),
            top_k: params.top_k,
            weights: common_weights,
            path_prefix: params.path_prefix.clone(),
            query_vec: Some(params.query_vec.clone()),
            vec_available: server.project_vec_available,
            record_access: false,
            include_archived: params.include_archived,
            include_superseded: false,
            mmr_threshold: None,
            graph_expand_hops: 0,
            graph_relation_filter: None,
            domain: None,
        };

        let project_results = server.with_project_store_read(|store| {
            store
                .search("", Some(project_opts))
                .map_err(|e| format!("Vector search failed in project DB: {}", e))
        })?;
        combined_results.extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
    }

    combined_results.sort_by(|a, b| {
        b.0.score
            .vector
            .partial_cmp(&a.0.score.vector)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut seen_ids = HashSet::new();
    let mut output: Vec<serde_json::Value> = Vec::new();
    for (result, db_scope) in combined_results {
        if !seen_ids.insert(result.entry.id.clone()) {
            continue;
        }
        let mut obj = match slim_entry(&result.entry, db_scope) {
            serde_json::Value::Object(m) => m,
            _ => serde_json::Map::new(),
        };
        obj.insert(
            "similarity".into(),
            json!((result.score.vector * 1000.0).round() / 1000.0),
        );
        output.push(serde_json::Value::Object(obj));
        if output.len() >= params.top_k {
            break;
        }
    }

    serde_json::to_string(&output).map_err(|e| format!("Failed to serialize response: {}", e))
}

/// Format a save-path error string. When the underlying SQLite error indicates
/// a readonly database, attach the resolved DB path, the active scope/profile,
/// and a concrete remediation hint. Non-readonly errors fall through to the
/// previous one-line format so existing callers (and tests) keep working.
fn format_save_error(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<&str>,
    err: &dyn std::fmt::Display,
) -> String {
    let err_str = err.to_string();
    let lower = err_str.to_ascii_lowercase();
    let is_readonly = lower.contains("readonly")
        || lower.contains("read-only")
        || lower.contains("read only")
        || lower.contains("attempt to write a readonly database");

    if !is_readonly {
        return match named_project {
            Some(name) => format!("Failed to save memory to '{}': {}", name, err_str),
            None => format!("Failed to save memory: {}", err_str),
        };
    }

    let db_path = match named_project {
        Some(name) => crate::MemoryServer::resolve_named_project_db_path(name)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| format!("<named project: {name}>")),
        None => match target_db {
            DbScope::Global => server.global_db_path.display().to_string(),
            DbScope::Project => server
                .project_db_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<no project DB configured>".to_string()),
        },
    };

    let profile_label = server
        .active_tool_profile()
        .map(|p| p.as_str())
        .unwrap_or_else(|| "admin".to_string());

    format!(
        "Failed to save memory: database is read-only.\n  \
         db_path: {db_path}\n  \
         scope: {scope}\n  \
         profile: {profile_label}\n  \
         hints:\n    \
         - Another process may hold an exclusive lock; check for stale `tachi` daemons.\n    \
         - File permissions may be wrong; ensure the user owns the DB file and parent dir.\n    \
         - The DB may have been opened read-only by an earlier CLI command — restart the daemon.\n    \
         - If targeting the wrong DB, pass --global-db / --project-db (or `project=` on the call).\n  \
         underlying: {err_str}",
        scope = target_db.as_str(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_core::types::MemoryEntry;
    use serde_json::json;

    fn test_entry(id: &str, text: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.into(),
            path: "/test".into(),
            summary: text[..text.len().min(30)].into(),
            text: text.into(),
            importance: 0.7,
            timestamp: chrono::Utc::now().to_rfc3339(),
            category: "fact".into(),
            topic: "".into(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: "".into(),
            source: "test".into(),
            scope: "general".into(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: json!({}),
            vector: None,
            retention_policy: None,
            domain: None,
        }
    }

    #[test]
    fn scrub_secrets_masks_bearer_tokens() {
        let input = "Authorization: Bearer sk-abc123def456ghi789jkl012mno345";
        let (output, count) = scrub_secrets(input);
        assert!(count > 0, "should detect bearer token");
        assert!(output.contains(REDACTED_SECRET));
        assert!(!output.contains("sk-abc123"));
    }

    #[test]
    fn scrub_secrets_masks_api_keys() {
        let input = r#"api_key: "sk-proj-abcdefghijklmnopqrstuvwxyz""#;
        let (output, count) = scrub_secrets(input);
        assert!(count > 0);
        assert!(output.contains(REDACTED_SECRET));
    }

    #[test]
    fn scrub_secrets_masks_aws_keys() {
        let input = "AWS key: AKIAIOSFODNN7EXAMPLE";
        let (output, count) = scrub_secrets(input);
        assert!(count > 0);
        assert!(output.contains(REDACTED_SECRET));
    }

    #[test]
    fn scrub_secrets_preserves_safe_text() {
        let input = "This is a normal text with no secrets at all.";
        let (output, count) = scrub_secrets(input);
        assert_eq!(count, 0);
        assert_eq!(output, input);
    }

    #[test]
    fn scrub_secrets_masks_github_tokens() {
        let input = "token=ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij";
        let (output, count) = scrub_secrets(input);
        assert!(count > 0);
        assert!(output.contains(REDACTED_SECRET));
    }

    #[test]
    fn path_root_extracts_first_segment() {
        assert_eq!(path_root("/project/alpha"), "project");
        assert_eq!(path_root("/wiki/entry"), "wiki");
        assert_eq!(path_root("no-slash"), "no-slash");
        assert_eq!(path_root("/"), "");
    }

    #[test]
    fn is_newer_than_compares_timestamps() {
        assert!(is_newer_than(
            "2025-01-02T00:00:00Z",
            "2025-01-01T00:00:00Z"
        ));
        assert!(!is_newer_than(
            "2025-01-01T00:00:00Z",
            "2025-01-02T00:00:00Z"
        ));
    }

    #[test]
    fn should_enqueue_enrichment_high_importance() {
        let mut e = test_entry("enr-1", "test");
        e.importance = 0.5;
        assert!(should_enqueue_enrichment(&e));

        e.importance = 0.3;
        e.vector = None;
        assert!(!should_enqueue_enrichment(&e));

        e.vector = Some(vec![0.1; 64]);
        assert!(should_enqueue_enrichment(&e));
    }

    #[test]
    fn should_reinforce_requires_vector_similarity_gray_zone() {
        let mut new_entry = test_entry("new", "canonical preference");
        let mut old_entry = test_entry("old", "nearby preference");
        new_entry.path = "/project/a".to_string();
        old_entry.path = "/project/b".to_string();
        new_entry.category = "preference".to_string();
        old_entry.category = "preference".to_string();

        assert!(should_reinforce(&new_entry, &old_entry, 1, 0.82, false));
        assert!(!should_reinforce(&new_entry, &old_entry, 0, 0.82, false));
        assert!(!should_reinforce(&new_entry, &old_entry, 1, 0.60, false));
        assert!(!should_reinforce(&new_entry, &old_entry, 1, 0.97, false));
        assert!(!should_reinforce(&new_entry, &old_entry, 1, 0.82, true));
    }

    #[test]
    fn confidence_reinforcement_updates_metadata_confidence() {
        let mut store = memory_core::MemoryStore::open_in_memory().unwrap();
        let mut old_entry = test_entry("old", "durable supported fact");
        old_entry.metadata = json!({ "confidence": 0.70 });
        store.upsert(&old_entry).unwrap();

        apply_confidence_reinforcement(&mut store, "old", 0.08, "2026-01-01T00:00:00Z")
            .unwrap();

        let updated = store.get("old").unwrap().unwrap();
        let confidence = updated
            .metadata
            .get("confidence")
            .and_then(|value| value.as_f64())
            .unwrap();
        assert!((confidence - 0.78).abs() < 1e-9, "confidence={confidence}");
        assert_eq!(
            updated
                .metadata
                .get("confidence_reinforced_at")
                .and_then(|value| value.as_str()),
            Some("2026-01-01T00:00:00Z")
        );
    }
}
