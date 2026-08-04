use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub enum RegisterOutputFormat {
    Text,
    Json,
}

#[derive(Debug)]
pub struct RegisterOptions {
    pub path: PathBuf,
    pub repo_root: PathBuf,
    pub branch: String,
    pub dispatch_id: Option<String>,
    pub pr: Option<String>,
    pub output: RegisterOutputFormat,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct WorktreeRegistry {
    version: u32,
    worktrees: Vec<WorktreeRecord>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct WorktreeRecord {
    path: String,
    repo_root: String,
    branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    dispatch_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pr: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, serde::Serialize)]
pub struct RegisterReport {
    pub action: &'static str,
    pub path: String,
    pub repo_root: String,
    pub branch: String,
    pub registry_path: String,
    pub marker_path: String,
    pub registered: bool,
}

pub fn run_wt_register(options: RegisterOptions) -> Result<(), String> {
    let output = options.output;
    let report = register_worktree(options)?;
    emit_register_report(&report, output)
}

/// Register without printing (used by `wt-open` so the open report stays the single surface).
pub fn register_worktree(options: RegisterOptions) -> Result<RegisterReport, String> {
    let path = canonicalize_existing(&options.path, "worktree path")?;
    let repo_root = canonicalize_existing(&options.repo_root, "repo root")?;
    if path == repo_root {
        return Err("refusing to register repository root/main worktree".to_string());
    }

    let registry_path = registry_path()?;
    let _lock = acquire_registry_lock(&registry_path)?;
    let marker_path = path.join(".tachi-worktree.json");
    let now = chrono::Utc::now().to_rfc3339();
    let mut registry = read_registry(&registry_path)?;
    let path_string = path.display().to_string();
    let record = WorktreeRecord {
        path: path_string.clone(),
        repo_root: repo_root.display().to_string(),
        branch: options.branch,
        dispatch_id: options.dispatch_id,
        pr: options.pr,
        created_at: now.clone(),
        updated_at: now,
    };

    upsert_record(&mut registry, record.clone());
    write_registry(&registry_path, &registry)?;
    write_marker(&marker_path, &record)?;

    Ok(RegisterReport {
        action: "wt-register",
        path: path_string,
        repo_root: repo_root.display().to_string(),
        branch: record.branch,
        registry_path: registry_path.display().to_string(),
        marker_path: marker_path.display().to_string(),
        registered: true,
    })
}

/// Snapshot of registered managed worktrees (for `tachi worktree list`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ListedWorktree {
    pub path: String,
    pub repo_root: String,
    pub branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatch_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub path_exists: bool,
}

pub fn list_registered_worktrees() -> Result<Vec<ListedWorktree>, String> {
    let registry_path = registry_path()?;
    let registry = read_registry(&registry_path)?;
    Ok(registry
        .worktrees
        .into_iter()
        .map(|record| {
            let path_exists = Path::new(&record.path).exists();
            ListedWorktree {
                path: record.path,
                repo_root: record.repo_root,
                branch: record.branch,
                dispatch_id: record.dispatch_id,
                pr: record.pr,
                created_at: record.created_at,
                updated_at: record.updated_at,
                path_exists,
            }
        })
        .collect())
}

/// Look one registered worktree up **without touching the filesystem for the
/// worktree path itself** (kckylechen1/tachi#1605).
///
/// A canonicalizing `paths_equal` comparison alone is right while the
/// directory still exists and useless once it does not: a stale row is
/// exactly the case where `canonicalize` fails. This matches the stored
/// path string literally first — the form the doctor's
/// `registered_worktree_missing` remediation prints, so the remediation it
/// tells an operator to run is actually executable — and falls back to the
/// canonicalizing comparison for a live path given in another spelling.
pub fn find_registry_entry(worktree_path: &Path) -> Result<Option<ListedWorktree>, String> {
    let needle = canonical_string(worktree_path);
    Ok(list_registered_worktrees()?.into_iter().find(|listed| {
        listed.path == needle || paths_equal(Path::new(&listed.path), worktree_path)
    }))
}

pub fn registry_contains(worktree_root: &Path) -> bool {
    let Ok(registry_path) = registry_path() else {
        return false;
    };
    let Ok(raw) = std::fs::read_to_string(registry_path) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    let needle = canonical_string(worktree_root);
    value_contains_path(&value, &needle)
}

