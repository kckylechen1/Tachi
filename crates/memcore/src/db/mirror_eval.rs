//! First-class mirror eval intake for harness-native subagents (#1066).
//!
//! `tachi_agent_eval` extends with `register -> observe -> adjudicate -> get`,
//! reusing the append-only PATTERN the dispatch-outcome ledger established
//! (#773 / #1035) — carrier-observed execution facts recorded separately
//! from leader/independent-reviewer judgment, only a terminal judgment event
//! making a row adjudicated — via the SAME `tachi_agent_eval` facade tool
//! the aggregate/telemetry/perf actions already use, never a second surface
//! or a duplicate `tachi_task(action="evaluate")` verb (the frozen
//! contract's "do not create a second ledger," confirmed by the issue's own
//! adjudication to mean exactly that surface-level prohibition).
//!
//! ## Companion tables, not the dispatch tables themselves
//!
//! These three tables are NEW storage, deliberately NOT literal rows in
//! `dispatch_outcomes`/`dispatch_adjudications` — codex round-2 finding #2
//! read the frozen contract's separate "internal reuse is mandatory:
//! Phase-1 identity receipt, dispatch outcomes, and #1035 adjudications"
//! clause as requiring literal single-table reuse, and flagged this as a
//! contract violation. Evaluated and rejected: `dispatch_outcomes` requires
//! `dispatch_id`/`vendor`/an enum-shaped `execution_outcome` describing work
//! Tachi actually dispatched, and `dispatch_adjudications` has NO columns
//! for `usefulness`/`failure_mode`/`first_review_findings`/`plan_delta`/
//! `next_prompt_delta`/`evidence_usable`/`used_in_final_claim`/
//! `human_override` plus a `verdict`/`not_required_reason` mutual-exclusion
//! CHECK constraint shaped around dispatch-only semantics. Force-fitting a
//! host-native subagent Tachi never dispatched into those tables risks
//! silently polluting existing dispatch-specific aggregation/reporting
//! queries that assume every row there IS a Tachi dispatch. This module
//! satisfies "internal reuse is mandatory" via the PATTERN (mirroring
//! `dispatch_adjudications`'s existence-check / event_key-idempotency /
//! `insertion_seq` contract exactly — see [`append_mirror_eval_adjudication`])
//! and via literal reuse of the Phase-1 identity primitives
//! (`tachi_dispatch::model_lineage_id`, see `agent_eval::mirror`), not via a
//! shared table. Whether the frozen contract's authors intended literal
//! table-level reuse instead is a genuine open question this fix-round does
//! NOT resolve unilaterally — flagged SCOPE-GAP on PR #1186 for an owner
//! ruling; see that comment for the full reasoning.
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
    /// Task kind frozen as contract metadata at registration (v39). This is
    /// REQUESTED-basis metadata — the mirror spine has no carrier-observed
    /// task fact and never pretends one.
    pub requested_task_type: Option<String>,
    /// Register-time role (v39), REQUESTED basis. Role CONFIRMATION never
    /// reads this column; it mirrors `requested_model`'s relationship to
    /// `effective_model`.
    pub requested_role: Option<String>,
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
    pub requested_task_type: Option<String>,
    pub requested_role: Option<String>,
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
        && existing.requested_task_type == new.requested_task_type
        && existing.requested_role == new.requested_role
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
          harness, native_child_id, requested_profile, requested_model, requested_agent,
          requested_task_type, requested_role, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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
            new.requested_task_type,
            new.requested_role,
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
                harness, native_child_id, requested_profile, requested_model, requested_agent,
                requested_task_type, requested_role, created_at
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
                harness, native_child_id, requested_profile, requested_model, requested_agent,
                requested_task_type, requested_role, created_at
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
                harness, native_child_id, requested_profile, requested_model, requested_agent,
                requested_task_type, requested_role, created_at
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
        requested_task_type: row.get(10)?,
        requested_role: row.get(11)?,
        created_at: row.get(12)?,
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
    /// Carrier-observed role (v39) — the ONLY role source a candidate
    /// projection may confirm against. No requested-basis fallback exists.
    pub effective_role: Option<String>,
    /// Explicit carrier-observed model revision (v39), separate from the
    /// model identity string. A legacy `@version` suffix on
    /// `effective_model` is a read-side fallback ONLY where this is NULL;
    /// NEW writes may not carry conflicting representations (see
    /// [`record_mirror_eval_observation`]).
    pub effective_model_revision: Option<String>,
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
    pub effective_role: Option<String>,
    pub effective_model_revision: Option<String>,
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
        && existing.effective_role == new.effective_role
        && existing.effective_model_revision == new.effective_model_revision
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
    // v39: NEW inputs may not carry conflicting revision representations. An
    // explicit `effective_model_revision` alongside an `effective_model`
    // whose `@version` suffix names a DIFFERENT revision is refused here —
    // the read side could never tell which representation to believe, and
    // silently preferring one would launder the conflict into confirmed
    // evidence. A suffix-equal revision, or either representation alone, is
    // fine; LEGACY rows (revision column NULL) keep their suffix as the
    // read-side fallback untouched.
    if let (Some(explicit), Some(model)) = (
        new.effective_model_revision.as_deref(),
        new.effective_model.as_deref(),
    ) {
        if let Some((_, suffix)) = model.rsplit_once('@') {
            let explicit = explicit.trim();
            if !explicit.is_empty() && suffix != explicit {
                return Err(MemoryError::InvalidArg(format!(
                    "conflicting model revision representations: effective_model_revision \
                     '{explicit}' disagrees with the '@{suffix}' suffix on effective_model \
                     '{model}'; supply one revision, or make them agree"
                )));
            }
        }
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
          result_ref, artifacts, effective_model, effective_backend, effective_harness,
          effective_role, effective_model_revision, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
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
            new.effective_role,
            new.effective_model_revision,
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
                result_ref, artifacts, effective_model, effective_backend, effective_harness,
                effective_role, effective_model_revision, created_at
         FROM mirror_eval_observations WHERE eval_run_id = ?1",
        [eval_run_id],
        row_to_observation,
    )
    .optional()
    .map_err(MemoryError::from)
}

