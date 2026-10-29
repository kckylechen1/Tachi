//! New local ownership is sealed by explicit claim + intake. It grants no
//! execution authority; other producers refuse before their first side effect.
use super::*;
use crate::claims_ops::project_ownership::{LocalProject, UNAVAILABLE};
use memcore::db::session_claims;

#[cfg(windows)]
mod windows_legacy;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FlowSeal {
    version: u32,
    claim_id: String,
    canonical_project: String,
    flow_id: String,
}

#[cfg(unix)]
fn anchor(path: &Path) -> Result<crate::managed_run_control::AnchoredRunStatus, String> {
    crate::managed_run_control::AnchoredRunStatus::open(path).map_err(|_| UNAVAILABLE.into())
}

/// Deny-only classifier for an exact receipt or an existing legacy scan.
/// It keeps only the seal; ignored receipt fields never become JSON Values.
#[cfg(unix)]
fn seal(path: &Path) -> Result<Option<FlowSeal>, String> {
    let anchor = anchor(path)?;
    let seal = anchor.read_field::<FlowSeal>("ownership", 1024 * 1024)?;
    anchor.validate_current_path()?;
    if seal.as_ref().is_some_and(|s| {
        s.version != 1
            || s.claim_id.is_empty()
            || s.canonical_project.is_empty()
            || path.file_name().and_then(|s| s.to_str()) != Some(&s.flow_id)
    }) {
        return Err(UNAVAILABLE.into());
    }
    Ok(seal)
}

#[cfg(windows)]
fn seal(path: &Path) -> Result<Option<FlowSeal>, String> {
    windows_legacy::preflight(path)?;
    Ok(None)
}

#[cfg(not(any(unix, windows)))]
fn seal(_path: &Path) -> Result<Option<FlowSeal>, String> {
    Err(UNAVAILABLE.into())
}

pub(crate) fn legacy_read_allowed(run_dir: &Path) -> bool {
    run_dir.join("status.json").exists() && matches!(seal(run_dir), Ok(None))
}

/// Only the existing verification advisory producer/reader may lack status.
/// This never confers claim authority and cannot authorize another artifact.
pub(crate) fn legacy_verification_preflight(run_dir: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use crate::managed_run_control::AnchoredRunStatus;
        match AnchoredRunStatus::open(run_dir) {
            Err(error) if error.is_absent() => Ok(()),
            Err(_) => Err(UNAVAILABLE.into()),
            Ok(anchor) => {
                if anchor
                    .read_field::<serde::de::IgnoredAny>("ownership", 1024 * 1024)?
                    .is_some()
                {
                    return Err(UNAVAILABLE.into());
                }
                anchor.validate_current_path()
            }
        }
    }
    #[cfg(windows)]
    {
        windows_legacy::preflight(run_dir)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = run_dir;
        Err(UNAVAILABLE.into())
    }
}

pub(crate) fn read_legacy_verification(run_dir: &Path) -> Result<Option<Value>, String> {
    #[cfg(unix)]
    {
        use crate::managed_run_control::AnchoredRunStatus;
        let anchor = match AnchoredRunStatus::open(run_dir) {
            Ok(anchor) => anchor,
            Err(error) if error.is_absent() => return Ok(None),
            Err(_) => return Err(UNAVAILABLE.into()),
        };
        anchor
            .read_legacy_verification()?
            .map(|bytes| serde_json::from_slice(&bytes).map_err(|_| UNAVAILABLE.into()))
            .transpose()
    }
    #[cfg(windows)]
    {
        windows_legacy::read_verification(run_dir)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = run_dir;
        Err(UNAVAILABLE.into())
    }
}

pub(crate) fn legacy_preflight(server: &MemoryServer, flow_id: Option<&str>) -> Result<(), String> {
    let Some(flow_id) = flow_id else {
        return Ok(());
    };
    let protected = server.with_global_store_read(|store| store.connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM session_claims WHERE flow_id=?1 AND canonical_project IS NOT NULL)",
        [flow_id], |row|row.get::<_,bool>(0),
    ).map_err(|_| UNAVAILABLE.to_string()))?;
    if protected {
        Err(UNAVAILABLE.into())
    } else {
        Ok(())
    }
}

