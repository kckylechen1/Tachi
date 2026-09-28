//! Atomic publication of the V2 staffing plan into the canonical run
//! `status.json` receipt.
//!
//! A model-derived plan is durable only as a committed `model_plan` object in
//! `<run_dir>/status.json`. The plan content and its exact
//! `model-invocation-v1` receipt share one write boundary: the object is
//! created only inside this module's single locked, fenced, atomic
//! `status.json` replacement. A failed, stale, empty, truncated, refused, or
//! cancelled attempt leaves the prior bytes untouched and publishes no
//! receipt. Later generic status writers preserve the committed object
//! verbatim (see `write_status_json_inner`), so a later `status_revision`
//! bump can never rebind the plan's `artifact_revision` or its receipt.
//!
//! This is a planner-specific persistence boundary, not a generic artifact
//! framework: it reuses the existing `AnchoredRunStatus` directory authority,
//! status mutex, cross-process mutation fence, `advance_status_revision`, and
//! owner-only atomic replacement.

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use tachi_llm::PersistedModelInvocationReceiptV1;

/// The only stage value this module publishes. The plan is the sole
/// model-derived stage in the surviving staffing kernel.
pub(super) const MODEL_PLAN_STAGE: &str = "plan";

/// The exact dispatch/stage binding key a committed plan receipt is bound to.
/// It is stored in the receipt's `memory_id` slot, which is reused here as an
/// opaque binding identity rather than a memory row id.
pub(super) fn plan_binding_id(dispatch_id: &str) -> String {
    format!("dispatch:{dispatch_id}:stage:plan")
}

/// A committed, content-bound model plan recovered from `status.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::dispatch_ops) struct CommittedModelPlan {
    pub content: String,
    pub payload_digest: String,
    pub artifact_revision: u64,
    pub plan_generated_at: Option<String>,
}

/// Snapshot taken before the provider is awaited.
pub(in crate::dispatch_ops) enum PlanCommitPre {
    /// No committed plan exists; a provider plan must be committed against
    /// this captured `status_revision`.
    NeedsCommit { revision: u64 },
    /// A valid committed plan already exists and must be reused unchanged.
    Reuse(CommittedModelPlan),
}

/// Outcome of a commit attempt.
enum CommitOutcome {
    /// A valid committed plan already exists (first winner). Nothing is
    /// written.
    AlreadyCommitted(CommittedModelPlan),
    /// New bytes must be atomically published.
    Publish(Map<String, Value>, CommittedModelPlan),
}

/// Typed outcome of a conditional planner-failure publication.
pub(in crate::dispatch_ops) enum PlanFailurePublication {
    /// This call published `state=FAILED` (+ failure metadata, no
    /// `model_plan`) at the captured revision. The caller must project the
    /// same terminal failure into kanban.
    Published,
    /// A newer authoritative state won (a valid committed plan, a
    /// terminal/cancelled run, or a newer status revision). Nothing was
    /// written; the caller must not terminalize status or kanban.
    Refused,
    /// The failure could not be persisted (status unreadable or the atomic
    /// replacement failed). The durable reconciliation state is unknown; the
    /// caller must not report a false terminal failure.
    PersistFailed,
}

/// Anchored handle for one run's canonical `status.json`. It retains the
/// opened run directory for its whole lifetime so a path or inode swap after
/// `open` cannot retarget the plan publication.
pub(in crate::dispatch_ops) struct PlanCommit {
    dispatch_id: String,
    #[cfg(unix)]
    anchor: crate::managed_run_control::AnchoredRunStatus,
    #[cfg(not(unix))]
    run_dir: PathBuf,
}

