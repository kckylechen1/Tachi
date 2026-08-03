use super::*;
use crate::manifest::{DbEntry, DbRole};
use crate::test_support::{CwdRestore, EnvRestore};
use std::path::{Path, PathBuf};

fn with_env_lock<F: FnOnce()>(f: F) {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    f();
}

fn manifest_entry(path: &Path, role: DbRole, scope_hint: &str) -> DbEntry {
    DbEntry {
        path: path.to_string_lossy().into_owned(),
        role,
        owner: "test".into(),
        schema_kind: "tachi".into(),
        vec_enabled: false,
        allow_write: true,
        last_doctor_at: String::new(),
        last_classification: "healthy".into(),
        scope_hint: scope_hint.into(),
        notes: String::new(),
    }
}

#[test]
#[cfg(unix)]
fn manifest_leaf_protects_valid_project_scope_when_role_is_stale() {
    let dir = tempfile::tempdir().expect("tempdir");
    let foreign_db = dir.path().join("foreign.db");
    std::fs::write(&foreign_db, b"foreign").expect("foreign DB");
    let linked_project = dir.path().join("linked-project.db");
    std::os::unix::fs::symlink(&foreign_db, &linked_project).expect("linked project DB");
    let stale_linked = manifest_entry(&linked_project, DbRole::Unknown, "project:test");
    let linked_error = manifest_db_leaf_exists(&stale_linked)
        .expect_err("valid project scope must reject a linked project DB");
    assert!(
        linked_error.contains("canonical repo DB path"),
        "{linked_error}"
    );

    let missing_target = dir.path().join("missing.db");
    let dangling_project = dir.path().join("dangling-project.db");
    std::os::unix::fs::symlink(&missing_target, &dangling_project).expect("dangling project DB");
    let stale_dangling = manifest_entry(&dangling_project, DbRole::Unknown, "project:test");
    let dangling_error = manifest_db_leaf_exists(&stale_dangling)
        .expect_err("valid project scope must reject a dangling project DB");
    assert!(
        dangling_error.contains("canonical repo DB path"),
        "{dangling_error}"
    );

    for scope_hint in [
        "project:",
        "project: bad",
        "project:../test",
        "project:test/path",
        "project:test\\path",
        "project:.hidden",
        "project:unnamed",
        "project:test:stale",
        "project",
        "projects:test",
    ] {
        let entry = manifest_entry(&linked_project, DbRole::Unknown, scope_hint);
        assert_eq!(
            manifest_db_leaf_exists(&entry),
            Ok(true),
            "non-canonical scope '{scope_hint}' must not become project authority"
        );
    }

    for (role, scope_hint) in [
        (DbRole::Global, "global"),
        (DbRole::Agent, "runtime"),
        (DbRole::Foundry, "foundry"),
        (DbRole::Unknown, "runtime"),
    ] {
        let entry = manifest_entry(&linked_project, role, scope_hint);
        assert_eq!(
            manifest_db_leaf_exists(&entry),
            Ok(true),
            "non-project role/scope '{scope_hint}' must retain target-following behavior"
        );
    }
}

#[test]
fn tachi_home_defaults_to_dot_tachi_under_user_home() {
    with_env_lock(|| {
        let tmp = tempfile::tempdir().expect("tmp");
        let saved_cwd = std::env::current_dir().expect("cwd");

        std::env::set_current_dir(tmp.path()).expect("set cwd");
        let _tachi_home = EnvRestore::remove("TACHI_HOME");
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");

        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let resolution = resolve_tachi_home();
        assert_eq!(resolution.path, home.join(".tachi"));
        assert_eq!(resolution.source, TachiHomeSource::UserDefault);

        std::env::set_current_dir(saved_cwd).expect("restore cwd");
    });
}