pub(crate) fn read_legacy_json(path: &Path) -> Option<Result<Option<Value>, String>> {
    let dir = path.parent()?;
    if !dir
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("flow_"))
    {
        return None;
    }
    #[cfg(windows)]
    {
        Some(windows_legacy::read_json(path))
    }
    #[cfg(not(any(unix, windows)))]
    {
        Some(Err(UNAVAILABLE.into()))
    }
    #[cfg(unix)]
    {
        Some((|| {
            if !path.exists() && !dir.exists() {
                return Ok(None);
            }
            let anchor = anchor(dir)?;
            let bytes = anchor.read_unsealed_component(
                path.file_name()
                    .and_then(|s| s.to_str())
                    .ok_or(UNAVAILABLE)?,
                1024 * 1024,
            )?;
            bytes
                .map(|bytes| serde_json::from_slice(&bytes).map_err(|_| UNAVAILABLE.into()))
                .transpose()
        })())
    }
}

pub(crate) fn refuse_legacy_path(path: &Path) -> Result<(), String> {
    for dir in path.ancestors().skip(1) {
        if dir
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("flow_"))
        {
            // New legacy status creation has no private predecessor to expose.
            let absent = path.file_name().is_some_and(|n| n == "status.json") && !path.exists();
            if !absent && !legacy_read_allowed(dir) {
                return Err(UNAVAILABLE.into());
            }
            break;
        }
    }
    Ok(())
}

/// Legacy writes keep their payload contract but cannot follow a replacement
/// directory or downgrade a seal. The existing run fence serializes creation
/// with the first seal publication across independent processes.
pub(crate) fn write_legacy(path: &Path, bytes: &[u8], append: bool) -> Option<Result<(), String>> {
    let flow = path.ancestors().skip(1).find(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("flow_"))
    })?;
    #[cfg(windows)]
    {
        Some(windows_legacy::write(flow, path, bytes, append))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (bytes, append, flow);
        Some(Err(UNAVAILABLE.into()))
    }
    #[cfg(unix)]
    {
        Some((|| {
            let root = anchor(flow)?;
            let lock = root.lock();
            let _lock = lock.lock().map_err(|_| UNAVAILABLE)?;
            if root
                .read_field::<serde::de::IgnoredAny>("ownership", 1024 * 1024)?
                .is_some()
            {
                return Err(UNAVAILABLE.into());
            }
            root.validate_current_path()?;
            let parent = path
                .parent()
                .ok_or(UNAVAILABLE)?
                .strip_prefix(flow)
                .map_err(|_| UNAVAILABLE)?;
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or(UNAVAILABLE)?;
            let fence = match root.acquire_fence() {
                Ok(fence) => fence,
                Err(_) if root.directory_read_only() => {
                    if append && parent.as_os_str().is_empty() && name == "events.jsonl" {
                        return root.append_existing_legacy_events(bytes);
                    }
                    if !append
                        && parent == Path::new("artifacts")
                        && name.starts_with("dispatch-")
                        && name.ends_with(".json")
                    {
                        let child = root.child_directory(Path::new("artifacts"))?;
                        let lock = child.lock();
                        let _lock = lock.lock().map_err(|_| UNAVAILABLE)?;
                        let fence = child.acquire_fence()?;
                        root.validate_current_path()?;
                        child.write_component_atomic(name, bytes, &fence)?;
                        root.validate_current_path()?;
                        return child.validate_current_path();
                    }
                    return Err(UNAVAILABLE.into());
                }
                Err(error) => return Err(error),
            };
            // A seal may have been published while this writer waited for its fence.
            if root
                .read_field::<serde::de::IgnoredAny>("ownership", 1024 * 1024)?
                .is_some()
            {
                return Err(UNAVAILABLE.into());
            }
            if parent.as_os_str().is_empty() {
                if append {
                    root.append_component(name, bytes, &fence)?;
                } else {
                    root.write_component_atomic(name, bytes, &fence)?;
                }
            } else {
                let child = root.child_directory(parent)?;
                let child_lock = child.lock();
                let _child_lock = child_lock.lock().map_err(|_| UNAVAILABLE)?;
                let child_fence = child.acquire_fence()?;
                if append {
                    child.append_component(name, bytes, &child_fence)?;
                } else {
                    child.write_component_atomic(name, bytes, &child_fence)?;
                }
                child.validate_current_path()?;
            }
            root.validate_current_path()
        })())
    }
}

