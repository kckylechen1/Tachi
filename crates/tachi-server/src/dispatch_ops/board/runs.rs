use super::paths::runs_dir_for_server;
use super::status::{
    is_abandoned_working_run, parse_status_updated_at, stale_after_secs,
    state_matches_filter_with_closure_kind, status_state,
};
use crate::dispatch_ops::probe_harness_server_status;
use crate::MemoryServer;
use chrono::Utc;
use serde_json::{json, Value};
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

// One additional directory entry is allowed to detect a truncated scan.
const BOARD_RUN_DIRECTORY_ENTRY_INSPECTION_HARD_MAX: usize = 256;
pub(super) const BOARD_RUN_DIRECTORY_CANDIDATE_HARD_MAX: usize =
    BOARD_RUN_DIRECTORY_ENTRY_INSPECTION_HARD_MAX - 1;
pub(super) const BOARD_RUN_STATUS_JSON_MAX_BYTES: u64 = 128 * 1024;

#[derive(Debug, Default)]
pub(super) struct RunTaskScan {
    pub(super) tasks: Vec<Value>,
    pub(super) inspected_entries: usize,
    pub(super) truncated: bool,
    pub(super) invalid_entries: usize,
    pub(super) error: Option<String>,
}

impl RunTaskScan {
    pub(super) fn failed(error: String) -> Self {
        Self {
            error: Some(error),
            ..Self::default()
        }
    }

    pub(super) fn incomplete(&self) -> bool {
        self.truncated || self.invalid_entries > 0 || self.error.is_some()
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
}

#[cfg(test)]
static RUN_DIRECTORY_ENTRY_VISITS: AtomicUsize = AtomicUsize::new(0);

pub(super) fn read_bounded_json_file(path: &Path) -> Result<Value, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("inspect {}: {error}", path.display()))?;
    if !metadata.file_type().is_file() {
        return Err(format!("refuse non-regular JSON file {}", path.display()));
    }
    if metadata.len() > BOARD_RUN_STATUS_JSON_MAX_BYTES {
        return Err(format!(
            "refuse oversized JSON file {} ({} bytes exceeds {} byte limit)",
            path.display(),
            metadata.len(),
            BOARD_RUN_STATUS_JSON_MAX_BYTES,
        ));
    }

    let mut raw = String::new();
    std::fs::File::open(path)
        .map_err(|error| format!("open {}: {error}", path.display()))?
        .take(BOARD_RUN_STATUS_JSON_MAX_BYTES.saturating_add(1))
        .read_to_string(&mut raw)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    if raw.len() as u64 > BOARD_RUN_STATUS_JSON_MAX_BYTES {
        return Err(format!(
            "refuse oversized JSON file {} (read exceeds {} byte limit)",
            path.display(),
            BOARD_RUN_STATUS_JSON_MAX_BYTES,
        ));
    }
    serde_json::from_str(&raw).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn result_written_for_run(run_dir: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(run_dir.join("result.md")) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err(format!(
            "refuse non-regular result marker {}",
            run_dir.join("result.md").display()
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "inspect result marker {}: {error}",
            run_dir.join("result.md").display()
        )),
    }
}

fn status_updated_at(status: &Value, status_path: &Path) -> Result<Option<String>, String> {
    if let Some(updated_at) = status.get("updated_at").and_then(Value::as_str) {
        return Ok(Some(updated_at.to_string()));
    }
    let metadata = std::fs::metadata(status_path).map_err(|error| {
        format!(
            "inspect status timestamp {}: {error}",
            status_path.display()
        )
    })?;
    let modified = metadata
        .modified()
        .map_err(|error| format!("read status timestamp {}: {error}", status_path.display()))?;
    Ok(Some(chrono::DateTime::<Utc>::from(modified).to_rfc3339()))
}

struct RunStateFields {
    result_written: bool,
    abandoned: bool,
    state: &'static str,
    updated_at: Option<String>,
    stale_reason: Option<String>,
}