#[test]
fn tachi_home_honors_tilde_and_tilde_slash() {
    with_env_lock(|| {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));

        let _tachi_home = EnvRestore::set("TACHI_HOME", "~");
        let resolution = resolve_tachi_home();
        assert_eq!(resolution.path, home);
        assert_eq!(
            resolution.source,
            TachiHomeSource::ExplicitEnv("TACHI_HOME")
        );

        std::env::set_var("TACHI_HOME", "~/custom-tachi");
        let resolution = resolve_tachi_home();
        assert_eq!(resolution.path, home.join("custom-tachi"));
        assert_eq!(
            resolution.source,
            TachiHomeSource::ExplicitEnv("TACHI_HOME")
        );
    });
}

#[test]
fn tachi_home_falls_back_to_tachi_app_home() {
    with_env_lock(|| {
        let _tachi_home = EnvRestore::remove("TACHI_HOME");
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::set("TACHI_APP_HOME", "/tmp/legacy-app-home");

        let resolution = resolve_tachi_home();
        assert_eq!(resolution.path, PathBuf::from("/tmp/legacy-app-home"));
        assert_eq!(
            resolution.source,
            TachiHomeSource::ExplicitEnv("TACHI_APP_HOME")
        );
    });
}

#[test]
fn tachi_home_detects_workspace_data_tachi_layout() {
    with_env_lock(|| {
        let tmp = tempfile::tempdir().expect("tmp");

        let _tachi_home = EnvRestore::remove("TACHI_HOME");
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");

        let repo = tmp.path().join("Quant_Analyzer_2026");
        let nested = repo.join("engine/v8");
        let local_home = repo.join("data/tachi");
        std::fs::create_dir_all(&nested).expect("nested");
        std::fs::create_dir_all(local_home.join("global")).expect("global parent");
        std::fs::write(local_home.join("global/memory.db"), b"").expect("global db");

        // Guard the cwd switch via RAII (not a bare set-then-restore pair):
        // if an assertion below panics, a bare restore statement is never
        // reached and the changed cwd leaks into every test that runs after
        // this one in the same process (same class of bug `with_tachi_home`
        // was hardened against for env vars in #1096 leaf-2a).
        let _cwd = CwdRestore::set(&nested);
        let local_home = std::fs::canonicalize(local_home).expect("canonical local home");
        let repo = std::fs::canonicalize(repo).expect("canonical repo");
        let resolution = resolve_tachi_home();
        assert_eq!(resolution.path, local_home);
        assert_eq!(resolution.source, TachiHomeSource::WorkspaceData);
        assert_eq!(
            plan_c_global_db_path("hyperion"),
            repo.join("data/tachi/projects/hyperion")
                .join(memcore::MEMORY_DB_FILENAME)
        );
    });
}

#[test]
fn named_project_from_path_accepts_canonical_layout() {
    let home = Path::new("/tmp/tachi-test-home");
    let path = home.join("projects/sigil/memory.db");
    assert_eq!(
        named_project_from_path_in_home(&path, home).as_deref(),
        Some("sigil")
    );
}

#[test]
fn named_project_from_path_honors_custom_tachi_home() {
    let home = Path::new("/tmp/custom-tachi-root");
    let path = home.join("projects/my_app/memory.db");
    assert_eq!(
        named_project_from_path_in_home(&path, home).as_deref(),
        Some("my_app")
    );
}

#[test]
fn list_named_projects_finds_dirs_with_memory_db() {
    with_env_lock(|| {
        let tmp = tempfile::tempdir().unwrap();
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", tmp.path());
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
    });
}

#[test]
fn named_project_from_path_rejects_external_projects_dir() {
    let path = PathBuf::from("/data/tachi/projects/hyperion/memory.db");
    assert!(named_project_from_path_in_home(&path, Path::new("/tmp/tachi-home")).is_none());
}

