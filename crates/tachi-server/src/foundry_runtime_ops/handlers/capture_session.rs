use super::super::capture::{persist_capture_entry, queue_capture_enrichment};
use super::super::helpers::{
    build_entry_path, build_openclaw_agent_root, dedup_strings, normalize_category, normalize_scope,
};
use super::super::maintenance::enqueue_capture_maintenance_jobs;
use super::super::recall::parse_session_capture_response;
use super::bracket::{extract_bracket_self_evolution_notes, matches_agent_tag};
use super::target::resolve_capture_target;
use crate::server_state::MemoryServer;
use crate::tool_params::CaptureSessionParams;
use crate::DbScope;
use chrono::Utc;
use memcore::MemoryEntry;
use serde_json::{json, Value};

/// #1114 (codex round-1 B3 fix): a `MemoryEntry` carried alongside its OWN
/// resolved write-affinity destination — computed once, per entry, before
/// provenance/persist/maintenance/continuity all need to agree on where the
/// row actually lives. A single `capture_session` batch's entries do NOT
/// all necessarily share one destination (a mismatched entry can reroute
/// independently of its siblings), so this is tracked per-entry rather than
/// once for the whole batch.
struct CapturedEntry {
    entry: MemoryEntry,
    target_db: DbScope,
    named_project: Option<String>,
}