fn run_state_fields(
    run_dir: &Path,
    status_path: &Path,
    status: &Value,
    now: chrono::DateTime<Utc>,
) -> Result<RunStateFields, String> {
    let result_written = result_written_for_run(run_dir)?;
    let updated_at_dt = parse_status_updated_at(status, status_path);
    let abandoned = is_abandoned_working_run(status, result_written, updated_at_dt, now);
    let state = if abandoned {
        "TASK_STATE_FAILED"
    } else {
        status_state(status, result_written)
    };
    let updated_at = status_updated_at(status, status_path)?;
    let stale_reason = if abandoned {
        Some(format!(
            "run ledger stayed WORKING for more than {}s without terminal status or exit_code",
            stale_after_secs(status)
        ))
    } else {
        None
    };
    Ok(RunStateFields {
        result_written,
        abandoned,
        state,
        updated_at,
        stale_reason,
    })
}

pub(super) fn dispatch_timestamp_key(name: &std::ffi::OsStr) -> Option<String> {
    let name = name.to_str()?;
    let bytes = name.as_bytes();
    if bytes.len() < 16 {
        return None;
    }
    for idx in 0..=bytes.len().saturating_sub(16) {
        let candidate = &bytes[idx..idx + 16];
        let valid = candidate[0..8].iter().all(u8::is_ascii_digit)
            && candidate[8] == b'T'
            && candidate[9..15].iter().all(u8::is_ascii_digit)
            && candidate[15] == b'Z';
        if valid {
            return Some(name[idx..idx + 16].to_string());
        }
    }
    None
}

pub(super) fn collect_run_tasks_from_dir(
    runs_dir: PathBuf,
    state_filter: &str,
    limit: usize,
) -> RunTaskScan {
    let candidate_limit = limit.min(BOARD_RUN_DIRECTORY_CANDIDATE_HARD_MAX);
    if candidate_limit == 0 {
        return RunTaskScan::default();
    }
    let read_dir = match std::fs::read_dir(&runs_dir) {
        Ok(read_dir) => read_dir,
        Err(error) if error.kind() == ErrorKind::NotFound => return RunTaskScan::default(),
        Err(error) => {
            let message = format!("inspect run ledger {}: {error}", runs_dir.display());
            tracing::warn!(
                runs_dir = %runs_dir.display(),
                error = %error,
                "board could not inspect run ledger"
            );
            return RunTaskScan::failed(message);
        }
    };

    let mut entries = Vec::with_capacity(candidate_limit);
    let mut inspected_entries = 0usize;
    let mut truncated = false;
    let mut invalid_entries = 0usize;
    for entry in read_dir.take(candidate_limit.saturating_add(1)) {
        inspected_entries += 1;

        #[cfg(test)]
        RUN_DIRECTORY_ENTRY_VISITS.fetch_add(1, Ordering::Relaxed);

        if inspected_entries > candidate_limit {
            truncated = true;
            break;
        }

        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                invalid_entries += 1;
                tracing::warn!(
                    runs_dir = %runs_dir.display(),
                    error = %error,
                    "board skipped unreadable run ledger entry"
                );
                continue;
            }
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                invalid_entries += 1;
                tracing::warn!(
                    path = %entry.path().display(),
                    error = %error,
                    "board skipped run entry with unreadable file type"
                );
                continue;
            }
        };
        if file_type.is_symlink() {
            invalid_entries += 1;
            tracing::warn!(
                path = %entry.path().display(),
                "board refused symlinked run entry"
            );
            continue;
        }
        if !file_type.is_dir() {
            continue;
        }
        if entry.file_name() == ".dispatch-dedupe" {
            continue;
        }
        entries.push(entry);
    }
    if truncated {
        tracing::warn!(
            runs_dir = %runs_dir.display(),
            inspected_entries,
            candidate_limit,
            "board run-ledger scan reached its inspection limit; sampled filesystem rows are incomplete and directory order is not a recency index"
        );
    }

    entries.sort_by(|a, b| {
        dispatch_timestamp_key(&b.file_name())
            .cmp(&dispatch_timestamp_key(&a.file_name()))
            .then_with(|| b.file_name().cmp(&a.file_name()))
    });

    let mut runs = Vec::new();
    let now = Utc::now();

    for entry in entries {
        if runs.len() >= limit {
            break;
        }
        let run_dir = entry.path();
        let status_path = run_dir.join("status.json");
        let status = match read_bounded_json_file(&status_path) {
            Ok(status) => status,
            Err(error) => {
                invalid_entries += 1;
                tracing::warn!(
                    run_dir = %run_dir.display(),
                    error = %error,
                    "board skipped invalid run status"
                );
                continue;
            }
        };
        let Some(dispatch_id) = status
            .get("dispatch_id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| {
                run_dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string)
            })
        else {
            invalid_entries += 1;
            tracing::warn!(
                run_dir = %run_dir.display(),
                "board skipped run status without a dispatch id"
            );
            continue;
        };
        let fields = match run_state_fields(&run_dir, &status_path, &status, now) {
            Ok(fields) => fields,
            Err(error) => {
                invalid_entries += 1;
                tracing::warn!(
                    run_dir = %run_dir.display(),
                    error = %error,
                    "board skipped run with invalid filesystem state"
                );
                continue;
            }
        };
        let closure_kind = status.get("closure_kind").and_then(Value::as_str);
        if !state_matches_filter_with_closure_kind(state_filter, fields.state, closure_kind) {
            continue;
        }
        runs.push(json!({
            "dispatch_id": dispatch_id,
            "agent": status.get("agent").cloned().unwrap_or(serde_json::Value::Null),
            "state": fields.state,
            "closure_kind": status.get("closure_kind").cloned().unwrap_or(serde_json::Value::Null),
            "exit_code": status.get("exit_code").cloned().unwrap_or(serde_json::Value::Null),
            "summary": status.get("task").cloned().unwrap_or(serde_json::Value::Null),
            "updated_at": fields.updated_at,
            "run_dir": run_dir.to_string_lossy(),
            "result_written": fields.result_written,
            "source": "run",
            "stale": fields.abandoned,
            "stale_reason": fields.stale_reason,
            "state_source": if fields.abandoned { "run_stale_timeout" } else { "run" },
            "harness_transport": status.get("harness_transport").cloned().unwrap_or(serde_json::Value::Null),
            "harness_server_url": status.get("harness_server_url").cloned().unwrap_or(serde_json::Value::Null),
            "harness_server_status": probe_harness_server_status(status.get("harness_server_url").and_then(Value::as_str)),
            "execution_backend": status.get("execution_backend").cloned().unwrap_or(serde_json::Value::Null),
            "identity_receipt": status.get("identity_receipt").cloned().unwrap_or(serde_json::Value::Null),
            "acpx": status.get("acpx").cloned().unwrap_or(serde_json::Value::Null),
            "acpx_events": status.get("acpx_events").cloned().unwrap_or(serde_json::Value::Null),
        }));
    }

    runs.sort_by(|a, b| {
        b.get("updated_at")
            .and_then(|v| v.as_str())
            .cmp(&a.get("updated_at").and_then(|v| v.as_str()))
    });
    RunTaskScan {
        tasks: runs,
        inspected_entries,
        truncated,
        invalid_entries,
        error: None,
    }
}

