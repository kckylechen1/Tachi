use super::*;
use std::path::{Path, PathBuf};

fn with_env_lock<F: FnOnce()>(f: F) {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    f();
}

fn restore_env(name: &str, value: Option<std::ffi::OsString>) {
    if let Some(v) = value {
        std::env::set_var(name, v);
    } else {
        std::env::remove_var(name);
    }
}

#[test]
fn tachi_home_defaults_to_dot_tachi_under_user_home() {
    with_env_lock(|| {
        let tmp = tempfile::tempdir().expect("tmp");
        let saved_cwd = std::env::current_dir().expect("cwd");
        let saved_home = std::env::var_os("TACHI_HOME");
        let saved_sigil = std::env::var_os("SIGIL_HOME");
        let saved_app = std::env::var_os("TACHI_APP_HOME");
        std::env::set_current_dir(tmp.path()).expect("set cwd");
        std::env::remove_var("TACHI_HOME");
        std::env::remove_var("SIGIL_HOME");
        std::env::remove_var("TACHI_APP_HOME");

        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        assert_eq!(tachi_home(), home.join(".tachi"));

        std::env::set_current_dir(saved_cwd).expect("restore cwd");
        restore_env("TACHI_HOME", saved_home);
        restore_env("SIGIL_HOME", saved_sigil);
        restore_env("TACHI_APP_HOME", saved_app);
    });
}

#[test]
fn tachi_home_honors_tilde_and_tilde_slash() {
    with_env_lock(|| {
        let saved_home = std::env::var_os("TACHI_HOME");
        let saved_sigil = std::env::var_os("SIGIL_HOME");
        let saved_app = std::env::var_os("TACHI_APP_HOME");
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));

        std::env::set_var("TACHI_HOME", "~");
        assert_eq!(tachi_home(), home);

        std::env::set_var("TACHI_HOME", "~/custom-tachi");
        assert_eq!(tachi_home(), home.join("custom-tachi"));

        restore_env("TACHI_HOME", saved_home);
        restore_env("SIGIL_HOME", saved_sigil);
        restore_env("TACHI_APP_HOME", saved_app);
    });
}

#[test]
fn tachi_home_falls_back_to_tachi_app_home() {
    with_env_lock(|| {
        let saved_home = std::env::var_os("TACHI_HOME");
        let saved_sigil = std::env::var_os("SIGIL_HOME");
        let saved_app = std::env::var_os("TACHI_APP_HOME");
        std::env::remove_var("TACHI_HOME");
        std::env::remove_var("SIGIL_HOME");
        std::env::set_var("TACHI_APP_HOME", "/tmp/legacy-app-home");

        assert_eq!(tachi_home(), PathBuf::from("/tmp/legacy-app-home"));

        restore_env("TACHI_HOME", saved_home);
        restore_env("SIGIL_HOME", saved_sigil);
        restore_env("TACHI_APP_HOME", saved_app);
    });
}

#[test]
fn tachi_home_detects_workspace_data_tachi_layout() {
    with_env_lock(|| {
        let tmp = tempfile::tempdir().expect("tmp");
        let saved_cwd = std::env::current_dir().expect("cwd");
        let saved_home = std::env::var_os("TACHI_HOME");
        let saved_sigil = std::env::var_os("SIGIL_HOME");
        let saved_app = std::env::var_os("TACHI_APP_HOME");
        std::env::remove_var("TACHI_HOME");
        std::env::remove_var("SIGIL_HOME");
        std::env::remove_var("TACHI_APP_HOME");

        let repo = tmp.path().join("Quant_Analyzer_2026");
        let nested = repo.join("engine/v8");
        let local_home = repo.join("data/tachi");
        std::fs::create_dir_all(&nested).expect("nested");
        std::fs::create_dir_all(local_home.join("global")).expect("global parent");
        std::fs::write(local_home.join("global/memory.db"), b"").expect("global db");

        std::env::set_current_dir(&nested).expect("set cwd");
        let local_home = std::fs::canonicalize(local_home).expect("canonical local home");
        let repo = std::fs::canonicalize(repo).expect("canonical repo");
        assert_eq!(tachi_home(), local_home);
        assert_eq!(
            plan_c_global_db_path("hyperion"),
            repo.join("data/tachi/projects/hyperion/memory.db")
        );

        std::env::set_current_dir(saved_cwd).expect("restore cwd");
        restore_env("TACHI_HOME", saved_home);
        restore_env("SIGIL_HOME", saved_sigil);
        restore_env("TACHI_APP_HOME", saved_app);
    });
}