pub(crate) async fn handle_capture_session(
    server: &MemoryServer,
    params: CaptureSessionParams,
) -> Result<String, String> {
    let combined_text = params
        .messages
        .iter()
        .map(|message| format!("{}: {}", message.role.trim(), message.content.trim()))
        .collect::<Vec<_>>()
        .join("\n");

    if combined_text.trim().is_empty() {
        return serde_json::to_string(&json!({
            "status": "skipped",
            "reason": "empty_messages",
            "captured": 0,
        }))
        .map_err(|e| format!("Failed to serialize capture_session response: {e}"));
    }

    if !params.force && combined_text.chars().count() < params.min_chars {
        return serde_json::to_string(&json!({
            "status": "skipped",
            "reason": "below_min_chars",
            "captured": 0,
        }))
        .map_err(|e| format!("Failed to serialize capture_session response: {e}"));
    }

    let requested_scope = normalize_scope(&params.scope, "project");
    let (target_db, named_project, db_path, warning) = resolve_capture_target(
        server,
        &requested_scope,
        params.project.as_deref(),
        params.project_explicit,
        &params.agent_id,
    );

    let base_path = params
        .path_prefix
        .clone()
        .unwrap_or_else(|| build_openclaw_agent_root(&params.agent_id));
    let source_ref_id = format!("{}:{}", params.conversation_id, params.turn_id);
    let self_evolution_path = format!("{}/self-evolution", base_path.trim_end_matches('/'));
    // User-preference scoping: agents with "user_memory" in their profile get
    // preference notes scoped to "user" instead of the requested scope.
    let is_user_memory_agent = matches_agent_tag(&params.agent_id, "user-memory")
        || matches_agent_tag(&params.agent_id, "jayne");

    let mut entries = Vec::<CapturedEntry>::new();
    for note in extract_bracket_self_evolution_notes(&params.agent_id, &params.messages) {
        // #1114 (codex round-1 B3 point ③): resolve this entry's routed
        // write-affinity destination BEFORE `inject_provenance` runs, so
        // provenance is stamped against where the row is actually going to
        // land, not the pre-gate default the gate is about to override.
        let (entry_target_db, entry_named_project) = resolve_capture_write_target(
            server,
            &note.id,
            None,
            &self_evolution_path,
            &note.category,
            target_db,
            named_project.as_deref(),
            db_path.as_ref(),
            params.project_explicit,
        )?;
        let metadata = crate::provenance::inject_provenance(
            server,
            json!({
                "source_refs": [{
                    "ref_type": "turn",
                    "ref_id": source_ref_id.clone(),
                }],
                "conversation_id": params.conversation_id,
                "turn_id": params.turn_id,
                "agent_id": params.agent_id,
                "message_count": params.messages.len(),
                "artifact_kind": "bracket_self_evolution",
            }),
            "capture_session",
            "bracket_self_evolution",
            Some(requested_scope.as_str()),
            entry_target_db,
            json!({
                "conversation_id": params.conversation_id,
                "turn_id": params.turn_id,
                "agent_id": params.agent_id,
                "path_prefix": base_path,
            }),
        );
        let strategy_keyword = if note.category == "preference" {
            "user-preference".to_string()
        } else {
            "strategy".to_string()
        };
        let entry_scope = if is_user_memory_agent && note.category == "preference" {
            "user".to_string()
        } else {
            requested_scope.clone()
        };

        entries.push(CapturedEntry {
            entry: MemoryEntry {
                id: note.id,
                path: self_evolution_path.clone(),
                summary: note.text.chars().take(100).collect(),
                text: note.text,
                importance: 0.70,
                timestamp: Utc::now().to_rfc3339(),
                valid_from: String::new(),
                valid_until: None,
                category: note.category,
                topic: "self_evolution".to_string(),
                keywords: dedup_strings(vec![
                    "self-evolution".to_string(),
                    "bracket-note".to_string(),
                    strategy_keyword,
                ]),
                persons: Vec::new(),
                entities: if is_user_memory_agent {
                    vec!["user".to_string()]
                } else {
                    Vec::new()
                },
                location: String::new(),
                source: "bracket_self_evolution".to_string(),
                scope: entry_scope,
                archived: false,
                access_count: 0,
                last_access: None,
                revision: 1,
                metadata,
                vector: None,
                retention_policy: None,
                domain: None,
                recall_count: 0,
                query_diversity: 0,
                tier: "raw".to_string(),
            },
            target_db: entry_target_db,
            named_project: entry_named_project,
        });
    }

    let payload = json!({
        "conversation_id": params.conversation_id,
        "turn_id": params.turn_id,
        "agent_id": params.agent_id,
        "messages": params.messages,
    });
    let request = serde_json::to_string_pretty(&payload)
        .map_err(|e| format!("Failed to serialize session capture payload: {e}"))?;
    let drafts = match server
        .llm
        .call_extract_llm(
            crate::prompts::SESSION_CAPTURE_PROMPT,
            &request,
            None,
            0.1,
            2400,
        )
        .await
    {
        Ok(raw) => match parse_session_capture_response(&raw) {
            Ok(drafts) => drafts,
            Err(err) if entries.is_empty() => {
                return serde_json::to_string(&json!({
                    "status": "failed",
                    "reason": "llm_capture_parse_failed",
                    "error": err,
                    "captured": 0,
                    "conversation_id": params.conversation_id,
                    "turn_id": params.turn_id,
                    "agent_id": params.agent_id,
                }))
                .map_err(|e| format!("Failed to serialize capture_session response: {e}"));
            }
            Err(_) => Vec::new(),
        },
        Err(err) if entries.is_empty() => {
            return serde_json::to_string(&json!({
                "status": "failed",
                "reason": "llm_capture_failed",
                "error": err,
                "captured": 0,
                "conversation_id": params.conversation_id,
                "turn_id": params.turn_id,
                "agent_id": params.agent_id,
            }))
            .map_err(|e| format!("Failed to serialize capture_session response: {e}"));
        }
        Err(_) => Vec::new(),
    };

    if drafts.is_empty() && entries.is_empty() {
        return serde_json::to_string(&json!({
            "status": "skipped",
            "reason": "no_durable_memories",
            "captured": 0,
        }))
        .map_err(|e| format!("Failed to serialize capture_session response: {e}"));
    }

    for draft in drafts {
        let topic = if draft.topic.trim().is_empty() {
            "session_capture".to_string()
        } else {
            draft.topic.trim().to_string()
        };
        let scope = normalize_scope(&draft.scope, &requested_scope);
        let entry_path = build_entry_path(&base_path, &topic);
        let entry_category = normalize_category(&draft.category);
        // LLM drafts always mint a fresh random id here (never caller
        // -supplied, never deterministic) — hoisted so the SAME id is used
        // for both the B4 existence pre-check below and the persisted entry.
        let entry_id = uuid::Uuid::new_v4().to_string();

        // #1114 (codex round-1 B3 point ③): resolve BEFORE provenance, same
        // reasoning as the bracket-note loop above.
        let (entry_target_db, entry_named_project) = resolve_capture_write_target(
            server,
            &entry_id,
            None,
            &entry_path,
            &entry_category,
            target_db,
            named_project.as_deref(),
            db_path.as_ref(),
            params.project_explicit,
        )?;
        let metadata = crate::provenance::inject_provenance(
            server,
            json!({
                "source_refs": [{
                    "ref_type": "turn",
                    "ref_id": source_ref_id.clone(),
                }],
                "conversation_id": params.conversation_id,
                "turn_id": params.turn_id,
                "agent_id": params.agent_id,
                "message_count": params.messages.len(),
            }),
            "capture_session",
            "session_capture",
            Some(scope.as_str()),
            entry_target_db,
            json!({
                "conversation_id": params.conversation_id,
                "turn_id": params.turn_id,
                "agent_id": params.agent_id,
                "path_prefix": base_path,
            }),
        );

        let summary = if draft.summary.trim().is_empty() {
            draft.text.chars().take(100).collect::<String>()
        } else {
            draft.summary.trim().to_string()
        };

        entries.push(CapturedEntry {
            entry: MemoryEntry {
                id: entry_id,
                path: entry_path,
                summary,
                text: draft.text.trim().to_string(),
                importance: draft.importance.clamp(0.0, 1.0),
                timestamp: Utc::now().to_rfc3339(),
                valid_from: String::new(),
                valid_until: None,
                category: entry_category,
                topic,
                keywords: dedup_strings(draft.keywords),
                persons: Vec::new(),
                entities: {
                    let mut entities = dedup_strings(draft.entities);
                    for name in draft.persons {
                        memcore::types::push_entity_name(&mut entities, &name);
                    }
                    entities
                },
                location: draft.location.trim().to_string(),
                source: "capture_session".to_string(),
                scope,
                archived: false,
                access_count: 0,
                last_access: None,
                revision: 1,
                metadata,
                vector: None,
                retention_policy: None,
                domain: None,
                recall_count: 0,
                query_diversity: 0,
                tier: "raw".to_string(),
            },
            target_db: entry_target_db,
            named_project: entry_named_project,
        });
    }

    let texts = entries
        .iter()
        .map(|captured| captured.entry.text.clone())
        .collect::<Vec<_>>();
    let texts: Vec<_> = texts
        .iter()
        .map(|t| crate::memory_search_ops::scrub_secrets(t).0)
        .collect();
    let embeddings = match server.llm.embed_voyage_batch(&texts, "document").await {
        Ok(vectors) => Some(vectors),
        Err(err) => {
            tracing::warn!("[capture_session] embedding failed, deferring enrichment: {err}");
            None
        }
    };
    if let Some(vectors) = embeddings.as_ref() {
        for (captured, vector) in entries.iter_mut().zip(vectors.iter()) {
            captured.entry.vector = Some(vector.clone());
        }
    }

    let mut saved_ids = Vec::new();
    // #1114 (codex round-1 B3 point ④): group persisted ids by their ACTUAL
    // (post-gate) destination — a maintenance job or continuity event
    // enqueued against the pre-gate default, for an entry that reroutes
    // elsewhere, resolves memory_ids against a store that never received
    // them: the maintenance worker (`with_foundry_store`) finds nothing and
    // silently skips the job forever, and a later continuity sweep over the
    // pre-gate store references a memory id that doesn't exist there.
    let mut by_destination: std::collections::BTreeMap<(String, Option<String>), Vec<String>> =
        std::collections::BTreeMap::new();

    for captured in &entries {
        persist_capture_entry(
            server,
            captured.target_db,
            captured.named_project.as_deref(),
            db_path.as_ref(),
            &captured.entry,
        )?;
        if embeddings.is_none() {
            queue_capture_enrichment(
                server,
                captured.target_db,
                captured.named_project.clone(),
                db_path.clone(),
                &captured.entry,
                false,
                Some(&params.agent_id),
                Some(&base_path),
            );
        }
        saved_ids.push(captured.entry.id.clone());
        by_destination
            .entry((
                captured.target_db.as_str().to_string(),
                captured.named_project.clone(),
            ))
            .or_default()
            .push(captured.entry.id.clone());
    }

    let saved_ids = dedup_strings(saved_ids);

    // The pre-gate default destination — preferred, when present among the
    // ACTUAL destinations reached, to populate the response's top-level
    // (pre-#1114-shaped) `maintenance_jobs`/`continuity` fields, so existing
    // callers parsing this response see NO shape change in the common
    // (nothing rerouted) case. When every entry rerouted AWAY from this
    // default (codex round-2 item 3 point ④), it will not appear in
    // `by_destination` at all — falling back to whichever destination
    // actually exists (below) instead of a hardcoded "skipped" placeholder
    // is what keeps the top-level `continuity.session_event` truthful in
    // that case.
    let default_destination_key = (target_db.as_str().to_string(), named_project.clone());

    let mut maintenance_jobs = Vec::new();
    let mut continuity_by_destination = Vec::new();
    // (destination_key, session_event, pipeline) tuples, in the same order
    // as `continuity_by_destination` — kept alongside the JSON so picking
    // the "primary" entry after the loop doesn't need to round-trip
    // through JSON value comparisons.
    let mut continuity_results_by_destination: Vec<((String, Option<String>), Value, Value)> =
        Vec::new();

    for (destination_key, raw_ids) in &by_destination {
        let ids = dedup_strings(raw_ids.clone());
        let ids = &ids;
        let (db_str, group_named_project) = destination_key;
        let group_target_db = if db_str.as_str() == DbScope::Global.as_str() {
            DbScope::Global
        } else {
            DbScope::Project
        };
        let mut jobs = enqueue_capture_maintenance_jobs(
            server,
            group_target_db,
            group_named_project.clone(),
            db_path.clone(),
            &params.agent_id,
            &base_path,
            ids,
            0,
            0,
        )?;
        maintenance_jobs.append(&mut jobs);

        let group_continuity_target = crate::continuity_ops::ContinuityEventTarget::new(
            group_target_db,
            group_named_project.clone(),
            db_path.clone(),
        );
        let session_event = crate::continuity_ops::emit_session_captured_event(
            server,
            &group_continuity_target,
            &params.conversation_id,
            &params.turn_id,
            &params.agent_id,
            &base_path,
            ids,
            params.messages.len(),
            // #1114 (codex round-2 item 3 point ②): label this event with
            // the destination it was ACTUALLY written to, not the stale
            // pre-gate `params.project` — `ContinuityEventTarget::
            // project_label`'s own precedence puts an "explicit_project"
            // argument ahead of the target's own `named_project`, so
            // passing the stale value here made the label lie about where
            // the row landed whenever `params.project` was `Some` (which,
            // since #1114's B2 fix, is nearly always true for a bound
            // session).
            group_named_project.as_deref(),
        );

        // #1114 (codex round-2 item 3 point ③): the session-continuity
        // PIPELINE analyzes the whole conversation (not specific memory
        // ids), but it still writes its OWN continuity events — those must
        // land where this destination group's rows actually are, not at
        // the pre-gate default. Running it once per destination that
        // actually received entries (rather than once, unconditionally, at
        // the pre-gate default) means a fully-rerouted capture no longer
        // writes pipeline events into a store none of its content ever
        // reached; a batch split across multiple destinations pays for
        // duplicate background analysis, which is the acceptable side of
        // this trade-off (a background LLM pass repeating is far cheaper
        // than a durable row landing in the wrong store).
        let pipeline_target = crate::continuity_ops::ContinuityEventTarget::new(
            group_target_db,
            group_named_project.clone(),
            db_path.clone(),
        );
        let pipeline = crate::continuity_ops::maybe_spawn_session_continuity_pipeline(
            server,
            pipeline_target,
            params.conversation_id.clone(),
            params.turn_id.clone(),
            params.agent_id.clone(),
            group_named_project.clone(),
            params.messages.clone(),
        );

        continuity_results_by_destination.push((
            destination_key.clone(),
            session_event.clone(),
            pipeline.clone(),
        ));
        continuity_by_destination.push(json!({
            "target_db": group_target_db.as_str(),
            "named_project": group_named_project,
            "memory_ids": ids,
            "session_event": session_event,
            "pipeline": pipeline,
        }));
    }

    // #1114 (codex round-2 item 3 point ④): prefer the pre-gate default
    // destination's session event/pipeline when it's among the ones
    // actually reached; otherwise (every entry rerouted away from it) fall
    // back to whichever destination DID receive entries, rather than
    // reporting a hardcoded "skipped: no_captured_entries" placeholder that
    // contradicts `by_destination`'s own (truthful) contents.
    let primary = continuity_results_by_destination
        .iter()
        .find(|(key, _, _)| key == &default_destination_key)
        .or_else(|| continuity_results_by_destination.first());
    let primary_session_event = primary
        .map(|(_, event, _)| event.clone())
        .unwrap_or_else(|| json!({"status": "skipped", "reason": "no_captured_entries"}));
    let primary_pipeline = primary
        .map(|(_, _, pipeline)| pipeline.clone())
        .unwrap_or_else(|| json!({"status": "skipped", "reason": "no_captured_entries"}));

    let mut response = serde_json::Map::new();
    response.insert("status".into(), json!("completed"));
    response.insert("captured".into(), json!(saved_ids.len()));
    response.insert("ids".into(), json!(saved_ids));
    response.insert("merged_ids".into(), json!(Vec::<String>::new()));
    response.insert("duplicate_ids".into(), json!(Vec::<String>::new()));
    response.insert("duplicates_skipped".into(), json!(0));
    response.insert("maintenance_jobs".into(), json!(maintenance_jobs));
    response.insert("db".into(), json!(target_db.as_str()));
    response.insert("path_prefix".into(), json!(base_path));
    response.insert(
        "continuity".into(),
        json!({
            "session_event": primary_session_event,
            "pipeline": primary_pipeline,
            "by_destination": continuity_by_destination,
        }),
    );
    if let Some(warning) = warning {
        response.insert("warning".into(), json!(warning));
    }

    serde_json::to_string(&Value::Object(response))
        .map_err(|e| format!("Failed to serialize capture_session response: {e}"))
}