/// Codex round-2 finding #4b: malformed `artifacts` JSON must fail loudly,
/// not silently become `[]` — an `artifacts` array feeds evidence a leader
/// or independent reviewer may rely on for a usable-evidence verdict, so
/// damaged storage must surface as a read error (fail-closed), never as an
/// indistinguishable "no artifacts" row. Mirrors the `FromSqlConversionFailure`
/// idiom already used for higher-integrity columns elsewhere in this crate
/// (e.g. `vault_db`, `exec_env`), not the lower-stakes `unwrap_or_default`
/// convention some purely-advisory metadata columns still use.
fn row_to_observation(row: &rusqlite::Row<'_>) -> rusqlite::Result<MirrorEvalObservation> {
    let artifacts_raw: String = row.get(7)?;
    let artifacts: Vec<String> = serde_json::from_str(&artifacts_raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            7,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("corrupt mirror_eval_observations.artifacts JSON: {e}"),
            )),
        )
    })?;
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
        effective_role: row.get(11)?,
        effective_model_revision: row.get(12)?,
        created_at: row.get(13)?,
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

/// Codex round-2 finding #4b: same fail-closed treatment as
/// `row_to_observation`'s `artifacts` — `first_review_findings` is
/// judgment evidence the adjudication verdict rests on, so corrupt JSON
/// must surface as a read error, never silently become an empty findings
/// list indistinguishable from "no findings."
fn row_to_adjudication(row: &rusqlite::Row<'_>) -> rusqlite::Result<MirrorEvalAdjudication> {
    let findings_raw: String = row.get(7)?;
    let first_review_findings: Vec<String> = serde_json::from_str(&findings_raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            7,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("corrupt mirror_eval_adjudications.first_review_findings JSON: {e}"),
            )),
        )
    })?;
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

