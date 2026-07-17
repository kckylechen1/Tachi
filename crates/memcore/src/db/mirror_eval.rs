//! First-class mirror eval intake for harness-native subagents (#1066).
//!
//! `tachi_agent_eval` extends with `register -> observe -> adjudicate -> get`,
//! mirroring the append-only pattern the dispatch-outcome ledger uses (#773 /
//! #1035): carrier-observed execution facts are recorded separately from
//! leader/independent-reviewer judgment, and only a terminal judgment event
//! makes a row adjudicated. This is a NEW ledger backing the SAME
//! `tachi_agent_eval` facade tool the aggregate/telemetry/perf actions
//! already use — not a second surface — for work Tachi did not dispatch
//! (a host-native subagent Tachi only observes).
//!
//! ## Three tables
//!
//! - `mirror_eval_runs` (`register`): one row per registered execution. A
//!   `native_child_id` (when the host exposes one) IS the idempotency
//!   anchor, per the frozen contract's "same native id + same payload is
//!   idempotent, same native id + differing payload is an explicit
//!   conflict": replaying the same native id with the same content (which
//!   includes `frozen_contract_ref`/`execution_origin` as ordinary compared
//!   fields) returns the original row; replaying it with ANY differing
//!   content — including a different parent contract or execution origin —
//!   is an explicit conflict (never silently overwritten, and never
//!   silently minted as a second row under the same native id — see
//!   [`register_mirror_eval_run`]). Absent a native id, registration cannot
//!   be deduped and always mints a fresh row.
//! - `mirror_eval_observations` (`observe`): at most ONE row per
//!   `eval_run_id` — the carrier-observed terminal facts (duration/cost,
//!   result/artifact refs, effective identity). The type has no judgment
//!   field at all — usefulness/failure-mode/plan-delta are structurally
//!   unreachable from this write path (#1066 AC-3).
//! - `mirror_eval_adjudications` (`adjudicate`): append-only leader/
//!   independent-reviewer judgment events, keyed by `event_key` for
//!   idempotent replay / explicit-conflict-on-correction, exactly mirroring
//!   [`super::dispatch_adjudications::append_dispatch_adjudication`]'s
//!   contract (existence check, event_key short-circuit with payload-match
//!   guard, durable per-run `insertion_seq`).
//!
//! Registration and observation intentionally do NOT hash a payload
//! fingerprint into a separate column; replay comparison is a direct
//! field-by-field comparison against the persisted row, exactly as
//! `dispatch_adjudications::replay_payload_matches` does — one less place for
//! a hashing bug to silently launder a changed payload as identical.

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::MemoryError;

use super::common::normalize_utc_iso_or_now;

/// Field separator for composite natural keys. A control character (never
/// legitimately typed by a human or emitted by an id generator) so no
/// caller-supplied field value can forge a key collision by embedding the
/// delimiter itself — unlike `"::"`, which real content might contain.
const KEY_SEP: char = '\u{1}';

// ─── register ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewMirrorEvalRun {
    pub frozen_contract_ref: String,
    pub execution_origin: String,
    pub lifecycle_owner: String,
    pub harness: Option<String>,
    pub native_child_id: Option<String>,
    pub requested_profile: Option<String>,
    pub requested_model: Option<String>,
    pub requested_agent: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MirrorEvalRun {
    pub eval_run_id: String,
    pub register_key: String,
    pub frozen_contract_ref: String,
    pub execution_origin: String,
    pub lifecycle_owner: String,
    pub harness: Option<String>,
    pub native_child_id: Option<String>,
    pub requested_profile: Option<String>,
    pub requested_model: Option<String>,
    pub requested_agent: Option<String>,
    pub created_at: String,
}

/// Deterministic natural key: the bare `native_child_id` when one is
/// present, else always-fresh (no identity anchor to dedupe against).
///
/// The frozen contract (#1066) is explicit: "Same native id + same payload
/// is idempotent; same native id + differing payload is an explicit
/// conflict" — the anchor is the native id ALONE, not a compound of
/// `(frozen_contract_ref, execution_origin, native_child_id)`. A compound
/// key would let the SAME native id silently mint a SECOND row whenever the
/// contract_ref or execution_origin differs, instead of surfacing the
/// conflict `register` is supposed to reject — exactly the case
/// `registration_content_matches` below already exists to catch (it compares
/// `frozen_contract_ref`/`execution_origin` as ordinary content fields, so a
/// mismatch on either one correctly fails the match and raises a conflict
/// once the key itself no longer launders them into different rows).
fn register_key(new: &NewMirrorEvalRun) -> String {
    match new
        .native_child_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(native_id) => native_id.to_string(),
        None => format!("no-native-id{KEY_SEP}{}", uuid::Uuid::new_v4()),
    }
}

