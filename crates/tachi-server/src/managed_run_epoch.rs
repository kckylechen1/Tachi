//! Durable managed-run identity and restart/orphan reconciliation.
//!
//! This module implements the owner-ratified S1 leaf: persist enough
//! identity/revision linkage for a Tachi-managed headless run to survive a
//! daemon restart as a durable work fact, then reconcile every nonterminal
//! run from an earlier controller epoch into an explicit orphan /
//! control-unavailable posture — without launching, signalling, cleaning, or
//! retrying anything.
//!
//! Core law:
//!
//! ```text
//! persisted run identity != persisted kill authority
//! ```
//!
//! - A controller epoch is minted opaque per daemon incarnation and is never
//!   persisted as global state; it appears only inside per-run identity
//!   records and in the owning server's memory.
//! - A later controller epoch may READ the durable identity and receipts but
//!   does not inherit the earlier epoch's process handle or cancellation
//!   authority: same managed_run_id + different controller epoch != resumed
//!   control authority.
//! - No PID/PGID/process handle/OS locator is persisted here at all. There is
//!   deliberately no signal, probe, kill, reap, retry, redispatch, WorkClaim
//!   takeover, or ExecEnv cleanup path in this module.
//! - Request-level restart idempotency carriers are a separate durable
//!   concern with a different authority; they must not be co-designed into
//!   this run-identity record.
//!
//! The record lives inside the existing canonical `status.json` receipt
//! spine (`managed_run_identity` stamped once at managed start, plus an
//! append-only `managed_run_reconciliation` observation). There is no second
//! lifecycle/status ledger: no new file per run, no new database table.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Mint an opaque controller-epoch id for one daemon/controller incarnation.
/// Deliberately unstructured: it carries identity only, never authority.
/// Minted per construction and never persisted as global state.
pub(crate) fn mint_controller_epoch() -> String {
    format!("ctrl-{}", uuid::Uuid::new_v4())
}

/// Identity refs collected at the managed start site. Everything here is a
/// ref, digest, or closed classification — never raw command, cwd, env,
/// credential, secret, unrestricted path, or process locator.
pub(crate) struct ManagedRunIdentityInput {
    pub(crate) controller_epoch_id: String,
    pub(crate) assignment_ref: String,
    /// One-way digest of the resolved assignment identity receipt.
    pub(crate) assignment_identity_digest: Option<String>,
    pub(crate) execution_grant_ref: String,
    pub(crate) exec_env_ref: Option<String>,
    /// One-way digest of the server-minted LaunchSpec serialization.
    pub(crate) launch_spec_digest: Option<String>,
    pub(crate) backend_name: String,
    /// One-way digest of the backend metadata payload (when the backend
    /// carries one).
    pub(crate) backend_metadata_digest: Option<String>,
}

/// Authority-surface refs minted inside the post-init dispatch block and
/// carried out for the durable identity record. Refs/digests only.
pub(crate) struct ManagedAuthorityRefs {
    pub(crate) execution_grant_ref: String,
    pub(crate) exec_env_ref: Option<String>,
    pub(crate) launch_spec_digest: Option<String>,
}

