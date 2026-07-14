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

    let mut entries = Vec::<MemoryEntry>::new();
    for note in extract_bracket_self_evolution_notes(&params.agent_id, &params.messages) {
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
            target_db,
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

        entries.push(MemoryEntry {
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
            target_db,
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

        entries.push(MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            path: build_entry_path(&base_path, &topic),
            summary,
            text: draft.text.trim().to_string(),
            importance: draft.importance.clamp(0.0, 1.0),
            timestamp: Utc::now().to_rfc3339(),
            valid_from: String::new(),
            valid_until: None,
            category: normalize_category(&draft.category),
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
        });
    }

    let texts = entries
        .iter()
        .map(|entry| entry.text.clone())
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
        for (entry, vector) in entries.iter_mut().zip(vectors.iter()) {
            entry.vector = Some(vector.clone());
        }
    }

    let mut saved_ids = Vec::new();

    for entry in &entries {
        // KNOWN LIMITATION: only this entry's own persisted row is
        // re-targeted on a reroute — `enqueue_capture_maintenance_jobs`
        // below and the continuity `session_event`/pipeline still use the
        // pre-gate `target_db`/`named_project` for the whole batch, so a
        // rerouted entry's downstream maintenance job would look for it in
        // the pre-gate store. This only matters when a genuine cross-domain
        // mismatch actually fires (rare); flagged for the next holder of
        // this path rather than silently accepted.
        let (entry_target_db, entry_named_project) = resolve_capture_entry_write_target(
            server,
            entry,
            target_db,
            named_project.as_deref(),
            db_path.as_ref(),
            params.project_explicit,
        )?;
        persist_capture_entry(
            server,
            entry_target_db,
            entry_named_project.as_deref(),
            db_path.as_ref(),
            entry,
        )?;
        if embeddings.is_none() {
            queue_capture_enrichment(
                server,
                entry_target_db,
                entry_named_project.clone(),
                db_path.clone(),
                entry,
                false,
                Some(&params.agent_id),
                Some(&base_path),
            );
        }
        saved_ids.push(entry.id.clone());
    }

    let saved_ids = dedup_strings(saved_ids);
    let maintenance_jobs = enqueue_capture_maintenance_jobs(
        server,
        target_db,
        named_project.clone(),
        db_path.clone(),
        &params.agent_id,
        &base_path,
        &saved_ids,
        0,
        0,
    )?;
    let continuity_target = crate::continuity_ops::ContinuityEventTarget::new(
        target_db,
        named_project.clone(),
        db_path.clone(),
    );
    let session_event = crate::continuity_ops::emit_session_captured_event(
        server,
        &continuity_target,
        &params.conversation_id,
        &params.turn_id,
        &params.agent_id,
        &base_path,
        &saved_ids,
        params.messages.len(),
        params.project.as_deref(),
    );
    let continuity_pipeline = crate::continuity_ops::maybe_spawn_session_continuity_pipeline(
        server,
        continuity_target,
        params.conversation_id.clone(),
        params.turn_id.clone(),
        params.agent_id.clone(),
        params.project.clone(),
        params.messages.clone(),
    );

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
            "session_event": session_event,
            "pipeline": continuity_pipeline,
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
/// exists to catch. Entries here carry `domain: None` on the wire, so derive
/// one the same way `resolve_save_domain` does for `save_memory` (transient
/// — only used for this routing decision, never written back onto `entry`).
/// A `db_path` target (the manifest agent-pinned branch of
/// `resolve_capture_target`) is a deliberate per-agent DB assignment and is
/// never scrutinized here, same posture as continuity's own `db_path` skip
/// in `continuity_ops::storage::upsert_projection_memory`.
///
/// Pulled out of `handle_capture_session`'s loop so the routing decision is
/// unit-testable without an LLM/embedding call — see the `tests` module.
fn resolve_capture_entry_write_target(
    server: &MemoryServer,
    entry: &MemoryEntry,
    target_db: DbScope,
    named_project: Option<&str>,
    db_path: Option<&std::path::PathBuf>,
    project_explicit: bool,
) -> Result<(DbScope, Option<String>), String> {
    if db_path.is_some() {
        return Ok((target_db, named_project.map(str::to_string)));
    }
    // `repair_target` returns `None` both when nothing needs repairing (an
    // already-clean domain is unchanged) and when there was truly nothing to
    // infer — same `.or_else` fallback `resolve_save_domain` (save_memory)
    // and `projection_domain_label` (continuity) both apply, so an already
    // -valid `entry.domain` is never dropped just because it didn't need a
    // repair.
    let domain = crate::repair::domain::repair_target(
        entry.domain.as_deref(),
        &entry.path,
        &entry.category,
        "foundry_capture",
    )
    .or_else(|| entry.domain.clone());
    let affinity =
        crate::memory_search_ops::save_memory::write_affinity::apply_write_affinity_for_domain(
            server,
            domain.as_deref(),
            target_db,
            named_project,
            project_explicit,
            false, // id_resolves_at_target: capture entries are freshly materialized here, no pre-check of an existing row at this call site
        )?;
    Ok((affinity.target_db, affinity.named_project))
}