#[test]
fn named_project_from_path_accepts_canonical_layout() {
    with_env_lock(|| {
        let saved = std::env::var_os("TACHI_HOME");
        std::env::set_var("TACHI_HOME", "/tmp/tachi-test-home");
        let path = PathBuf::from("/tmp/tachi-test-home/projects/sigil/memory.db");
        assert_eq!(named_project_from_path(&path).as_deref(), Some("sigil"));
        restore_env("TACHI_HOME", saved);
    });
}

#[test]
fn named_project_from_path_honors_custom_tachi_home() {
    with_env_lock(|| {
        let saved = std::env::var_os("TACHI_HOME");
        std::env::set_var("TACHI_HOME", "/tmp/custom-tachi-root");
        let path = PathBuf::from("/tmp/custom-tachi-root/projects/my_app/memory.db");
        assert_eq!(named_project_from_path(&path).as_deref(), Some("my_app"));
        restore_env("TACHI_HOME", saved);
    });
}

#[test]
fn list_named_projects_finds_dirs_with_memory_db() {
    with_env_lock(|| {
        let saved = std::env::var_os("TACHI_HOME");
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("TACHI_HOME", tmp.path());
        let projects = tmp.path().join("projects");
        for name in ["alpha", "beta", "nodb"] {
            std::fs::create_dir_all(projects.join(name)).unwrap();
        }
        std::fs::File::create(projects.join("alpha").join("memory.db")).unwrap();
        std::fs::File::create(projects.join("beta").join("memory.db")).unwrap();
        // "nodb" has no memory.db -> excluded.
        let mut got = list_named_projects();
        got.sort();
        assert_eq!(got, vec!["alpha".to_string(), "beta".to_string()]);
        restore_env("TACHI_HOME", saved);
    });
}

#[test]
fn named_project_from_path_rejects_external_projects_dir() {
    let path = PathBuf::from("/data/tachi/projects/hyperion/memory.db");
    assert!(named_project_from_path(&path).is_none());
}

#[test]
fn named_project_for_db_path_accepts_plan_c_symlink_target() {
    with_env_lock(|| {
        let tmp = tempfile::tempdir().expect("tmp");
        let saved = std::env::var_os("TACHI_HOME");
        let tachi_home = tmp.path().join("home");
        std::env::set_var("TACHI_HOME", &tachi_home);

        let repo = tmp.path().join("Quant Analyzer");
        let local_db = repo.join(".tachi/memory.db");
        std::fs::create_dir_all(local_db.parent().unwrap()).expect("local parent");
        std::fs::write(&local_db, b"").expect("local db placeholder");
        ensure_plan_c_symlink(&local_db, &repo);

        // The alias dir name now carries the stable-hash suffix; the reverse
        // lookup must return exactly that name and it must start with the
        // sanitized basename.
        let expected = plan_c_dir_name_from_root(&repo).expect("dir name");
        assert!(expected.starts_with("Quant_Analyzer-"), "{expected}");
        assert_eq!(
            named_project_for_db_path(&local_db).as_deref(),
            Some(expected.as_str())
        );

        restore_env("TACHI_HOME", saved);
    });
}

#[test]
fn plan_c_regular_alias_file_reports_split_brain() {
    with_env_lock(|| {
        let tmp = tempfile::tempdir().expect("tmp");
        let saved = std::env::var_os("TACHI_HOME");
        let tachi_home = tmp.path().join("home");
        std::env::set_var("TACHI_HOME", &tachi_home);

        let repo = tmp.path().join("Split Brain Repo");
        let local_db = repo.join(".tachi/memory.db");
        std::fs::create_dir_all(local_db.parent().unwrap()).expect("local parent");
        memory_core::MemoryStore::open(local_db.to_str().expect("local db"))
            .expect("create local db");

        let alias_db = plan_c_global_db_path("Split_Brain_Repo");
        std::fs::create_dir_all(alias_db.parent().unwrap()).expect("alias parent");
        memory_core::MemoryStore::open(alias_db.to_str().expect("alias db"))
            .expect("create alias db");

        let outcome = ensure_plan_c_symlink(&local_db, &repo);
        let PlanCLinkOutcome::SplitBrain(issue) = outcome else {
            panic!("expected split-brain outcome, got {outcome:?}");
        };
        assert_eq!(issue.project_name, "Split_Brain_Repo");
        assert_eq!(issue.canonical_db, local_db);
        assert_eq!(issue.alias_db, alias_db);
        assert_eq!(issue.canonical_rows, Some(0));
        assert_eq!(issue.alias_rows, Some(0));
        assert!(issue.warning_message().contains("Plan C split-brain"));
        assert!(
            !issue.alias_db.is_symlink(),
            "regular alias file must not be silently replaced"
        );

        restore_env("TACHI_HOME", saved);
    });
}