pub(crate) fn producer_preflight(
    server: &MemoryServer,
    flow_id: Option<&str>,
) -> Result<(), String> {
    let Some(flow_id) = flow_id.filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    // A not-yet-created legacy flow remains valid. A known canonical claim
    // cannot enter the legacy producer before intake has sealed its receipt.
    let protected = server.with_global_store_read(|store| store.connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM session_claims WHERE flow_id=?1 AND canonical_project IS NOT NULL)",
        [flow_id], |row| row.get::<_, bool>(0),
    ).map_err(|_| UNAVAILABLE.to_string()))?;
    if !flow_id.starts_with("flow_") {
        return if protected {
            Err(UNAVAILABLE.into())
        } else {
            Ok(())
        };
    }
    let path = run_dir_for_flow_id(flow_id)?;
    if !protected && !path.exists() {
        return Ok(());
    }
    match seal(&path) {
        Ok(None) if !protected => Ok(()),
        Ok(Some(seal)) => {
            if let Some(binding) = LocalProject::capture(server) {
                let admitted = binding
                    .with_current(server, |conn| {
                        Ok(session_claims::has_current_project_claim(
                            conn,
                            &binding.project,
                            &binding.identity,
                            &binding.connection_id,
                            &Utc::now().to_rfc3339(),
                        )? && session_claims::work_claim_project(conn, &seal.claim_id)?
                            .as_deref()
                            == Some(&binding.project)
                            && seal.canonical_project == binding.project)
                    })
                    .unwrap_or(false);
                if admitted {
                    return Err("sealed flow producer is unavailable; execution integration is not installed".into());
                }
            }
            Err(UNAVAILABLE.into())
        }
        _ => Err(UNAVAILABLE.into()),
    }
}

/// Capture one admitted request identity before any upstream issue read.
pub(crate) fn intake_preflight(
    server: &MemoryServer,
    params: &TachiTaskParams,
) -> Result<Option<LocalProject>, String> {
    let Some(binding) = LocalProject::capture(server) else {
        return Ok(None);
    };
    let flow_id = params
        .flow_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .ok_or(UNAVAILABLE)?;
    if params
        .project
        .as_deref()
        .is_some_and(|project| project.trim() != binding.project)
    {
        return Err(UNAVAILABLE.into());
    }
    binding.with_current(server, |conn| {
        let claim = session_claims::project_flow_claim(conn, &binding.project, flow_id)?;
        session_claims::with_local_project_claim_write(
            conn,
            &claim.claim_id,
            flow_id,
            &binding.project,
            &binding.identity,
            &binding.connection_id,
            |_, _| Ok(()),
        )
    })?;
    Ok(Some(binding))
}

/// Bound local intake always requires the pre-existing explicit writable claim.
/// Unbound callers retain the legacy producer only for unprotected flows.
pub(crate) fn claimed_intake(
    server: &MemoryServer,
    binding: Option<LocalProject>,
    flow_id: &str,
    objective: &str,
    issue: &IssueSnapshot,
    plan: &Value,
) -> Result<bool, String> {
    let Some(binding) = binding else {
        producer_preflight(server, Some(flow_id))?;
        return Ok(false);
    };
    #[cfg(not(unix))]
    {
        let _ = (binding, objective, issue, plan);
        Err(UNAVAILABLE.into())
    }
    #[cfg(unix)]
    {
        let path = run_dir_for_flow_id(flow_id)?;
        binding.with_current(server, |conn| {
            let claim = session_claims::project_flow_claim(conn, &binding.project, flow_id)?;
            let seal = FlowSeal {
                version: 1,
                claim_id: claim.claim_id.clone(),
                canonical_project: binding.project.clone(),
                flow_id: flow_id.into(),
            };
            session_claims::with_local_project_claim_write(
                conn,
                &claim.claim_id,
                flow_id,
                &binding.project,
                &binding.identity,
                &binding.connection_id,
                |_, _| {
                    persist_new_flow(&path, &seal, objective, issue, plan)
                        .map_err(|_| unavailable())
                },
            )
        })?;
        Ok(true)
    }
}

