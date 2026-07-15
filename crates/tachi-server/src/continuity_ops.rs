use chrono::{SecondsFormat, Utc};
use memcore::TachiEventQuery;
use memory_server_runtime::{query_limit, trim_opt};

use crate::tool_params::TachiEventParams;
use crate::MemoryServer;

mod context;
mod emit;
mod feedback;
mod outcome;
mod parsing;
mod pipeline;
mod projection;
mod promotion;
mod read_models;
mod storage;

pub(crate) use self::context::{build_a2a_context, build_continuity_context, list_active_patterns};
pub(crate) use self::emit::{
    emit_memory_saved_event, emit_pattern_feedback_event, emit_pattern_seen_events,
    emit_session_captured_event, emit_task_completion_events, emit_wiki_saved_event,
    WikiSavedEventInput,
};
pub(crate) use self::feedback::{
    attach_pattern_ref_to_row, emit_pattern_feedback_for_refs, pattern_feedback_refs_from_strings,
    pattern_ref_json,
};
pub(crate) use self::outcome::evaluate_outcome_labels;
pub(crate) use self::parsing::{parse_continuity_candidate_batch, parse_continuity_outcome_label};
pub(crate) use self::pipeline::maybe_spawn_session_continuity_pipeline;
pub(crate) use self::projection::{
    project_auto_continuity_events_for_target, project_continuity_events,
};
pub(crate) use self::promotion::promote_pattern_review_artifacts;
#[cfg(test)]
use self::storage::list_projection_memories;
pub(crate) use self::storage::ContinuityEventTarget;
#[cfg(test)]
use memcore::{
    AuthorityLevel, EffectScope, OutcomeEvidenceBasis, ProjectionKind, SessionOutcomeKind,
    TachiEventRecord,
};
#[cfg(test)]
use serde_json::json;

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn stable_event_payload_id(parts: &[&str]) -> String {
    let joined = parts
        .iter()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("|");
    format!("event-{}", crate::utils::stable_hash(&joined))
}

fn target_from_event_params(
    server: &MemoryServer,
    params: &TachiEventParams,
) -> ContinuityEventTarget {
    ContinuityEventTarget::from_default_write(server, params.project.as_deref())
}