/// Registration content compared for idempotent-replay-vs-conflict. Excludes
/// `native_child_id` (already the join key half of `register_key`) and every
/// generated/transient column (`eval_run_id`, `register_key`, `created_at`).
fn registration_content_matches(existing: &MirrorEvalRun, new: &NewMirrorEvalRun) -> bool {
    existing.frozen_contract_ref == new.frozen_contract_ref
        && existing.execution_origin == new.execution_origin
        && existing.lifecycle_owner == new.lifecycle_owner
        && existing.harness == new.harness
        && existing.requested_profile == new.requested_profile
        && existing.requested_model == new.requested_model
        && existing.requested_agent == new.requested_agent
}

/// Register a mirror eval run. Same native id + same content is idempotent
/// (returns the original row, no new write); same native id + different
/// content is an explicit conflict (#1066 AC-2). A registration carrying no
/// `native_child_id` always mints a fresh row — the host gave us nothing to
/// anchor idempotency on.
pub fn register_mirror_eval_run(
    conn: &Connection,
    new: &NewMirrorEvalRun,
) -> Result<MirrorEvalRun, MemoryError> {
    if new.frozen_contract_ref.trim().is_empty() {
        return Err(MemoryError::InvalidArg(
            "frozen_contract_ref is required to register a mirror eval run".to_string(),
        ));
    }
    if new.execution_origin.trim().is_empty() {
        return Err(MemoryError::InvalidArg(
            "execution_origin is required to register a mirror eval run".to_string(),
        ));
    }
    if new.lifecycle_owner.trim().is_empty() {
        return Err(MemoryError::InvalidArg(
            "lifecycle_owner is required to register a mirror eval run".to_string(),
        ));
    }

    let key = register_key(new);
    let has_native_id = new
        .native_child_id
        .as_deref()
        .map(str::trim)
        .is_some_and(|s| !s.is_empty());

    if has_native_id {
        if let Some(existing) = get_run_by_register_key(conn, &key)? {
            if registration_content_matches(&existing, new) {
                return Ok(existing);
            }
            return Err(MemoryError::InvalidArg(format!(
                "register conflict: native_child_id '{}' is already registered under \
                 frozen_contract_ref '{}' / execution_origin '{}' with different content; \
                 re-register with the SAME content to replay idempotently, or use a new \
                 native_child_id for genuinely different work",
                new.native_child_id.as_deref().unwrap_or(""),
                new.frozen_contract_ref,
                new.execution_origin
            )));
        }
    }

    let eval_run_id = uuid::Uuid::new_v4().to_string();
    let created_at = normalize_utc_iso_or_now("");
    conn.execute(
        "INSERT INTO mirror_eval_runs
         (eval_run_id, register_key, frozen_contract_ref, execution_origin, lifecycle_owner,
          harness, native_child_id, requested_profile, requested_model, requested_agent, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            eval_run_id,
            key,
            new.frozen_contract_ref,
            new.execution_origin,
            new.lifecycle_owner,
            new.harness,
            new.native_child_id,
            new.requested_profile,
            new.requested_model,
            new.requested_agent,
            created_at,
        ],
    )?;
    get_run_by_id(conn, &eval_run_id)?
        .ok_or_else(|| MemoryError::Internal("inserted mirror eval run vanished".to_string()))
}

pub fn get_run_by_id(
    conn: &Connection,
    eval_run_id: &str,
) -> Result<Option<MirrorEvalRun>, MemoryError> {
    conn.query_row(
        "SELECT eval_run_id, register_key, frozen_contract_ref, execution_origin, lifecycle_owner,
                harness, native_child_id, requested_profile, requested_model, requested_agent, created_at
         FROM mirror_eval_runs WHERE eval_run_id = ?1",
        [eval_run_id],
        row_to_run,
    )
    .optional()
    .map_err(MemoryError::from)
}

pub fn get_run_by_native_child_id(
    conn: &Connection,
    native_child_id: &str,
) -> Result<Option<MirrorEvalRun>, MemoryError> {
    // Most-recent-first defensively: `register_key` now anchors solely on
    // `native_child_id`, so a live registration for a given native id is
    // unique by construction (a same-id/differing-content re-register is
    // rejected as a conflict, never silently minted as a second row). The
    // ORDER BY/LIMIT 1 exists only to stay correct against any pre-fix rows
    // written under the old compound key, where more than one row COULD
    // share a native_child_id — never against new writes.
    conn.query_row(
        "SELECT eval_run_id, register_key, frozen_contract_ref, execution_origin, lifecycle_owner,
                harness, native_child_id, requested_profile, requested_model, requested_agent, created_at
         FROM mirror_eval_runs WHERE native_child_id = ?1
         ORDER BY created_at DESC, eval_run_id DESC LIMIT 1",
        [native_child_id],
        row_to_run,
    )
    .optional()
    .map_err(MemoryError::from)
}

