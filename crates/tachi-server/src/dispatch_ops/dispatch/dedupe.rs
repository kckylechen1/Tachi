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
    if let Some(state) = status.get("state") {
        // The current writer always includes exit_code, including null while
        // working. An explicit lifecycle state is authoritative; malformed or
        // unknown states must not release an active worker's reservation.
        return matches!(
            state.as_str(),
            Some("TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED")
        );
    }
    // Older receipts had no lifecycle state. Only a concrete exit status is
    // evidence of completion; the mere presence of the key is not.
    status
        .get("exit_code")
        .and_then(serde_json::Value::as_i64)
        .is_some()
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

/// A reservation may be reclaimed after terminalization before its old worker
/// finishes unwinding. The nonce prevents that old owner from deleting a successor.
#[derive(Debug)]
pub(super) struct DispatchDedupeReservation {
    path: PathBuf,
    reservation_id: String,
}

impl AsRef<Path> for DispatchDedupeReservation {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for DispatchDedupeReservation {
    fn drop(&mut self) {
        let Some(directory) = self.path.parent() else {
            return;
        };
        let result = (|| -> Result<(), String> {
            let _fence = acquire_dedupe_mutation_fence(directory)?;
            let existing = crate::task_lifecycle::read_json_file(&self.path)?;
            if existing
                .as_ref()
                .and_then(|value| value.get("reservation_id"))
                .and_then(serde_json::Value::as_str)
                == Some(self.reservation_id.as_str())
            {
                std::fs::remove_file(&self.path)
                    .map_err(|error| format!("remove dispatch reservation: {error}"))?;
                crate::utils::sync_parent_dir(&self.path)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            tracing::warn!(path = %self.path.display(), %error, "dispatch reservation cleanup failed");
        }
    }
}

/// A stable inode serializes inspect/reclaim/create and owner-checked release.
/// Keep the fence outside the receipt directory and never unlink it.
fn acquire_dedupe_mutation_fence(directory: &Path) -> Result<std::fs::File, String> {
    let mut fence_path = directory.as_os_str().to_os_string();
    fence_path.push(".lock");
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(PathBuf::from(fence_path))
        .map_err(|error| format!("open dispatch reservation fence: {error}"))?;
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("dispatch reservation fence must be a regular file".into());
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                let now = std::time::Instant::now();
                if now >= deadline {
                    return Err("dispatch reservation fence timed out".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(10).min(deadline - now));
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(format!("lock dispatch reservation fence: {error}"));
            }
        }
    }
}

pub(super) fn reserve_dispatch_dedupe_lock(
    lock_dir: &Path,
    scope: &str,
    task: &str,
    dispatch_id: &str,
    flow_id: Option<&str>,
) -> Result<DispatchDedupeReservation, String> {
    std::fs::create_dir_all(lock_dir).map_err(|e| format!("create dispatch dedupe dir: {e}"))?;
    let _fence = acquire_dedupe_mutation_fence(lock_dir)?;
    let task_hash = crate::utils::stable_hash(task);
    let lock_path = lock_dir.join(format!("{task_hash}.json"));
    if lock_path.exists() {
        let reclaim = match crate::task_lifecycle::read_json_file(&lock_path) {
            Ok(existing) => {
                let existing = existing.unwrap_or_else(|| json!({}));
                let existing_dispatch_id = existing
                    .get("dispatch_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("<unknown>");
                if dispatch_status_is_terminal(existing_dispatch_id)
                    || dispatch_dedupe_lock_is_stale(&existing, existing_dispatch_id)
                {
                    true
                } else {
                    let scope_label = flow_id.unwrap_or("global");
                    return Err(format!(
                        "duplicate dispatch blocked for {scope_label} scope and same task; active dispatch_id: {existing_dispatch_id}"
                    ));
                }
            }
            Err(_) if dispatch_dedupe_lock_file_is_stale(&lock_path) => true,
            Err(error) => return Err(error),
        };
        if reclaim {
            std::fs::remove_file(&lock_path)
                .map_err(|error| format!("reclaim dispatch reservation: {error}"))?;
        }
    }
    let reservation_id = uuid::Uuid::new_v4().to_string();
    let mut payload = json!({
        "scope": scope,
        "task_hash": task_hash,
        "dispatch_id": dispatch_id,
        "reservation_id": reservation_id,
        "task": task,
        "created_at": Utc::now().to_rfc3339(),
    });
    if let Some(flow_id) = flow_id {
        payload["flow_id"] = json!(flow_id);
    }
    let payload = serde_json::to_vec_pretty(&payload)
        .map_err(|error| format!("serialize dedupe lock: {error}"))?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&lock_path)
        .map_err(|error| format!("create dispatch dedupe lock: {error}"))?;
    let write_result = (|| -> Result<(), String> {
        use std::io::Write;
        file.write_all(&payload)
            .map_err(|error| format!("write dispatch dedupe lock: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("fsync dispatch dedupe lock: {error}"))?;
        crate::utils::sync_parent_dir(&lock_path)
    })();
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&lock_path);
        return Err(error);
    }
    Ok(DispatchDedupeReservation {
        path: lock_path,
        reservation_id,
    })
}

pub(super) fn reserve_flow_dispatch_slot(
    flow_id: Option<&str>,
    task: &str,
    dispatch_id: &str,
) -> Result<Option<DispatchDedupeReservation>, String> {
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
) -> Result<DispatchDedupeReservation, String> {
    reserve_dispatch_dedupe_lock(&dispatch_dedupe_root(), "global", task, dispatch_id, None)
}

pub(super) fn reserve_dispatch_slot(
    flow_id: Option<&str>,
    task: &str,
    dispatch_id: &str,
) -> Result<Option<DispatchDedupeReservation>, String> {
    if flow_id.filter(|id| !id.trim().is_empty()).is_some() {
        reserve_flow_dispatch_slot(flow_id, task, dispatch_id)
    } else {
        reserve_global_dispatch_slot(task, dispatch_id).map(Some)
    }
}

pub(super) fn release_flow_dispatch_slot(reservation: Option<DispatchDedupeReservation>) {
    drop(reservation);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_detection_prefers_state_and_requires_a_legacy_numeric_exit() {
        crate::test_support::with_tachi_home(|_| {
            let run = dispatch_runs_root().join("dispatch-state-contract");
            std::fs::create_dir_all(&run).unwrap();
            for (status, terminal) in [
                (
                    json!({"state": "TASK_STATE_WORKING", "exit_code": null}),
                    false,
                ),
                (
                    json!({"state": "TASK_STATE_WORKING", "exit_code": 0}),
                    false,
                ),
                (json!({"state": "future-state", "exit_code": 0}), false),
                (json!({"state": null, "exit_code": 0}), false),
                (
                    json!({"state": "TASK_STATE_COMPLETED", "exit_code": null}),
                    true,
                ),
                (json!({"state": "TASK_STATE_FAILED"}), true),
                (json!({"state": "TASK_STATE_CANCELED"}), true),
                (json!({"exit_code": 0}), true),
                (json!({"exit_code": 1}), true),
                (json!({"exit_code": null}), false),
                (json!({"exit_code": "0"}), false),
                (json!({}), false),
            ] {
                std::fs::write(run.join("status.json"), status.to_string()).unwrap();
                assert_eq!(
                    dispatch_status_is_terminal("dispatch-state-contract"),
                    terminal,
                    "status: {status}"
                );
            }
        });
    }

    #[test]
    fn concurrent_terminal_reclaim_admits_only_one_successor() {
        crate::test_support::with_tachi_home(|home| {
            let locks = home.join("dedupe");
            let first = reserve_dispatch_dedupe_lock(
                &locks,
                "global",
                "same task",
                "dispatch-terminal",
                None,
            )
            .unwrap();
            let run = dispatch_runs_root().join("dispatch-terminal");
            std::fs::create_dir_all(&run).unwrap();
            std::fs::write(
                run.join("status.json"),
                br#"{"state":"TASK_STATE_COMPLETED"}"#,
            )
            .unwrap();
            let start = std::sync::Barrier::new(8);
            let outcomes = std::thread::scope(|scope| {
                let handles = (0..8)
                    .map(|index| {
                        let locks = &locks;
                        let start = &start;
                        scope.spawn(move || {
                            start.wait();
                            reserve_dispatch_dedupe_lock(
                                locks,
                                "global",
                                "same task",
                                &format!("dispatch-next-{index}"),
                                None,
                            )
                        })
                    })
                    .collect::<Vec<_>>();
                handles
                    .into_iter()
                    .map(|thread| thread.join().unwrap())
                    .collect::<Vec<_>>()
            });
            assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
            drop(first);
            assert!(reserve_dispatch_dedupe_lock(
                &locks,
                "global",
                "same task",
                "dispatch-too-late",
                None,
            )
            .is_err());
            drop(outcomes);
            assert!(reserve_dispatch_dedupe_lock(
                &locks,
                "global",
                "same task",
                "dispatch-after-release",
                None,
            )
            .is_ok());
        });
    }

    #[test]
    fn completed_owner_cannot_release_a_replacement_dispatch_slot() {
        crate::test_support::with_tachi_home(|home| {
            let locks = home.join("dedupe");
            let first =
                reserve_dispatch_dedupe_lock(&locks, "global", "same task", "dispatch-first", None)
                    .unwrap();
            let run = dispatch_runs_root().join("dispatch-first");
            std::fs::create_dir_all(&run).unwrap();
            std::fs::write(
                run.join("status.json"),
                br#"{"state":"TASK_STATE_COMPLETED"}"#,
            )
            .unwrap();
            let replacement = reserve_dispatch_dedupe_lock(
                &locks,
                "global",
                "same task",
                "dispatch-second",
                None,
            )
            .unwrap();

            release_flow_dispatch_slot(Some(first));

            assert!(reserve_dispatch_dedupe_lock(
                &locks, "global", "same task", "dispatch-third", None,
            ).is_err(), "the old owner must not remove its successor's reservation");
            let value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&replacement).unwrap()).unwrap();
            assert_eq!(value["dispatch_id"], "dispatch-second");
            release_flow_dispatch_slot(Some(replacement));
        });
    }

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

    #[test]
    fn real_working_receipt_with_null_exit_code_keeps_its_reservation() {
        crate::test_support::with_tachi_home(|home| {
            let locks = home.join("dedupe");
            let dispatch_id = "dispatch-still-working";
            let _owner =
                reserve_dispatch_dedupe_lock(&locks, "global", "same task", dispatch_id, None)
                    .unwrap();
            let run = dispatch_runs_root().join(dispatch_id);
            std::fs::create_dir_all(&run).unwrap();
            crate::dispatch_ops::dispatch_v2::write_status_json(
                &run,
                dispatch_id,
                false,
                None,
                None,
                "n/a",
                None,
                None,
                None,
                None,
                Some(json!({"state": "TASK_STATE_WORKING"})),
            );
            let status = crate::task_lifecycle::read_json_file(&run.join("status.json"))
                .unwrap()
                .expect("the real status writer must publish a receipt");
            assert_eq!(status["state"], "TASK_STATE_WORKING");
            assert!(status
                .get("exit_code")
                .is_some_and(serde_json::Value::is_null));
            assert!(
                reserve_dispatch_dedupe_lock(
                    &locks,
                    "global",
                    "same task",
                    "dispatch-too-early",
                    None,
                )
                .is_err(),
                "an active receipt with exit_code:null must retain its reservation"
            );
        });
    }
}
