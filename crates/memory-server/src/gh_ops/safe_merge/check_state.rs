use super::*;
use async_trait::async_trait;
use chrono::Utc;
use serde::Serialize;
use std::path::Path;

const CHECK_STATE_FILE: &str = "check_state.json";
const CHECK_STATE_SCHEMA: &str = "tachi.github.check_state.v1";
const MISSING_FLOW_ID_REASON: &str =
    "missing flow_id: check-state snapshot was observed but not written to a Tachi flow ledger";

#[derive(Debug, Clone, Copy)]
pub(crate) struct CheckStateArtifactInput<'a> {
    pub(crate) repo: &'a str,
    pub(crate) pr_number: u64,
    pub(crate) pr_ref: Option<&'a str>,
    pub(crate) head_ref: Option<&'a str>,
    pub(crate) observed_at: &'a str,
    pub(crate) source: &'a str,
    pub(crate) dry_run: bool,
    pub(crate) checks: &'a [CheckRun],
    pub(crate) transition: Option<CheckStateTransition<'a>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CheckStateArtifactResult {
    pub(crate) persisted: bool,
    pub(crate) artifact_path: Option<String>,
    pub(crate) non_auditable_reason: Option<String>,
    pub(crate) failed_checks_recorded_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckStateRead {
    pub(crate) checks: Vec<CheckRun>,
    pub(crate) observed_head_sha: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CheckStateLedgerState {
    NoChecks,
    Pending,
    Failed,
    Passed,
    Skipped,
    Stale,
    ReaderError,
}

impl CheckStateLedgerState {
    fn as_str(self) -> &'static str {
        match self {
            CheckStateLedgerState::NoChecks => "no_checks",
            CheckStateLedgerState::Pending => "pending",
            CheckStateLedgerState::Failed => "failed",
            CheckStateLedgerState::Passed => "passed",
            CheckStateLedgerState::Skipped => "skipped",
            CheckStateLedgerState::Stale => "stale",
            CheckStateLedgerState::ReaderError => "reader_error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct CheckStateTransition<'a> {
    pub(crate) previous_state: Option<&'a str>,
    pub(crate) state: CheckStateLedgerState,
    pub(crate) changed: bool,
    pub(crate) expected_head_sha: Option<&'a str>,
    pub(crate) observed_head_sha: Option<&'a str>,
    pub(crate) read_error: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckStateIngestRequest<'a> {
    pub(crate) flow_id: &'a str,
    pub(crate) repo: &'a str,
    pub(crate) pr_number: u64,
    pub(crate) pr_ref: Option<&'a str>,
    pub(crate) head_ref: Option<&'a str>,
    pub(crate) expected_head_sha: Option<&'a str>,
    pub(crate) source: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CheckStateIngestResult {
    pub(crate) state: CheckStateLedgerState,
    pub(crate) previous_state: Option<String>,
    pub(crate) changed: bool,
    /// Degraded-input marker: `true` when the check-state reader returned an
    /// error (e.g. GitHub rate-limit / network failure) and the ledger was
    /// recorded against an empty check list. The merge decision is still
    /// computed from `pr_view`'s `checks` snapshot (independent of this
    /// ingest), but this flag lets operators notice the data was degraded so a
    /// permissive `Ready` is not mistaken for "all checks confirmed green".
    pub(crate) reader_error: bool,
    #[serde(flatten)]
    pub(crate) artifact: CheckStateArtifactResult,
}

#[async_trait]
pub(crate) trait CheckStateReader: Send + Sync {
    async fn read_check_state(&self, repo: &str, pr_number: u64)
        -> Result<CheckStateRead, GhError>;
}

#[async_trait]
impl<T: GhClient + ?Sized> CheckStateReader for T {
    /// Returns `observed_head_sha: None` today; the `Stale` ledger state is
    /// therefore unreachable in production until a real reader (see #605
    /// watcher) populates the observed head SHA.
    async fn read_check_state(
        &self,
        repo: &str,
        pr_number: u64,
    ) -> Result<CheckStateRead, GhError> {
        let checks = self.checks_list(repo, pr_number).await?;
        Ok(CheckStateRead {
            checks,
            observed_head_sha: None,
        })
    }
}

pub(crate) async fn ingest_check_state_transition<R: CheckStateReader + ?Sized>(
    reader: &R,
    request: &CheckStateIngestRequest<'_>,
) -> Result<CheckStateIngestResult, String> {
    let run_dir = run_dir_for_flow_id(request.flow_id)?;
    let previous_state = read_previous_check_state(&run_dir)?;
    let observed_at = Utc::now().to_rfc3339();
    let read = reader
        .read_check_state(request.repo, request.pr_number)
        .await
        .map_err(|err| err.to_string());

    let (checks, observed_head_sha, read_error) = match read {
        Ok(read) => (read.checks, read.observed_head_sha, None),
        Err(err) => (Vec::new(), None, Some(err)),
    };
    let state = classify_ledger_state(
        &checks,
        request.expected_head_sha,
        observed_head_sha.as_deref(),
        read_error.as_deref(),
    );
    let changed = previous_state.as_deref() != Some(state.as_str());
    let transition = CheckStateTransition {
        previous_state: previous_state.as_deref(),
        state,
        changed,
        expected_head_sha: request.expected_head_sha,
        observed_head_sha: observed_head_sha.as_deref(),
        read_error: read_error.as_deref(),
    };

    let artifact = write_check_state_artifact(
        Some(request.flow_id),
        &CheckStateArtifactInput {
            repo: request.repo,
            pr_number: request.pr_number,
            pr_ref: request.pr_ref,
            head_ref: request.head_ref,
            observed_at: observed_at.as_str(),
            source: request.source,
            dry_run: true,
            checks: &checks,
            transition: Some(transition),
        },
    )?;

    Ok(CheckStateIngestResult {
        state,
        previous_state,
        changed,
        reader_error: read_error.is_some(),
        artifact,
    })
}

pub(crate) fn write_check_state_artifact(
    flow_id: Option<&str>,
    input: &CheckStateArtifactInput<'_>,
) -> Result<CheckStateArtifactResult, String> {
    let aggregate = ChecksState::aggregate(input.checks);
    let failed_checks_recorded_only = matches!(aggregate, ChecksState::Failure);
    let Some(flow_id) = flow_id else {
        return Ok(CheckStateArtifactResult {
            persisted: false,
            artifact_path: None,
            non_auditable_reason: Some(MISSING_FLOW_ID_REASON.to_string()),
            failed_checks_recorded_only,
        });
    };

    let run_dir = run_dir_for_flow_id(flow_id)?;
    std::fs::create_dir_all(&run_dir).map_err(|e| format!("create check-state run dir: {e}"))?;
    let artifact_path = run_dir.join(CHECK_STATE_FILE);
    let artifact = build_check_state_artifact(flow_id, input, aggregate);
    write_json_atomic(&artifact_path, &artifact)?;

    merge_flow_artifact_status(
        &run_dir,
        json!({
            "artifacts": {
                "check_state": {
                    "path": artifact_path.to_string_lossy(),
                    "exists": true,
                    "schema": CHECK_STATE_SCHEMA,
                    "updated_at": input.observed_at,
                }
            }
        }),
    )?;
    merge_github_status(
        &run_dir,
        json!({
            "repo": input.repo,
            "pr_number": input.pr_number,
            "pr_ref": input.pr_ref,
            "head_ref": input.head_ref,
            "checks": {
                "state": checks_state_label(aggregate),
                "status": aggregate_status(aggregate),
                "conclusion": aggregate_conclusion(aggregate),
                "source": input.source,
                "dry_run": input.dry_run,
                "artifact": CHECK_STATE_FILE,
                "failed_checks_recorded_only": failed_checks_recorded_only,
                "updated_at": input.observed_at,
            }
        }),
    )?;

    Ok(CheckStateArtifactResult {
        persisted: true,
        artifact_path: Some(artifact_path.to_string_lossy().to_string()),
        non_auditable_reason: None,
        failed_checks_recorded_only,
    })
}

fn build_check_state_artifact(
    flow_id: &str,
    input: &CheckStateArtifactInput<'_>,
    aggregate: ChecksState,
) -> Value {
    let mut artifact = json!({
        "schema": CHECK_STATE_SCHEMA,
        "flow_id": flow_id,
        "repo": input.repo,
        "pr": {
            "number": input.pr_number,
            "ref": input.pr_ref,
            "head_ref": input.head_ref,
        },
        "observed_at": input.observed_at,
        "source": input.source,
        "dry_run": input.dry_run,
        "aggregate": {
            "state": checks_state_label(aggregate),
            "status": aggregate_status(aggregate),
            "conclusion": aggregate_conclusion(aggregate),
        },
        "buckets": check_buckets(input.checks),
        "checks": input.checks,
        "failed_checks_recorded_only": matches!(aggregate, ChecksState::Failure),
        "repair_attempted": false,
        "merge_attempted": false,
        "boundary": {
            "watch": "ingest",
            "repair": "dispatch",
            "adjudicate": "leader",
        },
    });
    if let Some(transition) = input.transition {
        artifact["transition"] = json!(transition);
    }
    artifact
}

fn read_previous_check_state(run_dir: &Path) -> Result<Option<String>, String> {
    let path = run_dir.join(CHECK_STATE_FILE);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("read {}: {err}", path.display())),
    };
    let parsed: Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
    Ok(parsed
        .get("transition")
        .and_then(|transition| transition.get("state"))
        .and_then(Value::as_str)
        .or_else(|| {
            parsed
                .get("aggregate")
                .and_then(|aggregate| aggregate.get("state"))
                .and_then(Value::as_str)
        })
        .map(normalize_previous_state))
}

fn normalize_previous_state(state: &str) -> String {
    match state {
        "failure" => CheckStateLedgerState::Failed.as_str().to_string(),
        "success" => CheckStateLedgerState::Passed.as_str().to_string(),
        "none" => CheckStateLedgerState::NoChecks.as_str().to_string(),
        "skipped" => CheckStateLedgerState::Skipped.as_str().to_string(),
        other => other.to_string(),
    }
}

fn classify_ledger_state(
    checks: &[CheckRun],
    expected_head_sha: Option<&str>,
    observed_head_sha: Option<&str>,
    read_error: Option<&str>,
) -> CheckStateLedgerState {
    if read_error.is_some() {
        return CheckStateLedgerState::ReaderError;
    }
    if expected_head_sha.is_some()
        && observed_head_sha.is_some()
        && expected_head_sha != observed_head_sha
    {
        return CheckStateLedgerState::Stale;
    }
    match ChecksState::aggregate(checks) {
        ChecksState::None => CheckStateLedgerState::NoChecks,
        ChecksState::Pending => CheckStateLedgerState::Pending,
        ChecksState::Skipped => CheckStateLedgerState::Skipped,
        ChecksState::Success => CheckStateLedgerState::Passed,
        ChecksState::Failure => CheckStateLedgerState::Failed,
    }
}

fn check_buckets(checks: &[CheckRun]) -> Value {
    let mut success = 0_u64;
    let mut failure = 0_u64;
    let mut skipped = 0_u64;
    let mut pending = 0_u64;
    let mut other = 0_u64;
    for check in checks {
        if check.status != "completed" {
            pending += 1;
            continue;
        }
        match check.conclusion.as_deref() {
            Some("success") | Some("neutral") => success += 1,
            Some("skipped") => skipped += 1,
            Some("failure") | Some("cancelled") | Some("timed_out") | Some("action_required") => {
                failure += 1
            }
            None => pending += 1,
            Some(_) => other += 1,
        }
    }
    json!({
        "success": success,
        "failure": failure,
        "pending": pending,
        "skipped": skipped,
        "other": other,
        "total": checks.len(),
    })
}

fn checks_state_label(state: ChecksState) -> &'static str {
    match state {
        ChecksState::None => "none",
        ChecksState::Pending => "pending",
        ChecksState::Skipped => "skipped",
        ChecksState::Success => "success",
        ChecksState::Failure => "failure",
    }
}