/// Digest helper. The inputs are serialized canonical JSON; env vars are
/// skipped by LaunchSpec serialization and digests are one-way regardless.
pub(crate) fn sha256_ref(bytes: &[u8]) -> String {
    format!("sha256:{}", hex_encode(&Sha256::digest(bytes)))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Best-effort host identifier for the identity record. Diagnostics only —
/// never an authority and never an OS locator of a process.
fn host_ref() -> String {
    let mut buffer = [0u8; 256];
    // SAFETY: `buffer` is a valid 256-byte array and libc::gethostname writes
    // at most its length into it, NUL-terminating when the name fits.
    let ok =
        unsafe { libc::gethostname(buffer.as_mut_ptr() as *mut libc::c_char, buffer.len()) == 0 };
    if ok {
        let end = buffer
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(buffer.len());
        if let Ok(name) = std::str::from_utf8(&buffer[..end]) {
            if !name.is_empty() {
                return name.to_string();
            }
        }
    }
    "unknown_host".to_string()
}

/// Build the closed durable identity record. The exact key set is asserted by
/// [`tests::identity_record_key_set_is_closed_and_secret_negative`]; adding a
/// key here must extend that allowlist deliberately.
pub(crate) fn build_managed_run_identity(
    dispatch_id: &str,
    input: &ManagedRunIdentityInput,
    receipt_revision_at_acceptance: u64,
) -> Value {
    json!({
        "managed_run_id": dispatch_id,
        "dispatch_id": dispatch_id,
        "controller_epoch_id": input.controller_epoch_id,
        "lifecycle_mode": "TachiManagedBatch",
        "backend_kind": "custom",
        "backend_name": input.backend_name,
        "backend_metadata_digest": input.backend_metadata_digest,
        "accepted_at": chrono::Utc::now().to_rfc3339(),
        "assignment_ref": input.assignment_ref,
        "assignment_identity_digest": input.assignment_identity_digest,
        "execution_grant_ref": input.execution_grant_ref,
        "exec_env_ref": input.exec_env_ref,
        "launch_spec_digest": input.launch_spec_digest,
        // Work-claim and attempt ownership/orphan semantics are owned
        // elsewhere; this record stores a ref only when one exists on this
        // path. Today none does — typed absence, never a guessed ref.
        "work_claim_ref": Value::Null,
        "attempt_ref": Value::Null,
        "host_ref": host_ref(),
        "receipt_revision_at_acceptance": receipt_revision_at_acceptance,
        // Closed set of run-relative artifact names; presence is resolved at
        // read time by the read projection, never treated as an unrestricted
        // path.
        "artifact_refs": ["plan.md", "prompt.md", "trajectory.jsonl", "result.md"],
    })
}

/// Append-only reconciliation observation, written at most once per run.
pub(crate) const RECONCILIATION_KEY: &str = "managed_run_reconciliation";
/// Durable identity record, stamped once at managed start and preserved by
/// every later canonical writer.
pub(crate) const IDENTITY_KEY: &str = "managed_run_identity";

/// Read the accepting controller epoch out of a canonical receipt, if the
/// receipt carries a durable identity record.
pub(crate) fn accepted_controller_epoch(status: &Value) -> Option<&str> {
    status
        .get(IDENTITY_KEY)
        .and_then(|identity| identity.get("controller_epoch_id"))
        .and_then(Value::as_str)
}

/// A run's state is terminal when the canonical state field names a terminal
/// classification. Terminal runs are never reopened and never reconciled.
fn is_terminal_state(status: &Value) -> bool {
    matches!(
        status.get("state").and_then(Value::as_str),
        Some("TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED")
    )
}

/// Volatile outcome of the startup reconciliation scan, recorded on the
/// owning server incarnation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StartupReconciliation {
    /// Number of `status.json` receipts examined under the runs root.
    pub(crate) scanned: usize,
    /// Nonterminal runs from an earlier epoch that received their single
    /// orphan/control-unavailable observation.
    pub(crate) orphaned: Vec<String>,
    /// Receipts that parse but contradict themselves (identity/dispatch
    /// mismatch). They get a typed `inconsistent` observation, never a
    /// guessed owner or state.
    pub(crate) inconsistent: Vec<String>,
    /// Receipts that exist but cannot be read/parsed. Left byte-identical;
    /// counted, never guessed about.
    pub(crate) unreadable: Vec<String>,
    /// Set when the reconciliation storage itself could not be read: the
    /// scan must then claim no clean current state.
    pub(crate) unavailable_reason: Option<String>,
    /// Per-run append failures (the receipt was reconcilable but the write
    /// failed). Never retried here; surfaced honestly.
    pub(crate) append_failures: Vec<String>,
}

impl StartupReconciliation {
    pub(crate) fn is_unavailable(&self) -> bool {
        self.unavailable_reason.is_some()
    }
}