/// #1114 (write_affinity module doc's F1 note): purpose-built write-affinity
/// scrutiny for the fresh-incoming-content path — bracket self-evolution
/// notes and LLM-drafted session-capture drafts land wherever
/// `resolve_capture_target` resolved, the exact ambiguous-default shape S1
/// exists to catch. Takes `path`/`category`/`domain` directly (not a built
/// `MemoryEntry`) so the routed destination can be resolved BEFORE the entry
/// (and its provenance) is constructed at all — see `handle_capture_session`'s
/// entry-building loops, where the ROUTED destination now feeds
/// `provenance::inject_provenance` directly instead of the pre-gate default
/// (codex round-1 B3 point ③: provenance must not be stamped against a
/// destination the write-affinity gate is about to override).
///
/// A `db_path` target (the manifest agent-pinned branch of
/// `resolve_capture_target`) is a deliberate per-agent DB assignment and is
/// never scrutinized here, same posture as continuity's own `db_path` skip
/// in `continuity_ops::storage::upsert_projection_memory`.
///
/// `id` (codex round-1 B4 fix): bracket self-evolution notes use a
/// deterministic `UUIDv5` (`build_bracket_self_evolution_id`, hashed from
/// `agent_id` + note text), not a fresh random id — a repeat capture of the
/// SAME note text is an update-in-place at wherever it already lives, not a
/// fresh row. `id_resolves_at_target` used to be hard-coded `false`, so a
/// repeat bracket capture whose domain routes elsewhere would reroute AGAIN
/// on every call, landing a duplicate copy at BOTH the original and the
/// rerouted store instead of updating the one row in place. Checking
/// existence at the PRE-gate target first (mirrors continuity's own
/// `get_projection_memory` pre-check in `projection.rs`) makes a second
/// capture of the same note skip the gate and update where it already is.
///
/// KNOWN LIMITATION (codex round-2 item 4③, deferred, not solved here):
/// this only catches the note sitting at the PRE-gate default. If the
/// registry routes this domain to store A on one capture and later (a
/// SECOND registry edit) to a DIFFERENT store B, this check never looks at
/// A — the note already there is invisible to it, and a second,
/// independent copy gets created at B. Fully closing that requires
/// persistent per-id "last known location" tracking (a schema-level
/// change), the same shape of gap #1115's check-then-insert atomicity work
/// is already scoped to address — deliberately not solved with an ad-hoc
/// migration in this leaf. Concurrent/racing inserts against the SAME
/// pre-gate id have the identical exposure for the same reason (this
/// lookup-then-decide sequence has no transactional atomicity).
fn resolve_capture_write_target(
    server: &MemoryServer,
    id: &str,
    domain: Option<&str>,
    path: &str,
    category: &str,
    target_db: DbScope,
    named_project: Option<&str>,
    db_path: Option<&std::path::PathBuf>,
    project_explicit: bool,
) -> Result<(DbScope, Option<String>), String> {
    if db_path.is_some() {
        return Ok((target_db, named_project.map(str::to_string)));
    }
    // #1114 (codex round-2 item 4 fix): `?` propagates a genuine read
    // failure instead of silently treating it as "doesn't exist" — a false
    // negative here (store unreadable right now, NOT actually empty) risks
    // creating exactly the duplicate-across-two-stores outcome this
    // existence check exists to prevent.
    let already_exists_at_target = capture_entry_exists_at(server, target_db, named_project, id)?;
    // `repair_target` returns `None` both when nothing needs repairing (an
    // already-clean domain is unchanged) and when there was truly nothing to
    // infer — same `.or_else` fallback `resolve_save_domain` (save_memory)
    // and `projection_domain_label` (continuity) both apply, so an already
    // -valid `domain` is never dropped just because it didn't need a repair.
    let resolved_domain =
        crate::repair::domain::repair_target(domain, path, category, "foundry_capture")
            .or_else(|| domain.map(str::to_string));
    let affinity =
        crate::memory_search_ops::save_memory::write_affinity::apply_write_affinity_for_domain(
            server,
            resolved_domain.as_deref(),
            target_db,
            named_project,
            project_explicit,
            already_exists_at_target,
        )?;
    Ok((affinity.target_db, affinity.named_project))
}

