use chrono::Utc;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

fn current_git_root() -> Option<PathBuf> {
    std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

pub(crate) fn arena_root() -> PathBuf {
    if let Ok(p) = std::env::var("TACHI_ARENA_ROOT") {
        return PathBuf::from(p);
    }
    if let Some(root) = current_git_root() {
        return root.join(".tachi").join("arena");
    }
    if let Ok(home) = std::env::var("TACHI_HOME") {
        return PathBuf::from(home).join("arena");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".tachi").join("arena");
    }
    std::env::temp_dir().join("tachi").join("arena")
}

fn tachi_home() -> PathBuf {
    if let Ok(home) = std::env::var("TACHI_HOME") {
        PathBuf::from(home)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".tachi")
    } else {
        std::env::temp_dir().join("tachi")
    }
}

/// tachi#1229: resolve the on-disk run directory for a linked dispatch,
/// fail-closed against the same path-traversal class tachi#1173 closed for
/// caller-supplied dispatch ids -- one hop further out.
///
/// Both inputs here are *indirect*: `dispatch_id` and `run_dir_hint` are read
/// from a status.json `run_dir` / `dispatch_id` field (written by other code,
/// `actions/collect.rs:48` and `refresh_linked_dispatch_fields` below), not a
/// raw tool-call param -- but a poisoned status file could still steer these
/// reads at arbitrary filesystem paths. So:
///
/// - `dispatch_id` is gated with the shared `[A-Za-z0-9_-]` allowlist so the
///   id-derived canonical path can't be a `../` payload;
/// - `run_dir_hint` is honored only when it canonicalizes to a directory that
///   still lives inside the canonical dispatch runs root (`~/.tachi/runs`).
///   A hint that escapes via `..`, an absolute path elsewhere, or a symlink
///   out of the runs tree is ignored fail-closed and the id-derived canonical
///   path is used instead.
///
/// Returns `None` (behaves identically to "run not found") for an invalid
/// dispatch id, giving a probe no signal about what does or doesn't exist.
fn dispatch_run_dir(dispatch_id: &str, run_dir_hint: Option<&str>) -> Option<PathBuf> {
    if !crate::dispatch_ops::is_valid_dispatch_id(dispatch_id) {
        return None;
    }
    let runs_root = tachi_home().join("runs");
    let canonical = runs_root.join(dispatch_id);
    let Some(hint) = run_dir_hint
        .map(str::trim)
        .filter(|hint| !hint.is_empty())
        .map(PathBuf::from)
    else {
        return Some(canonical);
    };
    if crate::dispatch_ops::canonical_dir_is_within(&hint, &runs_root) {
        Some(hint)
    } else {
        Some(canonical)
    }
}

pub(super) fn dispatch_response_summary(response: &Value) -> Value {
    json!({
        "dispatch_id": response.get("dispatch_id").cloned().unwrap_or(Value::Null),
        "run_dir": response.get("run_dir").cloned().unwrap_or(Value::Null),
        "agent": response.get("agent").cloned().unwrap_or(Value::Null),
        "selected_profile": response.get("selected_profile").cloned().unwrap_or(Value::Null),
        "harness_transport": response.get("harness_transport").cloned().unwrap_or(Value::Null),
        "harness_server_url": response.get("harness_server_url").cloned().unwrap_or(Value::Null),
        "source": "dispatch_response_summary",
        "redacted": true,
    })
}

fn insert_present(map: &mut Map<String, Value>, source: &Value, key: &str) {
    if let Some(value) = source.get(key).filter(|value| !value.is_null()) {
        map.insert(key.to_string(), value.clone());
    }
}

fn compact_dispatch_status(source: &Value) -> Value {
    let mut out = Map::new();
    for key in [
        "dispatch_id",
        "state",
        "agent",
        "profile",
        "selected_profile",
        "harness_transport",
        "harness_server_url",
        "exit_code",
        "result_written",
        "source",
        "redacted",
    ] {
        insert_present(&mut out, source, key);
    }
    Value::Object(out)
}