fn aggregate_status(state: ChecksState) -> &'static str {
    match state {
        ChecksState::Pending => "pending",
        ChecksState::None | ChecksState::Skipped | ChecksState::Success | ChecksState::Failure => {
            "completed"
        }
    }
}

fn aggregate_conclusion(state: ChecksState) -> Option<&'static str> {
    match state {
        ChecksState::None => None,
        ChecksState::Pending => None,
        ChecksState::Skipped => Some("skipped"),
        ChecksState::Success => Some("success"),
        ChecksState::Failure => Some("failure"),
    }
}

fn merge_flow_artifact_status(run_dir: &Path, patch: Value) -> Result<(), String> {
    let status_path = run_dir.join("status.json");
    let mut status = match std::fs::read_to_string(&status_path) {
        Ok(raw) => serde_json::from_str::<Value>(&raw)
            .map_err(|e| format!("parse {}: {e}", status_path.display()))?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(err) => return Err(format!("read {}: {err}", status_path.display())),
    };
    deep_merge(&mut status, patch);
    if let Some(obj) = status.as_object_mut() {
        obj.insert("updated_at".to_string(), json!(Utc::now().to_rfc3339()));
    }
    crate::utils::write_run_status_file(run_dir, &status)
}

fn deep_merge(target: &mut Value, patch: Value) {
    match (target, patch) {
        (Value::Object(target_obj), Value::Object(patch_obj)) => {
            for (key, value) in patch_obj {
                if value.is_null() {
                    target_obj.remove(&key);
                } else if let Some(existing) = target_obj.get_mut(&key) {
                    deep_merge(existing, value);
                } else {
                    target_obj.insert(key, value);
                }
            }
        }
        (slot, replacement) => *slot = replacement,
    }
}

fn write_json_atomic(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let raw = serde_json::to_string_pretty(value)
        .map_err(|e| format!("serialize check-state artifact: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, raw).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename check-state artifact: {e}"))?;
    Ok(())
}
