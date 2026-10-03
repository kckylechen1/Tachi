use serde_json::Value;

pub(super) const MAX_APPEND_ONLY_JSONL_LINE_BYTES: usize = 256 * 1024;

#[cfg(test)]
type PostRenameFailureKey = (
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
    std::ffi::OsString,
);

#[cfg(test)]
static POST_RENAME_FAILURES: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashSet<PostRenameFailureKey>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
pub(crate) struct ArtifactPostRenameFailureGuard {
    key: PostRenameFailureKey,
}

#[cfg(test)]
impl Drop for ArtifactPostRenameFailureGuard {
    fn drop(&mut self) {
        if let Some(failures) = POST_RENAME_FAILURES.get() {
            failures
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&self.key);
        }
    }
}

#[cfg(test)]
pub(crate) fn install_artifact_post_rename_failure(
    home: &std::path::Path,
    run_root: &std::path::Path,
    run_dir: &std::path::Path,
    basename: &str,
) -> ArtifactPostRenameFailureGuard {
    let key = (
        home.to_path_buf(),
        run_root.to_path_buf(),
        run_dir.canonicalize().expect("owned run directory exists"),
        std::ffi::OsString::from(basename),
    );
    assert!(POST_RENAME_FAILURES
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key.clone()));
    ArtifactPostRenameFailureGuard { key }
}

#[cfg(test)]
fn artifact_post_rename_failure_injected(path: &std::path::Path) -> bool {
    let (Some(home), Some(run_root), Some(failures)) = (
        std::env::var_os("TACHI_HOME"),
        std::env::var_os("TACHI_RUN_ROOT"),
        POST_RENAME_FAILURES.get(),
    ) else {
        return false;
    };
    let Ok(path) = path.canonicalize() else {
        return false;
    };
    let mut failures = failures
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let key = failures
        .iter()
        .find(|(owned_home, owned_runs, owned_run, basename)| {
            owned_home.as_os_str() == home.as_os_str()
                && owned_runs.as_os_str() == run_root.as_os_str()
                && path.starts_with(owned_run)
                && path.file_name() == Some(basename.as_os_str())
        })
        .cloned();
    key.is_some_and(|key| failures.remove(&key))
}

/// Atomically replace a file by writing a synced same-directory temp file first.
pub(crate) fn write_owner_only_file_atomic(
    path: &std::path::Path,
    bytes: &[u8],
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create parent dir: {e}"))?;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("tachi-file");
    let tmp_path = path.with_file_name(format!(
        "{file_name}.tmp.{}",
        uuid::Uuid::new_v4().as_simple()
    ));

    let result = (|| {
        #[cfg(unix)]
        let mut file = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp_path)
                .map_err(|e| format!("open temp {}: {e}", tmp_path.display()))?
        };
        #[cfg(not(unix))]
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|e| format!("open temp {}: {e}", tmp_path.display()))?;

        use std::io::Write;
        file.write_all(bytes)
            .map_err(|e| format!("write temp {}: {e}", tmp_path.display()))?;
        file.sync_all()
            .map_err(|e| format!("fsync temp {}: {e}", tmp_path.display()))?;
        drop(file);

        std::fs::rename(&tmp_path, path).map_err(|e| {
            format!(
                "rename temp {} -> {}: {e}",
                tmp_path.display(),
                path.display()
            )
        })?;
        #[cfg(test)]
        if artifact_post_rename_failure_injected(path) {
            return Err("injected artifact publication failure after rename".to_string());
        }
        sync_parent_dir(path)
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

pub(crate) fn write_json_file_owner_only(
    path: &std::path::Path,
    value: &Value,
) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(value).map_err(|e| format!("serialize json: {e}"))?;
    write_owner_only_file_atomic(path, &body)
}

pub(crate) fn write_run_status_file(
    run_dir: &std::path::Path,
    status: &Value,
) -> Result<(), String> {
    write_json_file_owner_only(&run_dir.join("status.json"), status)
}