impl PlanCommit {
    pub(in crate::dispatch_ops) fn open(run_dir: &Path, dispatch_id: &str) -> Result<Self, String> {
        #[cfg(unix)]
        {
            let anchor = crate::managed_run_control::AnchoredRunStatus::open_ordinary(run_dir)?;
            Ok(Self {
                dispatch_id: dispatch_id.to_string(),
                anchor,
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {
                dispatch_id: dispatch_id.to_string(),
                run_dir: run_dir.to_path_buf(),
            })
        }
    }

    fn status_path(&self) -> PathBuf {
        #[cfg(unix)]
        {
            self.anchor.status_path()
        }
        #[cfg(not(unix))]
        {
            self.run_dir.join("status.json")
        }
    }

    /// Snapshot the current plan-commit state under the status lock. A valid
    /// committed plan is returned for reuse; a present-but-untrusted
    /// `model_plan` is refused instead of silently overwritten.
    pub(in crate::dispatch_ops) fn read(&self) -> Result<PlanCommitPre, String> {
        let status = self.read_status()?;
        validate_status_identity(&status, &self.dispatch_id)?;
        // Reverse order: a completion close already committed is the winner;
        // never start or reuse a plan over it.
        if resolved_completion_committed(&status) {
            return Err(format!(
                "refusing to plan: run {} already has a committed resolved_completion",
                self.dispatch_id
            ));
        }
        if status_blocks_plan(&status) {
            return Err(format!(
                "refusing to plan: run {} is terminal or cancelled",
                self.dispatch_id
            ));
        }
        if let Some(model_plan) = status.get("model_plan") {
            let committed = validated_committed_model_plan(model_plan, &self.dispatch_id)
                .ok_or_else(|| {
                    format!(
                        "refusing to reuse an invalid committed model_plan for {}",
                        self.dispatch_id
                    )
                })?;
            return Ok(PlanCommitPre::Reuse(with_generated_at(committed, &status)));
        }
        Ok(PlanCommitPre::NeedsCommit {
            revision: status_revision(&status),
        })
    }

    /// Publish `content` and its exact serving `receipt` in one atomic
    /// `status.json` replacement. First winner wins: if a valid committed plan
    /// already exists for this dispatch/stage, it is returned unchanged.
    pub(in crate::dispatch_ops) fn commit(
        &self,
        expected_revision: u64,
        content: &str,
        receipt: &PersistedModelInvocationReceiptV1,
    ) -> Result<CommittedModelPlan, String> {
        if content.trim().is_empty() {
            return Err("refusing to commit an empty model plan".to_string());
        }
        self.commit_locked(expected_revision, content, receipt)
    }

    /// Conditionally publish a planner-stage FAILURE at the captured revision:
    /// `state=FAILED` plus failure metadata and **no** `model_plan`, in one
    /// locked, fenced, CAS-checked atomic replacement. It refuses (writing
    /// nothing) when a valid committed plan, a terminal/cancelled state, or a
    /// newer status revision already won, so a concurrent completion or
    /// cancellation is never overwritten. The `error` is bounded and stripped
    /// of control characters; no provider/model content is persisted.
    pub(in crate::dispatch_ops) fn publish_plan_failure(
        &self,
        expected_revision: u64,
        error: &str,
    ) -> PlanFailurePublication {
        self.publish_plan_failure_locked(expected_revision, &bounded_plan_error(error))
    }

    #[cfg(unix)]
    fn publish_plan_failure_locked(
        &self,
        expected_revision: u64,
        error: &str,
    ) -> PlanFailurePublication {
        let lock = self.anchor.lock();
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let fence = match self.anchor.acquire_fence() {
            Ok(fence) => fence,
            Err(_) => return PlanFailurePublication::PersistFailed,
        };
        let Ok(Some(Value::Object(status))) = self.anchor.read_json() else {
            return PlanFailurePublication::PersistFailed;
        };
        let Some(status) =
            prepare_failure_publication(status, &self.dispatch_id, expected_revision, error)
        else {
            return PlanFailurePublication::Refused;
        };
        let body = match serde_json::to_vec_pretty(&Value::Object(status)) {
            Ok(body) => body,
            Err(_) => return PlanFailurePublication::PersistFailed,
        };
        match self.anchor.write_atomic(&body, &fence) {
            Ok(()) => PlanFailurePublication::Published,
            Err(_) => PlanFailurePublication::PersistFailed,
        }
    }

    #[cfg(not(unix))]
    fn publish_plan_failure_locked(
        &self,
        expected_revision: u64,
        error: &str,
    ) -> PlanFailurePublication {
        let lock = super::status_json_lock_for(&self.run_dir);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let path = self.status_path();
        let Ok(Some(Value::Object(status))) = crate::task_lifecycle::read_json_file(&path) else {
            return PlanFailurePublication::PersistFailed;
        };
        let Some(status) =
            prepare_failure_publication(status, &self.dispatch_id, expected_revision, error)
        else {
            return PlanFailurePublication::Refused;
        };
        let body = match serde_json::to_vec_pretty(&Value::Object(status)) {
            Ok(body) => body,
            Err(_) => return PlanFailurePublication::PersistFailed,
        };
        match crate::utils::write_owner_only_file_atomic(&path, &body) {
            Ok(()) => PlanFailurePublication::Published,
            Err(_) => PlanFailurePublication::PersistFailed,
        }
    }

    #[cfg(unix)]
    fn commit_locked(
        &self,
        expected_revision: u64,
        content: &str,
        receipt: &PersistedModelInvocationReceiptV1,
    ) -> Result<CommittedModelPlan, String> {
        let lock = self.anchor.lock();
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let fence = self.anchor.acquire_fence()?;
        let Some(Value::Object(status)) = self.anchor.read_json()? else {
            return Err(format!(
                "missing or malformed {}",
                self.status_path().display()
            ));
        };
        match prepare_commit(
            status,
            &self.dispatch_id,
            expected_revision,
            content,
            receipt,
        )? {
            CommitOutcome::AlreadyCommitted(committed) => Ok(committed),
            CommitOutcome::Publish(status, committed) => {
                #[cfg(test)]
                if take_plan_commit_write_failure(&self.status_path()) {
                    return Err("injected plan commit write failure".to_string());
                }
                let body = serde_json::to_vec_pretty(&Value::Object(status))
                    .map_err(|error| format!("serialize status.json: {error}"))?;
                self.anchor.write_atomic(&body, &fence)?;
                Ok(committed)
            }
        }
    }

    #[cfg(not(unix))]
    fn commit_locked(
        &self,
        expected_revision: u64,
        content: &str,
        receipt: &PersistedModelInvocationReceiptV1,
    ) -> Result<CommittedModelPlan, String> {
        let lock = super::status_json_lock_for(&self.run_dir);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let path = self.status_path();
        let Some(Value::Object(status)) = crate::task_lifecycle::read_json_file(&path)? else {
            return Err(format!("missing or malformed {}", path.display()));
        };
        match prepare_commit(
            status,
            &self.dispatch_id,
            expected_revision,
            content,
            receipt,
        )? {
            CommitOutcome::AlreadyCommitted(committed) => Ok(committed),
            CommitOutcome::Publish(status, committed) => {
                #[cfg(test)]
                if take_plan_commit_write_failure(&path) {
                    return Err("injected plan commit write failure".to_string());
                }
                let body = serde_json::to_vec_pretty(&Value::Object(status))
                    .map_err(|error| format!("serialize {}: {error}", path.display()))?;
                crate::utils::write_owner_only_file_atomic(&path, &body)?;
                Ok(committed)
            }
        }
    }

    #[cfg(unix)]
    fn read_status(&self) -> Result<Map<String, Value>, String> {
        let lock = self.anchor.lock();
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(Value::Object(status)) = self.anchor.read_json()? else {
            return Err(format!(
                "missing or malformed {}",
                self.status_path().display()
            ));
        };
        Ok(status)
    }

    #[cfg(not(unix))]
    fn read_status(&self) -> Result<Map<String, Value>, String> {
        let lock = super::status_json_lock_for(&self.run_dir);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let path = self.status_path();
        let Some(Value::Object(status)) = crate::task_lifecycle::read_json_file(&path)? else {
            return Err(format!("missing or malformed {}", path.display()));
        };
        Ok(status)
    }
}

fn with_generated_at(
    mut committed: CommittedModelPlan,
    status: &Map<String, Value>,
) -> CommittedModelPlan {
    committed.plan_generated_at = status
        .get("plan_generated_at")
        .and_then(Value::as_str)
        .map(str::to_string);
    committed
}

fn prepare_commit(
    mut status: Map<String, Value>,
    dispatch_id: &str,
    expected_revision: u64,
    content: &str,
    receipt: &PersistedModelInvocationReceiptV1,
) -> Result<CommitOutcome, String> {
    validate_status_identity(&status, dispatch_id)?;
    if resolved_completion_committed(&status) {
        return Err(format!(
            "refusing to plan: run {dispatch_id} already has a committed resolved_completion"
        ));
    }
    if status_blocks_plan(&status) {
        return Err(format!(
            "refusing to plan: run {dispatch_id} is terminal or cancelled"
        ));
    }
    if let Some(existing) = status.get("model_plan") {
        let committed = validated_committed_model_plan(existing, dispatch_id).ok_or_else(|| {
            format!("refusing to overwrite an invalid committed model_plan for {dispatch_id}")
        })?;
        return Ok(CommitOutcome::AlreadyCommitted(with_generated_at(
            committed, &status,
        )));
    }
    let current_revision = status_revision(&status);
    if current_revision != expected_revision {
        return Err(format!(
            "stale plan revision for {dispatch_id}: expected {expected_revision}, found {current_revision}"
        ));
    }
    let next_revision = crate::managed_run_control::advance_status_revision(&mut status)?;
    // The persisted receipt's `revision` field is i64; refuse rather than
    // wrap once a status revision exceeds that range.
    let bound_revision = i64::try_from(next_revision).map_err(|_| {
        format!("plan artifact revision {next_revision} exceeds the persisted receipt i64 range")
    })?;
    let payload_digest = PersistedModelInvocationReceiptV1::content_hash_for(content);
    let invocation =
        receipt
            .clone()
            .bound_to_content(content, plan_binding_id(dispatch_id), bound_revision);
    let invocation = serde_json::to_value(&invocation)
        .map_err(|error| format!("serialize model invocation receipt: {error}"))?;
    let model_plan = json!({
        "dispatch_id": dispatch_id,
        "stage": MODEL_PLAN_STAGE,
        "content": content,
        "payload_digest": payload_digest,
        "artifact_revision": next_revision,
        "model_invocation": invocation,
    });
    status.insert("model_plan".to_string(), model_plan);
    let committed = with_generated_at(
        CommittedModelPlan {
            content: content.to_string(),
            payload_digest,
            artifact_revision: next_revision,
            plan_generated_at: None,
        },
        &status,
    );
    Ok(CommitOutcome::Publish(status, committed))
}

/// Maximum retained characters of a planner failure message written into the
/// canonical status. The message may name a typed provider class but must
/// never carry model content.
const MAX_PLAN_ERROR_CHARS: usize = 500;

fn bounded_plan_error(error: &str) -> String {
    let mut bounded: String = error
        .chars()
        .take(MAX_PLAN_ERROR_CHARS)
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    if error.chars().nth(MAX_PLAN_ERROR_CHARS).is_some() {
        bounded.push('…');
    }
    bounded
}

/// Build the candidate status for a planner-failure publication, or `None`
/// when an authoritative newer state must be preserved instead (valid
/// committed plan, terminal/cancelled run, or stale revision). The returned
/// status has no `model_plan` and its revision already advanced.
fn prepare_failure_publication(
    mut status: Map<String, Value>,
    dispatch_id: &str,
    expected_revision: u64,
    error: &str,
) -> Option<Map<String, Value>> {
    if status.get("dispatch_id").and_then(Value::as_str) != Some(dispatch_id) {
        return None;
    }
    // Reverse order: a committed completion close is the winner; a planner
    // failure must not overwrite it.
    if resolved_completion_committed(&status) {
        return None;
    }
    if status_blocks_plan(&status) {
        return None;
    }
    if let Some(model_plan) = status.get("model_plan") {
        if validated_committed_model_plan(model_plan, dispatch_id).is_some() {
            return None;
        }
    }
    if status_revision(&status) != expected_revision {
        return None;
    }
    // A failure publication never carries a plan (valid or forged).
    status.remove("model_plan");
    // Apply the terminal state BEFORE advancing so `advance_status_revision`
    // mirrors FAILED (not the superseded state) into any cancellation receipt.
    apply_failure_metadata(&mut status, error);
    crate::managed_run_control::advance_status_revision(&mut status).ok()?;
    Some(status)
}

fn apply_failure_metadata(status: &mut Map<String, Value>, error: &str) {
    status.insert(
        "state".to_string(),
        Value::String("TASK_STATE_FAILED".to_string()),
    );
    status.insert(
        "plan_review_status".to_string(),
        Value::String("failed".to_string()),
    );
    status.insert("exit_code".to_string(), Value::Number(1.into()));
    status.insert(
        "updated_at".to_string(),
        Value::String(chrono::Utc::now().to_rfc3339()),
    );
    status.insert(
        "plan_failure".to_string(),
        json!({
            "stage": MODEL_PLAN_STAGE,
            "error": error,
        }),
    );
}

/// The exact, closed field set of a bound
/// `PersistedModelInvocationReceiptV1` (13 allowlisted fields plus the three
/// `bound_to_content` binding fields). A committed plan receipt must carry
/// precisely this shape; a partial or extended object is not that type.
const PERSISTED_RECEIPT_KEYS: [&str; 16] = [
    "completion_status",
    "completion_tokens",
    "content_hash",
    "degraded",
    "effective_model",
    "effective_provider",
    "effective_version",
    "engine_kind",
    "fallback_chain",
    "lane",
    "latency_ms",
    "memory_id",
    "prompt_tokens",
    "revision",
    "schema",
    "total_tokens",
];

/// True only for a properly typed, bound, successful (non-truncated) receipt
/// that names an actual serving provider and binds this exact content digest,
/// dispatch/stage identity, and artifact revision. This mirrors the closed,
/// persisted-safe receipt shape rather than trusting a hash assertion alone.
fn valid_persisted_receipt_shape(
    invocation: &Map<String, Value>,
    digest: &str,
    binding_id: &str,
    artifact_revision: u64,
) -> bool {
    if invocation.len() != PERSISTED_RECEIPT_KEYS.len()
        || !PERSISTED_RECEIPT_KEYS
            .iter()
            .all(|key| invocation.contains_key(*key))
    {
        return false;
    }
    if invocation.get("schema").and_then(Value::as_str)
        != Some(tachi_llm::MODEL_INVOCATION_SCHEMA_V1)
    {
        return false;
    }
    if invocation.get("lane").and_then(Value::as_str) != Some("reasoning") {
        return false;
    }
    if !matches!(
        invocation.get("engine_kind").and_then(Value::as_str),
        Some("provider_http") | Some("claude_cli")
    ) {
        return false;
    }
    match invocation.get("effective_provider").and_then(Value::as_str) {
        Some(provider) if !provider.trim().is_empty() => {}
        _ => return false,
    }
    for optional_text in ["effective_model", "effective_version"] {
        match invocation.get(optional_text) {
            Some(Value::Null) => {}
            Some(Value::String(text)) if !text.trim().is_empty() => {}
            _ => return false,
        }
    }
    let Some(fallback_chain) = invocation.get("fallback_chain").and_then(Value::as_array) else {
        return false;
    };
    if !fallback_chain.iter().all(Value::is_string) {
        return false;
    }
    if !matches!(invocation.get("degraded"), Some(Value::Bool(_))) {
        return false;
    }
    // A committed plan is a successful, non-truncated artifact. `unknown`
    // stays valid for providers that omit `finish_reason` (legacy
    // compatibility); an explicit provider truncation must never be reused.
    if !matches!(
        invocation.get("completion_status").and_then(Value::as_str),
        Some("complete") | Some("unknown")
    ) {
        return false;
    }
    for tokens in ["prompt_tokens", "completion_tokens", "total_tokens"] {
        match invocation.get(tokens) {
            Some(Value::Null) => {}
            Some(Value::Number(number)) if number.as_i64().is_some_and(|value| value >= 0) => {}
            _ => return false,
        }
    }
    if invocation
        .get("latency_ms")
        .and_then(Value::as_u64)
        .is_none()
    {
        return false;
    }
    if invocation.get("content_hash").and_then(Value::as_str) != Some(digest) {
        return false;
    }
    if invocation.get("memory_id").and_then(Value::as_str) != Some(binding_id) {
        return false;
    }
    // The persisted revision is i64; a u64 artifact revision outside that
    // range can never match a persisted receipt and is refused, not wrapped.
    let Ok(artifact_revision_i64) = i64::try_from(artifact_revision) else {
        return false;
    };
    if invocation.get("revision").and_then(Value::as_i64) != Some(artifact_revision_i64) {
        return false;
    }
    true
}

/// Validate a stored `model_plan` against the exact binding this module
/// writes. Returns `None` for any forged, truncated, mismatched, or otherwise
/// untrusted value so a caller never reuses it as a first winner.
pub(super) fn validated_committed_model_plan(
    model_plan: &Value,
    dispatch_id: &str,
) -> Option<CommittedModelPlan> {
    let object = model_plan.as_object()?;
    if object.get("dispatch_id").and_then(Value::as_str) != Some(dispatch_id) {
        return None;
    }
    if object.get("stage").and_then(Value::as_str) != Some(MODEL_PLAN_STAGE) {
        return None;
    }
    let content = object.get("content").and_then(Value::as_str)?;
    if content.trim().is_empty() {
        return None;
    }
    let digest = object.get("payload_digest").and_then(Value::as_str)?;
    if digest != PersistedModelInvocationReceiptV1::content_hash_for(content) {
        return None;
    }
    let artifact_revision = object.get("artifact_revision").and_then(Value::as_u64)?;
    // A committed publication always lands at a positive revision.
    if artifact_revision < 1 {
        return None;
    }
    let invocation = object.get("model_invocation")?.as_object()?;
    if !valid_persisted_receipt_shape(
        invocation,
        digest,
        &plan_binding_id(dispatch_id),
        artifact_revision,
    ) {
        return None;
    }
    Some(CommittedModelPlan {
        content: content.to_string(),
        payload_digest: digest.to_string(),
        artifact_revision,
        plan_generated_at: None,
    })
}

fn validate_status_identity(status: &Map<String, Value>, dispatch_id: &str) -> Result<(), String> {
    match status.get("dispatch_id").and_then(Value::as_str) {
        Some(found) if found == dispatch_id => Ok(()),
        Some(found) => Err(format!(
            "status.json dispatch_id mismatch: expected {dispatch_id}, found {found}"
        )),
        None => Err(format!("status.json has no dispatch_id for {dispatch_id}")),
    }
}

fn status_revision(status: &Map<String, Value>) -> u64 {
    status
        .get("status_revision")
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

/// A plan may be published only into a non-terminal, non-cancelled run.
fn status_blocks_plan(status: &Map<String, Value>) -> bool {
    if status
        .get("state")
        .and_then(Value::as_str)
        .is_some_and(|state| {
            matches!(
                state,
                "TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED"
            )
        })
    {
        return true;
    }
    status
        .get("cancellation")
        .and_then(Value::as_object)
        .and_then(|cancellation| cancellation.get("receipt"))
        .is_some_and(|receipt| !receipt.is_null())
}

/// True only when the canonical status carries a committed planner-stage
/// failure winner: an actual `plan_failure.stage == "plan"` marker with root
/// `state == "TASK_STATE_FAILED"` and no committed `model_plan`. This is the
/// producer decision written by [`PlanCommit::publish_plan_failure`]. A bare
/// execution `TASK_STATE_FAILED` with no planner marker is NOT a planner
/// winner and must not deny an ordinary reviewed completion.
pub(crate) fn planner_failure_winner(status: &Map<String, Value>) -> bool {
    status
        .get("plan_failure")
        .and_then(Value::as_object)
        .and_then(|failure| failure.get("stage"))
        .and_then(Value::as_str)
        == Some(MODEL_PLAN_STAGE)
        && status.get("state").and_then(Value::as_str) == Some("TASK_STATE_FAILED")
        && !status.contains_key("model_plan")
}

/// True when any canonical `resolved_completion` close is already committed,
/// including failed/aborted outcomes and partial completion. It is the completion
/// winner, so a later planner success/failure publication must refuse rather
/// than split the root state from the resolved receipt.
pub(crate) fn resolved_completion_committed(status: &Map<String, Value>) -> bool {
    matches!(
        status
            .get("resolved_completion")
            .and_then(Value::as_object)
            .and_then(|completion| completion.get("state"))
            .and_then(Value::as_str),
        Some("TASK_STATE_COMPLETED")
            | Some("TASK_STATE_INPUT_REQUIRED")
            | Some("TASK_STATE_FAILED")
            | Some("TASK_STATE_CANCELED")
    )
}

#[cfg(test)]
fn plan_commit_write_failures() -> &'static std::sync::Mutex<std::collections::HashSet<PathBuf>> {
    static FAILURES: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
        std::sync::OnceLock::new();
    FAILURES.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

#[cfg(test)]
fn take_plan_commit_write_failure(status_path: &Path) -> bool {
    plan_commit_write_failures()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(status_path)
}

/// One-shot injected failure for the atomic plan publication boundary.
#[cfg(test)]
pub(crate) struct PlanCommitWriteFailureGuard {
    status_path: PathBuf,
}

#[cfg(test)]
impl Drop for PlanCommitWriteFailureGuard {
    fn drop(&mut self) {
        plan_commit_write_failures()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.status_path);
    }
}

#[cfg(test)]
pub(crate) fn fail_next_plan_commit_write(run_dir: &Path) -> PlanCommitWriteFailureGuard {
    // `PlanCommit::open` anchors the canonicalized run directory, so the key
    // must be canonicalized too (macOS tempdirs live behind /var -> /private).
    let canonical = run_dir
        .canonicalize()
        .unwrap_or_else(|_| run_dir.to_path_buf());
    let status_path = canonical.join("status.json");
    plan_commit_write_failures()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(status_path.clone());
    PlanCommitWriteFailureGuard { status_path }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_status(run_dir: &Path, dispatch_id: &str, revision: u64) {
        let status = json!({
            "dispatch_id": dispatch_id,
            "state": "TASK_STATE_WORKING",
            "status_revision": revision,
        });
        crate::utils::write_owner_only_file_atomic(
            &run_dir.join("status.json"),
            serde_json::to_vec_pretty(&status).unwrap().as_slice(),
        )
        .expect("seed status.json");
    }

    fn read_status(run_dir: &Path) -> Value {
        serde_json::from_slice(&std::fs::read(run_dir.join("status.json")).expect("status bytes"))
            .expect("status JSON")
    }

    fn plan_binding() -> PersistedModelInvocationReceiptV1 {
        PersistedModelInvocationReceiptV1::test_fixture_reasoning(5)
    }

    #[test]
    fn commit_publishes_content_bound_receipt_at_one_revision() {
        let temp = tempfile::tempdir().expect("run dir");
        seed_status(temp.path(), "d1", 3);
        let commit = PlanCommit::open(temp.path(), "d1").expect("open");
        let content = "## Goal\nAdd the feature.";
        let committed = commit
            .commit(3, content, &plan_binding())
            .expect("commit plan");
        assert_eq!(committed.artifact_revision, 4);
        assert_eq!(
            committed.payload_digest,
            PersistedModelInvocationReceiptV1::content_hash_for(content)
        );

        let status = read_status(temp.path());
        assert_eq!(status["status_revision"], 4);
        let model_plan = &status["model_plan"];
        assert_eq!(model_plan["dispatch_id"], "d1");
        assert_eq!(model_plan["stage"], "plan");
        assert_eq!(model_plan["content"], content);
        assert_eq!(
            model_plan["payload_digest"],
            PersistedModelInvocationReceiptV1::content_hash_for(content)
        );
        assert_eq!(model_plan["artifact_revision"], 4);
        let invocation = model_plan["model_invocation"].as_object().unwrap();
        assert_eq!(invocation["schema"], "model-invocation-v1");
        assert_eq!(invocation["lane"], "reasoning");
        assert_eq!(
            invocation["content_hash"],
            PersistedModelInvocationReceiptV1::content_hash_for(content)
        );
        assert_eq!(invocation["memory_id"], "dispatch:d1:stage:plan");
        assert_eq!(invocation["revision"], 4);
        // Secret-negative: the persisted receipt is the closed allowlist shape
        // and carries no credential/request/response material.
        let mut keys = invocation.keys().cloned().collect::<Vec<_>>();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "completion_status",
                "completion_tokens",
                "content_hash",
                "degraded",
                "effective_model",
                "effective_provider",
                "effective_version",
                "engine_kind",
                "fallback_chain",
                "lane",
                "latency_ms",
                "memory_id",
                "prompt_tokens",
                "revision",
                "schema",
                "total_tokens",
            ]
        );
    }

    #[test]
    fn repeat_commit_returns_first_winner_without_rewriting() {
        let temp = tempfile::tempdir().expect("run dir");
        seed_status(temp.path(), "d1", 3);
        let commit = PlanCommit::open(temp.path(), "d1").expect("open");
        let first = commit
            .commit(3, "## Goal\nfirst", &plan_binding())
            .expect("first commit");
        let bytes_after_first = std::fs::read(temp.path().join("status.json")).expect("bytes");

        let second = commit
            .commit(999, "## Goal\nsecond", &plan_binding())
            .expect("first winner returned");
        assert_eq!(second, first);
        assert_eq!(
            std::fs::read(temp.path().join("status.json")).expect("bytes"),
            bytes_after_first,
            "a losing commit must not rewrite the committed plan"
        );

        // The pre-call snapshot must reuse the committed plan, so a re-entrant
        // plan stage never makes a second model call.
        match commit.read().expect("read") {
            PlanCommitPre::Reuse(existing) => assert_eq!(existing.content, "## Goal\nfirst"),
            PlanCommitPre::NeedsCommit { .. } => panic!("expected an existing plan to reuse"),
        }
    }

    #[test]
    fn concurrent_commits_keep_the_first_winner() {
        let temp = tempfile::tempdir().expect("run dir");
        seed_status(temp.path(), "d1", 1);
        let dir_a = temp.path().to_path_buf();
        let dir_b = temp.path().to_path_buf();
        let receipt_a = plan_binding();
        let receipt_b = plan_binding();
        let content_a = "## Goal\nA".to_string();
        let content_b = "## Goal\nB".to_string();
        let results = std::thread::scope(|scope| {
            let handle_a = scope.spawn(move || {
                PlanCommit::open(&dir_a, "d1")
                    .expect("open a")
                    .commit(1, &content_a, &receipt_a)
            });
            let handle_b = scope.spawn(move || {
                PlanCommit::open(&dir_b, "d1")
                    .expect("open b")
                    .commit(1, &content_b, &receipt_b)
            });
            [
                handle_a.join().expect("join a"),
                handle_b.join().expect("join b"),
            ]
        });
        let contents = results
            .iter()
            .map(|result| {
                result
                    .as_ref()
                    .expect("both callers must observe a committed plan")
                    .content
                    .clone()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            contents[0], contents[1],
            "both return the same first winner"
        );
        let status = read_status(temp.path());
        assert_eq!(status["model_plan"]["content"], contents[0]);
        assert_eq!(
            status["model_plan"]["artifact_revision"], 2,
            "exactly one publication advances the committed artifact revision"
        );
    }

    #[test]
    fn stale_revision_and_terminal_run_publish_nothing() {
        let temp = tempfile::tempdir().expect("run dir");
        seed_status(temp.path(), "d1", 3);
        let commit = PlanCommit::open(temp.path(), "d1").expect("open");
        let before = std::fs::read(temp.path().join("status.json")).expect("bytes");
        let error = commit
            .commit(2, "## Goal\nstale", &plan_binding())
            .expect_err("stale revision must be refused");
        assert!(error.contains("stale plan revision"), "{error}");
        assert_eq!(
            std::fs::read(temp.path().join("status.json")).expect("bytes"),
            before
        );
        assert!(read_status(temp.path()).get("model_plan").is_none());

        let mut status = read_status(temp.path());
        status["state"] = json!("TASK_STATE_CANCELED");
        crate::utils::write_owner_only_file_atomic(
            &temp.path().join("status.json"),
            serde_json::to_vec_pretty(&status).unwrap().as_slice(),
        )
        .expect("reseed cancelled status");
        let cancelled = std::fs::read(temp.path().join("status.json")).expect("bytes");
        let error = commit
            .commit(4, "## Goal\ncancelled", &plan_binding())
            .expect_err("cancelled run must be refused");
        assert!(error.contains("terminal or cancelled"), "{error}");
        assert_eq!(
            std::fs::read(temp.path().join("status.json")).expect("bytes"),
            cancelled
        );
    }

    #[test]
    fn forged_or_mismatched_committed_plan_is_refused() {
        let temp = tempfile::tempdir().expect("run dir");
        let forged = json!({
            "dispatch_id": "d1",
            "state": "TASK_STATE_WORKING",
            "status_revision": 4,
            "model_plan": {
                "dispatch_id": "d1",
                "stage": "plan",
                "content": "## Goal\nforged",
                "payload_digest": "deadbeef",
                "artifact_revision": 4,
                "model_invocation": {"schema": "model-invocation-v1"},
            },
        });
        crate::utils::write_owner_only_file_atomic(
            &temp.path().join("status.json"),
            serde_json::to_vec_pretty(&forged).unwrap().as_slice(),
        )
        .expect("write forged status");
        let commit = PlanCommit::open(temp.path(), "d1").expect("open");
        assert!(
            commit.read().is_err(),
            "a mismatched committed plan must not be reused"
        );
        assert!(
            commit
                .commit(4, "## Goal\nreplacement", &plan_binding())
                .is_err(),
            "a forged committed plan must not be overwritten silently"
        );
    }

    #[test]
    fn injected_write_failure_leaves_prior_bytes_and_no_plan() {
        let temp = tempfile::tempdir().expect("run dir");
        seed_status(temp.path(), "d1", 5);
        let commit = PlanCommit::open(temp.path(), "d1").expect("open");
        let before = std::fs::read(temp.path().join("status.json")).expect("bytes");
        let _failure = fail_next_plan_commit_write(temp.path());
        let error = commit
            .commit(5, "## Goal\nnever lands", &plan_binding())
            .expect_err("injected write failure must surface");
        assert!(
            error.contains("injected plan commit write failure"),
            "{error}"
        );
        assert_eq!(
            std::fs::read(temp.path().join("status.json")).expect("bytes"),
            before,
            "a failed publication must leave the prior status bytes untouched"
        );
        assert!(read_status(temp.path()).get("model_plan").is_none());
    }

    #[test]
    fn generic_writer_preserves_committed_plan_and_blocks_extra_override() {
        let temp = tempfile::tempdir().expect("run dir");
        seed_status(temp.path(), "d1", 1);
        let commit = PlanCommit::open(temp.path(), "d1").expect("open");
        let committed = commit
            .commit(1, "## Goal\ncanonical", &plan_binding())
            .expect("commit plan");

        super::super::write_status_json(
            temp.path(),
            "d1",
            true,
            Some("fixed"),
            None,
            "approved",
            None,
            None,
            None,
            None,
            Some(json!({
                "state": "TASK_STATE_WORKING",
                "model_plan": {"forged": true},
            })),
        );
        // Route evidence and the terminal outcome are two further canonical
        // writers; provenance must survive every one of them, not just the
        // first rewrite.
        super::super::stamp_route_decision_id(temp.path(), "route-1664").expect("stamp route");
        super::super::write_status_json(
            temp.path(),
            "d1",
            true,
            Some("fixed"),
            Some("done"),
            "approved",
            Some(0),
            None,
            None,
            None,
            Some(json!({ "state": "TASK_STATE_COMPLETED" })),
        );

        let status = read_status(temp.path());
        assert_eq!(
            status["model_plan"]["content"], "## Goal\ncanonical",
            "a later generic writer must preserve the committed plan"
        );
        assert_eq!(
            status["model_plan"]["artifact_revision"], committed.artifact_revision,
            "a later status_revision bump must not rebind artifact_revision"
        );
        assert!(
            status["status_revision"].as_u64().unwrap_or(0) > committed.artifact_revision,
            "the generic writer still advances the root status revision"
        );
        assert_eq!(
            status["route_decision_id"], "route-1664",
            "the route writer must land without disturbing the plan"
        );
        assert_eq!(status["state"], "TASK_STATE_COMPLETED");
        let revalidated = validated_committed_model_plan(&status["model_plan"], "d1")
            .expect("provenance must stay valid after every status writer");
        assert_eq!(revalidated.artifact_revision, committed.artifact_revision);
        assert_eq!(revalidated.content, "## Goal\ncanonical");
    }

    #[test]
    fn validated_plan_rejects_binding_drift() {
        let temp = tempfile::tempdir().expect("run dir");
        seed_status(temp.path(), "d1", 1);
        let commit = PlanCommit::open(temp.path(), "d1").expect("open");
        commit
            .commit(1, "## Goal\nbound", &plan_binding())
            .expect("commit plan");
        let status = read_status(temp.path());
        let model_plan = status["model_plan"].clone();
        assert!(validated_committed_model_plan(&model_plan, "d1").is_some());

        let mut wrong_content = model_plan.clone();
        wrong_content["content"] = json!("## Goal\ntampered");
        assert!(validated_committed_model_plan(&wrong_content, "d1").is_none());

        let mut wrong_identity = model_plan.clone();
        wrong_identity["dispatch_id"] = json!("other");
        assert!(validated_committed_model_plan(&wrong_identity, "d1").is_none());

        let mut wrong_revision = model_plan.clone();
        wrong_revision["model_invocation"]["revision"] = json!(99);
        assert!(validated_committed_model_plan(&wrong_revision, "d1").is_none());

        let mut wrong_memory = model_plan.clone();
        wrong_memory["model_invocation"]["memory_id"] = json!("dispatch:other:stage:plan");
        assert!(validated_committed_model_plan(&wrong_memory, "d1").is_none());

        // Every content hash below still matches the bound content: the typed
        // receipt shape, not the hash alone, is what these cases discriminate.
        let mut truncated = model_plan.clone();
        truncated["model_invocation"]["completion_status"] = json!("truncated");
        assert!(
            validated_committed_model_plan(&truncated, "d1").is_none(),
            "a truncated completion must never be reused as a successful plan"
        );

        let mut missing_field = model_plan.clone();
        missing_field["model_invocation"]
            .as_object_mut()
            .expect("invocation object")
            .remove("latency_ms");
        assert!(
            validated_committed_model_plan(&missing_field, "d1").is_none(),
            "a partial receipt is not the closed persisted receipt type"
        );

        let mut extra_field = model_plan.clone();
        extra_field["model_invocation"]["unexpected"] = json!("forged");
        assert!(
            validated_committed_model_plan(&extra_field, "d1").is_none(),
            "an extended receipt is not the closed persisted receipt type"
        );

        let mut no_identity = model_plan.clone();
        no_identity["model_invocation"]["effective_provider"] = Value::Null;
        assert!(
            validated_committed_model_plan(&no_identity, "d1").is_none(),
            "a receipt without an actual serving provider is not provenance"
        );

        let mut zero_revision = model_plan.clone();
        zero_revision["artifact_revision"] = json!(0);
        zero_revision["model_invocation"]["revision"] = json!(0);
        assert!(
            validated_committed_model_plan(&zero_revision, "d1").is_none(),
            "a committed publication always lands at a positive revision"
        );
    }

    #[test]
    fn revision_overflowing_receipt_i64_range_is_refused_without_writing() {
        let temp = tempfile::tempdir().expect("run dir");
        seed_status(temp.path(), "d1", i64::MAX as u64);
        let commit = PlanCommit::open(temp.path(), "d1").expect("open");
        let before = std::fs::read(temp.path().join("status.json")).expect("bytes");
        let error = commit
            .commit(i64::MAX as u64, "## Goal\noverflow", &plan_binding())
            .expect_err("a revision beyond the receipt i64 range must be refused");
        assert!(error.contains("revision"), "{error}");
        assert_eq!(
            std::fs::read(temp.path().join("status.json")).expect("bytes"),
            before,
            "a refused overflow must leave the prior bytes untouched"
        );
        assert!(read_status(temp.path()).get("model_plan").is_none());
    }

    #[test]
    fn revision_at_receipt_i64_boundary_is_valid() {
        let temp = tempfile::tempdir().expect("run dir");
        let current = (i64::MAX as u64) - 1;
        seed_status(temp.path(), "d1", current);
        let commit = PlanCommit::open(temp.path(), "d1").expect("open");
        let committed = commit
            .commit(current, "## Goal\nboundary", &plan_binding())
            .expect("i64::MAX is a valid persisted revision");
        assert_eq!(committed.artifact_revision, i64::MAX as u64);
        let status = read_status(temp.path());
        assert_eq!(status["model_plan"]["artifact_revision"], i64::MAX);
        assert_eq!(
            status["model_plan"]["model_invocation"]["revision"],
            i64::MAX,
            "the receipt revision must bind exactly at the i64 boundary"
        );
    }

    #[test]
    fn failure_publication_wins_only_without_a_newer_winner() {
        let temp = tempfile::tempdir().expect("run dir");
        seed_status(temp.path(), "d1", 1);
        let commit = PlanCommit::open(temp.path(), "d1").expect("open");
        assert!(matches!(
            commit.publish_plan_failure(1, "provider exploded"),
            PlanFailurePublication::Published
        ));
        let status = read_status(temp.path());
        assert_eq!(status["state"], "TASK_STATE_FAILED");
        assert_eq!(status["plan_review_status"], "failed");
        assert_eq!(status["exit_code"], 1);
        assert_eq!(status["plan_failure"]["stage"], "plan");
        assert!(
            status.get("model_plan").is_none(),
            "a failure publication never carries a model_plan"
        );
        let revision_after = status["status_revision"].as_u64().unwrap();

        // A terminal winner refuses to be overwritten.
        let before = std::fs::read(temp.path().join("status.json")).expect("bytes");
        assert!(matches!(
            commit.publish_plan_failure(revision_after, "second failure"),
            PlanFailurePublication::Refused
        ));
        assert_eq!(
            std::fs::read(temp.path().join("status.json")).expect("bytes"),
            before
        );

        // A stale expected revision refuses too.
        assert!(matches!(
            commit.publish_plan_failure(0, "stale failure"),
            PlanFailurePublication::Refused
        ));
    }

    #[test]
    fn failure_publication_reports_persist_failed_when_status_is_unreadable() {
        let temp = tempfile::tempdir().expect("run dir");
        // A directory where status.json is expected makes the anchored read
        // fail: no failure may be fabricated and none may be reported as a
        // false terminal.
        std::fs::create_dir(temp.path().join("status.json")).expect("status directory");
        let commit = PlanCommit::open(temp.path(), "d1").expect("open");
        assert!(matches!(
            commit.publish_plan_failure(1, "unreadable status"),
            PlanFailurePublication::PersistFailed
        ));
    }

    #[test]
    fn failure_publication_refuses_to_clobber_a_committed_plan() {
        let temp = tempfile::tempdir().expect("run dir");
        seed_status(temp.path(), "d1", 1);
        let commit = PlanCommit::open(temp.path(), "d1").expect("open");
        commit
            .commit(1, "## Goal\nwinner", &plan_binding())
            .expect("commit plan");
        let before = std::fs::read(temp.path().join("status.json")).expect("bytes");
        assert!(matches!(
            commit.publish_plan_failure(2, "late failure"),
            PlanFailurePublication::Refused
        ));
        assert_eq!(
            std::fs::read(temp.path().join("status.json")).expect("bytes"),
            before,
            "a concurrent committed plan must never be clobbered by a late failure"
        );
    }

    #[test]
    fn planner_failure_winner_requires_the_producer_shape() {
        let winner = json!({
            "state": "TASK_STATE_FAILED",
            "plan_failure": {"stage": "plan", "error": "x"},
        });
        assert!(planner_failure_winner(winner.as_object().unwrap()));
        // A bare execution FAILED (no planner marker) is not a planner winner.
        let bare = json!({"state": "TASK_STATE_FAILED"});
        assert!(!planner_failure_winner(bare.as_object().unwrap()));
        // A committed plan means the plan phase won, not failed.
        let with_plan = json!({
            "state": "TASK_STATE_FAILED",
            "plan_failure": {"stage": "plan"},
            "model_plan": {"content": "## Goal\nx"},
        });
        assert!(!planner_failure_winner(with_plan.as_object().unwrap()));
        // A non-plan failure stage is not a planner winner.
        let other_stage = json!({
            "state": "TASK_STATE_FAILED",
            "plan_failure": {"stage": "execute"},
        });
        assert!(!planner_failure_winner(other_stage.as_object().unwrap()));
    }

    /// Reverse order: a committed completion close is the winner, so a later
    /// plan success OR failure publication must refuse. Root `state` alone is
    /// deliberately kept WORKING to prove the check is not root-state-based.
    #[test]
    fn plan_writes_refuse_a_committed_resolved_completion() {
        for completion_state in [
            "TASK_STATE_COMPLETED",
            "TASK_STATE_INPUT_REQUIRED",
            "TASK_STATE_FAILED",
            "TASK_STATE_CANCELED",
        ] {
            let temp = tempfile::tempdir().expect("run dir");
            let status = json!({
                "dispatch_id": "d1",
                "state": "TASK_STATE_WORKING",
                "status_revision": 4,
                "resolved_completion": {"state": completion_state},
            });
            crate::utils::write_owner_only_file_atomic(
                &temp.path().join("status.json"),
                serde_json::to_vec_pretty(&status).unwrap().as_slice(),
            )
            .expect("seed completion winner");
            let commit = PlanCommit::open(temp.path(), "d1").expect("open");
            let before = std::fs::read(temp.path().join("status.json")).expect("bytes");

            assert!(
                commit.read().is_err(),
                "a committed completion must block plan reuse"
            );
            assert!(
                commit.commit(4, "## Goal\nlate", &plan_binding()).is_err(),
                "a committed completion must block a plan success write"
            );
            assert!(
                matches!(
                    commit.publish_plan_failure(4, "late failure"),
                    PlanFailurePublication::Refused
                ),
                "a committed completion must block a plan failure write"
            );
            assert_eq!(
                std::fs::read(temp.path().join("status.json")).expect("bytes"),
                before,
                "no plan write may touch a committed completion winner"
            );
        }
    }
}