/// Display identity vs wire identity: a repo addressed through a deprecated
/// hash generation must be SHOWN under the gen-4 canonical name (the one
/// `server_methods/db.rs` migrates the binding to and the only one new
/// registrations emit), while an already-stable identity — gen-4 itself or
/// the gen-1 legacy bare basename, both of which a binding keeps per #1061 —
/// is left exactly as the caller spelled it.
#[test]
fn canonical_identity_for_display_replaces_only_deprecated_alias_generations() {
    let tmp = crate::test_support::non_skipped_fixture_tempdir("path-utils-");
    let repo = tmp.path().join("Sigil");
    let local_db = repo.join(".tachi").join("tachi-memory.db");
    std::fs::create_dir_all(local_db.parent().expect("local parent")).expect("local parent");
    std::fs::write(&local_db, b"").expect("local db placeholder");
    crate::test_support::assert_repo_local_db_fixture_not_skipped(&local_db);

    let canonical = plan_c_dir_name_from_root(&repo).expect("gen-4 identity");
    let gen3 = plan_c_previous_dir_name_from_root(&repo).expect("gen-3 identity");
    let gen2 = plan_c_previous_raw_dir_name_from_root(&repo).expect("gen-2 identity");
    let legacy = plan_c_legacy_dir_name_from_root(&repo).expect("gen-1 identity");
    assert_ne!(gen3, canonical, "generations must be distinguishable");

    assert_eq!(
        canonical_identity_for_display(&local_db, &gen3).as_deref(),
        Some(canonical.as_str()),
        "a gen-3 compatibility alias must display as the canonical identity"
    );
    assert_eq!(
        canonical_identity_for_display(&local_db, &gen2).as_deref(),
        Some(canonical.as_str()),
        "a gen-2 compatibility alias must display as the canonical identity"
    );
    assert_eq!(
        canonical_identity_for_display(&local_db, &canonical),
        None,
        "the canonical identity needs no substitute"
    );
    assert_eq!(
        canonical_identity_for_display(&local_db, &legacy),
        None,
        "the gen-1 legacy basename is a stable caller identity (#1061), not a \
         deprecated alias to be renamed under the caller"
    );

    // A path that is not a repo-local `.tachi/<db>` yields no root identity,
    // so callers keep showing what they have instead of inventing a name.
    let standalone = tmp.path().join("standalone.db");
    std::fs::write(&standalone, b"").expect("standalone db placeholder");
    assert_eq!(canonical_identity_for_display(&standalone, &gen3), None);
}

#[test]
fn named_project_for_db_path_accepts_plan_c_symlink_target() {
    with_env_lock(|| {
        let tmp = crate::test_support::non_skipped_fixture_tempdir("path-utils-");

        let tachi_home = tmp.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);

        let repo = tmp.path().join("Quant Analyzer");
        let local_db = repo.join(".tachi/memory.db");
        std::fs::create_dir_all(local_db.parent().unwrap()).expect("local parent");
        std::fs::write(&local_db, b"").expect("local db placeholder");
        crate::test_support::assert_repo_local_db_fixture_not_skipped(&local_db);
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
    });
}

#[test]
fn plan_c_regular_alias_file_reports_split_brain() {
    with_env_lock(|| {
        let tmp = crate::test_support::non_skipped_fixture_tempdir("path-utils-");

        let tachi_home = tmp.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);

        let repo = tmp.path().join("Split Brain Repo");
        let local_db = repo.join(".tachi/memory.db");
        std::fs::create_dir_all(local_db.parent().unwrap()).expect("local parent");
        memcore::MemoryStore::open(local_db.to_str().expect("local db")).expect("create local db");
        crate::test_support::assert_repo_local_db_fixture_not_skipped(&local_db);

        let alias_db = plan_c_global_db_path("Split_Brain_Repo");
        std::fs::create_dir_all(alias_db.parent().unwrap()).expect("alias parent");
        memcore::MemoryStore::open(alias_db.to_str().expect("alias db")).expect("create alias db");

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
    });
}

