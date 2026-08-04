use super::*;

pub(crate) fn new_dispatch_id(now: chrono::DateTime<Utc>, agent: &str) -> String {
    let timestamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    let sanitized = agent.replace(|c: char| !c.is_ascii_alphanumeric(), "-");
    let suffix = uuid::Uuid::new_v4().as_simple().to_string()[..8].to_string();
    format!("{}-{}-{}", timestamp, sanitized, suffix)
}

pub(crate) fn dispatch_runs_root() -> PathBuf {
    crate::path_utils::tachi_home().join("runs")
}

/// Outcome of loading a dispatch's frozen identity receipt from its run
/// artifact. `Corrupt` is a distinct state on purpose: a receipt that exists
/// but no longer parses must surface as explicitly unattributable, never
/// silently re-enable identity reconstruction from mutable profiles.
pub(crate) enum DispatchReceiptLoad {
    Missing,
    Corrupt,
    Present(Box<tachi_dispatch::DispatchIdentityReceipt>),
}

/// The identity receipt is frozen in status.json at dispatch acceptance. Later
/// lifecycle operations read this artifact rather than resolving mutable
/// profile definitions again.
///
/// tachi#1173 k2 fix: `dispatch_id` here is caller-supplied (via
/// `TachiCompleteParams::dispatch_id` on `tachi_complete`) and was joined
/// directly onto `dispatch_runs_root()` with no validation -- the same
/// path-traversal shape tachi#1173's board autopsy review closed in
/// `board::runs::collect_run_task_by_id` (eb473fd0). Gated the same way: a
/// character allowlist plus a canonicalize-and-confine defense-in-depth
/// check; an invalid or escaping id behaves identically to "receipt not
/// found" rather than surfacing an error.
pub(crate) fn load_dispatch_identity_receipt_checked(dispatch_id: &str) -> DispatchReceiptLoad {
    if !crate::dispatch_ops::is_valid_dispatch_id(dispatch_id) {
        return DispatchReceiptLoad::Missing;
    }
    let runs_root = dispatch_runs_root();
    let run_dir = runs_root.join(dispatch_id);
    if !run_dir.is_dir() || !crate::dispatch_ops::canonical_dir_is_within(&run_dir, &runs_root) {
        return DispatchReceiptLoad::Missing;
    }
    let status_path = run_dir.join("status.json");
    let Some(value) = crate::task_lifecycle::read_json_file(&status_path)
        .ok()
        .flatten()
        .and_then(|status| status.get("identity_receipt").cloned())
    else {
        return DispatchReceiptLoad::Missing;
    };
    if value.is_null() {
        return DispatchReceiptLoad::Missing;
    }
    match serde_json::from_value(value) {
        Ok(receipt) => DispatchReceiptLoad::Present(Box::new(receipt)),
        Err(error) => {
            // Loud by design: a present-but-unparseable receipt is an
            // integrity failure, and quietly degrading here would read as
            // "this dispatch never had a receipt".
            tracing::warn!(
                dispatch_id = %dispatch_id,
                error = %error,
                "dispatch identity receipt present but unparseable; attribution is unknown"
            );
            DispatchReceiptLoad::Corrupt
        }
    }
}

pub(super) fn dispatch_status_is_terminal(dispatch_id: &str) -> bool {
    let status_path = dispatch_runs_root().join(dispatch_id).join("status.json");
    let Ok(Some(status)) = crate::task_lifecycle::read_json_file(&status_path) else {
        return false;
    };
    let state = status
        .get("state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    matches!(
        state,
        "TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED"
    ) || status.get("exit_code").is_some()
}

pub(super) fn dispatch_dedupe_lock_is_stale(
    existing: &serde_json::Value,
    dispatch_id: &str,
) -> bool {
    let status_path = dispatch_runs_root().join(dispatch_id).join("status.json");
    if status_path.exists() {
        return false;
    }
    let Some(created_at) = existing
        .get("created_at")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    let Ok(created_at) = chrono::DateTime::parse_from_rfc3339(created_at) else {
        return false;
    };
    Utc::now().signed_duration_since(created_at.with_timezone(&Utc))
        > chrono::Duration::seconds(DISPATCH_DEDUPE_STALE_LOCK_SECS)
}