fn get_run_by_register_key(
    conn: &Connection,
    register_key: &str,
) -> Result<Option<MirrorEvalRun>, MemoryError> {
    conn.query_row(
        "SELECT eval_run_id, register_key, frozen_contract_ref, execution_origin, lifecycle_owner,
                harness, native_child_id, requested_profile, requested_model, requested_agent, created_at
         FROM mirror_eval_runs WHERE register_key = ?1",
        [register_key],
        row_to_run,
    )
    .optional()
    .map_err(MemoryError::from)
}

fn row_to_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<MirrorEvalRun> {
    Ok(MirrorEvalRun {
        eval_run_id: row.get(0)?,
        register_key: row.get(1)?,
        frozen_contract_ref: row.get(2)?,
        execution_origin: row.get(3)?,
        lifecycle_owner: row.get(4)?,
        harness: row.get(5)?,
        native_child_id: row.get(6)?,
        requested_profile: row.get(7)?,
        requested_model: row.get(8)?,
        requested_agent: row.get(9)?,
        created_at: row.get(10)?,
    })
}

// ─── observe ────────────────────────────────────────────────────────────────

/// Carrier-observed terminal facts. Deliberately carries NO judgment field
/// (no usefulness/failure-mode/plan-delta/evidence-usability) — those exist
/// only on [`NewMirrorEvalAdjudication`]. This is the structural half of
/// #1066 AC-3: observe cannot write judgment because the type it writes
/// through has nowhere to put one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewMirrorEvalObservation {
    pub eval_run_id: String,
    pub terminal_outcome: String,
    pub duration_ms: Option<u64>,
    pub cost_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub result_ref: Option<String>,
    pub artifacts: Vec<String>,
    pub effective_model: Option<String>,
    pub effective_backend: Option<String>,
    pub effective_harness: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MirrorEvalObservation {
    pub observation_id: String,
    pub eval_run_id: String,
    pub terminal_outcome: String,
    pub duration_ms: Option<u64>,
    pub cost_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub result_ref: Option<String>,
    pub artifacts: Vec<String>,
    pub effective_model: Option<String>,
    pub effective_backend: Option<String>,
    pub effective_harness: Option<String>,
    pub created_at: String,
}

fn observation_content_matches(
    existing: &MirrorEvalObservation,
    new: &NewMirrorEvalObservation,
) -> bool {
    existing.terminal_outcome == new.terminal_outcome
        && existing.duration_ms == new.duration_ms
        && existing.cost_tokens == new.cost_tokens
        && existing.cost_usd == new.cost_usd
        && existing.result_ref == new.result_ref
        && existing.artifacts == new.artifacts
        && existing.effective_model == new.effective_model
        && existing.effective_backend == new.effective_backend
        && existing.effective_harness == new.effective_harness
}

/// Record the (at most one) terminal observation for `eval_run_id`. Same
/// content is idempotent; different content is an explicit conflict — an
/// observation is a terminal fact snapshot, not a mutable log. Errors if the
/// parent run does not exist (mirrors the dispatch-adjudication existence
/// check — no orphan observation can be minted against a hallucinated run).
pub fn record_mirror_eval_observation(
    conn: &Connection,
    new: &NewMirrorEvalObservation,
) -> Result<MirrorEvalObservation, MemoryError> {
    if new.terminal_outcome.trim().is_empty() {
        return Err(MemoryError::InvalidArg(
            "terminal_outcome is required to record a mirror eval observation".to_string(),
        ));
    }
    if get_run_by_id(conn, &new.eval_run_id)?.is_none() {
        return Err(MemoryError::InvalidArg(format!(
            "unknown eval_run_id '{}': cannot observe a run that was never registered",
            new.eval_run_id
        )));
    }

    if let Some(existing) = get_observation(conn, &new.eval_run_id)? {
        if observation_content_matches(&existing, new) {
            return Ok(existing);
        }
        return Err(MemoryError::InvalidArg(format!(
            "observe conflict: eval_run_id '{}' already carries a terminal observation with \
             different content; an observation is a terminal snapshot and cannot be silently \
             rewritten",
            new.eval_run_id
        )));
    }

    let observation_id = uuid::Uuid::new_v4().to_string();
    let created_at = normalize_utc_iso_or_now("");
    let artifacts_json = serde_json::to_string(&new.artifacts).unwrap_or_else(|_| "[]".to_string());
    conn.execute(
        "INSERT INTO mirror_eval_observations
         (observation_id, eval_run_id, terminal_outcome, duration_ms, cost_tokens, cost_usd,
          result_ref, artifacts, effective_model, effective_backend, effective_harness, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            observation_id,
            new.eval_run_id,
            new.terminal_outcome,
            new.duration_ms.map(|v| v as i64),
            new.cost_tokens.map(|v| v as i64),
            new.cost_usd,
            new.result_ref,
            artifacts_json,
            new.effective_model,
            new.effective_backend,
            new.effective_harness,
            created_at,
        ],
    )?;
    get_observation(conn, &new.eval_run_id)?.ok_or_else(|| {
        MemoryError::Internal("inserted mirror eval observation vanished".to_string())
    })
}