/// #1114 (codex round-1 B4 fix, round-2 item 4 fix): does a row with this
/// id already exist at the PRE-gate target? A read FAILURE now propagates
/// as a real `Err` — treating it as "doesn't exist" (the original B4 shape,
/// matching `continuity_ops::projection::persist_timeline_graph_edges`'s
/// own tolerant endpoint-existence checks) is safe for THAT call site
/// because a false negative there only skips an optional graph edge; here a
/// false negative risks creating a duplicate row at the write-affinity
/// gate's proposed reroute destination while a genuine copy already exists
/// at the target this call couldn't read.
fn capture_entry_exists_at(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<&str>,
    id: &str,
) -> Result<bool, String> {
    let result = if let Some(project_name) = named_project {
        server.with_named_project_store_read(project_name, |store| {
            store.get(id).map_err(|e| e.to_string())
        })
    } else {
        server
            .with_store_for_scope_read(target_db, |store| store.get(id).map_err(|e| e.to_string()))
    };
    Ok(result?.is_some())
}

#[cfg(test)]
mod affinity_tests {
    use super::resolve_capture_write_target;
    use crate::server_state::MemoryServer;
    use crate::DbScope;
    use memcore::MemoryEntry;
    use serde_json::json;

    fn entry_with_domain(id: &str, domain: Option<&str>) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/openclaw/agent/self-evolution".to_string(),
            summary: String::new(),
            text: "captured note".to_string(),
            importance: 0.7,
            timestamp: "2026-07-14T00:00:00Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "preference".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "capture_session".to_string(),
            scope: "project".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: json!({}),
            vector: None,
            retention_policy: None,
            domain: domain.map(str::to_string),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    fn two_project_server(home: &std::path::Path, bound_project: &str) -> MemoryServer {
        let quant_db = home.join("projects").join(bound_project).join("memory.db");
        std::fs::create_dir_all(quant_db.parent().unwrap()).expect("mkdir bound project");
        let global_db = home.join("global").join("memory.db");
        std::fs::create_dir_all(global_db.parent().unwrap()).expect("mkdir global");
        // `MemoryServer::new(global, project)` — the project path (not the
        // first/global one) is what `bound_project_label` resolves the bound
        // name from via the Plan C `projects/<name>/memory.db` convention.
        MemoryServer::new(global_db, Some(quant_db)).expect("bind daemon")
    }