#[cfg(test)]
pub(crate) fn read_to_string_allow_missing(
    path: &std::path::Path,
    label: &str,
) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(raw) => Ok(Some(raw)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => {
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "{label} read failed"
            );
            Err(format!("read {label} {}: {err}", path.display()))
        }
    }
}

pub(crate) fn append_run_event(run_dir: &std::path::Path, event: Value) -> Result<(), String> {
    let line = serde_json::to_string(&event).map_err(|e| format!("serialize event: {e}"))?;
    append_owner_only_jsonl_line(&run_dir.join("events.jsonl"), &line)
}

pub(crate) fn sync_parent_dir(path: &std::path::Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        if let Some(parent) = path.parent() {
            let dir = std::fs::File::open(parent)
                .map_err(|e| format!("open parent dir {}: {e}", parent.display()))?;
            dir.sync_all()
                .map_err(|e| format!("fsync parent dir {}: {e}", parent.display()))?;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

pub(crate) fn append_owner_only_jsonl_line(
    path: &std::path::Path,
    line: &str,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create parent dir: {e}"))?;
    }
    let mut bytes = Vec::with_capacity(line.len() + 1);
    bytes.extend_from_slice(line.as_bytes());
    if !line.ends_with('\n') {
        bytes.push(b'\n');
    }
    if bytes.len() > MAX_APPEND_ONLY_JSONL_LINE_BYTES {
        return Err(format!(
            "append-only JSONL line {} exceeds {} byte cap",
            path.display(),
            MAX_APPEND_ONLY_JSONL_LINE_BYTES
        ));
    }

    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("open {}: {e}", path.display()))?
    };
    #[cfg(not(unix))]
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;

    use std::io::Write;
    file.write_all(&bytes)
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    file.sync_all()
        .map_err(|e| format!("fsync {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[cfg(test)]
mod post_rename_failure_tests {
    use super::*;
    use crate::test_support::EnvRestore;

    #[test]
    fn post_rename_fault_is_scoped_and_consumed_once() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("owned home");
        let runs = tempfile::tempdir().expect("owned run root");
        let foreign = tempfile::tempdir().expect("unrelated environment");
        let run_dir = runs.path().join("owned-run");
        std::fs::create_dir(&run_dir).expect("owned run");
        let _home = EnvRestore::set_path("TACHI_HOME", home.path());
        let _runs = EnvRestore::set_path("TACHI_RUN_ROOT", runs.path());
        let _fault = install_artifact_post_rename_failure(
            home.path(),
            runs.path(),
            &run_dir,
            "dsh-events.jsonl",
        );
        write_owner_only_file_atomic(
            &runs.path().join("other-run/dsh-events.jsonl"),
            b"other run",
        )
        .expect("different run cannot consume fault");
        write_owner_only_file_atomic(&run_dir.join("dsh-stderr.log"), b"other channel")
            .expect("different basename cannot consume fault");
        let target = run_dir.join("dsh-events.jsonl");
        {
            let _other_home = EnvRestore::set_path("TACHI_HOME", foreign.path());
            write_owner_only_file_atomic(&target, b"other home")
                .expect("different home cannot consume fault");
        }
        {
            let _other_runs = EnvRestore::set_path("TACHI_RUN_ROOT", foreign.path());
            write_owner_only_file_atomic(&target, b"other run root")
                .expect("different run root cannot consume fault");
        }
        let error = write_owner_only_file_atomic(&target, b"renamed bytes")
            .expect_err("exact owned target consumes post-rename fault");
        assert!(error.contains("after rename"), "{error}");
        assert_eq!(
            std::fs::read(&target).expect("rename already committed"),
            b"renamed bytes"
        );
        write_owner_only_file_atomic(&target, b"ordinary overwrite")
            .expect("one-shot consumed; shared writer retains normal overwrite semantics");
        assert_eq!(
            std::fs::read(&target).expect("ordinary helper result"),
            b"ordinary overwrite"
        );
    }
}