/// Scan the runs root once at startup: terminal stays terminal; every
/// nonterminal managed run whose accepting epoch differs from
/// `current_epoch` receives exactly one typed orphan/control-unavailable
/// observation appended to its existing canonical receipt. Repeated
/// reconciliation is idempotent; a run that already carries an orphan
/// transition is never appended again.
///
/// This function performs no OS action of any kind: no signal, probe, kill,
/// reap, retry, redispatch, WorkClaim mutation, or ExecEnv/worktree cleanup.
pub(crate) fn reconcile_interrupted_managed_runs(
    runs_root: &Path,
    current_epoch: &str,
) -> StartupReconciliation {
    let mut outcome = StartupReconciliation {
        scanned: 0,
        orphaned: Vec::new(),
        inconsistent: Vec::new(),
        unreadable: Vec::new(),
        unavailable_reason: None,
        append_failures: Vec::new(),
    };
    let entries = match std::fs::read_dir(runs_root) {
        Ok(entries) => entries,
        Err(error) => {
            outcome.unavailable_reason = Some(format!("runs root unreadable: {error}"));
            return outcome;
        }
    };
    for entry in entries.flatten() {
        let run_dir = entry.path();
        if !run_dir.is_dir() {
            continue;
        }
        let status_path = run_dir.join("status.json");
        if !status_path.exists() {
            continue;
        }
        outcome.scanned += 1;
        let status = match crate::task_lifecycle::read_json_file(&status_path) {
            Ok(Some(status)) => status,
            Ok(None) => {
                outcome.unreadable.push(run_dir.display().to_string());
                continue;
            }
            Err(_) => {
                outcome.unreadable.push(run_dir.display().to_string());
                continue;
            }
        };
        let Some(identity) = status.get(IDENTITY_KEY).and_then(Value::as_object) else {
            // No durable identity record: this receipt predates durable
            // identity or is not a managed run. Reconciliation makes no
            // claim about it.
            continue;
        };
        let dispatch_id = status.get("dispatch_id").and_then(Value::as_str);
        let identity_run_id = identity.get("managed_run_id").and_then(Value::as_str);
        if dispatch_id.is_none() || dispatch_id != identity_run_id {
            let label = dispatch_id
                .map(str::to_string)
                .unwrap_or_else(|| run_dir.display().to_string());
            match append_reconciliation_observation(
                &run_dir,
                "inconsistent",
                current_epoch,
                accepted_controller_epoch(&status),
                status.get("state").and_then(Value::as_str),
            ) {
                Ok(true) => outcome.inconsistent.push(label),
                Ok(false) => {}
                Err(failure) => outcome.append_failures.push(format!("{label}:{failure}")),
            }
            continue;
        }
        let dispatch_id = dispatch_id.unwrap_or_default().to_string();
        if is_terminal_state(&status) {
            // Terminal stays terminal — byte/semantically terminal, never
            // reopened, no orphan observation.
            continue;
        }
        if accepted_controller_epoch(&status) == Some(current_epoch) {
            // This incarnation accepted the run itself; its lifecycle is
            // same-daemon and not reconciled here.
            continue;
        }
        if already_reconciled(&status) {
            // Exactly-once: an orphan transition already exists. A later
            // epoch must not duplicate it, create a replacement worker, or
            // rewrite prior evidence.
            continue;
        }
        match append_reconciliation_observation(
            &run_dir,
            "orphaned_control_unavailable",
            current_epoch,
            accepted_controller_epoch(&status),
            status.get("state").and_then(Value::as_str),
        ) {
            Ok(true) => outcome.orphaned.push(dispatch_id),
            Ok(false) => {}
            Err(failure) => outcome
                .append_failures
                .push(format!("{dispatch_id}:{failure}")),
        }
    }
    outcome
}

/// A run is already reconciled when its receipt carries an orphan
/// transition. Idempotency is keyed on the transition's existence, not on
/// which epoch recorded it.
fn already_reconciled(status: &Value) -> bool {
    status
        .get(RECONCILIATION_KEY)
        .and_then(|reconciliation| reconciliation.get("transitions"))
        .and_then(Value::as_array)
        .is_some_and(|transitions| {
            transitions.iter().any(|transition| {
                transition.get("verdict").and_then(Value::as_str)
                    == Some("orphaned_control_unavailable")
            })
        })
}