pub fn get_observation(
    conn: &Connection,
    eval_run_id: &str,
) -> Result<Option<MirrorEvalObservation>, MemoryError> {
    conn.query_row(
        "SELECT observation_id, eval_run_id, terminal_outcome, duration_ms, cost_tokens, cost_usd,
                result_ref, artifacts, effective_model, effective_backend, effective_harness, created_at
         FROM mirror_eval_observations WHERE eval_run_id = ?1",
        [eval_run_id],
        row_to_observation,
    )
    .optional()
    .map_err(MemoryError::from)
}

fn row_to_observation(row: &rusqlite::Row<'_>) -> rusqlite::Result<MirrorEvalObservation> {
    let artifacts_raw: String = row.get(7)?;
    let artifacts: Vec<String> = serde_json::from_str(&artifacts_raw).unwrap_or_default();
    Ok(MirrorEvalObservation {
        observation_id: row.get(0)?,
        eval_run_id: row.get(1)?,
        terminal_outcome: row.get(2)?,
        duration_ms: row.get::<_, Option<i64>>(3)?.map(|v| v as u64),
        cost_tokens: row.get::<_, Option<i64>>(4)?.map(|v| v as u64),
        cost_usd: row.get(5)?,
        result_ref: row.get(6)?,
        artifacts,
        effective_model: row.get(8)?,
        effective_backend: row.get(9)?,
        effective_harness: row.get(10)?,
        created_at: row.get(11)?,
    })
}

// ─── adjudicate ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewMirrorEvalAdjudication {
    pub adjudication_id: String,
    pub eval_run_id: String,
    pub event_key: String,
    pub actor: String,
    pub verifier_model: Option<String>,
    pub usefulness: String,
    pub failure_mode: Option<String>,
    pub first_review_findings: Vec<String>,
    pub plan_delta: Option<String>,
    pub next_prompt_delta: Option<String>,
    pub evidence_usable: bool,
    pub used_in_final_claim: bool,
    pub human_override: bool,
    pub evidence_ref: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MirrorEvalAdjudication {
    pub adjudication_id: String,
    pub eval_run_id: String,
    pub event_key: String,
    pub actor: String,
    pub verifier_model: Option<String>,
    pub usefulness: String,
    pub failure_mode: Option<String>,
    pub first_review_findings: Vec<String>,
    pub plan_delta: Option<String>,
    pub next_prompt_delta: Option<String>,
    pub evidence_usable: bool,
    pub used_in_final_claim: bool,
    pub human_override: bool,
    pub evidence_ref: String,
    pub created_at: String,
    pub insertion_seq: i64,
}

fn adjudication_payload_matches(
    existing: &MirrorEvalAdjudication,
    new: &NewMirrorEvalAdjudication,
) -> bool {
    existing.eval_run_id == new.eval_run_id
        && existing.actor == new.actor
        && existing.verifier_model == new.verifier_model
        && existing.usefulness == new.usefulness
        && existing.failure_mode == new.failure_mode
        && existing.first_review_findings == new.first_review_findings
        && existing.plan_delta == new.plan_delta
        && existing.next_prompt_delta == new.next_prompt_delta
        && existing.evidence_usable == new.evidence_usable
        && existing.used_in_final_claim == new.used_in_final_claim
        && existing.human_override == new.human_override
        && existing.evidence_ref == new.evidence_ref
}