#[cfg(unix)]
#[test]
fn plan_c_wrong_target_symlink_is_refused_without_mutating_alias() {
    with_env_lock(|| {
        let tmp = crate::test_support::non_skipped_fixture_tempdir("path-utils-");
        let tachi_home = tmp.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);

        let repo = tmp.path().join("Wrong-Target-Repo");
        let local_db = repo.join(".tachi/tachi-memory.db");
        std::fs::create_dir_all(local_db.parent().unwrap()).expect("local parent");
        std::fs::write(&local_db, b"canonical").expect("local DB");
        crate::test_support::assert_repo_local_db_fixture_not_skipped(&local_db);

        let alias_name = plan_c_dir_name_from_root(&repo).expect("alias name");
        let alias_db = plan_c_global_db_path(&alias_name);
        let wrong_db = tmp.path().join("wrong-target.db");
        std::fs::write(&wrong_db, b"wrong target").expect("wrong DB");
        std::fs::create_dir_all(alias_db.parent().unwrap()).expect("alias parent");
        std::os::unix::fs::symlink(&wrong_db, &alias_db).expect("wrong alias symlink");

        let outcome = ensure_plan_c_symlink(&local_db, &repo);
        assert!(matches!(
            outcome,
            PlanCLinkOutcome::AliasIntegrity(PlanCAliasIntegrity::WrongTarget { .. })
        ));
        assert_eq!(
            std::fs::canonicalize(&alias_db).expect("alias remains readable"),
            std::fs::canonicalize(&wrong_db).expect("wrong target remains readable"),
            "detection must not replace the existing alias"
        );
    });
}

#[cfg(unix)]
#[test]
fn plan_c_matching_symlink_remains_valid() {
    with_env_lock(|| {
        let tmp = crate::test_support::non_skipped_fixture_tempdir("path-utils-");
        let tachi_home = tmp.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);

        let repo = tmp.path().join("Matching-Alias-Repo");
        let local_db = repo.join(".tachi/tachi-memory.db");
        std::fs::create_dir_all(local_db.parent().unwrap()).expect("local parent");
        std::fs::write(&local_db, b"canonical").expect("local DB");
        crate::test_support::assert_repo_local_db_fixture_not_skipped(&local_db);
        let alias_name = plan_c_dir_name_from_root(&repo).expect("alias name");
        let alias_db = plan_c_global_db_path(&alias_name);
        std::fs::create_dir_all(alias_db.parent().unwrap()).expect("alias parent");
        std::os::unix::fs::symlink(&local_db, &alias_db).expect("matching alias symlink");

        assert!(matches!(
            inspect_plan_c_alias_in_home(&local_db, &repo, &tachi_home),
            PlanCAliasInspection::MatchingSymlink
        ));
        assert!(matches!(
            ensure_plan_c_symlink(&local_db, &repo),
            PlanCLinkOutcome::AlreadyLinked
        ));
    });
}

#[cfg(unix)]
#[test]
fn plan_c_symlink_eexist_race_regular_file_returns_split_brain() {
    with_env_lock(|| {
        let tmp = crate::test_support::non_skipped_fixture_tempdir("path-utils-");
        let tachi_home = tmp.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
        let repo = tmp.path().join("Race-Regular-Repo");
        let local_db = repo.join(".tachi/tachi-memory.db");
        std::fs::create_dir_all(local_db.parent().unwrap()).expect("local parent");
        std::fs::write(&local_db, b"canonical").expect("local DB");

        let outcome =
            super::symlink::ensure_plan_c_symlink_with_test_hook(&local_db, &repo, |alias_db| {
                std::fs::write(alias_db, b"racing-regular").expect("racing alias")
            });

        let PlanCLinkOutcome::SplitBrain(issue) = outcome else {
            panic!("expected split-brain race outcome, got {outcome:?}");
        };
        assert_eq!(std::fs::read(&issue.alias_db).unwrap(), b"racing-regular");
        assert!(!issue.alias_db.is_symlink());
    });
}