/// Append one typed reconciliation observation to the run's existing
/// canonical receipt under the per-run status lock, advancing the status
/// revision (the append is revision-bound like every canonical write).
/// Prior receipt content is preserved byte-semantically: only
/// `managed_run_reconciliation` and `status_revision` change.
fn append_reconciliation_observation(
    run_dir: &Path,
    verdict: &str,
    reconciling_epoch: &str,
    accepted_epoch: Option<&str>,
    prior_state: Option<&str>,
) -> Result<bool, String> {
    let status_path = run_dir.join("status.json");
    let lock = crate::dispatch_ops::status_json_lock_for(run_dir);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // Re-read under the lock: a completion that landed between the scan and
    // this append must win; a run that turned terminal is never appended to.
    let bytes = std::fs::read(&status_path).map_err(|error| format!("read_failed:{error}"))?;
    let mut status: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("receipt_unparsable_at_append:{error}"))?;
    if let Some(existing) = status
        .get(RECONCILIATION_KEY)
        .and_then(|reconciliation| reconciliation.get("transitions"))
        .and_then(Value::as_array)
    {
        if !existing.is_empty() {
            // Exactly-once: any prior reconciliation observation (orphan or
            // inconsistent) already exists. Never duplicate.
            return Ok(false);
        }
    }
    if verdict == "orphaned_control_unavailable" && is_terminal_state(&status) {
        // Lost the race honestly; nothing to append.
        return Ok(false);
    }
    let transition = json!({
        "verdict": verdict,
        "observed_at": chrono::Utc::now().to_rfc3339(),
        "reconciling_controller_epoch_id": reconciling_epoch,
        "accepted_controller_epoch_id": accepted_epoch,
        "prior_state": prior_state,
        "execution_state": if verdict == "inconsistent" {
            Value::String("unknown".to_string())
        } else {
            Value::String("orphaned".to_string())
        },
        "control_state": "unavailable",
    });
    let object = status
        .as_object_mut()
        .ok_or_else(|| "receipt_not_an_object".to_string())?;
    let reconciliation = object
        .entry(RECONCILIATION_KEY)
        .or_insert_with(|| json!({ "transitions": [] }));
    let transitions = reconciliation
        .as_object_mut()
        .ok_or_else(|| "reconciliation_shape_not_an_object".to_string())?
        .entry("transitions")
        .or_insert_with(|| Value::Array(Vec::new()));
    let transitions = transitions
        .as_array_mut()
        .ok_or_else(|| "reconciliation_transitions_not_an_array".to_string())?;
    transitions.push(transition);
    if !object.contains_key("status_revision") {
        return Err("missing_status_revision".to_string());
    }
    crate::managed_run_control::advance_status_revision(object)?;
    let body =
        serde_json::to_vec_pretty(&status).map_err(|error| format!("serialize_failed:{error}"))?;
    crate::utils::write_owner_only_file_atomic(&status_path, &body)
        .map(|()| true)
        .map_err(|error| format!("write_failed:{error}"))
}

/// Read-time projection exposing execution state, control state, controller
/// epoch, reconciliation state, and artifact availability as SEPARATE facts.
/// Response-only: the durable receipt is never rewritten by a read.
///
/// Returns `None` for receipts without a durable identity record — their
/// read shape is unchanged.
pub(crate) fn read_projection(
    status: &Value,
    run_dir: &Path,
    current_epoch: &str,
    has_live_same_daemon_control: bool,
) -> Option<Value> {
    let identity = status.get(IDENTITY_KEY)?;
    let accepted_epoch = accepted_controller_epoch(status);
    let state = status.get("state").and_then(Value::as_str);
    let terminal = is_terminal_state(status);
    let reconciled = already_reconciled(status);

    let execution_state = if terminal {
        // Terminal wins over everything; a stale pre-restart receipt can
        // never regress a newer terminal state, and reconciliation never
        // fabricates a terminal classification for a nonterminal run.
        Value::String(state.unwrap_or_default().to_string())
    } else if reconciled {
        json!("orphaned")
    } else if accepted_epoch.is_some() && accepted_epoch != Some(current_epoch) {
        json!("orphaned")
    } else if has_live_same_daemon_control {
        json!("running")
    } else {
        json!("unknown")
    };
    let control_state = if terminal {
        json!("not_applicable")
    } else if !terminal && has_live_same_daemon_control && accepted_epoch == Some(current_epoch) {
        json!("available")
    } else {
        json!("unavailable")
    };
    let mut artifacts = serde_json::Map::new();
    for name in identity
        .get("artifact_refs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        artifacts.insert(name.to_string(), Value::Bool(run_dir.join(name).is_file()));
    }
    Some(json!({
        "execution_state": execution_state,
        "control_state": control_state,
        "controller_epoch_id": accepted_epoch,
        "current_controller_epoch_id": current_epoch,
        "reconciliation": status.get(RECONCILIATION_KEY).cloned().unwrap_or(Value::Null),
        "artifacts_available": Value::Object(artifacts),
    }))
}

#[cfg(test)]
mod tests;