/// Append a leader/independent-reviewer judgment event. Mirrors
/// [`super::dispatch_adjudications::append_dispatch_adjudication`] exactly:
/// one transaction, parent-existence check BEFORE the event_key
/// short-circuit, a same-key/different-outcome collision guard, a
/// same-key/same-payload idempotent replay, and a durable per-run
/// `insertion_seq` for ordering corrections (a correction uses a NEW
/// event_key and appends rather than overwrites — history is never lost).
pub fn append_mirror_eval_adjudication(
    conn: &Connection,
    new: &NewMirrorEvalAdjudication,
) -> Result<MirrorEvalAdjudication, MemoryError> {
    if new.actor.trim().is_empty() {
        return Err(MemoryError::InvalidArg(
            "actor is required to adjudicate a mirror eval run".to_string(),
        ));
    }
    if new.usefulness.trim().is_empty() {
        return Err(MemoryError::InvalidArg(
            "usefulness is required to adjudicate a mirror eval run".to_string(),
        ));
    }
    if new.evidence_ref.trim().is_empty() {
        return Err(MemoryError::InvalidArg(
            "evidence_ref is required to adjudicate a mirror eval run".to_string(),
        ));
    }

    let tx = conn.unchecked_transaction()?;

    let parent_exists: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM mirror_eval_runs WHERE eval_run_id = ?1",
            params![new.eval_run_id],
            |row| row.get(0),
        )
        .optional()?;
    if parent_exists.is_none() {
        return Err(MemoryError::InvalidArg(format!(
            "unknown eval_run_id '{}': cannot adjudicate a run that was never registered",
            new.eval_run_id
        )));
    }

    if let Some(existing) = get_adjudication_by_event_key(&tx, &new.event_key)? {
        if existing.eval_run_id != new.eval_run_id {
            return Err(MemoryError::InvalidArg(
                "event_key collision or misuse: existing adjudication belongs to a different \
                 eval_run_id"
                    .to_string(),
            ));
        }
        if !adjudication_payload_matches(&existing, new) {
            return Err(MemoryError::InvalidArg(
                "event_key replay payload mismatch: the existing adjudication is not \
                 canonically equivalent; use a new event_key for a correction"
                    .to_string(),
            ));
        }
        return Ok(existing);
    }

    let insertion_seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(insertion_seq), 0) + 1 FROM mirror_eval_adjudications \
         WHERE eval_run_id = ?1",
        params![new.eval_run_id],
        |row| row.get(0),
    )?;

    let created_at = normalize_utc_iso_or_now("");
    let findings_json =
        serde_json::to_string(&new.first_review_findings).unwrap_or_else(|_| "[]".to_string());
    tx.execute(
        "INSERT INTO mirror_eval_adjudications
         (adjudication_id, eval_run_id, event_key, actor, verifier_model, usefulness,
          failure_mode, first_review_findings, plan_delta, next_prompt_delta, evidence_usable,
          used_in_final_claim, human_override, evidence_ref, created_at, insertion_seq)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            new.adjudication_id,
            new.eval_run_id,
            new.event_key,
            new.actor,
            new.verifier_model,
            new.usefulness,
            new.failure_mode,
            findings_json,
            new.plan_delta,
            new.next_prompt_delta,
            new.evidence_usable as i64,
            new.used_in_final_claim as i64,
            new.human_override as i64,
            new.evidence_ref,
            created_at,
            insertion_seq,
        ],
    )?;
    let result = get_adjudication_by_event_key(&tx, &new.event_key)?.ok_or_else(|| {
        MemoryError::Internal("inserted mirror eval adjudication vanished".to_string())
    })?;
    tx.commit()?;
    Ok(result)
}

/// All adjudication events for `eval_run_id`, oldest first (`insertion_seq`
/// order). The LAST element is the authoritative/current terminal judgment —
/// a correction appends rather than overwrites, exactly like
/// `dispatch_adjudications`.
pub fn list_adjudications_for_run(
    conn: &Connection,
    eval_run_id: &str,
) -> Result<Vec<MirrorEvalAdjudication>, MemoryError> {
    let mut statement = conn.prepare(
        "SELECT adjudication_id, eval_run_id, event_key, actor, verifier_model, usefulness,
                failure_mode, first_review_findings, plan_delta, next_prompt_delta,
                evidence_usable, used_in_final_claim, human_override, evidence_ref, created_at,
                insertion_seq
         FROM mirror_eval_adjudications WHERE eval_run_id = ?1 ORDER BY insertion_seq",
    )?;
    let rows = statement
        .query_map([eval_run_id], row_to_adjudication)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Whether any terminal adjudication event exists for `eval_run_id`. Row
/// presence IS the adjudication state (#1035 frozen-spec precedent).
pub fn run_is_adjudicated(conn: &Connection, eval_run_id: &str) -> Result<bool, MemoryError> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM mirror_eval_adjudications WHERE eval_run_id = ?1)",
        [eval_run_id],
        |row| row.get(0),
    )?;
    Ok(exists)
}

fn get_adjudication_by_event_key(
    conn: &Connection,
    event_key: &str,
) -> Result<Option<MirrorEvalAdjudication>, MemoryError> {
    conn.query_row(
        "SELECT adjudication_id, eval_run_id, event_key, actor, verifier_model, usefulness,
                failure_mode, first_review_findings, plan_delta, next_prompt_delta,
                evidence_usable, used_in_final_claim, human_override, evidence_ref, created_at,
                insertion_seq
         FROM mirror_eval_adjudications WHERE event_key = ?1",
        [event_key],
        row_to_adjudication,
    )
    .optional()
    .map_err(MemoryError::from)
}