pub(super) fn compact_mission_status(status: &Value) -> Value {
    let mut out = Map::new();
    for key in [
        "arena_id",
        "mission_id",
        "state",
        "harness",
        "requested_harness",
        "role",
        "launch_mode",
        "launch_status",
        "launch_requested",
        "launched",
        "launch_error",
        "dispatch_id",
        "dispatch_agent",
        "dispatch_profile_name",
        "profile",
        "model",
        "project",
        "flow_id",
        "issue_ref",
        "pr_ref",
        "collection_state",
        "plan_written",
        "result_written",
        "result_source",
        "artifact_read_error",
        "timeout_secs",
        "created_at",
        "updated_at",
        "collected_at",
        "completed_at",
        "abort_reason",
        "reap_reason",
    ] {
        insert_present(&mut out, status, key);
    }
    if let Some(task) = status.get("task").and_then(Value::as_str) {
        out.insert(
            "task_preview".to_string(),
            json!(crate::utils::compact_text_line(task, 180)),
        );
    }
    if let Some(link) = status.get("dispatch_link") {
        out.insert("dispatch_link".to_string(), compact_dispatch_status(link));
    }
    if let Some(linked) = status.get("linked_dispatch") {
        out.insert(
            "linked_dispatch".to_string(),
            compact_dispatch_status(linked),
        );
    }
    Value::Object(out)
}

fn read_linked_dispatch_status(dispatch_id: &str, run_dir_hint: Option<&str>) -> Option<Value> {
    let run_dir = dispatch_run_dir(dispatch_id, run_dir_hint)?;
    let status_path = run_dir.join("status.json");
    let status = read_json_file(&status_path).ok()?;
    let result_written = run_dir.join("result.md").exists();
    Some(json!({
        "dispatch_id": dispatch_id,
        "state": status.get("state").cloned().unwrap_or(Value::Null),
        "agent": status.get("agent").cloned().unwrap_or(Value::Null),
        "profile": status.get("profile").cloned().unwrap_or(Value::Null),
        "harness_transport": status.get("harness_transport").cloned().unwrap_or(Value::Null),
        "harness_server_url": status.get("harness_server_url").cloned().unwrap_or(Value::Null),
        "exit_code": status.get("exit_code").cloned().unwrap_or(Value::Null),
        "updated_at": status.get("updated_at").cloned().unwrap_or(Value::Null),
        "run_dir": run_dir.to_string_lossy().to_string(),
        "result_written": result_written,
        "source": "dispatch_run_summary",
        "redacted": true,
    }))
}

pub(super) fn read_linked_dispatch_result(
    dispatch_id: &str,
    run_dir_hint: Option<&str>,
) -> Option<String> {
    let run_dir = dispatch_run_dir(dispatch_id, run_dir_hint)?;
    let raw = std::fs::read_to_string(run_dir.join("result.md")).ok()?;
    if raw.trim().is_empty() {
        None
    } else {
        Some(raw)
    }
}

pub(super) fn refresh_linked_dispatch_fields(status: &mut Value) {
    let Some(obj) = status.as_object_mut() else {
        return;
    };
    let Some(dispatch_id) = obj
        .get("dispatch_id")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return;
    };
    obj.remove("dispatch_response");
    let run_dir_hint = obj.get("run_dir").and_then(Value::as_str);
    if let Some(linked) = read_linked_dispatch_status(&dispatch_id, run_dir_hint) {
        let linked_result_written = linked
            .get("result_written")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mission_result_written = obj
            .get("result_written")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if linked_result_written && !mission_result_written {
            obj.insert(
                "collection_state".to_string(),
                json!("pending_collect_from_dispatch"),
            );
        }
        obj.insert("linked_dispatch".to_string(), linked);
    }
}

fn slugify(s: &str, fallback: &str) -> String {
    let s = s.trim().to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut last_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed: String = out.trim_matches('-').chars().take(40).collect();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed
    }
}

pub(super) fn new_arena_id(title: Option<&str>, objective: Option<&str>) -> String {
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let basis = title.or(objective).unwrap_or("arena");
    let suffix = uuid::Uuid::new_v4().as_simple().to_string()[..8].to_string();
    format!("arena_{}_{}_{}", stamp, slugify(basis, "arena"), suffix)
}

