//! Durable start association inside the canonical run's existing owner binding.
//! A retained reservation is never reclaimed by timeout, terminalization or a
//! Tasks TTL. Missing/incomplete evidence never permits an automatic re-launch.

use super::{current_owner, read_binding, read_status, AnchoredRunStatus, ResultOwner, RunBinding};
use crate::MemoryServer;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tachi_params::{StaffAssignmentRequest, TachiStaffParams};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SubmissionBinding {
    key_digest: String,
    input_digest: String,
}

/// Validated lookup data, not launch authority. Only `reserve` can mint the
/// single reservation consumed by the canonical launcher.
#[derive(Clone)]
pub(crate) struct SubmissionRequest {
    owner: ResultOwner,
    submission_id: String,
    dispatch_id: String,
    binding: SubmissionBinding,
}

pub(crate) struct SubmissionReservation {
    anchor: AnchoredRunStatus,
    run_dir: PathBuf,
    owner: ResultOwner,
    dispatch_id: String,
    binding: SubmissionBinding,
}

pub(crate) enum ReservationOutcome {
    Reserved(SubmissionReservation),
    Existing(String),
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn validate_key(key: &str) -> Result<(), String> {
    if key.is_empty()
        || key.len() > 128
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(
            "submission_id must contain 1-128 ASCII letters, digits, '.', '_' or '-'".into(),
        );
    }
    Ok(())
}

fn scoped_dispatch_id(owner: &ResultOwner, key: &str) -> Result<String, String> {
    let bytes = serde_json::to_vec(&json!({
        "schema": "tachi.staff_submission_key.v1", "owner": owner, "submission_id": key,
    }))
    .map_err(|_| "submission unavailable")?;
    Ok(format!("submission-{}", digest(&bytes)))
}

impl SubmissionRequest {
    pub(crate) fn from_start(
        server: &MemoryServer,
        request: &StaffAssignmentRequest,
    ) -> Result<Option<Self>, String> {
        let Some(key) = request.submission_id.as_deref() else {
            return Ok(None);
        };
        validate_key(key)?;
        let owner = current_owner(server).map_err(|_| "submission unavailable")?;
        let project = request.project.as_deref().ok_or("submission unavailable")?;
        let (canonical, _) = server.resolve_server_named_project_binding(project)?;
        if canonical != owner.canonical_project {
            return Err("submission unavailable".into());
        }
        // Freeze semantic input before mutable profile defaults are applied.
        // Explicit fields keep future optional fields from changing old keys.
        let input = json!({
            "schema": "tachi.staff_submission_input.v1",
            "staffing_reason": request.staffing_reason,
            "task": request.task.trim(),
            "profile": request.profile,
            "worker": request.worker,
            "stage": request.stage,
            "execution_level": request.execution_level,
            "issue_ref": request.issue_ref,
            "pr_ref": request.pr_ref,
            "flow_id": request.flow_id,
            "project": owner.canonical_project,
            "completion_predicate": request.completion_predicate,
            "recommendation_ref": request.recommendation_ref,
            "declared_file_scope": request.declared_file_scope,
        });
        let dispatch_id = scoped_dispatch_id(&owner, key)?;
        Ok(Some(Self {
            owner,
            submission_id: key.to_string(),
            dispatch_id,
            binding: SubmissionBinding {
                key_digest: digest(key.as_bytes()),
                input_digest: digest(
                    &serde_json::to_vec(&input).map_err(|_| "submission unavailable")?,
                ),
            },
        }))
    }

    fn run_dir(&self) -> PathBuf {
        crate::dispatch_ops::dispatch_runs_root().join(&self.dispatch_id)
    }

    pub(crate) fn existing(&self) -> Result<Option<String>, String> {
        match std::fs::symlink_metadata(self.run_dir()) {
            Ok(_) => self.response(true).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err("submission storage unavailable".into()),
        }
    }

    /// Exclusive directory creation is the reservation. A crash before the
    /// binding is durable leaves an unconfirmable reservation, never a retry.
    pub(crate) fn reserve(&self) -> Result<ReservationOutcome, String> {
        let run_dir = self.run_dir();
        std::fs::create_dir_all(crate::dispatch_ops::dispatch_runs_root())
            .map_err(|_| "submission storage unavailable")?;
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&run_dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return self.response(true).map(ReservationOutcome::Existing);
            }
            Err(_) => return Err("submission storage unavailable".into()),
        }
        crate::utils::sync_parent_dir(&run_dir)?;
        let anchor =
            AnchoredRunStatus::open(&run_dir).map_err(|_| "submission storage unavailable")?;
        let lock = anchor.lock();
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let fence = anchor.acquire_fence()?;
        if anchor.read_mcp_binding()?.is_some() {
            return Err("submission storage unavailable".into());
        }
        let binding = RunBinding {
            schema: 2,
            dispatch_id: self.dispatch_id.clone(),
            owner: self.owner.clone(),
            submission: Some(self.binding.clone()),
            result: None,
        };
        anchor.write_mcp_binding(
            &serde_json::to_vec(&binding).map_err(|_| "submission storage unavailable")?,
            &fence,
        )?;
        Ok(ReservationOutcome::Reserved(SubmissionReservation {
            anchor,
            run_dir,
            owner: self.owner.clone(),
            dispatch_id: self.dispatch_id.clone(),
            binding: self.binding.clone(),
        }))
    }

    fn response(&self, replayed: bool) -> Result<String, String> {
        recover(
            &self.owner,
            &self.submission_id,
            &self.dispatch_id,
            Some(&self.binding.input_digest),
            replayed,
        )
    }

    pub(crate) fn decorate_acceptance(&self, raw: &str) -> Result<String, String> {
        let mut accepted: Value =
            serde_json::from_str(raw).map_err(|_| "submission receipt unavailable")?;
        if accepted["dispatch_id"] != self.dispatch_id {
            return Err("submission receipt unavailable".into());
        }
        let recovered: Value = serde_json::from_str(&self.response(false)?)
            .map_err(|_| "submission receipt unavailable")?;
        accepted["submission"] = recovered["submission"].clone();
        serde_json::to_string(&accepted).map_err(|_| "submission receipt unavailable".into())
    }
}