/// Exact-row removal, used directly by the ordinary close path and as the
/// building block behind [`remove_registry_entry_exact_if_still_gone`],
/// which the registry-only stale-close execute path calls (kckylechen1/tachi#1605
/// fix round, codex review; round-2 review, FIX 2/FIX 3). This does **not**
/// canonicalize either side — canonicalizing here is exactly the TOCTOU
/// the review flagged: a stale
/// path can be recreated as a symlink to a DIFFERENT, LIVE registered
/// worktree between plan and execute, and `paths_equal`'s canonicalizing
/// comparison would then resolve the stale spelling to the live target's
/// canonical path and `retain` would drop that row too — silently erasing
/// live ownership evidence, all of it if multiple rows canonicalize equal.
///
/// Byte equality against the row's own stored `path` string (as returned by
/// [`find_registry_entry`]/[`list_registered_worktrees`]) removes at most
/// the one row that was actually planned, and never touches any other row
/// no matter what now lives on disk at that path.
///
/// If more than one row shares the identical stored `path` string, this
/// refuses to delete anything rather than guess (round-2 cross-vendor
/// review, FIX 1): a duplicate-path registry is itself corrupt state, and
/// picking the "first" match by iteration order is a guess dressed up as a
/// decision — nothing about iteration order says which duplicate row was
/// actually the one planned for removal.
pub fn remove_registry_entry_exact(stored_path: &str) -> Result<bool, String> {
    let registry_path = registry_path()?;
    if !registry_path.exists() {
        return Ok(false);
    }
    let _lock = acquire_registry_lock(&registry_path)?;
    let mut registry = read_registry(&registry_path)?;
    if !remove_matching_row_exact(&mut registry, stored_path)? {
        return Ok(false);
    }
    write_registry(&registry_path, &registry)?;
    Ok(true)
}

/// Shared count-then-remove logic behind [`remove_registry_entry_exact`].
/// Mutates `registry` in place; callers persist it. `Ok(true)` = one row
/// removed, `Ok(false)` = zero rows matched, `Err` (naming the path and the
/// count) = more than one row matched — refusing to guess which to drop.
fn remove_matching_row_exact(
    registry: &mut WorktreeRegistry,
    stored_path: &str,
) -> Result<bool, String> {
    let count = registry
        .worktrees
        .iter()
        .filter(|record| record.path == stored_path)
        .count();
    if count == 0 {
        return Ok(false);
    }
    if count > 1 {
        return Err(format!(
            "registry holds {count} rows with identical path {stored_path}; refusing to guess \
             which to drop; repair the registry first"
        ));
    }
    registry
        .worktrees
        .retain(|record| record.path != stored_path);
    Ok(true)
}

/// Locked precondition-then-remove for the registry-only stale-close
/// execute path (round-2 cross-vendor review, FIX 3). The plan-time and
/// pre-execute "does this still fail to resolve?" checks in `wt_clean.rs`
/// run OUTSIDE the registry file lock, so a concurrent registration
/// landing at the identical path between that pre-execute re-check and the
/// eventual exact-delete could still be deleted without refusal — a
/// re-check/use race. This folds the precondition into the SAME critical
/// section as the mutation: nothing can register at `stored_path` between
/// the check and the removal, because both run while this function alone
/// holds the registry lock (`acquire_registry_lock`, the same lock
/// `register_worktree` takes).
///
/// Uses `symlink_metadata` rather than `exists()`/`canonicalize()`: a
/// dangling symlink left at the path is still "something present" for the
/// purposes of this refusal — it is not the verifiably, truly-gone state
/// the stale-row close was planned against — so it must refuse too; only
/// an outright missing directory entry counts as still-gone.
pub fn remove_registry_entry_exact_if_still_gone(stored_path: &str) -> Result<bool, String> {
    let registry_path = registry_path()?;
    if !registry_path.exists() {
        return Ok(false);
    }
    let _lock = acquire_registry_lock(&registry_path)?;
    if Path::new(stored_path).symlink_metadata().is_ok() {
        return Err(format!(
            "refusing registry-only close: {stored_path} now resolves to something on disk \
             (it did not at plan time) — this row may no longer be stale; re-run `wt-remove` \
             to re-plan against current state before dropping it"
        ));
    }
    let mut registry = read_registry(&registry_path)?;
    if !remove_matching_row_exact(&mut registry, stored_path)? {
        return Ok(false);
    }
    write_registry(&registry_path, &registry)?;
    Ok(true)
}