pub(crate) fn collect_run_task_for_server(
    server: &MemoryServer,
    dispatch_id: &str,
) -> Result<Option<serde_json::Value>, String> {
    collect_run_task_by_id(&runs_dir_for_server(server), dispatch_id)
}

/// tachi#1173 board autopsy review: `dispatch_id` here is caller-supplied
/// (via `tachi_task(action='wait'|'status'|'cancel')` or `tachi_board`'s
/// `flow_id` expansion) and gets joined directly onto `runs_dir` below. A
/// value containing a path separator or a `..` component would otherwise let
/// a caller read (or, worse, have `read_failure_tail` read) an arbitrary file
/// outside `~/.tachi/runs` -- e.g. `dispatch_id = "../../../../etc/passwd"`.
/// tachi#1173 k2 fix: the character allowlist and the canonicalize-and-confine
/// defense-in-depth layer are now the shared `dispatch_ops::path_gate` gate
/// (three more caller-supplied-dispatch_id call sites needed the identical
/// check) rather than a copy local to this module.
pub(super) fn collect_run_task_by_id(
    runs_dir: &Path,
    dispatch_id: &str,
) -> Result<Option<serde_json::Value>, String> {
    if !crate::dispatch_ops::is_valid_dispatch_id(dispatch_id) {
        return Err("invalid dispatch_id for run status lookup".to_string());
    }
    let run_dir = runs_dir.join(dispatch_id);
    let run_metadata = match std::fs::symlink_metadata(&run_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "inspect run directory {}: {error}",
                run_dir.display()
            ));
        }
    };
    if run_metadata.file_type().is_symlink() || !run_metadata.file_type().is_dir() {
        return Err(format!(
            "refuse non-directory run status path {}",
            run_dir.display()
        ));
    }
    if !crate::dispatch_ops::canonical_dir_is_within(&run_dir, runs_dir) {
        return Err(format!(
            "refuse run status path outside ledger {}",
            run_dir.display()
        ));
    }
    collect_run_task_from_dir(&run_dir)
}

