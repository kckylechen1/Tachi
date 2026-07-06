use super::*;
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CheckStateArtifactResult {
    pub(crate) persisted: bool,
    pub(crate) artifact_path: Option<String>,
    pub(crate) non_auditable_reason: Option<String>,
    pub(crate) failed_checks_recorded_only: bool,
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
    json!({
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
    })
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