impl SubmissionReservation {
    pub(crate) fn dispatch_id(&self) -> &str {
        &self.dispatch_id
    }

    /// The launch path consumes only its own opened reservation. The existing
    /// binding is immutable; result waiters can later add their own Tasks handle.
    pub(crate) fn verify_launch(
        &self,
        server: &MemoryServer,
        run_dir: &Path,
        dispatch_id: &str,
        project: Option<&str>,
    ) -> Result<(), String> {
        let owner = current_owner(server).map_err(|_| "submission unavailable")?;
        let (project, _) = server
            .resolve_server_named_project_binding(project.ok_or("submission unavailable")?)?;
        if owner != self.owner
            || project != self.owner.canonical_project
            || dispatch_id != self.dispatch_id
            || run_dir != self.run_dir.as_path()
        {
            return Err("submission unavailable".into());
        }
        // Windows anchors deny directory deletion/rename through retained
        // handles. Unix must also verify the accepted directory entry.
        #[cfg(unix)]
        self.anchor
            .validate_current_path()
            .map_err(|_| "submission unavailable")?;
        let binding = read_binding(&self.anchor, &owner, dispatch_id)
            .map_err(|_| "submission unavailable")?;
        if binding.submission.as_ref() != Some(&self.binding) {
            return Err("submission unavailable".into());
        }
        Ok(())
    }
}

fn unconfirmed(key: &str, replayed: bool) -> Value {
    json!({
        "dispatch_id": null,
        "submission": {
            "submission_id": key, "binding_state": "unconfirmed", "replayed": replayed,
            "retry_safe": false,
            "reason": "No confirmable submission binding; absence does not prove that execution never started.",
        },
    })
}

fn recover(
    owner: &ResultOwner,
    key: &str,
    dispatch_id: &str,
    expected_digest: Option<&str>,
    replayed: bool,
) -> Result<String, String> {
    let run_dir = crate::dispatch_ops::dispatch_runs_root().join(dispatch_id);
    let found = AnchoredRunStatus::open(&run_dir).ok().and_then(|anchor| {
        #[cfg(unix)]
        anchor.validate_current_path().ok()?;
        let binding = read_binding(&anchor, owner, dispatch_id).ok()?;
        let submission = binding.submission?;
        if submission.key_digest != digest(key.as_bytes()) {
            return None;
        }
        let receipt = read_status(&anchor, dispatch_id).ok();
        #[cfg(unix)]
        anchor.validate_current_path().ok()?;
        Some((submission, receipt))
    });
    let Some((binding, receipt)) = found else {
        return serde_json::to_string(&unconfirmed(key, replayed)).map_err(|e| e.to_string());
    };
    if expected_digest.is_some_and(|expected| expected != binding.input_digest) {
        return Err("submission_id conflicts with its original canonical request".into());
    }
    let response = json!({
        "dispatch_id": dispatch_id,
        "submission": {
            "submission_id": key,
            "binding_state": if receipt.is_some() { "bound" } else { "incomplete" },
            "request_digest": binding.input_digest,
            "scope": owner,
            "replayed": replayed,
            "retry_safe": false,
            "retention": "canonical_run_no_automatic_expiry",
        },
        "receipt_available": receipt.is_some(),
        "status_revision": receipt.as_ref().and_then(|value| value["status_revision"].as_u64()),
    });
    serde_json::to_string(&response).map_err(|e| e.to_string())
}

/// Shared with the narrow status-poll rate policy; malformed reads keep loop
/// detection and are rejected by this same owning validation.
pub(crate) fn recovery_key(params: &TachiStaffParams) -> Result<&str, String> {
    let key = params
        .submission_id
        .as_deref()
        .ok_or("submission_id is required")?;
    validate_key(key)?;
    for (name, supplied) in [
        ("dispatch_id", params.dispatch_id.is_some()),
        ("task", params.task.is_some()),
        ("staffing_reason", params.staffing_reason.is_some()),
        ("profile", params.profile.is_some()),
        ("worker", params.worker.is_some()),
        ("stage", params.stage.is_some()),
        ("issue_ref", params.issue_ref.is_some()),
        ("pr_ref", params.pr_ref.is_some()),
        ("flow_id", params.flow_id.is_some()),
        ("recommendation_ref", params.recommendation_ref.is_some()),
        ("declared_file_scope", params.declared_file_scope.is_some()),
        (
            "expected_status_revision",
            params.expected_status_revision.is_some(),
        ),
    ] {
        if supplied {
            return Err(format!(
                "submission recovery rejects `{name}`; supply only submission_id"
            ));
        }
    }
    Ok(key)
}

/// Key-only recovery is an alternate read form of Staff status, not a new tool.
pub(crate) fn status(server: &MemoryServer, params: &TachiStaffParams) -> Result<String, String> {
    let key = recovery_key(params)?;
    let owner = current_owner(server).map_err(|_| "submission unavailable")?;
    if let Some(project) = params.project.as_deref() {
        let (canonical, _) = server.resolve_server_named_project_binding(project)?;
        if canonical != owner.canonical_project {
            return Err("submission unavailable".into());
        }
    }
    recover(&owner, key, &scoped_dispatch_id(&owner, key)?, None, false)
}

#[cfg(test)]
mod tests;