#[cfg(unix)]
#[test]
fn plan_c_symlink_eexist_race_wrong_target_returns_integrity() {
    with_env_lock(|| {
        let tmp = crate::test_support::non_skipped_fixture_tempdir("path-utils-");
        let tachi_home = tmp.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
        let repo = tmp.path().join("Race-Wrong-Repo");
        let local_db = repo.join(".tachi/tachi-memory.db");
        std::fs::create_dir_all(local_db.parent().unwrap()).expect("local parent");
        std::fs::write(&local_db, b"canonical").expect("local DB");
        let wrong_db = tmp.path().join("wrong-target.db");
        std::fs::write(&wrong_db, b"wrong").expect("wrong DB");

        let outcome =
            super::symlink::ensure_plan_c_symlink_with_test_hook(&local_db, &repo, |alias_db| {
                std::os::unix::fs::symlink(&wrong_db, alias_db).expect("racing wrong symlink")
            });

        let PlanCLinkOutcome::AliasIntegrity(PlanCAliasIntegrity::WrongTarget {
            alias_db,
            actual_db,
            ..
        }) = outcome
        else {
            panic!("expected wrong-target integrity outcome, got {outcome:?}");
        };
        assert_eq!(actual_db, std::fs::canonicalize(&wrong_db).unwrap());
        assert_eq!(std::fs::read_link(alias_db).unwrap(), wrong_db);
    });
}

#[cfg(unix)]
#[test]
fn plan_c_symlink_eexist_race_matching_target_is_accepted() {
    with_env_lock(|| {
        let tmp = crate::test_support::non_skipped_fixture_tempdir("path-utils-");
        let tachi_home = tmp.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
        let repo = tmp.path().join("Race-Matching-Repo");
        let local_db = repo.join(".tachi/tachi-memory.db");
        std::fs::create_dir_all(local_db.parent().unwrap()).expect("local parent");
        std::fs::write(&local_db, b"canonical").expect("local DB");

        let outcome =
            super::symlink::ensure_plan_c_symlink_with_test_hook(&local_db, &repo, |alias_db| {
                std::os::unix::fs::symlink(&local_db, alias_db).expect("racing matching symlink")
            });

        assert!(matches!(outcome, PlanCLinkOutcome::AlreadyLinked));
        let alias_name = plan_c_dir_name_from_root(&repo).expect("alias name");
        let alias_db = plan_c_global_db_path(&alias_name);
        assert_eq!(std::fs::read_link(alias_db).unwrap(), local_db);
    });
}

#[cfg(unix)]
#[test]
fn plan_c_dangling_alias_is_a_typed_integrity_failure() {
    with_env_lock(|| {
        let tmp = crate::test_support::non_skipped_fixture_tempdir("path-utils-");
        let tachi_home = tmp.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);

        let repo = tmp.path().join("Dangling-Alias-Repo");
        let local_db = repo.join(".tachi/tachi-memory.db");
        std::fs::create_dir_all(local_db.parent().unwrap()).expect("local parent");
        std::fs::write(&local_db, b"canonical").expect("local DB");
        let alias_name = plan_c_dir_name_from_root(&repo).expect("alias name");
        let alias_db = plan_c_global_db_path(&alias_name);
        std::fs::create_dir_all(alias_db.parent().unwrap()).expect("alias parent");
        std::os::unix::fs::symlink(tmp.path().join("missing-target.db"), &alias_db)
            .expect("dangling alias symlink");

        assert!(matches!(
            inspect_plan_c_alias_in_home(&local_db, &repo, &tachi_home),
            PlanCAliasInspection::Integrity(PlanCAliasIntegrity::IdentityUnresolved { .. })
        ));
        assert!(matches!(
            ensure_plan_c_symlink(&local_db, &repo),
            PlanCLinkOutcome::AliasIntegrity(PlanCAliasIntegrity::IdentityUnresolved { .. })
        ));
        assert!(alias_db.is_symlink(), "dangling alias must not be replaced");
    });
}