#[cfg(test)]
mod affinity_tests {
    use super::resolve_capture_entry_write_target;
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

            let quant_db = home.join("projects").join("quant").join("memory.db");
            std::fs::create_dir_all(quant_db.parent().unwrap()).expect("mkdir quant");
            let global_db = home.join("global").join("memory.db");
            std::fs::create_dir_all(global_db.parent().unwrap()).expect("mkdir global");
            // `MemoryServer::new(global, project)` — the project path (not the
            // first/global one) is what `bound_project_label` resolves "quant"
            // from via the Plan C `projects/<name>/memory.db` convention.
            let server =
                MemoryServer::new(global_db, Some(quant_db.clone())).expect("bind quant daemon");
            // Pre-mount "hapi" (open-or-create semantics — see write_affinity's
            // F8 doc note) so `named_project_db_exists("hapi")` is true.
            server
                .with_named_project_store("hapi", |_store| Ok::<(), String>(()))
                .expect("create hapi store");

            let entry = entry_with_domain("cap-1", Some("equity_trading"));
            let (target_db, named_project) = resolve_capture_entry_write_target(
                &server,
                &entry,
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

            let quant_db = home.join("projects").join("quant").join("memory.db");
            std::fs::create_dir_all(quant_db.parent().unwrap()).expect("mkdir quant");
            let global_db = home.join("global").join("memory.db");
            std::fs::create_dir_all(global_db.parent().unwrap()).expect("mkdir global");
            // `MemoryServer::new(global, project)` — the project path (not the
            // first/global one) is what `bound_project_label` resolves "quant"
            // from via the Plan C `projects/<name>/memory.db` convention.
            let server =
                MemoryServer::new(global_db, Some(quant_db.clone())).expect("bind quant daemon");
            // "hapi" is never mounted here.

            let entry = entry_with_domain("cap-2", Some("equity_trading"));
            let err = resolve_capture_entry_write_target(
                &server,
                &entry,
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
            let quant_db = home.join("projects").join("quant").join("memory.db");
            std::fs::create_dir_all(quant_db.parent().unwrap()).expect("mkdir quant");
            let global_db = home.join("global").join("memory.db");
            std::fs::create_dir_all(global_db.parent().unwrap()).expect("mkdir global");
            // `MemoryServer::new(global, project)` — the project path (not the
            // first/global one) is what `bound_project_label` resolves "quant"
            // from via the Plan C `projects/<name>/memory.db` convention.
            let server =
                MemoryServer::new(global_db, Some(quant_db.clone())).expect("bind quant daemon");

            let entry = entry_with_domain("cap-3", None);
            let (target_db, named_project) = resolve_capture_entry_write_target(
                &server,
                &entry,
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

            let quant_db = home.join("projects").join("quant").join("memory.db");
            std::fs::create_dir_all(quant_db.parent().unwrap()).expect("mkdir quant");
            let global_db = home.join("global").join("memory.db");
            std::fs::create_dir_all(global_db.parent().unwrap()).expect("mkdir global");
            // `MemoryServer::new(global, project)` — the project path (not the
            // first/global one) is what `bound_project_label` resolves "quant"
            // from via the Plan C `projects/<name>/memory.db` convention.
            let server =
                MemoryServer::new(global_db, Some(quant_db.clone())).expect("bind quant daemon");

            let entry = entry_with_domain("cap-4", Some("equity_trading"));
            let pinned = home.join("projects").join("pinned-agent-db").join("memory.db");
            let (target_db, named_project) = resolve_capture_entry_write_target(
                &server,
                &entry,
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
}