    /// #1114 (Oz r3 fixture fix): mount a real, schema-initialized named
    /// -project DB rather than a zero-byte placeholder file — see
    /// `continuity_ops::storage::tests::mount_named_project_db`'s doc for
    /// why a zero-byte file only survives a WRITE-first touch, not a READ
    /// -first one (`with_named_project_store_read` does not run schema
    /// init the way the write path's `MemoryStore::open_with_label` does).
    fn mount_named_project_db(home: &std::path::Path, name: &str) -> std::path::PathBuf {
        let db_path = home.join("projects").join(name).join("memory.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).expect("mkdir named project");
        memcore::MemoryStore::open_with_label(db_path.to_str().expect("utf-8 db path"), name)
            .expect("init named project schema");
        db_path
    }

    /// #1114 discriminating test (red before this PR): capture content whose
    /// domain is registered to a DIFFERENT, mounted store than the daemon's
    /// own bound project must be rerouted there, not silently land in the
    /// bound project just because that's where `resolve_capture_target`
    /// pointed by default. Before this change, `capture_session.rs` called
    /// `persist_capture_entry` directly with the pre-gate target — this is
    /// exactly the cross-domain-drift shape #1041/#1114 exist to catch.
    #[test]
    fn cross_domain_capture_entry_reroutes_to_registered_mounted_store() {
        crate::test_support::with_tachi_home(|home| {
            std::fs::write(
                home.join("routing.json"),
                r#"{"domain_routes":[{"project":"hapi","domains":["equity_trading"]}]}"#,
            )
            .expect("write routing.json");
            let server = two_project_server(home, "quant");
            let _hapi_db = mount_named_project_db(home, "hapi");

            let (target_db, named_project) = resolve_capture_write_target(
                &server,
                "cap-1",
                Some("equity_trading"),
                "/openclaw/agent/self-evolution",
                "preference",
                DbScope::Project,
                Some("quant"), // transport-injected default == daemon's own bound project
                None,
                false, // project_explicit: NOT a caller decision
            )
            .expect("resolve write target");

            assert_eq!(target_db, DbScope::Project);
            assert_eq!(
                named_project.as_deref(),
                Some("hapi"),
                "equity_trading content on an unrelated (quant) daemon must reroute to hapi"
            );
        });
    }

    /// Same mismatch, but the registered store is NOT mounted — must refuse
    /// loudly rather than silently landing the entry in the bound store.
    #[test]
    fn cross_domain_capture_entry_refuses_when_registered_store_unmounted() {
        crate::test_support::with_tachi_home(|home| {
            std::fs::write(
                home.join("routing.json"),
                r#"{"domain_routes":[{"project":"hapi","domains":["equity_trading"]}]}"#,
            )
            .expect("write routing.json");
            let server = two_project_server(home, "quant");
            // "hapi" is never mounted here.

            let err = resolve_capture_write_target(
                &server,
                "cap-2",
                Some("equity_trading"),
                "/openclaw/agent/self-evolution",
                "preference",
                DbScope::Project,
                Some("quant"),
                None,
                false,
            )
            .expect_err("must refuse, not silently write cross-domain");
            assert!(err.contains("equity_trading"));
            assert!(err.contains("hapi"));
        });
    }

    /// Same-domain (or unregistered-domain) content on its own daemon is
    /// unaffected — the common case must not be disturbed by this gate.
    #[test]
    fn same_domain_capture_entry_is_unaffected() {
        crate::test_support::with_tachi_home(|home| {
            let server = two_project_server(home, "quant");

            let (target_db, named_project) = resolve_capture_write_target(
                &server,
                "cap-3",
                None,
                "/openclaw/agent/self-evolution",
                "preference",
                DbScope::Project,
                Some("quant"),
                None,
                false,
            )
            .expect("resolve write target");

            assert_eq!(target_db, DbScope::Project);
            assert_eq!(named_project.as_deref(), Some("quant"));
        });
    }

    /// An explicit `db_path` target (the manifest agent-pinned capture
    /// branch) is never scrutinized — it passes through unchanged even for
    /// mismatched, registered domain content.
    #[test]
    fn db_path_target_skips_the_gate_entirely() {
        crate::test_support::with_tachi_home(|home| {
            std::fs::write(
                home.join("routing.json"),
                r#"{"domain_routes":[{"project":"hapi","domains":["equity_trading"]}]}"#,
            )
            .expect("write routing.json");
            let server = two_project_server(home, "quant");

            let pinned = home
                .join("projects")
                .join("pinned-agent-db")
                .join("memory.db");
            let (target_db, named_project) = resolve_capture_write_target(
                &server,
                "cap-4",
                Some("equity_trading"),
                "/openclaw/agent/self-evolution",
                "preference",
                DbScope::Project,
                None,
                Some(&pinned),
                false,
            )
            .expect("resolve write target");

            assert_eq!(target_db, DbScope::Project);
            assert!(named_project.is_none());
        });
    }

    /// #1114 codex round-1 B4 discriminating test: bracket self-evolution
    /// notes hash to a stable, deterministic `UUIDv5` (same agent + same
    /// note text -> same id every time) — NOT a fresh random id each
    /// capture. Scenario: a note was captured back when `equity_trading` had
    /// no registered route (landed at the daemon's own bound "quant" store,
    /// the ordinary passthrough case), and the SAME note text gets captured
    /// AGAIN later, after `equity_trading` has since been registered to
    /// route to "hapi". The repeat capture must update the row that's
    /// ALREADY at "quant" in place, not reroute to "hapi" and create a
    /// SECOND, independent copy of the identical note split across two
    /// stores. Before the B4 fix, `id_resolves_at_target` was hard-coded
    /// `false`, so every repeat capture blindly re-evaluated domain routing
    /// from scratch regardless of where the row already lived.
    #[test]
    fn repeat_capture_of_same_deterministic_id_updates_in_place_not_rerouted() {
        crate::test_support::with_tachi_home(|home| {
            let server = two_project_server(home, "quant");
            // The note's FIRST capture landed here, back before
            // `equity_trading` had any registered route at all.
            server
                .with_project_store(|store| {
                    store
                        .upsert(&entry_with_domain(
                            "bracket-self-evolution:stable-hash",
                            Some("equity_trading"),
                        ))
                        .map_err(|e| e.to_string())
                })
                .expect("seed the note's original row at the pre-gate target");

            // `equity_trading` is now registered to route to "hapi", and
            // "hapi" is mounted — if the gate did not check for an existing
            // row first, a repeat capture would reroute there.
            std::fs::write(
                home.join("routing.json"),
                r#"{"domain_routes":[{"project":"hapi","domains":["equity_trading"]}]}"#,
            )
            .expect("write routing.json");
            let _hapi_db = mount_named_project_db(home, "hapi");

            let (round2_db, round2_project) = resolve_capture_write_target(
                &server,
                "bracket-self-evolution:stable-hash",
                Some("equity_trading"),
                "/openclaw/agent/self-evolution",
                "preference",
                DbScope::Project,
                Some("quant"),
                None,
                false,
            )
            .expect("repeat capture resolve");
            assert_eq!(
                round2_db,
                DbScope::Project,
                "the repeat capture must update in place, not reroute"
            );
            assert_eq!(
                round2_project.as_deref(),
                Some("quant"),
                "the repeat capture must target wherever the row ALREADY \
                 lives (quant), not re-evaluate domain routing and split it \
                 across two stores"
            );
        });
    }

    /// #1114 codex round-2 item 4② discriminating test: a genuine read
    /// FAILURE at the pre-gate target must propagate as a real `Err`, not
    /// be silently treated as "this id doesn't exist here" — see
    /// `continuity_ops::storage::tests::read_failure_at_pretarget_propagates_instead_of_being_treated_as_absence`
    /// for the fuller reasoning (same fix, same shape, applied to
    /// `capture_entry_exists_at` instead of `get_projection_memory`).
    #[test]
    fn read_failure_at_pretarget_propagates_instead_of_being_treated_as_absence() {
        crate::test_support::with_tachi_home(|home| {
            let server = two_project_server(home, "quant");
            // "corrupt" exists per the filesystem but is not a valid SQLite
            // database at all — genuinely unreadable, not merely empty.
            let corrupt_db = home.join("projects").join("corrupt").join("memory.db");
            std::fs::create_dir_all(corrupt_db.parent().unwrap()).expect("mkdir corrupt");
            std::fs::write(&corrupt_db, b"this is not a sqlite database file at all")
                .expect("write garbage bytes");

            let err = resolve_capture_write_target(
                &server,
                "cap-read-failure",
                Some("equity_trading"),
                "/openclaw/agent/self-evolution",
                "preference",
                DbScope::Project,
                Some("corrupt"),
                None,
                false,
            )
            .expect_err(
                "a genuinely unreadable pre-gate target must propagate an error, \
                 not silently proceed as if the row didn't exist",
            );
            assert!(!err.is_empty());
        });
    }

    // #1114 codex round-2 item 4①/③ KNOWN LIMITATION: NOT reproduced as an
    // integration test in this module — see `continuity_ops::storage::
    // tests`' own note (right above its equivalent, removed test) for why:
    // `RoutingConfig::get_checked()` caches for the rest of the PROCESS's
    // lifetime once loaded, so a second `routing.json` rewrite mid-test
    // would silently have no effect on a second `resolve_capture_write_
    // target` call — and in a real daemon, routing.json is effectively
    // frozen for that daemon's whole uptime the same way, so this scenario
    // can only happen ACROSS a restart, not within one test process. The
    // underlying gate mechanism's lack of memory across two calls with
    // DIFFERENT configs is characterized at the DI level instead:
    // `memory_search_ops::save_memory::write_affinity::tests::
    // config_change_between_calls_reroutes_a_stable_id_to_a_different_store`.
    // Closing this fully requires persistent per-id location tracking,
    // deferred alongside #1115's atomicity work, not solved here.
}

/// #1114 (codex round-2 item 5 fix): a handler-LEVEL discriminating test —
/// every other #1114 capture test in this file drives `resolve_capture_
/// write_target` (or the affinity DI core) directly, hand-injecting
/// `Some("equity_trading")` as the domain. Production never does that:
/// `handle_capture_session` always calls the gate with `domain: None` and
/// lets it get INFERRED from the entry's own `path`/`category` via
/// `repair::domain::repair_target`. A test that injects the domain
/// directly would stay green even if the gate call were deleted from the
/// handler entirely — it never proves the WIRING inside the handler itself.
/// This test goes through the real `handle_capture_session` entry point
/// with a bracket-self-evolution note (the only capture path that needs no
/// live LLM call) whose `path_prefix` drives `infer_domain_from_row` to
/// "equity_trading" the same way a real caller's path would, and asserts
/// the captured row actually lands in the registered "hapi" store — not
/// the daemon's own bound "quant". Deleting the handler's gate call turns
/// this from green to red.
#[cfg(test)]
mod handler_tests {
    use super::handle_capture_session;
    use crate::server_state::MemoryServer;
    use crate::tool_params::{CaptureSessionParams, Message};