/// Windowed full-lifecycle views, newest first, capped at `limit` — the
/// read surface `tachi_agent_eval(action='candidate_projection')` projects
/// over. Read-only, same store/checkout semantics as every other reader in
/// this module; `until = None` means an unbounded upper end.
///
/// ## Instant semantics (offset-safe)
///
/// Selection, ordering, and the cap compare `created_at` as an ACTUAL
/// INSTANT via SQLite `julianday`, never lexically: a run stamped
/// `2026-12-31T23:30:00-01:00` is the instant `2027-01-01T00:30:00Z`, so a
/// lexical comparison against a `2027-01-01T00:00:00.000Z` cutoff would
/// wrongly include it (and the inverse offset would wrongly omit a valid
/// run). Ties at the same instant break by `eval_run_id DESC`, so the same
/// data always yields the same row sequence; the bound is
/// lower-inclusive/upper-exclusive at instant granularity.
///
/// `julianday` returns NULL for unparseable timestamps, so malformed rows
/// fail the filter here; it also ACCEPTS strings RFC3339 does not
/// (date-only forms, `now`), so callers remain the validity authority:
/// the projection re-validates each run's `created_at` with RFC3339
/// parsing before admitting it. The cap applies BEFORE that caller-side
/// validation — a bounded scan, not a load-all — so a page whose rows the
/// caller rejects is not backfilled by rescanning past the limit.
pub fn list_mirror_eval_run_views(
    conn: &Connection,
    since: &str,
    until: Option<&str>,
    limit: usize,
) -> Result<Vec<MirrorEvalRunView>, MemoryError> {
    let mut statement = conn.prepare(
        "SELECT eval_run_id FROM mirror_eval_runs \
         WHERE julianday(created_at) >= julianday(?1) \
           AND (?2 IS NULL OR julianday(created_at) < julianday(?2)) \
         ORDER BY julianday(created_at) DESC, eval_run_id DESC LIMIT ?3",
    )?;
    let eval_run_ids: Vec<String> = statement
        .query_map(params![since, until, limit as i64], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut views = Vec::with_capacity(eval_run_ids.len());
    for eval_run_id in eval_run_ids {
        if let Some(view) = get_mirror_eval_run_view(conn, Some(&eval_run_id), None)? {
            views.push(view);
        }
    }
    Ok(views)
}

/// AC-8 / codex round-2 finding #3b: every test below is net-new-capability
/// coverage with no pre-existing entry point on `origin/main` to regress —
/// structural-justification exception (compile-red, not behavioral-red),
/// per the canonical explanation in
/// `memcore::db::migrations::mirror_eval`'s module doc.
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
            // v34 fields: legacy-shaped registrations omit them (NULL).
            requested_task_type: None,
            requested_role: None,
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
        let err = register_mirror_eval_run(&conn, &cross_origin)
            .expect_err("same native id under a different execution_origin must be rejected");
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

    /// v34: the new register fields round-trip and join the replay
    /// comparator — omitted-omitted stays idempotent, and a replay that
    /// newly supplies a v34 field is an explicit conflict (a legacy row's
    /// NULL is "omitted", never equal to a supplied value).
    #[test]
    fn v39_register_fields_roundtrip_and_gate_replay() {
        let conn = open_conn();

        // Legacy-shaped registration (v34 fields omitted).
        let legacy = register_mirror_eval_run(&conn, &base_register("v34-reg-legacy")).unwrap();
        // Replay with the SAME omitted fields: idempotent.
        let replay = register_mirror_eval_run(&conn, &base_register("v34-reg-legacy")).unwrap();
        assert_eq!(replay.eval_run_id, legacy.eval_run_id);
        // Replay that newly supplies requested_task_type: explicit conflict.
        let mut upgraded = base_register("v34-reg-legacy");
        upgraded.requested_task_type = Some("fix_request".to_string());
        let err = register_mirror_eval_run(&conn, &upgraded)
            .expect_err("supplying a v34 field against a legacy row must conflict");
        assert!(err.to_string().contains("conflict"), "got: {err}");

        // A NEW registration carrying the v34 fields round-trips them.
        let mut full = base_register("v34-reg-full");
        full.requested_task_type = Some("review_request".to_string());
        full.requested_role = Some("reviewer".to_string());
        let run = register_mirror_eval_run(&conn, &full).unwrap();
        let reread = get_run_by_id(&conn, &run.eval_run_id).unwrap().unwrap();
        assert_eq!(
            reread.requested_task_type.as_deref(),
            Some("review_request")
        );
        assert_eq!(reread.requested_role.as_deref(), Some("reviewer"));
        // And replays identically.
        let replay = register_mirror_eval_run(&conn, &full).unwrap();
        assert_eq!(replay.eval_run_id, run.eval_run_id);
    }

    /// v34: the new observation fields round-trip and join the replay
    /// comparator, with the same NULL-vs-supplied conflict discipline.
    #[test]
    fn v39_observation_fields_roundtrip_and_gate_replay() {
        let conn = open_conn();
        let run = register_mirror_eval_run(&conn, &base_register("v34-obs-1")).unwrap();

        let observed = record_mirror_eval_observation(
            &conn,
            &NewMirrorEvalObservation {
                eval_run_id: run.eval_run_id.clone(),
                terminal_outcome: "success".to_string(),
                effective_model: Some("openai/gpt-5".to_string()),
                effective_role: Some("implementation".to_string()),
                effective_model_revision: Some("2026-03-10".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        // Round-trip.
        let reread = get_observation(&conn, &run.eval_run_id).unwrap().unwrap();
        assert_eq!(reread.effective_role.as_deref(), Some("implementation"));
        assert_eq!(
            reread.effective_model_revision.as_deref(),
            Some("2026-03-10")
        );
        // Same content replay: idempotent (same observation row).
        let replay = record_mirror_eval_observation(
            &conn,
            &NewMirrorEvalObservation {
                eval_run_id: run.eval_run_id.clone(),
                terminal_outcome: "success".to_string(),
                effective_model: Some("openai/gpt-5".to_string()),
                effective_role: Some("implementation".to_string()),
                effective_model_revision: Some("2026-03-10".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(replay.observation_id, observed.observation_id);

        // Legacy-shaped observation on a second run: v34 fields omitted.
        let legacy_run = register_mirror_eval_run(&conn, &base_register("v34-obs-2")).unwrap();
        let legacy_obs = NewMirrorEvalObservation {
            eval_run_id: legacy_run.eval_run_id.clone(),
            terminal_outcome: "success".to_string(),
            effective_model: Some("openai/gpt-5@2026-03-10".to_string()),
            ..Default::default()
        };
        record_mirror_eval_observation(&conn, &legacy_obs).unwrap();
        let reread = get_observation(&conn, &legacy_run.eval_run_id)
            .unwrap()
            .unwrap();
        assert_eq!(reread.effective_role, None);
        assert_eq!(reread.effective_model_revision, None);
        // Replay identical: idempotent; replay newly supplying a v34 field:
        // conflict.
        record_mirror_eval_observation(&conn, &legacy_obs).unwrap();
        let mut changed = legacy_obs.clone();
        changed.effective_role = Some("explorer".to_string());
        let err = record_mirror_eval_observation(&conn, &changed)
            .expect_err("a changed terminal snapshot must be rejected");
        assert!(err.to_string().contains("conflict"), "got: {err}");
    }

    /// v34: NEW observations may not carry conflicting revision
    /// representations — an explicit `effective_model_revision` that
    /// disagrees with the `@version` suffix on `effective_model` is refused
    /// before any write. Agreeing representations are accepted.
    #[test]
    fn v39_conflicting_revision_representations_are_refused_on_new_writes() {
        let conn = open_conn();
        let conflict_run =
            register_mirror_eval_run(&conn, &base_register("v34-rev-conflict")).unwrap();
        let err = record_mirror_eval_observation(
            &conn,
            &NewMirrorEvalObservation {
                eval_run_id: conflict_run.eval_run_id.clone(),
                terminal_outcome: "success".to_string(),
                effective_model: Some("openai/gpt-5@2026-03-10".to_string()),
                effective_model_revision: Some("2026-09-01".to_string()),
                ..Default::default()
            },
        )
        .expect_err("conflicting revision representations must be refused");
        assert!(
            err.to_string().contains("conflicting model revision"),
            "got: {err}"
        );
        assert!(
            get_observation(&conn, &conflict_run.eval_run_id)
                .unwrap()
                .is_none(),
            "the refused observation must not land"
        );

        // Agreeing representations are fine.
        let agree_run = register_mirror_eval_run(&conn, &base_register("v34-rev-agree")).unwrap();
        let observed = record_mirror_eval_observation(
            &conn,
            &NewMirrorEvalObservation {
                eval_run_id: agree_run.eval_run_id.clone(),
                terminal_outcome: "success".to_string(),
                effective_model: Some("openai/gpt-5@2026-03-10".to_string()),
                effective_model_revision: Some("2026-03-10".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            observed.effective_model_revision.as_deref(),
            Some("2026-03-10")
        );
    }

    /// The v39 boundary through the PUBLIC store-open funnel: a portable
    /// store omits the product-only mirror tables entirely and opens (and
    /// REOPENS — the integrity validation runs against the stamped-current
    /// schema) without demanding the v34 identity columns; a full product
    /// store missing a v34 column fails closed at open.
    #[test]
    fn v39_identity_columns_follow_the_product_profile_boundary_at_open() {
        use crate::db::{DbOpenContext, StoreProfile};

        // Portable: fresh open + reopen must both succeed and omit the
        // product-only mirror tables (and therefore their v34 columns).
        let portable_dir = tempfile::tempdir().unwrap();
        let portable_path = portable_dir.path().join("portable.db");
        let context =
            DbOpenContext::open_existing_deny().with_profile(StoreProfile::PortableKernel);
        let store =
            crate::MemoryStore::open_with_context(portable_path.to_str().unwrap(), &context)
                .expect("fresh portable open must not demand product mirror columns");
        for table in ["mirror_eval_runs", "mirror_eval_observations"] {
            let present: i64 = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name=?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(present, 0, "portable must omit product table {table}");
        }
        drop(store);
        crate::MemoryStore::open_with_context(portable_path.to_str().unwrap(), &context).expect(
            "portable REOPEN of a stamped-current schema must not demand product mirror columns",
        );

        // Full product: build a current store, damage the v34 shape, and
        // require the reopen to fail closed naming the missing column.
        let product_dir = tempfile::tempdir().unwrap();
        let product_path = product_dir.path().join("product.db");
        let product_context = DbOpenContext::open_existing_deny();
        let store =
            crate::MemoryStore::open_with_context(product_path.to_str().unwrap(), &product_context)
                .expect("fresh product open");
        drop(store);
        // Damage the v34 shape through a SECOND, unrestricted connection:
        // the store's own connections deny schema-mutating statements by
        // design (the test-failure-injection canon), so the corruption is
        // injected the same way an external writer would produce it.
        let external = rusqlite::Connection::open(product_path.to_str().unwrap()).unwrap();
        external
            .execute(
                "ALTER TABLE mirror_eval_runs DROP COLUMN requested_task_type",
                [],
            )
            .unwrap();
        drop(external);
        let reopened =
            crate::MemoryStore::open_with_context(product_path.to_str().unwrap(), &product_context);
        let err = reopened
            .err()
            .expect("a product store missing a v34 identity column must fail closed");
        assert!(
            err.to_string().contains("v39 mirror eval identity"),
            "the failure must name the v34 boundary: {err}"
        );
        assert!(
            err.to_string().contains("requested_task_type"),
            "the failure must name the missing column: {err}"
        );
    }

    /// The windowed view list: newest first, window-bounded, capped, and
    /// each view carries the full register/observe/adjudicate join.
    #[test]
    fn list_mirror_eval_run_views_is_windowed_newest_first_and_capped() {
        let conn = open_conn();
        let older = register_mirror_eval_run(&conn, &base_register("native-window-1")).unwrap();
        let newer = register_mirror_eval_run(&conn, &base_register("native-window-2")).unwrap();
        record_mirror_eval_observation(
            &conn,
            &NewMirrorEvalObservation {
                eval_run_id: newer.eval_run_id.clone(),
                terminal_outcome: "success".to_string(),
                effective_model: Some("anthropic/claude-sonnet".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        append_mirror_eval_adjudication(
            &conn,
            &NewMirrorEvalAdjudication {
                adjudication_id: "adj-window".to_string(),
                eval_run_id: newer.eval_run_id.clone(),
                event_key: "adj-window-key".to_string(),
                actor: "leader".to_string(),
                usefulness: "useful".to_string(),
                evidence_usable: true,
                evidence_ref: "run-window".to_string(),
                next_prompt_delta: Some("quote the failing test first".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        // `until` bounded to a past instant excludes everything.
        assert!(list_mirror_eval_run_views(
            &conn,
            "2020-01-01T00:00:00.000Z",
            Some("2020-01-02T00:00:00.000Z"),
            50
        )
        .unwrap()
        .is_empty());

        let views =
            list_mirror_eval_run_views(&conn, "2020-01-01T00:00:00.000Z", None, 50).unwrap();
        assert_eq!(views.len(), 2);
        // Both runs were created within the same normalized instant here, so
        // the eval_run_id DESC tiebreak decides; what must hold is that the
        // set is exactly {older, newer} and the adjudicated one carries its
        // full join.
        let ids: Vec<&str> = views.iter().map(|v| v.run.eval_run_id.as_str()).collect();
        assert!(ids.contains(&older.eval_run_id.as_str()));
        assert!(ids.contains(&newer.eval_run_id.as_str()));
        let adjudicated = views
            .iter()
            .find(|v| v.run.eval_run_id == newer.eval_run_id)
            .unwrap();
        assert!(adjudicated.observation.is_some());
        assert_eq!(adjudicated.adjudications.len(), 1);
        assert_eq!(
            adjudicated
                .current_adjudication()
                .unwrap()
                .next_prompt_delta
                .as_deref(),
            Some("quote the failing test first")
        );

        let capped =
            list_mirror_eval_run_views(&conn, "2020-01-01T00:00:00.000Z", None, 1).unwrap();
        assert_eq!(
            capped.len(),
            1,
            "the read is capped, not silently unbounded"
        );
    }

    /// Force a run's created_at to an explicit timestamp (offset-bearing and
    /// malformed fixtures for the instant-semantics tests below).
    fn force_run_created_at(conn: &Connection, eval_run_id: &str, created_at: &str) {
        conn.execute(
            "UPDATE mirror_eval_runs SET created_at = ?1 WHERE eval_run_id = ?2",
            params![created_at, eval_run_id],
        )
        .unwrap();
    }

    /// The windowed read selects by ACTUAL INSTANT, not lexically:
    /// offset-bearing timestamps land on their real instant on both ends
    /// (an offset run that merely LOOKS lexically in-window is excluded;
    /// one that looks lexically out-of-window but is instant-in-window is
    /// included), the bound is lower-inclusive/upper-exclusive at instant
    /// granularity, and unparseable timestamps fail the filter.
    #[test]
    fn list_mirror_eval_run_views_compares_actual_instants_not_strings() {
        let conn = open_conn();
        // Window under test: [2027-01-01T00:00:00Z, 2027-01-02T00:00:00Z).
        let since = "2027-01-01T00:00:00.000Z";
        let until = "2027-01-02T00:00:00.000Z";

        // Lexically "2026-12-31..." < since, but the instant is exactly
        // `since` — lower-inclusive, so a valid run that a lexical filter
        // would OMIT is included (the inverse-offset omission case).
        let offset_at_since =
            register_mirror_eval_run(&conn, &base_register("instant-lower-offset")).unwrap();
        force_run_created_at(
            &conn,
            &offset_at_since.eval_run_id,
            "2026-12-31T19:00:00-05:00",
        );

        // Lexically < until, but the instant (2027-01-01T00:30:00Z) is
        // inside the window — included.
        let offset_in_window =
            register_mirror_eval_run(&conn, &base_register("instant-inside-offset")).unwrap();
        force_run_created_at(
            &conn,
            &offset_in_window.eval_run_id,
            "2026-12-31T23:30:00-01:00",
        );

        // Exact upper instant (as a Z string): excluded, upper-exclusive.
        let exact_upper =
            register_mirror_eval_run(&conn, &base_register("instant-upper-exact")).unwrap();
        force_run_created_at(&conn, &exact_upper.eval_run_id, until);

        // Same upper instant expressed with an offset: also excluded.
        let offset_upper =
            register_mirror_eval_run(&conn, &base_register("instant-upper-offset")).unwrap();
        force_run_created_at(
            &conn,
            &offset_upper.eval_run_id,
            "2027-01-02T02:00:00+02:00",
        );

        // Unparseable timestamp: julianday NULL fails the filter.
        let malformed =
            register_mirror_eval_run(&conn, &base_register("instant-malformed")).unwrap();
        force_run_created_at(&conn, &malformed.eval_run_id, "not-a-timestamp");

        let views = list_mirror_eval_run_views(&conn, since, Some(until), 50).unwrap();
        let ids: Vec<&str> = views.iter().map(|v| v.run.eval_run_id.as_str()).collect();
        assert!(
            ids.contains(&offset_at_since.eval_run_id.as_str()),
            "lower-inclusive"
        );
        assert!(
            ids.contains(&offset_in_window.eval_run_id.as_str()),
            "offset in-window"
        );
        assert!(
            !ids.contains(&exact_upper.eval_run_id.as_str()),
            "exact upper instant is excluded"
        );
        assert!(
            !ids.contains(&offset_upper.eval_run_id.as_str()),
            "the same instant with an offset is equally excluded"
        );
        assert!(
            !ids.contains(&malformed.eval_run_id.as_str()),
            "an unparseable timestamp never passes the filter"
        );
    }

    /// Ordering is by ACTUAL instant (newest first) with `eval_run_id DESC`
    /// breaking same-instant ties, and the cap applies to that instant
    /// order. A date-only form SQLite accepts is returned by the preselect —
    /// RFC3339 validity remains the caller's authority, and the cap applies
    /// before that validation (bounded scan).
    #[test]
    fn list_mirror_eval_run_views_orders_by_instant_with_id_tiebreak() {
        let conn = open_conn();
        // Lexical order of these strings is X > Z > Y, but the INSTANT order
        // is Y (2027-01-01T00:00:00Z) > Z (2026-12-31T23:30:00Z) >
        // X (2026-12-31T23:00:00Z): actual-instant ordering must win.
        let x = register_mirror_eval_run(&conn, &base_register("order-x")).unwrap();
        force_run_created_at(&conn, &x.eval_run_id, "2027-01-01T01:00:00+02:00");
        let y = register_mirror_eval_run(&conn, &base_register("order-y")).unwrap();
        force_run_created_at(&conn, &y.eval_run_id, "2026-12-31T21:00:00-03:00");
        let z = register_mirror_eval_run(&conn, &base_register("order-z")).unwrap();
        force_run_created_at(&conn, &z.eval_run_id, "2026-12-31T23:30:00Z");
        // Same instant as Y — the eval_run_id DESC tiebreak decides between
        // them deterministically.
        let w = register_mirror_eval_run(&conn, &base_register("order-w")).unwrap();
        force_run_created_at(&conn, &w.eval_run_id, "2027-01-01T02:00:00+02:00");
        // Date-only form: SQLite julianday accepts it; the preselect returns
        // it and the caller's RFC3339 validation remains authoritative.
        let date_only = register_mirror_eval_run(&conn, &base_register("order-date-only")).unwrap();
        force_run_created_at(&conn, &date_only.eval_run_id, "2026-06-01");

        let views =
            list_mirror_eval_run_views(&conn, "2026-01-01T00:00:00.000Z", None, 50).unwrap();
        let ids: Vec<&str> = views.iter().map(|v| v.run.eval_run_id.as_str()).collect();
        // The same-instant pair (Y, W) occupies the first two positions in
        // eval_run_id DESC order (UUIDs are random, so assert the pair
        // occupies both slots and each precedes Z), then Z, then X, then
        // the date-only row (2026-06-01).
        let same_instant_pair = [y.eval_run_id.as_str(), w.eval_run_id.as_str()];
        assert!(
            ids.iter().take(2).all(|id| same_instant_pair.contains(id)),
            "the same-instant pair leads the instant order: {ids:?}"
        );
        assert_eq!(
            ids.iter()
                .filter(|id| same_instant_pair.contains(id))
                .count(),
            2
        );
        assert!(
            ids.iter()
                .position(|id| *id == z.eval_run_id.as_str())
                .unwrap()
                < ids
                    .iter()
                    .position(|id| *id == x.eval_run_id.as_str())
                    .unwrap(),
            "actual-instant ordering must beat the lexical order X > Z > Y"
        );
        assert!(ids.contains(&date_only.eval_run_id.as_str()));

        // The cap applies to the instant order (newest first).
        let capped =
            list_mirror_eval_run_views(&conn, "2026-01-01T00:00:00.000Z", None, 2).unwrap();
        assert_eq!(capped.len(), 2);
        let capped_ids: Vec<&str> = capped.iter().map(|v| v.run.eval_run_id.as_str()).collect();
        assert!(
            capped_ids.contains(&ids[0]) && capped_ids.contains(&ids[1]),
            "the cap keeps the newest actual instants"
        );
    }
}
