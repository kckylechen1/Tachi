//! Host-native spawn receipt bridge (#1249, #757 leaf a).
//!
//! Host agent runtimes (OpenClaw `subagent_spawned`, integration #1) spawn
//! subagents Tachi never dispatched. Those spawns are observed via plugin
//! event but were never recorded in any canonical ledger, so the dispatch
//! census / router could not see them (recovery scans only Tachi-created run
//! dirs — `dispatch_ops::dispatch::recovery`).
//!
//! ## Where the receipt lands (Option B, owner-ratified on #1249)
//!
//! The #757 leader ruling's "Execution shape" wrote *"canonical
//! `dispatch_outcomes` row"*, but that wording predated reconciliation with
//! the already-merged **`mirror_eval` (#1066)** ledger, which was purpose-built
//! for exactly this population: *"a host-native subagent Tachi only observes"*
//! (`memcore::db::mirror_eval`, `schema::ddl`). #1066 deliberately kept these
//! rows OUT of `dispatch_outcomes` to avoid polluting dispatch-only
//! aggregation, and reserved the "literal table reuse?" question as an owner
//! SCOPE-GAP (#1186). The #1249 ruling discharges #1186 as
//! REJECTED-literal-reuse: this bridge writes the host-native receipt into the
//! `mirror_eval` ledger via the SAME `register` / `observe` primitives the
//! `tachi_agent_eval` facade uses — it does not fork a second receipt
//! vocabulary and does not touch `dispatch_outcomes`. A unioned census/router
//! read surface over both ledgers is a separate follow-up, not this leaf.
//!
//! ## Lifecycle → primitive mapping
//!
//! | Host event | mirror_eval action |
//! |---|---|
//! | `host.subagent_spawned` | `register` (mint/replay the run) |
//! | `host.subagent_ended`   | `observe` (terminal fact snapshot) |
//!
//! Idempotency anchor is the host-native child id (`native_child_id =
//! child_session_key`); a re-delivered `spawned` replays onto the same run
//! (`register_mirror_eval_run` returns the existing row when content matches),
//! and a re-delivered `ended` replays onto the same observation. Both writes go
//! to the GLOBAL store — the same store `agent_eval::mirror` writes through — so
//! a bridge-written run and a facade-written observation for the same
//! `native_child_id` resolve to one row.
//!
//! ## Fail-safe
//!
//! [`bridge_host_spawn_event`] never returns an error and never fails the
//! enclosing `tachi_event(action="emit")`: a missing field, a register
//! conflict, or a DB error is logged and swallowed. The event ledger write is
//! authoritative; the mirror_eval projection is best-effort, exactly like the
//! `dispatch_outcome` writers are best-effort alongside `tachi_complete`.

use memcore::{NewMirrorEvalObservation, NewMirrorEvalRun, TachiEventRecord};

use crate::MemoryServer;

/// Event types this bridge acts on. Every other event_type is a no-op.
pub(crate) const SPAWNED_EVENT_TYPE: &str = "host.subagent_spawned";
pub(crate) const ENDED_EVENT_TYPE: &str = "host.subagent_ended";

/// `execution_origin` for every run this bridge registers — the #1066
/// vocabulary token for a host-native subagent Tachi only observes.
const EXECUTION_ORIGIN: &str = "host_native_subagent";

/// `lifecycle_owner` — Tachi never claims it can wait/cancel/close a
/// host-owned worker (per the register contract, `orchestration.rs`).
const LIFECYCLE_OWNER: &str = "host";

/// Sentinel `frozen_contract_ref` for a host-native spawn observed without an
/// issue/PR binding. `register` requires a non-empty contract ref; a stable
/// sentinel keeps idempotent replay stable, because the register content-match
/// compares `frozen_contract_ref` field-by-field — a per-call value (e.g. a
/// UUID) would turn every re-delivery into a spurious register CONFLICT.
const UNBOUND_CONTRACT: &str = "host_native_subagent:unbound";