#[cfg(unix)]
#[test]
fn plan_c_looped_alias_is_a_typed_integrity_failure() {
    with_env_lock(|| {
        let tmp = crate::test_support::non_skipped_fixture_tempdir("path-utils-");
        let tachi_home = tmp.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);

        let repo = tmp.path().join("Looped-Alias-Repo");
        let local_db = repo.join(".tachi/tachi-memory.db");
        std::fs::create_dir_all(local_db.parent().unwrap()).expect("local parent");
        std::fs::write(&local_db, b"canonical").expect("local DB");
        let alias_name = plan_c_dir_name_from_root(&repo).expect("alias name");
        let alias_db = plan_c_global_db_path(&alias_name);
        std::fs::create_dir_all(alias_db.parent().unwrap()).expect("alias parent");
        std::os::unix::fs::symlink(&alias_db, &alias_db).expect("looped alias symlink");

        assert!(matches!(
            inspect_plan_c_alias_in_home(&local_db, &repo, &tachi_home),
            PlanCAliasInspection::Integrity(PlanCAliasIntegrity::IdentityUnresolved { .. })
        ));
        assert!(matches!(
            ensure_plan_c_symlink(&local_db, &repo),
            PlanCLinkOutcome::AliasIntegrity(PlanCAliasIntegrity::IdentityUnresolved { .. })
        ));
    });
}

#[test]
fn plan_c_alias_resolution_rejects_divergent_compatibility_candidates() {
    with_env_lock(|| {
        let tmp = crate::test_support::non_skipped_fixture_tempdir("path-utils-");
        let tachi_home = tmp.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
        let repo = tmp.path().join("Repo");
        std::fs::create_dir(&repo).expect("repo");
        let current = plan_c_dir_name_from_root(&repo).expect("current identity");
        let previous = plan_c_previous_dir_name_from_root(&repo).expect("previous identity");
        assert_ne!(current, previous);

        for (name, bytes) in [
            (&current, b"current".as_slice()),
            (&previous, b"previous".as_slice()),
        ] {
            let db = plan_c_global_db_path(name);
            std::fs::create_dir_all(db.parent().unwrap()).expect("alias parent");
            std::fs::write(db, bytes).expect("alias DB");
        }

        let error = plan_c_alias_db_for_root_in_home(&repo, &tachi_home)
            .expect_err("divergent current and compatibility aliases must fail closed");
        assert!(
            error.contains("ambiguous across divergent aliases"),
            "{error}"
        );
    });
}

#[test]
fn validate_project_db_relpath_rejects_parent_dir() {
    assert!(validate_project_db_relpath(Path::new("../secrets.db")).is_err());
}

#[test]
fn plan_c_dir_name_sanitizes_spaces() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("My Cool Repo");
    std::fs::create_dir(&root).expect("root");
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
    let tmp = tempfile::tempdir().expect("tempdir");
    let a = tmp.path().join("workspace-a/api");
    let b = tmp.path().join("workspace-b/api");
    std::fs::create_dir_all(&a).expect("a root");
    std::fs::create_dir_all(&b).expect("b root");
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
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("workspace/service");
    std::fs::create_dir_all(&root).expect("root");
    let first = plan_c_dir_name_from_root(&root).expect("first");
    let second = plan_c_dir_name_from_root(&root).expect("second");
    assert_eq!(first, second);
    assert!(first.starts_with("service-"), "{first}");
    // A cryptographic, collision-resistant suffix follows the readable prefix.
    let suffix = first.strip_prefix("service-").expect("suffix");
    assert_eq!(suffix.len(), 24);
    assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()), "{suffix}");
}

#[test]
fn plan_c_identity_bounds_prefix_and_never_generates_unnamed_for_non_ascii_roots() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let non_ascii_root = tmp.path().join("workspace/量化");
    std::fs::create_dir_all(&non_ascii_root).expect("non-ASCII root");
    let non_ascii = plan_c_dir_name_from_root(&non_ascii_root).expect("non-ASCII project identity");
    assert!(non_ascii.starts_with("project-"), "{non_ascii}");
    assert!(!non_ascii.starts_with("unnamed-"), "{non_ascii}");

    let long_root = tmp.path().join("a".repeat(200));
    std::fs::create_dir(&long_root).expect("long root");
    let long = plan_c_dir_name_from_root(&long_root).expect("bounded project identity");
    let (prefix, suffix) = long.rsplit_once('-').expect("hash suffix");
    assert!(prefix.len() <= 48, "prefix was not bounded: {prefix}");
    assert_eq!(suffix.len(), 24);
}