fn event_query_from_params(params: &TachiEventParams) -> TachiEventQuery {
    TachiEventQuery {
        project: trim_opt(&params.project),
        domain: trim_opt(&params.domain),
        event_type: trim_opt(&params.event_type),
        session_id: trim_opt(&params.session_id),
        source_repo: trim_opt(&params.source_repo),
        adapter: trim_opt(&params.adapter),
        limit: query_limit(params.limit),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DbScope;
    use memcore::MemoryEntry;

    /// Minimal ordinary (non-component) memory so a causal edge's endpoints
    /// exist and projection reaches the ontology gate rather than the
    /// endpoint-missing skip.
    fn min_memory_entry(id: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/".to_string(),
            summary: String::new(),
            text: "kill-test memory".to_string(),
            importance: 0.5,
            timestamp: now_rfc3339(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "test".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: json!({}),
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    /// Sol post-adjudication kill-test ①: continuity projection forwards a
    /// dynamic `relation="owns"` (a #772 grandfathered relation) between two
    /// ordinary, non-component memories. The generic `add_edge` choke point
    /// must reject it — and projection must stay **fail-soft**: the edge is
    /// dropped into `skipped`, zero edges land, and the run still completes
    /// (no panic, no hard error bubbling up to boot/projection).
    #[test]
    fn continuity_projection_rejects_grandfathered_relation_fail_soft() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = dir.path().join("memory.db");
        let server = MemoryServer::new(db, None).expect("test server");

        // Two ordinary memories so both edge endpoints exist.
        server
            .with_global_store(|store| {
                store
                    .upsert(&min_memory_entry("mem-src"))
                    .map_err(|e| e.to_string())?;
                store
                    .upsert(&min_memory_entry("mem-tgt"))
                    .map_err(|e| e.to_string())?;
                // A timeline event carrying a causal edge with a grandfathered
                // relation — exactly the laundering path this rework closes.
                let event = TachiEventRecord {
                    id: "timeline-owns-1".to_string(),
                    source_repo: "tachi".to_string(),
                    adapter: "test".to_string(),
                    project: "sigil".to_string(),
                    domain: "agent_os".to_string(),
                    session_id: "s1".to_string(),
                    actor: "codex".to_string(),
                    event_type: "session.captured".to_string(),
                    authority: AuthorityLevel::CollectOnly,
                    effects: vec![EffectScope::None],
                    projection_hints: vec![ProjectionKind::Timeline],
                    payload: json!({
                        "summary": "Timeline with a laundered governance edge",
                        "text": "projection should reject the owns relation",
                        "projection_key": "timeline-owns",
                        "causal_edges": [{
                            "source_id": "mem-src",
                            "target_id": "mem-tgt",
                            "relation": "owns",
                        }],
                    }),
                    provenance: json!({"source": "test"}),
                    created_at: now_rfc3339(),
                };
                store.insert_tachi_event(&event).map_err(|e| e.to_string())
            })
            .expect("seed event + memories");

        // Fail-soft: the run completes without erroring out.
        let report = project_auto_continuity_events_for_target(
            &server,
            ContinuityEventTarget::new(DbScope::Global, None, None),
            20,
        )
        .expect("projection must not hard-error on a rejected edge");
        assert_eq!(
            report["status"],
            json!("completed"),
            "projection must stay fail-soft, got: {report}"
        );
        // The timeline projection itself was produced (so we actually reached
        // the edge-persist path), but the grandfathered edge was skipped.
        assert!(
            report["projected_count"].as_u64().unwrap_or(0) >= 1,
            "timeline projection should have been produced: {report}"
        );

        // Zero edges landed in the store.
        server
            .with_global_store_read(|store| {
                let out = store
                    .get_edges("mem-src", "outgoing", None)
                    .map_err(|e| e.to_string())?;
                assert!(
                    out.is_empty(),
                    "grandfathered relation must not land via projection, got {out:?}"
                );
                Ok::<(), String>(())
            })
            .expect("read edges");
    }

    #[test]
    fn parses_continuity_candidates_with_projection_aliases() {
        let raw = r#"{
          "candidates": [
            {
              "projection": "timeline",
              "summary": "User reframed scope",
              "text": "The session moved from ROI judgment to code mapping.",
              "confidence": 0.74,
              "evidence_refs": ["message:4"]
            },
            {
              "kind": "worldbook",
              "summary": "Tachi substrate",
              "metadata": {"domain": "agent_os"}
            }
          ],
          "open_threads": ["wire projectors"]
        }"#;

        let parsed = parse_continuity_candidate_batch(raw).expect("parse candidates");
        assert_eq!(parsed.candidates.len(), 2);
        assert_eq!(parsed.candidates[0].projection, ProjectionKind::Timeline);
        assert_eq!(parsed.candidates[1].projection, ProjectionKind::WorldBook);
        assert_eq!(parsed.open_threads, vec!["wire projectors"]);
    }

    #[test]
    fn parses_outcome_label_into_typed_axes() {
        let raw = r#"{
          "outcome": "partial_reframe",
          "evidence_basis": "interlocutor_argument",
          "confidence": 0.61,
          "rationale": "Both sides changed scope.",
          "claims": ["enum too coarse"],
          "open_questions": ["external label source"]
        }"#;

        let parsed = parse_continuity_outcome_label(raw).expect("parse outcome");
        assert_eq!(parsed.outcome, SessionOutcomeKind::PartialReframe);
        assert_eq!(
            parsed.evidence_basis,
            OutcomeEvidenceBasis::InterlocutorArgument
        );
        assert_eq!(parsed.claims, vec!["enum too coarse"]);
    }

    #[test]
    fn emits_session_captured_event_to_target_store() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = dir.path().join("memory.db");
        let server = MemoryServer::new(db, None).expect("test server");
        let target = ContinuityEventTarget::new(DbScope::Global, None, None);
        let status = emit_session_captured_event(
            &server,
            &target,
            "conversation-1",
            "turn-1",
            "codex",
            "/agents/codex",
            &["memory-1".to_string()],
            3,
            Some("sigil"),
        );
        assert_eq!(status["status"], json!("saved"));

        let events = server
            .with_global_store_read(|store| {
                store
                    .list_tachi_events(&memcore::TachiEventQuery {
                        event_type: Some("session.captured".to_string()),
                        session_id: Some("conversation-1".to_string()),
                        limit: 5,
                        ..memcore::TachiEventQuery::default()
                    })
                    .map_err(|e| e.to_string())
            })
            .expect("list events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].project, "sigil");
        assert_eq!(
            events[0].projection_hints,
            vec![ProjectionKind::Timeline, ProjectionKind::ProjectCycle]
        );
        assert_eq!(
            events[0].payload["captured_memory_ids"][0],
            json!("memory-1")
        );
    }

    #[test]
    fn auto_projection_skips_execution_gate_events() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = dir.path().join("memory.db");
        let server = MemoryServer::new(db, None).expect("test server");
        server
            .with_global_store(|store| {
                let allowed = TachiEventRecord {
                    id: "pattern-candidate-1".to_string(),
                    source_repo: "tachi".to_string(),
                    adapter: "test".to_string(),
                    project: "sigil".to_string(),
                    domain: "agent_os".to_string(),
                    session_id: "s1".to_string(),
                    actor: "codex".to_string(),
                    event_type: "pattern.candidate".to_string(),
                    authority: AuthorityLevel::CollectOnly,
                    effects: vec![EffectScope::None],
                    projection_hints: vec![ProjectionKind::Pattern],
                    payload: json!({
                        "summary": "Continuity-first planning",
                        "text": "Use continuity evidence before picking the next project-management action.",
                        "projection_key": "continuity-first"
                    }),
                    provenance: json!({"source": "test"}),
                    created_at: now_rfc3339(),
                };
                let blocked = TachiEventRecord {
                    id: "execution-gate-1".to_string(),
                    source_repo: "tachi".to_string(),
                    adapter: "test".to_string(),
                    project: "sigil".to_string(),
                    domain: "agent_os".to_string(),
                    session_id: "s1".to_string(),
                    actor: "codex".to_string(),
                    event_type: "evidence_gate.required".to_string(),
                    authority: AuthorityLevel::ExecutionGate,
                    effects: vec![EffectScope::Execution],
                    projection_hints: vec![ProjectionKind::EvidenceGate],
                    payload: json!({
                        "summary": "Do not ship without tests",
                        "projection_key": "must-test"
                    }),
                    provenance: json!({"source": "test"}),
                    created_at: now_rfc3339(),
                };
                store.insert_tachi_event(&allowed).map_err(|e| e.to_string())?;
                store.insert_tachi_event(&blocked).map_err(|e| e.to_string())
            })
            .expect("seed events");

        let report = project_auto_continuity_events_for_target(
            &server,
            ContinuityEventTarget::new(DbScope::Global, None, None),
            20,
        )
        .expect("project events");
        assert_eq!(report["projected_count"], json!(1));
        assert_eq!(report["skipped_count"], json!(1));

        let patterns = list_projection_memories(
            &server,
            &ContinuityEventTarget::new(DbScope::Global, None, None),
            "/user/patterns",
            10,
        )
        .expect("list patterns");
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0].summary, "Continuity-first planning");
        let gates = list_projection_memories(
            &server,
            &ContinuityEventTarget::new(DbScope::Global, None, None),
            "/evidence-gates",
            10,
        )
        .expect("list gates");
        assert!(gates.is_empty());
    }

    /// #1114 codex round-3 item 4 fix: replaces a HOLLOW test (`storage.rs`'s
    /// `read_failure_at_pretarget_propagates_instead_of_being_treated_as_absence`,
    /// removed) that only asserted `get_projection_memory` itself returns
    /// `Err` on a read failure — true both before AND after the #1114 fix,
    /// since that helper already propagated errors at the base level. The
    /// line actually changed was ONE LAYER UP, in `projection.rs`'s loop:
    /// `id_resolves_at_pretarget = get_projection_memory(...).ok().flatten()
    /// .is_some()` silently collapsed a read failure to `false` ("doesn't
    /// exist") before the #1114 fix; `?` now propagates it. This test drives
    /// the REAL entry point (`project_auto_continuity_events_for_target`)
    /// and proves THAT layer surfaces the failure as a real `Err`, not a
    /// silently-completed report.
    ///
    /// Constructing "read_events succeeds, but the projection-write target's
    /// existence check fails" against the SAME store (continuity events and
    /// their projections share one store) needs the failure to be scoped to
    /// exactly the `memories` table, not the whole file — a raw
    /// `DROP TABLE memories` (leaving `tachi_events` intact) does that
    /// precisely, without needing any `RoutingConfig`/`routing.json`
    /// involvement at all: this pre-gate check runs unconditionally, before
    /// domain resolution or the write-affinity gate are ever reached, so
    /// this test has NONE of the routing-provider exposure #1114's OTHER,
    /// routing-dependent tests carry (see
    /// `memory_search_ops::save_memory::write_affinity::tests::
    /// config_change_between_calls_reroutes_a_stable_id_to_a_different_store`'s
    /// doc for that limitation, and this file's own
    /// `project_continuity_events_hard_aborts_on_write_affinity_refusal`/
    /// `project_auto_continuity_events_soft_continues_with_typed_error_kind_on_refusal`
    /// below, which DO carry it) — this one is reliable under both nextest
    /// and plain `cargo test`.
    #[test]
    fn project_auto_continuity_events_propagates_a_downstream_read_failure() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = dir.path().join("memory.db");
        let server = MemoryServer::new(db.clone(), None).expect("test server");

        server
            .with_global_store(|store| {
                let event = TachiEventRecord {
                    id: "session-captured-read-failure".to_string(),
                    source_repo: "tachi".to_string(),
                    adapter: "test".to_string(),
                    project: "sigil".to_string(),
                    domain: "agent_os".to_string(),
                    session_id: "s1".to_string(),
                    actor: "codex".to_string(),
                    event_type: "session.captured".to_string(),
                    authority: AuthorityLevel::RawFact,
                    effects: vec![EffectScope::MemoryWrite, EffectScope::ProjectCycle],
                    projection_hints: vec![ProjectionKind::Timeline, ProjectionKind::ProjectCycle],
                    payload: json!({
                        "summary": "session captured before the memories table vanished",
                        "text": "auto-projectable event seeded for the read-failure test",
                        "projection_key": "read-failure-timeline",
                    }),
                    provenance: json!({"source": "test"}),
                    created_at: now_rfc3339(),
                };
                store.insert_tachi_event(&event).map_err(|e| e.to_string())
            })
            .expect("seed one auto-projectable event");

        // Drop ONLY the `memories` table (leaving `tachi_events` intact) —
        // `read_events` (which queries `tachi_events`) still succeeds and
        // returns the seeded event; the per-projection pre-gate existence
        // check (which queries `memories`) now genuinely fails to read,
        // exactly the scenario the #1114 fix propagates instead of
        // silently swallowing.
        {
            let raw = rusqlite::Connection::open(&db).expect("open raw sqlite connection");
            raw.execute("DROP TABLE memories", [])
                .expect("drop the memories table");
        }

        let result = project_auto_continuity_events_for_target(
            &server,
            ContinuityEventTarget::new(DbScope::Global, None, None),
            20,
        );
        let err = result.expect_err(
            "a genuine read failure on the projection-write target must propagate as Err, \
             not silently complete as if the row simply didn't exist",
        );
        assert!(
            err.contains("memories") || err.contains("no such table"),
            "expected the underlying SQLite error to surface, got: {err}"
        );
    }

    /// `params.domain`, unlike an event's own `domain` field, is only a
    /// QUERY filter here (`event_query_from_params` feeds it straight into
    /// `TachiEventQuery::domain`) — left `None` so the call picks up every
    /// seeded event regardless of its own domain, matching how a real
    /// `action=project` call is normally invoked (no domain filter).
    fn project_action_params() -> TachiEventParams {
        TachiEventParams {
            action: "project".to_string(),
            format: None,
            id: None,
            source_repo: None,
            adapter: None,
            project: None,
            project_explicit: false,
            domain: None,
            session_id: None,
            actor: None,
            event_type: None,
            authority: None,
            effects: Vec::new(),
            projection_hints: vec!["timeline".to_string()],
            payload: None,
            provenance: None,
            created_at: None,
            limit: 20,
            path_prefix: None,
            dry_run: false,
        }
    }

    fn seed_trading_event(server: &MemoryServer, id: &str, key: &str) {
        server
            .with_project_store(|store| {
                let event = TachiEventRecord {
                    id: id.to_string(),
                    source_repo: "tachi".to_string(),
                    adapter: "test".to_string(),
                    project: "quant".to_string(),
                    domain: "equity_trading".to_string(),
                    session_id: "s1".to_string(),
                    actor: "codex".to_string(),
                    event_type: "session.captured".to_string(),
                    authority: AuthorityLevel::RawFact,
                    effects: vec![EffectScope::MemoryWrite, EffectScope::ProjectCycle],
                    projection_hints: vec![ProjectionKind::Timeline],
                    payload: json!({
                        "summary": "trading content routed to an unmounted store",
                        "text": "write-affinity refusal scenario",
                        "projection_key": key,
                    }),
                    provenance: json!({"source": "test"}),
                    created_at: now_rfc3339(),
                };
                store.insert_tachi_event(&event).map_err(|e| e.to_string())
            })
            .expect("seed trading event");
    }

    fn seed_engineering_event(server: &MemoryServer, id: &str, key: &str) {
        server
            .with_project_store(|store| {
                let event = TachiEventRecord {
                    id: id.to_string(),
                    source_repo: "tachi".to_string(),
                    adapter: "test".to_string(),
                    project: "quant".to_string(),
                    domain: "engineering".to_string(),
                    session_id: "s1".to_string(),
                    actor: "codex".to_string(),
                    event_type: "session.captured".to_string(),
                    authority: AuthorityLevel::RawFact,
                    effects: vec![EffectScope::MemoryWrite, EffectScope::ProjectCycle],
                    projection_hints: vec![ProjectionKind::Timeline],
                    payload: json!({
                        "summary": "unregistered-domain content, must project normally",
                        "text": "ordinary passthrough scenario",
                        "projection_key": key,
                    }),
                    provenance: json!({"source": "test"}),
                    created_at: now_rfc3339(),
                };
                store.insert_tachi_event(&event).map_err(|e| e.to_string())
            })
            .expect("seed engineering event");
    }

    fn bound_quant_server(home: &std::path::Path) -> MemoryServer {
        let quant_db = home.join("projects").join("quant").join("memory.db");
        std::fs::create_dir_all(quant_db.parent().unwrap()).expect("mkdir quant");
        let global_db = home.join("global").join("memory.db");
        std::fs::create_dir_all(global_db.parent().unwrap()).expect("mkdir global");
        MemoryServer::new(global_db, Some(quant_db)).expect("bind quant daemon")
    }

    // #1114 codex round-3 item 5 note (applies to BOTH tests below, not
    // repeated per-test): these two construct their own `MemoryServer`, so
    // `apply_write_affinity_for_domain` reads that server's home-bound
    // provider. Other servers in a shared-process `cargo test` cannot poison
    // this route; this is the #1126 identity-bound cache invariant.

    /// #1114 codex round-3 item 3 point ① discriminating RED test: a live,
    /// explicit `action=project` call (`project_continuity_events`,
    /// `auto_only == false` always) must HARD-ABORT when the write-affinity
    /// gate refuses a row — a real `Err`, never silently folded into an
    /// `Ok({"status": "partial", ...})` the caller might not inspect.
    /// Reverting `projection.rs`'s `!auto_only` branch of the hard-abort
    /// condition turns this from red to green incorrectly (i.e. makes it
    /// pass when it shouldn't) — this test is what would catch that.
    #[test]
    fn project_continuity_events_hard_aborts_on_write_affinity_refusal() {
        crate::test_support::with_tachi_home(|home| {
            std::fs::write(
                home.join("routing.json"),
                r#"{"domain_routes":[{"project":"hapi","domains":["equity_trading"]}]}"#,
            )
            .expect("write routing.json");
            let server = bound_quant_server(home);
            // "hapi" is registered but never mounted.
            seed_trading_event(&server, "explicit-refusal-1", "explicit-refusal");

            let params = project_action_params();
            let result = project_continuity_events(&server, &params);
            let err = result.expect_err(
                "an explicit action=project call must hard-abort on a write-affinity \
                 refusal, not silently report status: partial",
            );
            assert!(err.contains("equity_trading"));
            assert!(err.contains("hapi"));
        });
    }

    /// #1114 codex round-3 item 3 point ② discriminating RED test: the
    /// background auto-sweep (`project_auto_continuity_events_for_target`,
    /// `auto_only == true` always) must SOFT-CONTINUE on an ordinary
    /// per-domain write-affinity refusal — `Ok(...)` with `status:
    /// "partial"`, the refused row's error entry carrying a typed
    /// `error_kind`, and OTHER rows in the same sweep still processed.
    /// Reverting the auto-sweep to hard-abort (or dropping `error_kind`
    /// from the JSON) turns this from green to red.
    #[test]
    fn project_auto_continuity_events_soft_continues_with_typed_error_kind_on_refusal() {
        crate::test_support::with_tachi_home(|home| {
            std::fs::write(
                home.join("routing.json"),
                r#"{"domain_routes":[{"project":"hapi","domains":["equity_trading"]}]}"#,
            )
            .expect("write routing.json");
            let server = bound_quant_server(home);
            // "hapi" is registered but never mounted -> the trading event
            // must be refused; "engineering" has no registered route at
            // all -> must project normally (proves the sweep continues).
            seed_trading_event(&server, "auto-refusal-1", "auto-refusal");
            seed_engineering_event(&server, "auto-ok-1", "auto-ok");

            let report = project_auto_continuity_events_for_target(
                &server,
                ContinuityEventTarget::new(DbScope::Project, None, None),
                20,
            )
            .expect(
                "the auto-sweep must soft-continue (Ok), not hard-abort, on an \
                 ordinary per-domain refusal",
            );
            assert_eq!(
                report["status"],
                json!("partial"),
                "one refused row must still surface as a partial batch: {report}"
            );
            assert!(
                report["projected_count"].as_u64().unwrap_or(0) >= 1,
                "the unaffected (engineering) event must still have been \
                 projected — the sweep must not stop at the first refusal: {report}"
            );
            let errors = report["errors"].as_array().expect("errors array");
            assert_eq!(
                errors.len(),
                1,
                "exactly the trading row must be refused: {report}"
            );
            assert_eq!(
                errors[0]["error_kind"],
                json!("unmounted_route"),
                "the refusal must carry a typed, machine-checkable kind, not just a \
                 stringified message: {report}"
            );
        });
    }
}