pub(super) fn dispatch_dedupe_lock_file_is_stale(lock_path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(lock_path) else {
        return true;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    let Ok(age) = std::time::SystemTime::now().duration_since(modified) else {
        return false;
    };
    age > std::time::Duration::from_secs(DISPATCH_DEDUPE_STALE_LOCK_SECS as u64)
}

pub(super) fn dispatch_dedupe_root() -> PathBuf {
    dispatch_runs_root().join(".dispatch-dedupe")
}

pub(super) fn reserve_dispatch_dedupe_lock(
    lock_dir: &Path,
    scope: &str,
    task: &str,
    dispatch_id: &str,
    flow_id: Option<&str>,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(lock_dir).map_err(|e| format!("create dispatch dedupe dir: {e}"))?;
    let task_hash = crate::utils::stable_hash(task);
    let lock_path = lock_dir.join(format!("{task_hash}.json"));
    let mut payload = json!({
        "scope": scope,
        "task_hash": task_hash,
        "dispatch_id": dispatch_id,
        "task": task,
        "created_at": Utc::now().to_rfc3339(),
    });
    if let Some(flow_id) = flow_id {
        payload["flow_id"] = json!(flow_id);
    }
    let payload =
        serde_json::to_vec_pretty(&payload).map_err(|e| format!("serialize dedupe lock: {e}"))?;

    let mut lock_options = std::fs::OpenOptions::new();
    lock_options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        lock_options.mode(0o600);
    }

    match lock_options.open(&lock_path) {
        Ok(mut file) => {
            use std::io::Write;
            let write_result = (|| -> Result<(), String> {
                file.write_all(&payload)
                    .map_err(|e| format!("write dispatch dedupe lock: {e}"))?;
                file.sync_all()
                    .map_err(|e| format!("fsync dispatch dedupe lock: {e}"))?;
                crate::utils::sync_parent_dir(&lock_path)
            })();
            if let Err(err) = write_result {
                let _ = std::fs::remove_file(&lock_path);
                return Err(err);
            }
            Ok(lock_path)
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = match crate::task_lifecycle::read_json_file(&lock_path) {
                Ok(Some(existing)) => existing,
                Ok(None) => json!({}),
                Err(_) if dispatch_dedupe_lock_file_is_stale(&lock_path) => {
                    let _ = std::fs::remove_file(&lock_path);
                    return reserve_dispatch_dedupe_lock(
                        lock_dir,
                        scope,
                        task,
                        dispatch_id,
                        flow_id,
                    );
                }
                Err(err) => return Err(err),
            };
            let existing_dispatch_id = existing
                .get("dispatch_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("<unknown>");
            if dispatch_status_is_terminal(existing_dispatch_id) {
                let _ = std::fs::remove_file(&lock_path);
                return reserve_dispatch_dedupe_lock(lock_dir, scope, task, dispatch_id, flow_id);
            }
            if dispatch_dedupe_lock_is_stale(&existing, existing_dispatch_id) {
                let _ = std::fs::remove_file(&lock_path);
                return reserve_dispatch_dedupe_lock(lock_dir, scope, task, dispatch_id, flow_id);
            }
            let scope_label = flow_id.unwrap_or("global");
            Err(format!(
                "duplicate dispatch blocked for {scope_label} scope and same task; active dispatch_id: {existing_dispatch_id}"
            ))
        }
        Err(err) => Err(format!("create dispatch dedupe lock: {err}")),
    }
}

pub(super) fn reserve_flow_dispatch_slot(
    flow_id: Option<&str>,
    task: &str,
    dispatch_id: &str,
) -> Result<Option<PathBuf>, String> {
    let Some(flow_id) = flow_id.filter(|id| !id.trim().is_empty()) else {
        return Ok(None);
    };
    let Ok(run_dir) = crate::task_lifecycle::run_dir_for_flow_id(flow_id) else {
        return Ok(None);
    };
    let lock_dir = run_dir.join(".dispatch-dedupe");
    reserve_dispatch_dedupe_lock(&lock_dir, "flow", task, dispatch_id, Some(flow_id)).map(Some)
}

pub(super) fn reserve_global_dispatch_slot(
    task: &str,
    dispatch_id: &str,
) -> Result<PathBuf, String> {
    reserve_dispatch_dedupe_lock(&dispatch_dedupe_root(), "global", task, dispatch_id, None)
}

pub(super) fn reserve_dispatch_slot(
    flow_id: Option<&str>,
    task: &str,
    dispatch_id: &str,
) -> Result<Option<PathBuf>, String> {
    if flow_id.filter(|id| !id.trim().is_empty()).is_some() {
        reserve_flow_dispatch_slot(flow_id, task, dispatch_id)
    } else {
        reserve_global_dispatch_slot(task, dispatch_id).map(Some)
    }
}

pub(super) fn release_flow_dispatch_slot(lock_path: Option<PathBuf>) {
    if let Some(path) = lock_path {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tachi#1173 k2 fix discriminator: a caller-supplied `dispatch_id`
    /// containing a path-traversal or absolute-path payload must be rejected
    /// fail-closed by `load_dispatch_identity_receipt_checked` -- and must
    /// never surface the `identity_receipt` from a decoy `status.json`
    /// planted outside `dispatch_runs_root()` that a successful escape would
    /// have read. Reproduces RED against eb473fd0 (the SHA this fix is
    /// layered onto): that commit gates `board::runs::collect_run_task_by_id`
    /// but leaves this call site un-gated.
    #[test]
    fn load_dispatch_identity_receipt_checked_rejects_path_traversal_dispatch_id() {
        crate::test_support::with_tachi_home(|home| {
            let runs_dir = home.join("runs");
            std::fs::create_dir_all(&runs_dir).expect("create runs dir");

            // Decoy directory *outside* runs_dir. A successful traversal
            // escape would resolve into here and read this status.json's
            // identity_receipt.
            let decoy_dir = home.join("decoy");
            std::fs::create_dir_all(&decoy_dir).expect("create decoy dir");
            std::fs::write(
                decoy_dir.join("status.json"),
                serde_json::json!({
                    "dispatch_id": "decoy-should-never-be-read",
                    "identity_receipt": {
                        "role": "leaked-via-traversal",
                    },
                })
                .to_string(),
            )
            .expect("write decoy status.json");

            for malicious in [
                "../decoy",
                "../../decoy",
                "..",
                "",
                "/etc/passwd",
                "a/../../decoy",
            ] {
                let result = load_dispatch_identity_receipt_checked(malicious);
                assert!(
                    matches!(result, DispatchReceiptLoad::Missing),
                    "dispatch_id {malicious:?} must be rejected fail-closed (treated as \
                     missing), not resolved outside runs_dir"
                );
            }
        });
    }

    /// A dispatch_id shaped like a real one (matches the character
    /// allowlist, has a run directory, but no status.json/receipt yet) must
    /// still resolve to `Missing`, not get rejected by the gate itself --
    /// the gate must not break the ordinary "no receipt written yet" path.
    #[test]
    fn load_dispatch_identity_receipt_checked_still_missing_for_legit_id_without_receipt() {
        crate::test_support::with_tachi_home(|home| {
            let runs_dir = home.join("runs");
            let dispatch_id = "20260718T101010Z-claude-abc12345";
            std::fs::create_dir_all(runs_dir.join(dispatch_id)).expect("create run dir");

            let result = load_dispatch_identity_receipt_checked(dispatch_id);
            assert!(matches!(result, DispatchReceiptLoad::Missing));
        });
    }
}