    // `with_tachi_home` is a plain sync closure (it restores TACHI_HOME the
    // instant the closure returns) — not `#[tokio::test]`-compatible
    // directly. Every other capture_session test in this file relies on
    // that sync-closure shape, so rather than reach for a different
    // env-var pattern for just this one test, this test stays `#[test]`
    // and blocks on the handler's async body from INSIDE the closure, where
    // TACHI_HOME is still set.
    #[test]
    fn handle_capture_session_infers_domain_from_entry_and_reroutes() {
        crate::test_support::with_tachi_home(|home| {
            std::fs::write(
                home.join("routing.json"),
                r#"{"domain_routes":[{"project":"hapi","domains":["equity_trading"]}]}"#,
            )
            .expect("write routing.json");

            let quant_db = home.join("projects").join("quant").join("memory.db");
            std::fs::create_dir_all(quant_db.parent().unwrap()).expect("mkdir quant");
            let global_db = home.join("global").join("memory.db");
            std::fs::create_dir_all(global_db.parent().unwrap()).expect("mkdir global");

            let hapi_db = home.join("projects").join("hapi").join("memory.db");
            std::fs::create_dir_all(hapi_db.parent().unwrap()).expect("mkdir hapi");
            memcore::MemoryStore::open_with_label(hapi_db.to_str().expect("utf-8 db path"), "hapi")
                .expect("init hapi schema");

            let params = CaptureSessionParams {
                conversation_id: "conv-1".to_string(),
                turn_id: "turn-1".to_string(),
                agent_id: "handler-test-agent".to_string(),
                messages: vec![Message {
                    role: "assistant".to_string(),
                    // A bracket self-evolution note: matches the "记住了"
                    // strategy pattern `extract_bracket_self_evolution_notes`
                    // requires — this path needs NO live LLM call, unlike
                    // the session-capture draft path.
                    content: "（记住了这次交易复盘的重要经验，下次要更谨慎）".to_string(),
                }],
                // #1114: this path prefix is what drives the HANDLER's own
                // domain inference (`infer_domain_from_row`'s `/trading/
                // equity` prefix rule) to "equity_trading" — the test never
                // tells the gate what domain to use.
                path_prefix: Some("/trading/equity".to_string()),
                scope: "project".to_string(),
                project: None,
                project_explicit: false,
                min_chars: 1,
                force: true,
            };

            // Oz r5 fixture fix: `handle_capture_session` unconditionally
            // calls `enqueue_capture_maintenance_jobs`, which `try_send`s
            // onto the foundry-maintenance mpsc channel — under `#[cfg(test)]`,
            // `background_workers_enabled()` (server_state/init.rs:37-46)
            // defaults OFF unless `TACHI_TEST_ENABLE_BACKGROUND_WORKERS` is
            // set, so `MemoryServer::new` never spawns the worker that would
            // hold the receiver open; the sender side is immediately
            // disconnected and the real handler call fails with "foundry
            // maintenance worker unavailable". This is a TEST-fixture gap,
            // not a production one — the fix is enabling the real worker for
            // this test, NOT teaching the handler to tolerate a missing one
            // (that would mask a genuinely dead worker in production, the
            // same "拒必有声" reasoning the write-affinity gate itself
            // follows). `MemoryServer::new` spawns via `tokio::spawn` when
            // workers are enabled, so it — and everything else — now runs
            // INSIDE the same `block_on`'d runtime, not constructed before
            // it: calling `tokio::spawn` with no active runtime context
            // panics.
            let _background_workers =
                crate::test_support::EnvRestore::set("TACHI_TEST_ENABLE_BACKGROUND_WORKERS", "1");

            // The spawned foundry-maintenance worker's `while let Some(item)
            // = rx.recv().await` loop (maintenance/worker.rs:120) only ever
            // exits when its Sender is dropped — but `server` (which owns
            // it) is kept alive below for the post-capture read assertions,
            // so that task genuinely never finishes on its own within this
            // test's lifetime. Explicitly bounding shutdown here (rather
            // than trusting `Runtime`'s implicit `Drop` to reap a
            // still-running spawned task within some unstated time budget)
            // is what guarantees this test can never hang instead of
            // failing fast.
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            let (response, server) = rt.block_on(async move {
                let server =
                    MemoryServer::new(global_db, Some(quant_db)).expect("bind quant daemon");
                let response = handle_capture_session(&server, params)
                    .await
                    .expect("capture_session should complete");
                (response, server)
            });
            rt.shutdown_timeout(std::time::Duration::from_millis(500));
            let parsed: serde_json::Value = serde_json::from_str(&response).expect("response JSON");
            assert_eq!(
                parsed["captured"].as_u64(),
                Some(1),
                "the bracket note must be captured: {parsed}"
            );

            let in_hapi = server
                .with_named_project_store_read("hapi", |store| {
                    store
                        .list_by_path("/trading/equity", 10, false)
                        .map_err(|e| e.to_string())
                })
                .expect("read hapi");
            assert_eq!(
                in_hapi.len(),
                1,
                "equity_trading content (inferred by the HANDLER from the entry's \
                 own path, not injected by this test) must reroute into hapi: {in_hapi:?}"
            );

            let in_quant = server
                .with_project_store_read(|store| {
                    store
                        .list_by_path("/trading/equity", 10, false)
                        .map_err(|e| e.to_string())
                })
                .expect("read quant");
            assert!(
                in_quant.is_empty(),
                "must NOT silently land in the daemon's own bound (quant) store: {in_quant:?}"
            );
        });
    }