#[cfg(unix)]
fn persist_new_flow(
    path: &Path,
    seal: &FlowSeal,
    objective: &str,
    issue: &IssueSnapshot,
    plan: &Value,
) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt;
    let flow_id = seal.flow_id.as_str();
    std::fs::create_dir_all(path.parent().ok_or(UNAVAILABLE)?).map_err(|_| UNAVAILABLE)?;
    // Existing unsealed directories are never adopted, even empty ones.
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .map_err(|_| UNAVAILABLE)?;
    let anchor = anchor(path)?;
    let lock = anchor.lock();
    let _lock = lock.lock().map_err(|_| UNAVAILABLE)?;
    let fence = anchor.acquire_fence()?;
    anchor.require_fresh_directory(&fence)?;
    let mut status = json!({"ownership":seal,"flow_id":flow_id,"created_at":Utc::now().to_rfc3339(),"state":"incomplete","dispatch_ids":[]});
    anchor.validate_current_path()?;
    anchor.write_component_atomic(
        "status.json",
        &serde_json::to_vec(&status).map_err(|_| UNAVAILABLE)?,
        &fence,
    )?;
    anchor.validate_current_path()?;
    // Every private byte follows the durable seal. Interrupted production is
    // incomplete and cannot be reclassified as an unsealed legacy record.
    let handoff = build_pr_handoff_body(
        flow_id,
        objective,
        Some(&format!("{}#{}", issue.repo, issue.number)),
        &json!({"task":objective,"automation_plan":plan}),
        None,
        None,
        &string_array_field(plan, "leader_gate_reasons"),
    );
    for (name, bytes) in [
        ("pr_handoff.md", handoff.into_bytes()),
        (
            "instruction.md",
            format!("# {flow_id}\n\n{objective}\n").into_bytes(),
        ),
    ] {
        anchor.validate_current_path()?;
        anchor.write_component_atomic(name, &bytes, &fence)?;
    }
    status["state"] = json!("flow_bound");
    status["stage"] = json!("intake");
    status["task"] = json!(objective);
    status["issue_ref"] = json!(format!("{}#{}", issue.repo, issue.number));
    status["github"] = issue_to_json(issue);
    status["automation_plan"] = plan.clone();
    status["doc_paths"] = json!(issue.doc_paths);
    status["spec_paths"] = json!(issue.spec_paths);
    status["updated_at"] = json!(Utc::now().to_rfc3339());
    anchor.validate_current_path()?;
    anchor.write_component_atomic(
        "status.json",
        &serde_json::to_vec(&status).map_err(|_| UNAVAILABLE)?,
        &fence,
    )?;
    anchor.validate_current_path()
}

fn unavailable() -> memcore::MemoryError {
    memcore::MemoryError::InvalidArg(UNAVAILABLE.into())
}

/// The caller filtered canonical claims before invoking this exact-file read.
#[cfg(unix)]
pub(crate) fn read_owned_status(
    binding: &LocalProject,
    claim: &memcore::SessionClaim,
) -> Result<Value, memcore::MemoryError> {
    if claim.state != memcore::ClaimState::Active
        || claim.mode != Some(memcore::WorkClaimMode::Writable)
        || !claim
            .lease_expires_at
            .as_deref()
            .and_then(|lease| chrono::DateTime::parse_from_rfc3339(lease).ok())
            .is_some_and(|lease| lease > Utc::now())
    {
        return Err(unavailable());
    }
    let read = || -> Result<Value, String> {
        let flow_id = claim.flow_id.as_deref().ok_or(UNAVAILABLE)?;
        let path = run_dir_for_flow_id(flow_id)?;
        let anchor = anchor(&path)?;
        anchor.validate_current_path()?;
        let bytes = anchor.read_checked_status::<FlowSeal>(1024 * 1024, |seal| {
            seal.version == 1
                && seal.claim_id == claim.claim_id
                && seal.flow_id == flow_id
                && seal.canonical_project == binding.project
        })?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| UNAVAILABLE)?;
        if value.get("state").and_then(Value::as_str) == Some("incomplete") {
            return Err(UNAVAILABLE.into());
        }
        anchor.validate_current_path()?;
        Ok(value)
    };
    read().map_err(|_| unavailable())
}

#[cfg(not(unix))]
pub(crate) fn read_owned_status(
    _binding: &LocalProject,
    _claim: &memcore::SessionClaim,
) -> Result<Value, memcore::MemoryError> {
    Err(unavailable())
}

#[cfg(all(test, unix))]
mod tests;