/// Bridge a host lifecycle event into the `mirror_eval` ledger.
///
/// Fail-safe: dispatches on `event.event_type`; any error is logged and
/// swallowed so the enclosing `emit` is never affected. A non-host-spawn event
/// is an immediate no-op.
pub(crate) fn bridge_host_spawn_event(server: &MemoryServer, event: &TachiEventRecord) {
    let result = match event.event_type.as_str() {
        SPAWNED_EVENT_TYPE => register_spawn(server, event),
        ENDED_EVENT_TYPE => observe_terminal(server, event),
        _ => return,
    };
    if let Err(error) = result {
        tracing::warn!(
            error = %error,
            event_type = %event.event_type,
            "host-native spawn receipt bridge skipped (fail-safe; emit unaffected)"
        );
    }
}

/// `host.subagent_spawned` → `register`. Mints a mirror_eval run keyed on the
/// host child id; a duplicate delivery replays onto the same row.
fn register_spawn(server: &MemoryServer, event: &TachiEventRecord) -> Result<(), String> {
    let native_child_id = child_session_key(event)
        .ok_or("host.subagent_spawned carried no child_session_key to anchor on")?;
    let new = NewMirrorEvalRun {
        frozen_contract_ref: contract_ref(event),
        execution_origin: EXECUTION_ORIGIN.to_string(),
        lifecycle_owner: LIFECYCLE_OWNER.to_string(),
        harness: harness(event),
        native_child_id: Some(native_child_id),
        requested_profile: None,
        requested_model: None,
        // The host tells us a subagent label, not a resolved model/profile —
        // record it as the requested agent, never invent a model identity.
        requested_agent: label(event),
    };
    server.with_global_store(|store| {
        memcore::register_mirror_eval_run(store.connection(), &new)
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
}

/// `host.subagent_ended` → `observe`. Resolves the run by host child id and
/// records the terminal fact snapshot with the host outcome normalized to the
/// kanban vocabulary. A missing run (spawned never bridged, e.g. daemon
/// restarted between the two events) is a swallowed skip via the caller.
fn observe_terminal(server: &MemoryServer, event: &TachiEventRecord) -> Result<(), String> {
    let native_child_id = child_session_key(event)
        .ok_or("host.subagent_ended carried no child_session_key to resolve on")?;
    let eval_run_id = server
        .with_global_store_read(|store| {
            memcore::get_run_by_native_child_id(store.connection(), &native_child_id)
                .map_err(|e| e.to_string())
        })?
        .map(|run| run.eval_run_id)
        .ok_or_else(|| {
            format!(
                "no mirror_eval run registered for native_child_id '{native_child_id}' \
                 (spawned event not bridged?); terminal observation skipped"
            )
        })?;
    let new = NewMirrorEvalObservation {
        eval_run_id,
        terminal_outcome: normalize_host_outcome(host_outcome(event).as_deref()),
        duration_ms: duration_ms(event),
        ..Default::default()
    };
    server.with_global_store(|store| {
        memcore::record_mirror_eval_observation(store.connection(), &new)
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
}

/// Map a free-text host outcome to the SAME kanban terminal vocabulary
/// `status_ops::normalize_dispatch_outcome` and `dispatch_outcomes` speak
/// (`completed`/`failed`/`aborted`/`partial`/`unknown`) — this bridge reuses
/// that vocabulary rather than forking a mirror-only one. `unknown` (never an
/// empty string) is the floor, so `record_mirror_eval_observation`'s non-empty
/// `terminal_outcome` gate is always satisfied.
fn normalize_host_outcome(raw: Option<&str>) -> String {
    match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some(
            "success" | "succeeded" | "completed" | "complete" | "done" | "ok" | "pass"
            | "passed",
        ) => "completed",
        Some("failure" | "failed" | "fail" | "error" | "errored") => "failed",
        Some("cancelled" | "canceled" | "aborted" | "abort" | "interrupted" | "killed") => {
            "aborted"
        }
        Some("partial" | "input_required" | "needs_input" | "incomplete") => "partial",
        _ => "unknown",
    }
    .to_string()
}

/// Deterministic secret scrub applied to every free-text/id field before it
/// enters storage or is used as a lookup key — the SAME scrub
/// `agent_eval::mirror` applies on the facade path, so a bridge-written run and
/// a facade-written observation for the same (non-secret-shaped) child id
/// resolve to one row. A legitimate id scrubs to itself.
fn scrub(text: &str) -> String {
    crate::memory_search_ops::scrub_secrets(text).0
}

fn payload_str(event: &TachiEventRecord, key: &str) -> Option<String> {
    event
        .payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Host-native child id, scrubbed. Anchors register + observe idempotency.
fn child_session_key(event: &TachiEventRecord) -> Option<String> {
    payload_str(event, "child_session_key").map(|value| scrub(&value))
}

fn label(event: &TachiEventRecord) -> Option<String> {
    payload_str(event, "label").map(|value| scrub(&value))
}

/// The submitting adapter is the host harness (e.g. `openclaw`).
fn harness(event: &TachiEventRecord) -> Option<String> {
    let adapter = event.adapter.trim();
    (!adapter.is_empty()).then(|| scrub(adapter))
}

fn host_outcome(event: &TachiEventRecord) -> Option<String> {
    payload_str(event, "outcome")
}

fn duration_ms(event: &TachiEventRecord) -> Option<u64> {
    event
        .payload
        .get("duration_ms")
        .and_then(serde_json::Value::as_u64)
}

/// A declared issue/PR contract if the host event carries one, else the stable
/// unbound sentinel. Checked in payload first, then provenance.
fn contract_ref(event: &TachiEventRecord) -> String {
    for key in ["issue_ref", "frozen_contract_ref", "contract_ref"] {
        if let Some(value) = payload_str(event, key) {
            return scrub(&value);
        }
        if let Some(value) = event
            .provenance
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return scrub(value);
        }
    }
    UNBOUND_CONTRACT.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use memcore::{AuthorityLevel, EffectScope, ProjectionKind};
    use serde_json::json;

    /// Hermetic server over a temp global DB. mirror_eval is a DB ledger (not a
    /// `TACHI_RUN_ROOT` run-dir artifact), so isolation comes from the temp
    /// global store, not env pinning — the run-dir path this bridge never
    /// touches. Mirrors `complete_ops::dispatch_outcome`'s `test_server`.
    fn test_server() -> (MemoryServer, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let global_db = dir.path().join("global.sqlite");
        let server = MemoryServer::new(global_db, None).expect("server");
        (server, dir)
    }

    fn host_event(event_type: &str, payload: serde_json::Value) -> TachiEventRecord {
        TachiEventRecord {
            id: uuid::Uuid::new_v4().to_string(),
            source_repo: "openclaw".to_string(),
            adapter: "openclaw".to_string(),
            project: String::new(),
            domain: "agent_host".to_string(),
            session_id: "parent-session".to_string(),
            actor: "main".to_string(),
            event_type: event_type.to_string(),
            authority: AuthorityLevel::Advisory,
            effects: vec![EffectScope::ProjectCycle],
            projection_hints: vec![ProjectionKind::Timeline],
            payload,
            provenance: json!({}),
            created_at: "2026-07-19T00:00:00Z".to_string(),
        }
    }

    fn run_by_child(server: &MemoryServer, native_child_id: &str) -> memcore::MirrorEvalRun {
        server
            .with_global_store_read(|store| {
                memcore::get_run_by_native_child_id(store.connection(), native_child_id)
                    .map_err(|e| e.to_string())
            })
            .expect("run query")
            .expect("run present")
    }

    fn run_count(server: &MemoryServer) -> i64 {
        server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row("SELECT COUNT(*) FROM mirror_eval_runs", [], |r| r.get(0))
                    .map_err(|e| e.to_string())
            })
            .expect("count")
    }

    /// (a) `spawned` → register is idempotent under duplicate delivery: two
    /// identical `host.subagent_spawned` events for the same child id mint ONE
    /// run, tagged as a host-native subagent owned by the host.
    #[test]
    fn spawned_registers_idempotently_under_duplicate_delivery() {
        let (server, _dir) = test_server();
        let event = host_event(
            SPAWNED_EVENT_TYPE,
            json!({ "child_session_key": "child-1", "label": "reviewer-lane" }),
        );

        bridge_host_spawn_event(&server, &event);
        bridge_host_spawn_event(&server, &event);

        assert_eq!(run_count(&server), 1, "duplicate spawned must not duplicate the run");
        let run = run_by_child(&server, "child-1");
        assert_eq!(run.execution_origin, "host_native_subagent");
        assert_eq!(run.lifecycle_owner, "host");
        assert_eq!(run.harness.as_deref(), Some("openclaw"));
        assert_eq!(run.native_child_id.as_deref(), Some("child-1"));
        assert_eq!(run.requested_agent.as_deref(), Some("reviewer-lane"));
        assert_eq!(run.frozen_contract_ref, "host_native_subagent:unbound");
    }

    /// (b) `ended` → observe records the terminal fact with the host outcome
    /// normalized to the kanban vocabulary.
    #[test]
    fn ended_observes_terminal_with_normalized_outcome() {
        let (server, _dir) = test_server();
        bridge_host_spawn_event(
            &server,
            &host_event(
                SPAWNED_EVENT_TYPE,
                json!({ "child_session_key": "child-2", "label": "impl-lane" }),
            ),
        );
        bridge_host_spawn_event(
            &server,
            &host_event(
                ENDED_EVENT_TYPE,
                json!({ "child_session_key": "child-2", "outcome": "success", "duration_ms": 4200 }),
            ),
        );

        let (terminal_outcome, duration_ms): (String, Option<i64>) = server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT o.terminal_outcome, o.duration_ms \
                         FROM mirror_eval_observations o \
                         JOIN mirror_eval_runs r ON r.eval_run_id = o.eval_run_id \
                         WHERE r.native_child_id = 'child-2'",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(|e| e.to_string())
            })
            .expect("observation row");
        assert_eq!(terminal_outcome, "completed", "'success' normalizes to the kanban 'completed'");
        assert_eq!(duration_ms, Some(4200));
    }

    /// (b′) An unrecognized host outcome floors to `unknown` (never empty), so
    /// the observation's non-empty `terminal_outcome` gate is still satisfied.
    #[test]
    fn ended_with_unknown_outcome_floors_to_unknown() {
        let (server, _dir) = test_server();
        bridge_host_spawn_event(
            &server,
            &host_event(SPAWNED_EVENT_TYPE, json!({ "child_session_key": "child-3" })),
        );
        bridge_host_spawn_event(
            &server,
            &host_event(
                ENDED_EVENT_TYPE,
                json!({ "child_session_key": "child-3", "outcome": "weird-host-state" }),
            ),
        );

        let terminal_outcome: String = server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT o.terminal_outcome FROM mirror_eval_observations o \
                         JOIN mirror_eval_runs r ON r.eval_run_id = o.eval_run_id \
                         WHERE r.native_child_id = 'child-3'",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|e| e.to_string())
            })
            .expect("observation row");
        assert_eq!(terminal_outcome, "unknown");
    }

    /// (c) Bridge failure is contained: an `ended` whose run was never
    /// registered (spawned lost) writes nothing and does not panic/propagate —
    /// the caller (`emit_event`) proceeds unaffected.
    #[test]
    fn ended_without_prior_register_is_a_contained_skip() {
        let (server, _dir) = test_server();

        // No spawned event first — observe cannot resolve a run.
        bridge_host_spawn_event(
            &server,
            &host_event(
                ENDED_EVENT_TYPE,
                json!({ "child_session_key": "orphan-child", "outcome": "success" }),
            ),
        );

        assert_eq!(run_count(&server), 0, "no run minted");
        let obs_count: i64 = server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row("SELECT COUNT(*) FROM mirror_eval_observations", [], |r| r.get(0))
                    .map_err(|e| e.to_string())
            })
            .expect("count");
        assert_eq!(obs_count, 0, "no orphan observation written");
    }

    /// (d) An ordinary non-subagent event never touches mirror_eval.
    #[test]
    fn ordinary_event_does_not_touch_mirror_eval() {
        let (server, _dir) = test_server();
        bridge_host_spawn_event(
            &server,
            &host_event(
                "memory.saved",
                json!({ "child_session_key": "not-a-spawn", "outcome": "success" }),
            ),
        );
        bridge_host_spawn_event(
            &server,
            &host_event("host.before_prompt", json!({ "turn_id": "t-1" })),
        );

        assert_eq!(run_count(&server), 0, "non-spawn events mint no mirror_eval run");
    }
}