fn row_to_adjudication(row: &rusqlite::Row<'_>) -> rusqlite::Result<MirrorEvalAdjudication> {
    let findings_raw: String = row.get(7)?;
    let first_review_findings: Vec<String> =
        serde_json::from_str(&findings_raw).unwrap_or_default();
    Ok(MirrorEvalAdjudication {
        adjudication_id: row.get(0)?,
        eval_run_id: row.get(1)?,
        event_key: row.get(2)?,
        actor: row.get(3)?,
        verifier_model: row.get(4)?,
        usefulness: row.get(5)?,
        failure_mode: row.get(6)?,
        first_review_findings,
        plan_delta: row.get(8)?,
        next_prompt_delta: row.get(9)?,
        evidence_usable: row.get::<_, i64>(10)? != 0,
        used_in_final_claim: row.get::<_, i64>(11)? != 0,
        human_override: row.get::<_, i64>(12)? != 0,
        evidence_ref: row.get(13)?,
        created_at: row.get(14)?,
        insertion_seq: row.get(15)?,
    })
}

// ─── get (full lifecycle view) ──────────────────────────────────────────────

/// The `register + observe + adjudicate` join for one run — what `get`
/// resolves and what `tachi_task(complete)` projection reads. `adjudications`
/// is the full append-only history; the last element (if any) is the
/// authoritative current judgment.
#[derive(Debug, Clone, PartialEq)]
pub struct MirrorEvalRunView {
    pub run: MirrorEvalRun,
    pub observation: Option<MirrorEvalObservation>,
    pub adjudications: Vec<MirrorEvalAdjudication>,
}

impl MirrorEvalRunView {
    pub fn is_adjudicated(&self) -> bool {
        !self.adjudications.is_empty()
    }

    /// The current (last-appended) judgment, or `None` if never adjudicated.
    pub fn current_adjudication(&self) -> Option<&MirrorEvalAdjudication> {
        self.adjudications.last()
    }
}