pub(super) fn new_mission_id(role: Option<&str>, prompt: Option<&str>) -> String {
    let suffix = uuid::Uuid::new_v4().as_simple().to_string()[..8].to_string();
    let basis = role.or(prompt).unwrap_or("mission");
    format!("mission_{}_{}", slugify(basis, "mission"), suffix)
}

pub(crate) fn validate_arena_id(id: &str) -> Result<(), String> {
    validate_id(id, "arena_", "arena_id")
}

pub(crate) fn validate_mission_id(id: &str) -> Result<(), String> {
    validate_id(id, "mission_", "mission_id")
}

fn validate_id(id: &str, prefix: &str, label: &str) -> Result<(), String> {
    if !id.starts_with(prefix)
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "Invalid {label}: '{id}'. Expected prefix '{prefix}' and only ASCII letters, numbers, '_' or '-' with no path traversal."
        ));
    }
    Ok(())
}

pub(super) fn nonempty_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false)
}

pub(super) fn arena_dir(arena_id: &str) -> Result<PathBuf, String> {
    validate_arena_id(arena_id)?;
    Ok(arena_root().join(arena_id))
}

pub(super) fn mission_dir(arena_id: &str, mission_id: &str) -> Result<PathBuf, String> {
    validate_mission_id(mission_id)?;
    Ok(arena_dir(arena_id)?.join("missions").join(mission_id))
}

pub(super) fn read_json_file(path: &Path) -> Result<Value, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))
}

pub(super) enum ArenaArtifactRead {
    Present(String),
    Missing,
    Error(String),
}

pub(super) fn read_arena_artifact(path: &Path, label: &str) -> ArenaArtifactRead {
    match crate::utils::read_to_string_allow_missing(path, label) {
        Ok(Some(raw)) => ArenaArtifactRead::Present(raw),
        Ok(None) => ArenaArtifactRead::Missing,
        Err(err) => ArenaArtifactRead::Error(err),
    }
}

pub(super) fn update_mission_status(
    arena_id: &str,
    mission_id: &str,
    patch: Value,
) -> Result<Value, String> {
    let dir = mission_dir(arena_id, mission_id)?;
    let status_path = dir.join("status.json");
    let mut status = read_json_file(&status_path)?;
    if let Some(obj) = status.as_object_mut() {
        if let Some(patch_obj) = patch.as_object() {
            for (key, value) in patch_obj {
                obj.insert(key.clone(), value.clone());
            }
        }
        obj.insert("updated_at".to_string(), json!(Utc::now().to_rfc3339()));
        obj.insert(
            "plan_written".to_string(),
            json!(nonempty_file(&dir.join("plan.md"))),
        );
        obj.insert(
            "result_written".to_string(),
            json!(nonempty_file(&dir.join("result.md"))),
        );
    }
    refresh_linked_dispatch_fields(&mut status);
    crate::utils::write_json_file_owner_only(&status_path, &status)?;
    Ok(status)
}

pub(super) fn mission_statuses(arena_id: &str) -> Result<Vec<Value>, String> {
    let dir = arena_dir(arena_id)?;
    let missions_dir = dir.join("missions");
    if !missions_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&missions_dir)
        .map_err(|e| format!("read missions dir {}: {e}", missions_dir.display()))?
    {
        let entry = entry.map_err(|e| format!("read mission dir entry: {e}"))?;
        let status_path = entry.path().join("status.json");
        if status_path.exists() {
            let mut status = read_json_file(&status_path)?;
            if let Some(obj) = status.as_object_mut() {
                let dir = entry.path();
                obj.insert(
                    "plan_written".to_string(),
                    json!(nonempty_file(&dir.join("plan.md"))),
                );
                obj.insert(
                    "result_written".to_string(),
                    json!(nonempty_file(&dir.join("result.md"))),
                );
            }
            refresh_linked_dispatch_fields(&mut status);
            out.push(status);
        }
    }
    out.sort_by(|a, b| {
        a.get("created_at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(b.get("created_at").and_then(Value::as_str).unwrap_or(""))
    });
    Ok(out)
}

pub(super) fn active_state(state: &str) -> bool {
    matches!(state, "ready" | "running")
}