#[test]
fn validate_project_db_relpath_rejects_parent_dir() {
    assert!(validate_project_db_relpath(Path::new("../secrets.db")).is_err());
}

#[test]
fn plan_c_dir_name_sanitizes_spaces() {
    let root = PathBuf::from("/tmp/My Cool Repo");
    // Legacy name is the bare sanitized basename; the current name carries a
    // stable-hash suffix but still starts with the sanitized basename.
    assert_eq!(
        plan_c_legacy_dir_name_from_root(&root).as_deref(),
        Some("My_Cool_Repo")
    );
    let name = plan_c_dir_name_from_root(&root).expect("dir name");
    assert!(name.starts_with("My_Cool_Repo-"), "{name}");
}

#[test]
fn plan_c_dir_name_same_basename_distinct_roots_differ() {
    // Two different absolute roots that share a basename must produce
    // DISTINCT alias dir names so they no longer collide on one alias path.
    let a = PathBuf::from("/tmp/workspace-a/api");
    let b = PathBuf::from("/tmp/workspace-b/api");
    let name_a = plan_c_dir_name_from_root(&a).expect("a");
    let name_b = plan_c_dir_name_from_root(&b).expect("b");
    assert!(name_a.starts_with("api-"), "{name_a}");
    assert!(name_b.starts_with("api-"), "{name_b}");
    assert_ne!(
        name_a, name_b,
        "same-basename repos must get distinct alias dirs"
    );
}

#[test]
fn plan_c_dir_name_is_stable_for_same_root() {
    // The same root must always hash to the same alias dir name.
    let root = PathBuf::from("/tmp/workspace/service");
    let first = plan_c_dir_name_from_root(&root).expect("first");
    let second = plan_c_dir_name_from_root(&root).expect("second");
    assert_eq!(first, second);
    assert!(first.starts_with("service-"), "{first}");
    // 8 hex chars of suffix after the "service-" prefix.
    let suffix = first.strip_prefix("service-").expect("suffix");
    assert_eq!(suffix.len(), 8);
    assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()), "{suffix}");
}

/// Regression for issue #493: on case-insensitive filesystems (macOS APFS,
/// Windows NTFS default) `std::fs::canonicalize` resolves symlinks and
/// `.`/`..` but does NOT fold letter case, so the same repo addressed with
/// different casing (`/Users/x/Desktop/Repo` vs `/Users/x/desktop/repo`)
/// derived two distinct stable-hash suffixes — splitting the project
/// identity and intermittently breaking strict resolver paths. The Plan C
/// alias rule now case-folds the canonical path before hashing on those
/// platforms, so case-only spelling differences map to one stable alias.
#[cfg(any(target_os = "macos", target_os = "windows"))]
#[test]
fn plan_c_dir_name_case_stable_on_case_insensitive_fs() {
    // Non-existent paths keep canonicalize on its fallback branch (the raw
    // input), so the result is deterministic regardless of the test host.
    let upper = Path::new("/Users/plan_c/Quant_Analyzer_2026");
    let lower = Path::new("/Users/plan_c/quant_analyzer_2026");
    let name_upper = plan_c_dir_name_from_root(upper).expect("upper name");
    let name_lower = plan_c_dir_name_from_root(lower).expect("lower name");
    let (_, suffix_upper) = name_upper.rsplit_once('-').expect("upper suffix");
    let (_, suffix_lower) = name_lower.rsplit_once('-').expect("lower suffix");
    assert_eq!(
        suffix_upper, suffix_lower,
        "stable-hash suffix must be identical for case-only path differences"
    );
    // The basename is intentionally NOT folded (preserves legacy casing and
    // existing assertions); under case-insensitive FS semantics the two alias
    // names still denote the same directory.
    assert_eq!(
        name_upper.to_lowercase(),
        name_lower.to_lowercase(),
        "alias must be equal under case-insensitive FS semantics"
    );
}

/// On case-sensitive filesystems (Linux, etc.) case-only path differences are
/// genuinely different paths and must keep producing distinct alias
/// identities. Guards against accidentally globalizing the case-fold beyond
/// the case-insensitive-FS rule.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[test]
fn plan_c_dir_name_case_distinct_on_case_sensitive_fs() {
    let upper = Path::new("/tmp/plan_c/Repo");
    let lower = Path::new("/tmp/plan_c/repo");
    let name_upper = plan_c_dir_name_from_root(upper).expect("upper name");
    let name_lower = plan_c_dir_name_from_root(lower).expect("lower name");
    assert_ne!(
        name_upper, name_lower,
        "case-sensitive platforms must not collapse case-only paths into one alias"
    );
}