fn collect_run_task_from_dir(run_dir: &Path) -> Result<Option<serde_json::Value>, String> {
    let status_path = run_dir.join("status.json");
    match std::fs::symlink_metadata(&status_path) {
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "inspect run status {}: {error}",
                status_path.display()
            ));
        }
    }
    let status = read_bounded_json_file(&status_path)?;
    let dispatch_id = status
        .get("dispatch_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            run_dir
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })
        .ok_or_else(|| format!("run status {} has no dispatch id", status_path.display()))?;
    let fields = run_state_fields(run_dir, &status_path, &status, Utc::now())?;
    Ok(Some(json!({
        "dispatch_id": dispatch_id,
        "agent": status.get("agent").cloned().unwrap_or(serde_json::Value::Null),
        "state": fields.state,
        "closure_kind": status.get("closure_kind").cloned().unwrap_or(serde_json::Value::Null),
        "exit_code": status.get("exit_code").cloned().unwrap_or(serde_json::Value::Null),
        "summary": status.get("task").cloned().unwrap_or(serde_json::Value::Null),
        "updated_at": fields.updated_at,
        "run_dir": run_dir.to_string_lossy(),
        "result_written": fields.result_written,
        "source": "run",
        "stale": fields.abandoned,
        "stale_reason": fields.stale_reason,
        "state_source": if fields.abandoned { "run_stale_timeout" } else { "run" },
        "harness_transport": status.get("harness_transport").cloned().unwrap_or(serde_json::Value::Null),
        "harness_server_url": status.get("harness_server_url").cloned().unwrap_or(serde_json::Value::Null),
        "harness_server_status": probe_harness_server_status(status.get("harness_server_url").and_then(Value::as_str)),
        "execution_backend": status.get("execution_backend").cloned().unwrap_or(serde_json::Value::Null),
        "identity_receipt": status.get("identity_receipt").cloned().unwrap_or(serde_json::Value::Null),
        "acpx": status.get("acpx").cloned().unwrap_or(serde_json::Value::Null),
        "acpx_events": status.get("acpx_events").cloned().unwrap_or(serde_json::Value::Null),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset_run_directory_entry_visits() {
        RUN_DIRECTORY_ENTRY_VISITS.store(0, Ordering::Relaxed);
    }

    fn run_directory_entry_visits() -> usize {
        RUN_DIRECTORY_ENTRY_VISITS.load(Ordering::Relaxed)
    }

    #[test]
    fn collect_run_tasks_does_not_enumerate_past_its_hard_scan_budget() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let runs_dir = tmp.path().join("runs");
        std::fs::create_dir_all(&runs_dir).expect("create runs_dir");
        for index in 0..BOARD_RUN_DIRECTORY_ENTRY_INSPECTION_HARD_MAX + 16 {
            std::fs::write(runs_dir.join(format!("ignored-{index:04}")), "fixture")
                .expect("write ignored entry");
        }

        reset_run_directory_entry_visits();
        let tasks = collect_run_tasks_from_dir(runs_dir, "all", usize::MAX);

        assert!(tasks.is_empty(), "non-directory fixtures are not run rows");
        assert_eq!(
            run_directory_entry_visits(),
            BOARD_RUN_DIRECTORY_ENTRY_INSPECTION_HARD_MAX,
            "the one-entry truncation probe is inspection work and must be counted",
        );
        assert!(
            run_directory_entry_visits() <= BOARD_RUN_DIRECTORY_ENTRY_INSPECTION_HARD_MAX,
            "oversized scan budget must not enumerate every run directory; visited {} entries",
            run_directory_entry_visits(),
        );
    }

    #[test]
    fn collect_run_tasks_with_zero_budget_does_not_inspect_directory_entries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let runs_dir = tmp.path().join("runs");
        std::fs::create_dir_all(&runs_dir).expect("create runs_dir");
        std::fs::write(runs_dir.join("ignored"), "fixture").expect("write ignored entry");

        reset_run_directory_entry_visits();
        let tasks = collect_run_tasks_from_dir(runs_dir, "all", 0);

        assert!(tasks.is_empty(), "zero budget must return no run rows");
        assert_eq!(
            run_directory_entry_visits(),
            0,
            "zero budget must not inspect directory entries",
        );
    }

    #[cfg(unix)]
    #[test]
    fn collect_run_tasks_rejects_symlinked_run_directories() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let runs_dir = tmp.path().join("runs");
        std::fs::create_dir_all(&runs_dir).expect("create runs_dir");

        let outside_run = tmp.path().join("outside-run");
        std::fs::create_dir_all(&outside_run).expect("create outside run");
        std::fs::write(
            outside_run.join("status.json"),
            serde_json::json!({
                "dispatch_id": "outside-run-must-not-surface",
                "state": "TASK_STATE_WORKING",
            })
            .to_string(),
        )
        .expect("write outside status");
        std::os::unix::fs::symlink(&outside_run, runs_dir.join("20260725T000000Z-link"))
            .expect("create run symlink");

        let tasks = collect_run_tasks_from_dir(runs_dir, "all", 1);

        assert!(
            tasks.is_empty(),
            "the board scan must fail closed rather than follow a run-directory symlink: {tasks:?}"
        );
    }

    /// tachi#1173 board autopsy review discriminator: a `dispatch_id`
    /// containing a path-traversal or absolute-path payload must be rejected
    /// fail-closed -- and, critically, must never surface content from a
    /// decoy file planted outside `runs_dir` that a successful escape would
    /// have read.
    #[test]
    fn collect_run_task_by_id_rejects_path_traversal_dispatch_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let runs_dir = tmp.path().join("runs");
        std::fs::create_dir_all(&runs_dir).expect("create runs_dir");

        // Decoy directory *outside* runs_dir. A successful traversal escape
        // would resolve into here and read this status.json.
        let decoy_dir = tmp.path().join("decoy");
        std::fs::create_dir_all(&decoy_dir).expect("create decoy dir");
        std::fs::write(
            decoy_dir.join("status.json"),
            serde_json::json!({
                "dispatch_id": "decoy-should-never-be-read",
                "state": "TASK_STATE_COMPLETED",
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
            let result = collect_run_task_by_id(&runs_dir, malicious);
            assert!(
                result.is_err(),
                "dispatch_id {malicious:?} must be rejected fail-closed (treated as \
                 a loud error), not resolved outside runs_dir; got: {result:?}"
            );
        }
    }

    /// Defense-in-depth layer 2: even a dispatch_id that passes the character
    /// allowlist (looks like a normal single path component) must not be
    /// able to escape runs_dir via a symlinked run directory.
    #[cfg(unix)]
    #[test]
    fn collect_run_task_by_id_rejects_symlinked_escape() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let runs_dir = tmp.path().join("runs");
        std::fs::create_dir_all(&runs_dir).expect("create runs_dir");

        let decoy_target = tmp.path().join("decoy-target");
        std::fs::create_dir_all(&decoy_target).expect("create decoy target dir");
        std::fs::write(
            decoy_target.join("status.json"),
            serde_json::json!({
                "dispatch_id": "decoy-should-never-be-read",
                "state": "TASK_STATE_COMPLETED",
            })
            .to_string(),
        )
        .expect("write decoy status.json");

        let link_name = "valid-looking-id-2454fead";
        std::os::unix::fs::symlink(&decoy_target, runs_dir.join(link_name))
            .expect("create symlink");

        let result = collect_run_task_by_id(&runs_dir, link_name);
        assert!(
            result.is_err(),
            "a symlinked run_dir resolving outside runs_dir must be rejected even though \
             its name alone passes the character allowlist; got: {result:?}"
        );
    }

    /// Regression guard: a legitimate, real-shaped dispatch id (matching
    /// `dispatch::dedupe::new_dispatch_id`'s timestamp-agent-suffix format)
    /// must still resolve normally after the validation gate above.
    #[test]
    fn collect_run_task_by_id_still_resolves_legit_dispatch_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let runs_dir = tmp.path().join("runs");
        let dispatch_id = "20260715T104905Z-claude-2454fead";
        let run_dir = runs_dir.join(dispatch_id);
        std::fs::create_dir_all(&run_dir).expect("create run dir");
        std::fs::write(
            run_dir.join("status.json"),
            serde_json::json!({
                "dispatch_id": dispatch_id,
                "state": "TASK_STATE_COMPLETED",
            })
            .to_string(),
        )
        .expect("write status.json");

        let task = collect_run_task_by_id(&runs_dir, dispatch_id).expect("read valid status");
        assert!(
            task.is_some(),
            "a legitimate, valid-charset dispatch id must still resolve: {task:?}"
        );
        assert_eq!(
            task.unwrap().get("dispatch_id").and_then(|v| v.as_str()),
            Some(dispatch_id)
        );
    }

    #[test]
    fn collect_run_task_by_id_preserves_missing_status_as_not_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let runs_dir = tmp.path().join("runs");
        let dispatch_id = "20260725T000000Z-codex-missing";
        std::fs::create_dir_all(runs_dir.join(dispatch_id)).expect("create run dir");

        let task = collect_run_task_by_id(&runs_dir, dispatch_id).expect("missing is not an error");
        assert!(task.is_none(), "a missing status remains not-found");
    }

    #[cfg(unix)]
    #[test]
    fn collect_run_task_by_id_surfaces_symlinked_status_as_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let runs_dir = tmp.path().join("runs");
        let dispatch_id = "20260725T000000Z-codex-status-link";
        let run_dir = runs_dir.join(dispatch_id);
        std::fs::create_dir_all(&run_dir).expect("create run dir");
        let outside = tmp.path().join("outside-status.json");
        std::fs::write(&outside, json!({"dispatch_id": dispatch_id}).to_string())
            .expect("write outside status");
        std::os::unix::fs::symlink(&outside, run_dir.join("status.json")).expect("symlink status");

        let error = collect_run_task_by_id(&runs_dir, dispatch_id)
            .expect_err("symlinked status must be a loud error");
        assert!(
            error.contains("refuse non-regular JSON file"),
            "unexpected symlink error: {error}"
        );
    }

    #[test]
    fn collect_run_task_by_id_surfaces_oversized_status_as_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let runs_dir = tmp.path().join("runs");
        let dispatch_id = "20260725T000000Z-codex-oversized";
        let run_dir = runs_dir.join(dispatch_id);
        std::fs::create_dir_all(&run_dir).expect("create run dir");
        std::fs::write(
            run_dir.join("status.json"),
            vec![b' '; BOARD_RUN_STATUS_JSON_MAX_BYTES as usize + 1],
        )
        .expect("write oversized status");

        let error = collect_run_task_by_id(&runs_dir, dispatch_id)
            .expect_err("oversized status must be a loud error");
        assert!(
            error.contains("refuse oversized JSON file"),
            "unexpected oversized status error: {error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn collect_run_task_by_id_surfaces_symlinked_result_marker_as_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let runs_dir = tmp.path().join("runs");
        let dispatch_id = "20260725T000000Z-codex-result-link";
        let run_dir = runs_dir.join(dispatch_id);
        std::fs::create_dir_all(&run_dir).expect("create run dir");
        std::fs::write(
            run_dir.join("status.json"),
            json!({
                "dispatch_id": dispatch_id,
                "state": "TASK_STATE_COMPLETED",
                "updated_at": Utc::now().to_rfc3339(),
            })
            .to_string(),
        )
        .expect("write status");
        let outside = tmp.path().join("outside-result.md");
        std::fs::write(&outside, "done").expect("write outside result");
        std::os::unix::fs::symlink(&outside, run_dir.join("result.md"))
            .expect("symlink result marker");

        let error = collect_run_task_by_id(&runs_dir, dispatch_id)
            .expect_err("symlinked result marker must be a loud error");
        assert!(
            error.contains("refuse non-regular result marker"),
            "unexpected result marker error: {error}"
        );
    }
}