/// Resolve the full lifecycle view by `eval_run_id` or `native_child_id`
/// (exactly one must be `Some`). Returns `Ok(None)` when nothing matches —
/// callers turn that into their own not-found error text.
pub fn get_mirror_eval_run_view(
    conn: &Connection,
    eval_run_id: Option<&str>,
    native_child_id: Option<&str>,
) -> Result<Option<MirrorEvalRunView>, MemoryError> {
    let run = match (eval_run_id, native_child_id) {
        (Some(id), _) if !id.trim().is_empty() => get_run_by_id(conn, id)?,
        (_, Some(native_id)) if !native_id.trim().is_empty() => {
            get_run_by_native_child_id(conn, native_id)?
        }
        _ => {
            return Err(MemoryError::InvalidArg(
                "get requires eval_run_id or native_child_id".to_string(),
            ))
        }
    };
    let Some(run) = run else {
        return Ok(None);
    };
    let observation = get_observation(conn, &run.eval_run_id)?;
    let adjudications = list_adjudications_for_run(conn, &run.eval_run_id)?;
    Ok(Some(MirrorEvalRunView {
        run,
        observation,
        adjudications,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_conn() -> Connection {
        crate::db::enable_simple_auto_extension().unwrap();
        crate::db::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        conn
    }

    fn base_register(native_id: &str) -> NewMirrorEvalRun {
        NewMirrorEvalRun {
            frozen_contract_ref: "kckylechen1/tachi#1066".to_string(),
            execution_origin: "host_native_subagent".to_string(),
            lifecycle_owner: "host".to_string(),
            harness: Some("claude_code_task_tool".to_string()),
            native_child_id: Some(native_id.to_string()),
            requested_profile: Some("explore".to_string()),
            requested_model: Some("anthropic/claude-sonnet".to_string()),
            requested_agent: Some("claude".to_string()),
        }
    }

    /// AC-2: same native id + same payload replays idempotently — no second
    /// row, same eval_run_id returned.
    #[test]
    fn register_replay_same_native_id_same_payload_is_idempotent() {
        let conn = open_conn();
        let first = register_mirror_eval_run(&conn, &base_register("native-1")).unwrap();
        let replay = register_mirror_eval_run(&conn, &base_register("native-1")).unwrap();
        assert_eq!(replay.eval_run_id, first.eval_run_id);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM mirror_eval_runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "idempotent replay must not duplicate the row");
    }

    /// AC-2: same native id + DIFFERENT payload is an explicit conflict, not
    /// a silent overwrite. RED on a naive upsert-by-native-id implementation.
    #[test]
    fn register_same_native_id_differing_payload_is_explicit_conflict() {
        let conn = open_conn();
        register_mirror_eval_run(&conn, &base_register("native-2")).unwrap();

        let mut conflicting = base_register("native-2");
        conflicting.requested_model = Some("openai/gpt-5".to_string());
        let err = register_mirror_eval_run(&conn, &conflicting)
            .expect_err("differing payload under the same native id must be rejected");
        assert!(
            err.to_string().contains("conflict"),
            "error must name the conflict: {err}"
        );

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM mirror_eval_runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "the rejected conflict must not land a second row");
    }

    /// AC-2 / codex round-2 finding #2: reusing the SAME native id under a
    /// DIFFERENT `frozen_contract_ref` (or `execution_origin`) must be an
    /// explicit conflict too — the frozen contract anchors idempotency on
    /// the native id ALONE, not a `(contract_ref, execution_origin,
    /// native_id)` compound. BEHAVIORAL RED on the pre-fix compound-key
    /// `register_key`: that implementation minted a SILENT SECOND ROW here
    /// instead of rejecting the conflict, because the compound key differed
    /// even though the native id was identical.
    #[test]
    fn register_same_native_id_different_contract_ref_is_explicit_conflict() {
        let conn = open_conn();
        register_mirror_eval_run(&conn, &base_register("native-cross-contract")).unwrap();

        let mut cross_contract = base_register("native-cross-contract");
        cross_contract.frozen_contract_ref = "kckylechen1/tachi#9999".to_string();
        let err = register_mirror_eval_run(&conn, &cross_contract).expect_err(
            "same native id under a different frozen_contract_ref must be rejected, not \
             silently registered as a second row",
        );
        assert!(
            err.to_string().contains("conflict"),
            "error must name the conflict: {err}"
        );

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM mirror_eval_runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count, 1,
            "the same native id must never anchor two rows under different contracts"
        );
    }

    /// Same native id, different `execution_origin`: same conflict rule as
    /// the contract_ref case above, exercised on the other compound-key
    /// field the pre-fix implementation also let through.
    #[test]
    fn register_same_native_id_different_execution_origin_is_explicit_conflict() {
        let conn = open_conn();
        register_mirror_eval_run(&conn, &base_register("native-cross-origin")).unwrap();

        let mut cross_origin = base_register("native-cross-origin");
        cross_origin.execution_origin = "some_other_origin".to_string();
        let err = register_mirror_eval_run(&conn, &cross_origin).expect_err(
            "same native id under a different execution_origin must be rejected",
        );
        assert!(err.to_string().contains("conflict"));

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM mirror_eval_runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    /// Registrations with no native id can never be deduped against each
    /// other — every call mints a fresh run.
    #[test]
    fn register_without_native_child_id_never_dedupes() {
        let conn = open_conn();
        let mut no_native = base_register("unused");
        no_native.native_child_id = None;
        let first = register_mirror_eval_run(&conn, &no_native).unwrap();
        let second = register_mirror_eval_run(&conn, &no_native).unwrap();
        assert_ne!(
            first.eval_run_id, second.eval_run_id,
            "no native id means no idempotency anchor"
        );
    }

    /// AC-6: the eval_run_id / frozen_contract_ref linkage set at register
    /// stays immutable through observe and adjudicate.
    #[test]
    fn receipt_linkage_immutable_through_observe_and_adjudicate() {
        let conn = open_conn();
        let run = register_mirror_eval_run(&conn, &base_register("native-3")).unwrap();

        record_mirror_eval_observation(
            &conn,
            &NewMirrorEvalObservation {
                eval_run_id: run.eval_run_id.clone(),
                terminal_outcome: "success".to_string(),
                ..Default::default()
            },
        )
        .unwrap();

        append_mirror_eval_adjudication(
            &conn,
            &NewMirrorEvalAdjudication {
                adjudication_id: "adj-1".to_string(),
                eval_run_id: run.eval_run_id.clone(),
                event_key: "adj-1-key".to_string(),
                actor: "leader".to_string(),
                verifier_model: Some("openai/gpt-5".to_string()),
                usefulness: "useful".to_string(),
                evidence_usable: true,
                evidence_ref: "run-1".to_string(),
                ..Default::default()
            },
        )
        .unwrap();

        let reread = get_run_by_id(&conn, &run.eval_run_id).unwrap().unwrap();
        assert_eq!(reread.eval_run_id, run.eval_run_id);
        assert_eq!(reread.frozen_contract_ref, run.frozen_contract_ref);
        assert_eq!(reread.register_key, run.register_key);

        // A register replay after observe/adjudicate still returns the SAME
        // eval_run_id — the linkage was never disturbed by downstream writes.
        let replay = register_mirror_eval_run(&conn, &base_register("native-3")).unwrap();
        assert_eq!(replay.eval_run_id, run.eval_run_id);
    }

    /// AC-3: observe has no field to carry judgment through — the persisted
    /// row after observe carries zero adjudications regardless of what the
    /// caller might try to smuggle in. RED against a design where observe
    /// writes straight into the adjudications table.
    #[test]
    fn observe_cannot_write_judgment() {
        let conn = open_conn();
        let run = register_mirror_eval_run(&conn, &base_register("native-4")).unwrap();

        record_mirror_eval_observation(
            &conn,
            &NewMirrorEvalObservation {
                eval_run_id: run.eval_run_id.clone(),
                terminal_outcome: "success".to_string(),
                duration_ms: Some(5_000),
                ..Default::default()
            },
        )
        .unwrap();

        assert!(
            !run_is_adjudicated(&conn, &run.eval_run_id).unwrap(),
            "observe alone must never make a run adjudicated"
        );
        let view = get_mirror_eval_run_view(&conn, Some(&run.eval_run_id), None)
            .unwrap()
            .unwrap();
        assert!(view.adjudications.is_empty());
        assert!(view.current_adjudication().is_none());
    }

    /// Observe is a terminal snapshot: same content replays idempotently,
    /// different content is a conflict, mirroring register's contract.
    #[test]
    fn observe_replay_idempotent_conflict_on_mismatch() {
        let conn = open_conn();
        let run = register_mirror_eval_run(&conn, &base_register("native-5")).unwrap();
        let obs = NewMirrorEvalObservation {
            eval_run_id: run.eval_run_id.clone(),
            terminal_outcome: "success".to_string(),
            duration_ms: Some(1_000),
            ..Default::default()
        };
        let first = record_mirror_eval_observation(&conn, &obs).unwrap();
        let replay = record_mirror_eval_observation(&conn, &obs).unwrap();
        assert_eq!(first.observation_id, replay.observation_id);

        let mut conflicting = obs;
        conflicting.duration_ms = Some(9_999);
        let err = record_mirror_eval_observation(&conn, &conflicting)
            .expect_err("a changed terminal snapshot must be rejected, not silently rewritten");
        assert!(err.to_string().contains("conflict"));
    }

    /// Adjudicate append-only: same event_key replays idempotently, a
    /// correction under a NEW event_key appends rather than overwrites.
    #[test]
    fn adjudicate_replay_idempotent_and_correction_appends() {
        let conn = open_conn();
        let run = register_mirror_eval_run(&conn, &base_register("native-6")).unwrap();

        let event = NewMirrorEvalAdjudication {
            adjudication_id: "adj-a".to_string(),
            eval_run_id: run.eval_run_id.clone(),
            event_key: "event-a".to_string(),
            actor: "leader".to_string(),
            usefulness: "useful".to_string(),
            evidence_usable: true,
            evidence_ref: "run-a".to_string(),
            ..Default::default()
        };
        let first = append_mirror_eval_adjudication(&conn, &event).unwrap();
        let replay = append_mirror_eval_adjudication(&conn, &event).unwrap();
        assert_eq!(replay, first);

        let mut correction = event.clone();
        correction.adjudication_id = "adj-b".to_string();
        correction.event_key = "event-b".to_string();
        correction.usefulness = "failed".to_string();
        append_mirror_eval_adjudication(&conn, &correction).unwrap();

        let history = list_adjudications_for_run(&conn, &run.eval_run_id).unwrap();
        assert_eq!(history.len(), 2, "correction appends, never overwrites");
        assert_eq!(history[0].usefulness, "useful");
        assert_eq!(history[1].usefulness, "failed");
    }

    /// Adjudicate against an unknown eval_run_id is rejected and writes
    /// nothing — no orphan judgment against a hallucinated run.
    #[test]
    fn adjudicate_unknown_eval_run_id_is_rejected() {
        let conn = open_conn();
        let err = append_mirror_eval_adjudication(
            &conn,
            &NewMirrorEvalAdjudication {
                adjudication_id: "adj-orphan".to_string(),
                eval_run_id: "does-not-exist".to_string(),
                event_key: "event-orphan".to_string(),
                actor: "leader".to_string(),
                usefulness: "useful".to_string(),
                evidence_usable: true,
                evidence_ref: "run-orphan".to_string(),
                ..Default::default()
            },
        )
        .expect_err("unknown eval_run_id must be rejected");
        assert!(err.to_string().contains("unknown eval_run_id"));

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM mirror_eval_adjudications", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    /// `get` resolves by native_child_id when no eval_run_id is supplied.
    #[test]
    fn get_resolves_by_native_child_id() {
        let conn = open_conn();
        let run = register_mirror_eval_run(&conn, &base_register("native-7")).unwrap();
        let view = get_mirror_eval_run_view(&conn, None, Some("native-7"))
            .unwrap()
            .unwrap();
        assert_eq!(view.run.eval_run_id, run.eval_run_id);
    }

    #[test]
    fn get_missing_run_returns_none() {
        let conn = open_conn();
        let view = get_mirror_eval_run_view(&conn, Some("nope"), None).unwrap();
        assert!(view.is_none());
    }
}