#[cfg(test)]
mod dispatch_run_dir_gate_tests {
    //! tachi#1229: discriminator tests for the fail-closed path gate on the
    //! indirect `run_dir_hint` / `dispatch_id` inputs to `dispatch_run_dir`.
    use super::*;

    // Serialize TACHI_HOME mutation within this module. Under the repo's
    // canonical nextest runner each test is its own process (env is already
    // isolated); this also guards the plain shared-process `cargo test` case.
    fn env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    struct HomeGuard {
        _guard: std::sync::MutexGuard<'static, ()>,
        original: Option<std::ffi::OsString>,
        root: PathBuf,
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            // SAFETY: env access is serialized by env_lock() for this module.
            unsafe {
                match self.original.as_ref() {
                    Some(value) => std::env::set_var("TACHI_HOME", value),
                    None => std::env::remove_var("TACHI_HOME"),
                }
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn set_home() -> HomeGuard {
        let guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let original = std::env::var_os("TACHI_HOME");
        let root =
            std::env::temp_dir().join(format!("tachi-runhint-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("runs")).unwrap();
        // SAFETY: serialized by env_lock(); restored/removed in Drop.
        unsafe {
            std::env::set_var("TACHI_HOME", &root);
        }
        HomeGuard {
            _guard: guard,
            original,
            root,
        }
    }

    #[test]
    fn legit_hint_within_runs_root_is_honored() {
        let home = set_home();
        let id = "20260718T101010Z-claude-abc12345";
        let legit = home.root.join("runs").join(id);
        std::fs::create_dir_all(&legit).unwrap();
        let hint = legit.to_string_lossy().to_string();
        let resolved = dispatch_run_dir(id, Some(&hint)).expect("valid id");
        assert_eq!(
            resolved.canonicalize().unwrap(),
            legit.canonicalize().unwrap()
        );
    }

    #[test]
    fn absolute_hint_outside_runs_root_is_rejected() {
        let home = set_home();
        let id = "abc123";
        std::fs::create_dir_all(home.root.join("runs").join(id)).unwrap();
        // Decoy directory outside the runs tree entirely.
        let decoy = home.root.join("decoy");
        std::fs::create_dir_all(&decoy).unwrap();
        let hint = decoy.to_string_lossy().to_string();
        let resolved = dispatch_run_dir(id, Some(&hint)).expect("valid id");
        // Falls back to the id-derived canonical path, never the decoy.
        assert_eq!(resolved, home.root.join("runs").join(id));
        assert_ne!(resolved.canonicalize().ok(), decoy.canonicalize().ok());
    }

    #[test]
    fn parent_traversal_hint_is_rejected() {
        let home = set_home();
        let id = "abc123";
        std::fs::create_dir_all(home.root.join("runs").join(id)).unwrap();
        let outside = home.root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        // `..`-escape out of the runs tree, resolving to an existing dir.
        let hint = format!("{}/runs/{}/../../outside", home.root.display(), id);
        let resolved = dispatch_run_dir(id, Some(&hint)).expect("valid id");
        assert_eq!(resolved, home.root.join("runs").join(id));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_hint_escaping_runs_root_is_rejected() {
        let home = set_home();
        let id = "abc123";
        std::fs::create_dir_all(home.root.join("runs").join(id)).unwrap();
        let outside = home.root.join("outside_target");
        std::fs::create_dir_all(&outside).unwrap();
        // Symlink planted *inside* the runs tree that points back out of it.
        let link = home.root.join("runs").join("escape");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let hint = link.to_string_lossy().to_string();
        let resolved = dispatch_run_dir(id, Some(&hint)).expect("valid id");
        // canonicalize resolves the symlink to `outside_target` → rejected.
        assert_eq!(resolved, home.root.join("runs").join(id));
    }

    #[test]
    fn invalid_dispatch_id_is_fail_closed() {
        let _home = set_home();
        assert!(dispatch_run_dir("../../etc", None).is_none());
        assert!(dispatch_run_dir("a/b", Some("whatever")).is_none());
        assert!(dispatch_run_dir("", None).is_none());
    }
}