fn upsert_record(registry: &mut WorktreeRegistry, record: WorktreeRecord) {
    if let Some(existing) = registry
        .worktrees
        .iter_mut()
        .find(|existing| paths_equal(Path::new(&existing.path), Path::new(&record.path)))
    {
        let created_at = existing.created_at.clone();
        *existing = record;
        existing.created_at = created_at;
    } else {
        registry.worktrees.push(record);
    }
}

fn read_registry(path: &Path) -> Result<WorktreeRegistry, String> {
    if !path.exists() {
        return Ok(WorktreeRegistry {
            version: 1,
            worktrees: Vec::new(),
        });
    }
    let raw = std::fs::read_to_string(path).map_err(|err| format!("read registry: {err}"))?;
    serde_json::from_str(&raw).map_err(|err| format!("parse registry: {err}"))
}

fn write_registry(path: &Path, registry: &WorktreeRegistry) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("create registry dir: {err}"))?;
    }
    let tmp_path = path.with_extension("json.tmp");
    let raw = serde_json::to_string_pretty(registry)
        .map_err(|err| format!("serialize registry: {err}"))?;
    std::fs::write(&tmp_path, format!("{raw}\n"))
        .map_err(|err| format!("write registry tmp: {err}"))?;
    std::fs::rename(&tmp_path, path).map_err(|err| format!("rename registry tmp: {err}"))
}

struct RegistryLock {
    path: PathBuf,
    file: Option<std::fs::File>,
}

impl Drop for RegistryLock {
    fn drop(&mut self) {
        let _ = self.file.take();
        let _ = std::fs::remove_file(&self.path);
    }
}

fn acquire_registry_lock(registry_path: &Path) -> Result<RegistryLock, String> {
    if let Some(parent) = registry_path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("create registry dir: {err}"))?;
    }
    let lock_path = registry_path.with_extension("json.lock");
    for _ in 0..200 {
        match std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&lock_path)
        {
            Ok(file) => {
                return Ok(RegistryLock {
                    path: lock_path,
                    file: Some(file),
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(err) => return Err(format!("create registry lock: {err}")),
        }
    }
    Err(format!(
        "timed out waiting for registry lock {}",
        lock_path.display()
    ))
}

fn write_marker(path: &Path, record: &WorktreeRecord) -> Result<(), String> {
    let raw =
        serde_json::to_string_pretty(record).map_err(|err| format!("serialize marker: {err}"))?;
    std::fs::write(path, format!("{raw}\n")).map_err(|err| format!("write marker: {err}"))
}

fn emit_register_report(
    report: &RegisterReport,
    output: RegisterOutputFormat,
) -> Result<(), String> {
    match output {
        RegisterOutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(report)
                .map_err(|err| format!("serialize report: {err}"))?
        ),
        RegisterOutputFormat::Text => {
            println!("tachi-clean wt-register");
            println!("  path: {}", report.path);
            println!("  repo_root: {}", report.repo_root);
            println!("  branch: {}", report.branch);
            println!("  registered: {}", report.registered);
            println!("  registry: {}", report.registry_path);
            println!("  marker: {}", report.marker_path);
        }
    }
    Ok(())
}

fn registry_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| "neither HOME nor USERPROFILE is set".to_string())?;
    Ok(PathBuf::from(home).join(".tachi").join("worktrees.json"))
}

fn canonicalize_existing(path: &Path, label: &str) -> Result<PathBuf, String> {
    std::fs::canonicalize(path).map_err(|err| format!("cannot canonicalize {label}: {err}"))
}

fn value_contains_path(value: &serde_json::Value, needle: &str) -> bool {
    match value {
        serde_json::Value::String(s) => paths_equal(Path::new(s), Path::new(needle)),
        serde_json::Value::Array(items) => {
            items.iter().any(|item| value_contains_path(item, needle))
        }
        serde_json::Value::Object(map) => map.iter().any(|(key, value)| {
            (key == "path"
                && value
                    .as_str()
                    .is_some_and(|s| paths_equal(Path::new(s), Path::new(needle))))
                || value_contains_path(value, needle)
        }),
        _ => false,
    }
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    let a = std::fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
    let b = std::fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    a == b
}