#[test]
fn plan_c_identity_uses_physical_root_identity_for_unicode_normalization_forms() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let nfc_root = tmp.path().join("one/caf\u{e9}");
    let nfd_root = tmp.path().join("two/cafe\u{301}");
    std::fs::create_dir_all(&nfc_root).expect("NFC root");
    std::fs::create_dir_all(&nfd_root).expect("NFD root");
    let nfc = plan_c_dir_name_from_root(&nfc_root).expect("NFC identity");
    let nfd = plan_c_dir_name_from_root(&nfd_root).expect("NFD identity");
    assert_ne!(
        nfc, nfd,
        "distinct physical root paths remain distinct even when display names are canonically equivalent"
    );
}

/// Regression for issue #493: on case-insensitive filesystems (macOS APFS,
/// Windows NTFS default) `std::fs::canonicalize` resolves symlinks and
/// `.`/`..` but does NOT fold letter case, so the same repo addressed with
/// different casing (`/Users/x/Desktop/Repo` vs `/Users/x/desktop/repo`)
/// derived two distinct stable-hash suffixes — splitting the project
/// identity and intermittently breaking strict resolver paths. The Plan C
/// alias rule now case-folds the canonical path before hashing on those
/// platforms, so case-only spelling differences map to one stable alias.
#[cfg(unix)]
#[test]
fn plan_c_identity_is_identical_through_differently_named_symlink() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let physical = tmp.path().join("Physical-Repo");
    let alias = tmp.path().join("different-display-name");
    std::fs::create_dir(&physical).expect("physical root");
    std::os::unix::fs::symlink(&physical, &alias).expect("root symlink");
    let physical_name = plan_c_dir_name_from_root(&physical).expect("physical identity");
    let alias_name = plan_c_dir_name_from_root(&alias).expect("symlink identity");
    assert_eq!(
        physical_name, alias_name,
        "both readable prefix and digest must derive from the canonical physical root"
    );
}

/// On case-sensitive filesystems (Linux, etc.) case-only path differences are
/// genuinely different paths and must keep producing distinct alias
/// identities. Guards against accidentally globalizing the case-fold beyond
/// the case-insensitive-FS rule.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[test]
fn plan_c_dir_name_case_distinct_on_case_sensitive_fs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let upper = tmp.path().join("Repo");
    let lower = tmp.path().join("repo");
    std::fs::create_dir(&upper).expect("upper root");
    std::fs::create_dir(&lower).expect("lower root");
    let name_upper = plan_c_dir_name_from_root(&upper).expect("upper name");
    let name_lower = plan_c_dir_name_from_root(&lower).expect("lower name");
    assert_ne!(
        name_upper, name_lower,
        "case-sensitive platforms must not collapse case-only paths into one alias"
    );
}

