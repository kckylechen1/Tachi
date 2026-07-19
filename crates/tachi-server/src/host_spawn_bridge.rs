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
            "success" | "succeeded" | "completed" | "complete" | "done" | "ok" | "pass" | "passed",
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

/// Host-native child id, derived into the collision-safe identity anchor
/// (`native_child_id`). Anchors BOTH register and observe idempotency — they
/// call this one function so the stored anchor and the lookup anchor are
/// always derived identically.
fn child_session_key(event: &TachiEventRecord) -> Option<String> {
    payload_str(event, "child_session_key").map(|value| anchor_from_child_key(&value))
}

/// Derive the mirror_eval identity anchor from a host child session key,
/// collision-safe under secret scrubbing (C2 review fix).
///
/// `scrub_secrets` collapses EVERY secret-shaped value to the SAME `[REDACTED]`
/// token, and mirror_eval dedups solely on the stored anchor
/// (`register_key`, `mirror_eval::register_key`) — so two DISTINCT concurrent
/// children whose session keys both look secret-shaped would otherwise collapse
/// into one run, silently merging/dropping one. When redaction actually fires
/// (`scrubbed != raw`), suffix a deterministic one-way digest of the ORIGINAL
/// key: distinct originals stay distinct anchors (uniqueness), the same
/// original always derives the same anchor (idempotent replay), and NO secret
/// material is stored (the scrubbed token carries none, and the FNV-1a
/// `stable_hash` digest is one-way — the repo's existing content-hash helper;
/// no sha2/blake3 dep exists to reuse). A key that scrubs to itself is stored
/// verbatim with no suffix, so ordinary session keys are unchanged.
fn anchor_from_child_key(raw: &str) -> String {
    let scrubbed = scrub(raw);
    if scrubbed == raw {
        scrubbed
    } else {
        format!("{scrubbed}#{}", crate::utils::stable_hash(raw))
    }
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

    /// (a) `spawned` → register is idempotent under duplicate delivery AND the
    /// duplicate CONVERGES rather than erroring: two identical
    /// `host.subagent_spawned` events for the same child id mint ONE run.
    ///
    /// C6 discrimination: this calls `register_spawn` DIRECTLY (not the
    /// swallowing `bridge_host_spawn_event`) and asserts the second delivery
    /// returns `Ok` — the "already registered, content matches" converge path
    /// in `mirror_eval::register_mirror_eval_run`. If mirror_eval's dedup were
    /// removed, the second insert would hit the `register_key` UNIQUE
    /// constraint and return `Err`, turning this assertion RED — whereas an
    /// assertion only on the swallowing entrypoint + row count would still
    /// pass (the fail-safe wrapper hides the Err).
    #[test]
    fn spawned_register_converges_under_duplicate_delivery() {
        let (server, _dir) = test_server();
        let event = host_event(
            SPAWNED_EVENT_TYPE,
            json!({ "child_session_key": "child-1", "label": "reviewer-lane" }),
        );

        register_spawn(&server, &event).expect("first register mints the run");
        register_spawn(&server, &event)
            .expect("duplicate register must CONVERGE (Ok), never a UNIQUE-constraint Err");

        assert_eq!(
            run_count(&server),
            1,
            "duplicate spawned must not duplicate the run"
        );
        let run = run_by_child(&server, "child-1");
        assert_eq!(run.execution_origin, "host_native_subagent");
        assert_eq!(run.lifecycle_owner, "host");
        assert_eq!(run.harness.as_deref(), Some("openclaw"));
        assert_eq!(run.native_child_id.as_deref(), Some("child-1"));
        assert_eq!(run.requested_agent.as_deref(), Some("reviewer-lane"));
        assert_eq!(run.frozen_contract_ref, "host_native_subagent:unbound");
    }

    /// C2: two DISTINCT child session keys that both scrub to the same
    /// `[REDACTED]` token must NOT collide onto one mirror_eval run, and the
    /// stored anchor must carry no raw secret material. Idempotent replay of an
    /// existing secret-shaped key still converges onto its own run.
    #[test]
    fn distinct_secret_shaped_keys_get_distinct_anchors() {
        let (server, _dir) = test_server();
        // Both match the `sk-[A-Za-z0-9_-]{20,}` scrub pattern → both redact to
        // the identical `[REDACTED]` token, the pre-fix collision.
        let key_a = "sk-aaaaaaaaaaaaaaaaaaaa11";
        let key_b = "sk-bbbbbbbbbbbbbbbbbbbb22";
        assert_eq!(
            scrub(key_a),
            scrub(key_b),
            "precondition: both keys redact to the same token"
        );
        let anchor_a = anchor_from_child_key(key_a);
        let anchor_b = anchor_from_child_key(key_b);
        assert_ne!(
            anchor_a, anchor_b,
            "distinct secret-shaped keys must derive distinct anchors, not collide"
        );
        assert!(
            !anchor_a.contains("aaaaaaaa") && !anchor_b.contains("bbbbbbbb"),
            "no raw secret material may appear in the stored anchor"
        );

        register_spawn(
            &server,
            &host_event(SPAWNED_EVENT_TYPE, json!({ "child_session_key": key_a })),
        )
        .expect("register key_a");
        register_spawn(
            &server,
            &host_event(SPAWNED_EVENT_TYPE, json!({ "child_session_key": key_b })),
        )
        .expect("register key_b");
        assert_eq!(
            run_count(&server),
            2,
            "two distinct secret-shaped children must be two runs, not one collided row"
        );

        // Idempotent replay: the SAME original key derives the SAME anchor.
        register_spawn(
            &server,
            &host_event(SPAWNED_EVENT_TYPE, json!({ "child_session_key": key_a })),
        )
        .expect("replay of key_a converges");
        assert_eq!(
            run_count(&server),
            2,
            "replaying an existing secret-shaped key mints no third run"
        );
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
        assert_eq!(
            terminal_outcome, "completed",
            "'success' normalizes to the kanban 'completed'"
        );
        assert_eq!(duration_ms, Some(4200));
    }

    /// Fetch (observation count, terminal_outcome) for a child's run.
    fn observation_of(server: &MemoryServer, native_child_id: &str) -> (i64, Option<String>) {
        server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT COUNT(*), MAX(o.terminal_outcome) \
                         FROM mirror_eval_observations o \
                         JOIN mirror_eval_runs r ON r.eval_run_id = o.eval_run_id \
                         WHERE r.native_child_id = ?1",
                        [native_child_id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(|e| e.to_string())
            })
            .expect("observation query")
    }

    /// C6 gap: a duplicate `ended` delivery must neither duplicate nor
    /// destructively overwrite the terminal observation. `observe` allows at
    /// most one row per run; an identical re-delivery converges (Ok), a genuine
    /// content change is an explicit conflict — never a silent second row.
    #[test]
    fn ended_duplicate_delivery_does_not_duplicate_or_overwrite_observation() {
        let (server, _dir) = test_server();
        register_spawn(
            &server,
            &host_event(
                SPAWNED_EVENT_TYPE,
                json!({ "child_session_key": "child-dup" }),
            ),
        )
        .expect("register");
        let ended = host_event(
            ENDED_EVENT_TYPE,
            json!({ "child_session_key": "child-dup", "outcome": "success", "duration_ms": 100 }),
        );
        observe_terminal(&server, &ended).expect("first observe");
        observe_terminal(&server, &ended)
            .expect("duplicate ended must CONVERGE (Ok), not error or duplicate");

        let (count, outcome) = observation_of(&server, "child-dup");
        assert_eq!(
            count, 1,
            "duplicate ended must not duplicate the observation"
        );
        assert_eq!(outcome.as_deref(), Some("completed"));
    }

    /// C6 gap: a `spawned` replay arriving AFTER `ended` must converge onto the
    /// same run and must NOT resurrect/reset the terminal observation — the
    /// terminal fact is a snapshot, not something a late lifecycle event can
    /// roll back to in-flight.
    #[test]
    fn spawned_replay_after_ended_does_not_reset_terminal() {
        let (server, _dir) = test_server();
        let spawned = host_event(
            SPAWNED_EVENT_TYPE,
            json!({ "child_session_key": "child-late", "label": "impl-lane" }),
        );
        register_spawn(&server, &spawned).expect("register");
        observe_terminal(
            &server,
            &host_event(
                ENDED_EVENT_TYPE,
                json!({ "child_session_key": "child-late", "outcome": "failure" }),
            ),
        )
        .expect("observe terminal");

        // Late/duplicate spawned replay.
        register_spawn(&server, &spawned).expect("late spawned replay converges");

        assert_eq!(
            run_count(&server),
            1,
            "late spawned replay mints no second run"
        );
        let (count, outcome) = observation_of(&server, "child-late");
        assert_eq!(
            count, 1,
            "terminal observation survives the late spawned replay"
        );
        assert_eq!(
            outcome.as_deref(),
            Some("failed"),
            "terminal outcome must not be reset by a late spawned replay"
        );
    }

    /// (b′) An unrecognized host outcome floors to `unknown` (never empty), so
    /// the observation's non-empty `terminal_outcome` gate is still satisfied.
    #[test]
    fn ended_with_unknown_outcome_floors_to_unknown() {
        let (server, _dir) = test_server();
        bridge_host_spawn_event(
            &server,
            &host_event(
                SPAWNED_EVENT_TYPE,
                json!({ "child_session_key": "child-3" }),
            ),
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
                    .query_row("SELECT COUNT(*) FROM mirror_eval_observations", [], |r| {
                        r.get(0)
                    })
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

        assert_eq!(
            run_count(&server),
            0,
            "non-spawn events mint no mirror_eval run"
        );
    }
}