fn canonical_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::home_env_lock as env_lock;

    #[test]
    fn nested_registry_path_is_detected() {
        let value = serde_json::json!({
            "worktrees": [
                {"path": "/tmp/a"},
                {"metadata": {"path": "/tmp/b"}}
            ]
        });

        assert!(value_contains_path(&value, "/tmp/a"));
        assert!(value_contains_path(&value, "/tmp/b"));
        assert!(!value_contains_path(&value, "/tmp/c"));
    }

    #[test]
    fn register_then_remove_updates_registry() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let old_home = std::env::var_os("HOME");
        let root =
            std::env::temp_dir().join(format!("tachi-clean-registry-test-{}", std::process::id()));
        let home = root.join("home");
        let repo = root.join("repo");
        let worktree = root.join("wt");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::env::set_var("HOME", &home);

        run_wt_register(RegisterOptions {
            path: worktree.clone(),
            repo_root: repo,
            branch: "feature/test".to_string(),
            dispatch_id: Some("dispatch-1".to_string()),
            pr: None,
            output: RegisterOutputFormat::Json,
        })
        .unwrap();

        assert!(registry_contains(&worktree));
        assert!(worktree.join(".tachi-worktree.json").exists());
        let stored_path = find_registry_entry(&worktree)
            .unwrap()
            .expect("worktree is registered")
            .path;
        assert!(remove_registry_entry_exact(&stored_path).unwrap());
        assert!(!registry_contains(&worktree));

        match old_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// kckylechen1/tachi#1605 fix round (codex review): `remove_registry_entry`'s
    /// canonicalizing `paths_equal` collapses two rows whose stored path
    /// strings differ but now canonicalize to the same real directory (the
    /// exact shape a TOCTOU symlink swap produces) — `retain` drops BOTH.
    /// `remove_registry_entry_exact` must match only the literal stored
    /// string of the planned row and leave the other row untouched.
    #[test]
    fn remove_registry_entry_exact_matches_only_the_planned_row_when_rows_canonicalize_equal() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let old_home = std::env::var_os("HOME");
        let root = std::env::temp_dir().join(format!(
            "tachi-clean-exact-match-test-{}",
            std::process::id()
        ));
        let home = root.join("home");
        let real_dir = root.join("real");
        let link_path = root.join("link");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&real_dir).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real_dir, &link_path).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&real_dir, &link_path).unwrap();
        std::env::set_var("HOME", &home);

        let registry_path = registry_path().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let registry = WorktreeRegistry {
            version: 1,
            worktrees: vec![
                WorktreeRecord {
                    path: real_dir.display().to_string(),
                    repo_root: root.display().to_string(),
                    branch: "feature/real".to_string(),
                    dispatch_id: None,
                    pr: None,
                    created_at: now.clone(),
                    updated_at: now.clone(),
                },
                WorktreeRecord {
                    path: link_path.display().to_string(),
                    repo_root: root.display().to_string(),
                    branch: "feature/link".to_string(),
                    dispatch_id: None,
                    pr: None,
                    created_at: now.clone(),
                    updated_at: now,
                },
            ],
        };
        write_registry(&registry_path, &registry).unwrap();

        // Sanity: this is precisely the case the OLD `remove_registry_entry`
        // (canonicalizing `paths_equal`) would collapse into one match —
        // both stored strings resolve to the same real directory.
        assert_eq!(
            std::fs::canonicalize(&real_dir).unwrap(),
            std::fs::canonicalize(&link_path).unwrap(),
            "test setup must produce two distinct stored paths that canonicalize equal"
        );

        assert!(remove_registry_entry_exact(&real_dir.display().to_string()).unwrap());

        let remaining = read_registry(&registry_path).unwrap().worktrees;
        assert_eq!(
            remaining.len(),
            1,
            "exact-string removal must drop exactly the matched row, not every row \
             that canonicalizes equal: {remaining:?}"
        );
        assert_eq!(
            remaining[0].branch, "feature/link",
            "the OTHER row (different stored string) must survive untouched"
        );

        match old_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Round-2 cross-vendor review, FIX 1: `remove_registry_entry_exact` must
    /// refuse — not silently pick the first match — when two rows share the
    /// IDENTICAL stored `path` string. Before this fix the old code walked
    /// `retain` and dropped only the first hit it encountered, which is a
    /// guess dressed up as a decision: nothing about iteration order tells
    /// you which of two identically-spelled rows was actually the one
    /// planned for removal. This pins that a duplicate-path registry is
    /// refused outright, typed, naming the path and the count, and that
    /// BOTH rows survive the refusal untouched.
    #[test]
    fn remove_registry_entry_exact_refuses_on_duplicate_stored_path_rows() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let old_home = std::env::var_os("HOME");
        let root = std::env::temp_dir().join(format!(
            "tachi-clean-duplicate-rows-test-{}",
            std::process::id()
        ));
        let home = root.join("home");
        let dup_path = root.join("dup-wt").display().to_string();
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("HOME", &home);

        let registry_path = registry_path().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let registry = WorktreeRegistry {
            version: 1,
            worktrees: vec![
                WorktreeRecord {
                    path: dup_path.clone(),
                    repo_root: root.display().to_string(),
                    branch: "feature/dup-a".to_string(),
                    dispatch_id: None,
                    pr: None,
                    created_at: now.clone(),
                    updated_at: now.clone(),
                },
                WorktreeRecord {
                    path: dup_path.clone(),
                    repo_root: root.display().to_string(),
                    branch: "feature/dup-b".to_string(),
                    dispatch_id: None,
                    pr: None,
                    created_at: now.clone(),
                    updated_at: now,
                },
            ],
        };
        write_registry(&registry_path, &registry).unwrap();

        let err = remove_registry_entry_exact(&dup_path)
            .expect_err("must refuse rather than guess which duplicate row to drop");
        assert!(err.contains(&dup_path), "refusal must name the path: {err}");
        assert!(
            err.contains('2'),
            "refusal must name the count of matching rows: {err}"
        );
        assert!(
            err.contains("refusing"),
            "refusal must be explicit about refusing, not just describing state: {err}"
        );

        let remaining = read_registry(&registry_path).unwrap().worktrees;
        assert_eq!(
            remaining.len(),
            2,
            "BOTH duplicate rows must survive a refused exact-remove: {remaining:?}"
        );

        match old_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Round-2 cross-vendor review, FIX 3: pins that the "path must not
    /// resolve" precondition is authoritative when checked directly through
    /// `remove_registry_entry_exact_if_still_gone`, independent of any
    /// unlocked pre-check a caller might have already run. This does not
    /// simulate real thread interleaving — it calls the locked variant
    /// directly with the path already present, which is exactly the state
    /// a race would produce between an earlier unlocked "still gone?"
    /// check and this call. If this function's own internal check did not
    /// hold, nothing about calling it later under a lock would save it.
    #[test]
    fn remove_registry_entry_exact_if_still_gone_refuses_when_path_resolves() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let old_home = std::env::var_os("HOME");
        let root = std::env::temp_dir().join(format!(
            "tachi-clean-locked-precondition-test-{}",
            std::process::id()
        ));
        let home = root.join("home");
        let present_path = root.join("recreated-wt");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("HOME", &home);

        let registry_path = registry_path().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let stored_path = present_path.display().to_string();
        let registry = WorktreeRegistry {
            version: 1,
            worktrees: vec![WorktreeRecord {
                path: stored_path.clone(),
                repo_root: root.display().to_string(),
                branch: "feature/locked-precondition".to_string(),
                dispatch_id: None,
                pr: None,
                created_at: now.clone(),
                updated_at: now,
            }],
        };
        write_registry(&registry_path, &registry).unwrap();

        // The precondition: this path exists on disk (recreated between an
        // earlier unlocked staleness check and this call), so the row it
        // maps to is no longer verifiably stale.
        std::fs::create_dir_all(&present_path).unwrap();

        let err = remove_registry_entry_exact_if_still_gone(&stored_path)
            .expect_err("must refuse once the planned path resolves again, even called directly");
        assert!(
            err.contains("resolves") && err.contains("re-run"),
            "refusal must be typed and point the operator at re-planning: {err}"
        );

        let remaining = read_registry(&registry_path).unwrap().worktrees;
        assert_eq!(
            remaining.len(),
            1,
            "the row must survive the refusal, not be dropped: {remaining:?}"
        );

        match old_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn registry_path_falls_back_to_userprofile() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let old_home = std::env::var_os("HOME");
        let old_userprofile = std::env::var_os("USERPROFILE");
        std::env::remove_var("HOME");
        std::env::set_var("USERPROFILE", "/tmp/tachi-userprofile");

        let path = registry_path().unwrap();

        match old_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match old_userprofile {
            Some(value) => std::env::set_var("USERPROFILE", value),
            None => std::env::remove_var("USERPROFILE"),
        }
        assert_eq!(
            path,
            PathBuf::from("/tmp/tachi-userprofile")
                .join(".tachi")
                .join("worktrees.json")
        );
    }
}