/// Hole 1 (#1356 salvage): the gen-2 raw-canonical FNV-8 alias scheme (#424,
/// commit b1c3bb26) must be a compatibility candidate. On case-insensitive
/// hosts a repo whose canonical path contains ASCII uppercase materialized a
/// gen-2 suffix that differs from the gen-3 case-folded suffix (#493) and from
/// the gen-4 BLAKE2s suffix (this PR). The salvage draft's {gen-4, gen-3, gen-1}
/// chain missed it, so resolution would ignore real on-disk data and let the
/// caller register a fresh empty DB — orphaning it (fail-closed violation).
#[cfg(any(target_os = "macos", target_os = "windows"))]
#[test]
fn plan_c_gen2_raw_canonical_alias_is_recognized_not_orphaned() {
    with_env_lock(|| {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tachi_home = tmp.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
        // An uppercase segment makes the raw vs case-folded canonical paths
        // differ, so this host+path genuinely exercises the gen-2 gap.
        let repo = tmp.path().join("Gen2Repo");
        std::fs::create_dir(&repo).expect("repo");

        let gen2 = plan_c_previous_raw_dir_name_from_root(&repo).expect("gen-2 identity");
        let gen3 = plan_c_previous_dir_name_from_root(&repo).expect("gen-3 identity");
        let gen4 = plan_c_dir_name_from_root(&repo).expect("gen-4 identity");
        assert_ne!(
            gen2, gen3,
            "test host does not exercise the gen-2/gen-3 split"
        );
        assert_ne!(gen2, gen4);

        // Only the gen-2 alias exists on disk (real historical data).
        let gen2_db = plan_c_global_db_path(&gen2);
        std::fs::create_dir_all(gen2_db.parent().unwrap()).expect("gen-2 alias parent");
        std::fs::write(&gen2_db, b"gen-2 data").expect("gen-2 alias DB");

        let resolved = plan_c_alias_db_for_root_in_home(&repo, &tachi_home)
            .expect("gen-2 alias must resolve, not fail");
        assert_eq!(
            std::fs::canonicalize(&resolved).unwrap(),
            std::fs::canonicalize(&gen2_db).unwrap(),
            "gen-2 alias data must be reused, never orphaned by a fresh gen-4 path"
        );
    });
}

/// Hole 1 (#1356 salvage): a genuinely new project — no on-disk alias under ANY
/// of the four naming generations — must still resolve to a fresh gen-4 path so
/// the fail-closed candidate set never over-closes and blocks new registration.
#[test]
fn plan_c_genuinely_absent_project_resolves_to_fresh_gen4_path() {
    with_env_lock(|| {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tachi_home = tmp.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
        let repo = tmp.path().join("BrandNew");
        std::fs::create_dir(&repo).expect("repo");

        let gen4 = plan_c_dir_name_from_root(&repo).expect("gen-4 identity");
        let resolved =
            plan_c_alias_db_for_root_in_home(&repo, &tachi_home).expect("fresh registration path");
        assert_eq!(resolved, plan_c_global_db_path(&gen4));
        assert!(matches!(
            inspect_plan_c_alias_in_home(&repo.join(".tachi/tachi-memory.db"), &repo, &tachi_home),
            PlanCAliasInspection::Absent
        ));
    });
}

/// Compatibility aliases that diverge are an alias-integrity finding, never a
/// fabricated clean split-brain result.
#[test]
fn plan_c_split_brain_defers_to_routing_gate_on_ambiguous_identity() {
    with_env_lock(|| {
        let tmp = crate::test_support::non_skipped_fixture_tempdir("path-utils-");
        let tachi_home = tmp.path().join("home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
        let repo = tmp.path().join("Repo");
        std::fs::create_dir(&repo).expect("repo");
        let local_db = repo.join(".tachi/tachi-memory.db");
        std::fs::create_dir_all(local_db.parent().unwrap()).expect("local db parent");
        std::fs::write(&local_db, b"repo-local").expect("local db");

        let current = plan_c_dir_name_from_root(&repo).expect("current identity");
        let previous = plan_c_previous_dir_name_from_root(&repo).expect("previous identity");
        assert_ne!(current, previous);
        for (name, bytes) in [
            (&current, b"one".as_slice()),
            (&previous, b"two".as_slice()),
        ] {
            let db = plan_c_global_db_path(name);
            std::fs::create_dir_all(db.parent().unwrap()).expect("alias parent");
            std::fs::write(db, bytes).expect("divergent alias DB");
        }

        // The routing gate fails closed on the ambiguity...
        let gate = plan_c_alias_db_for_root_in_home(&repo, &tachi_home);
        assert!(
            gate.is_err(),
            "ambiguous identity must fail closed at the gate"
        );
        // ...and the diagnostic surface returns the typed integrity failure.
        assert!(matches!(
            inspect_plan_c_alias_in_home(&local_db, &repo, &tachi_home),
            PlanCAliasInspection::Integrity(PlanCAliasIntegrity::IdentityUnresolved { .. })
        ));
    });
}