    // #1114 codex round-3 item 3 point ③, capture_session-side (label +
    // by_destination fallback): DELIBERATELY NOT covered by a new runtime
    // test this round. `emit_session_captured_event` (what such a test
    // would check) runs AFTER `enqueue_capture_maintenance_jobs` within the
    // SAME per-destination-group loop iteration — unlike
    // `handle_capture_session_infers_domain_from_entry_and_reroutes` above
    // (which only needs state written BEFORE that call), there is no way to
    // observe this fix without the maintenance-enqueue call actually
    // succeeding, which needs `TACHI_TEST_ENABLE_BACKGROUND_WORKERS`
    // enabled. That would reintroduce EXACTLY the process-env-var
    // concurrency race item 5 was fixed for (`EnvRestore` prevents the
    // value LEAKING after this test ends; it does not prevent OTHER,
    // concurrently-running tests that don't hold `global_test_lock` from
    // OBSERVING it while set — and per codex's own grep,
    // `server_state/init.rs`'s tests around lines 371-379 are exactly such
    // tests) — multiplying the exact class of risk this round exists to
    // eliminate, not adding a new instance of it. A safe test would need
    // either (a) reordering `capture_session.rs` so the continuity event is
    // written before maintenance-enqueue, or (b) extracting the
    // destination-group loop body into a directly-testable seam — both are
    // production changes, out of scope for a test-quality-only round. The
    // fix itself (verified correct by codex's own production-logic review
    // this round) stays confirmed by code inspection only:
    // `emit_session_captured_event`'s call now passes `group_named_project
    // .as_deref()` instead of the stale `params.project.as_deref()`, and
    // `primary_session_event`/`primary_pipeline` fall back to whichever
    // destination group actually exists instead of a hardcoded "skipped"
    // placeholder — see capture_session.rs's own inline doc comments at
    // those two call sites for the reasoning.
}
