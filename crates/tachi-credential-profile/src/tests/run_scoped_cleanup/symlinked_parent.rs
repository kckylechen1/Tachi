use super::*;

// The worker that ran in the run directory can rewrite anything under it, so a
// path-based removal would follow its symlink and delete the host file.
#[test]
fn credential_run_scoped_cleanup_refuses_to_traverse_a_symlinked_parent() {
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-cleanup-symlinked-parent-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth json metadata");

    let host = tempfile::tempdir().expect("host dir outside the run dir");
    let run_dir = tempfile::tempdir().expect("temp run dir");
    let profile = crate::CredentialProfile {
        provider: None,
        description: None,
        entries: [("auth_json".to_string(), "CODEX_AUTH_JSON".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::AllowedConsumers::default(),
        materializers: vec![crate::CredentialMaterializer {
            kind: "config_overlay".to_string(),
            source: "auth_json".to_string(),
            target: "{credentials_dir}/auth.json".to_string(),
            chmod: None,
            template: None,
        }],
    };
    let secret_values = [(
        "CODEX_AUTH_JSON".to_string(),
        r#"{"token":"secret-token"}"#.to_string(),
    )]
    .into_iter()
    .collect();
    crate::apply_credential_materialization(
        "codex_shared",
        &profile,
        "codex_cli",
        &store,
        &secret_values,
        &crate::CredentialApplyOptions {
            allow_existing: false,
            run_dir: Some(run_dir.path().to_path_buf()),
        },
    )
    .expect("apply run-scoped materialization");

    // The worker swaps its credentials directory for a link to a host
    // directory holding a file of the very same name and bytes.
    let credentials_dir = std::fs::canonicalize(run_dir.path())
        .expect("canonical run dir")
        .join("credentials");
    let host_credentials = host.path().join("credentials");
    let host_auth = host_credentials.join("auth.json");
    std::fs::create_dir_all(&host_credentials).expect("create host credentials dir");
    std::fs::write(&host_auth, r#"{"token":"secret-token"}"#).expect("write host file");
    std::fs::remove_dir_all(&credentials_dir).expect("drop materialized credentials dir");
    std::os::unix::fs::symlink(&host_credentials, &credentials_dir)
        .expect("worker plants a symlinked parent");
    assert!(
        credentials_dir.join("auth.json").exists(),
        "the planted link resolves to the host file"
    );

    let dry_run =
        crate::cleanup_ephemeral_credential_materializations(&store, run_dir.path(), true)
            .expect("dry-run cleanup");
    assert!(dry_run.would_remove.is_empty(), "{dry_run:?}");
    assert!(
        dry_run.errors.iter().any(|error| error.contains("symlink")),
        "{dry_run:?}"
    );

    let applied =
        crate::cleanup_ephemeral_credential_materializations(&store, run_dir.path(), false)
            .expect("apply cleanup");

    assert_eq!(
        std::fs::read_to_string(&host_auth).ok().as_deref(),
        Some(r#"{"token":"secret-token"}"#),
        "cleanup deleted a host file through the worker's symlink: {applied:?}"
    );
    assert!(applied.removed.is_empty(), "{applied:?}");
    assert!(applied.missing.is_empty(), "{applied:?}");
    assert!(
        applied.errors.iter().any(|error| error.contains("symlink")),
        "{applied:?}"
    );
    assert!(
        std::fs::symlink_metadata(&credentials_dir)
            .expect("planted link")
            .file_type()
            .is_symlink(),
        "cleanup must leave the worker's link alone"
    );
    let rows = store
        .list_state(crate::CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .expect("list managed credential metadata");
    assert_eq!(rows.len(), 1);
    assert_ne!(
        serde_json::from_str::<serde_json::Value>(&rows[0].value_json).expect("metadata JSON")
            ["cleanup_status"],
        serde_json::json!("cleaned"),
        "a refused target must stay open for the next cleanup"
    );
}

/// The run directory is reached through a link that lives where a worker may
/// write. Where the link points at cleanup time says nothing about where it
/// pointed when the credentials were recorded, so the cleanup refuses the
/// whole run directory: nothing is removed and the records stay open.
#[test]
fn credential_run_scoped_cleanup_refuses_a_run_dir_behind_a_link() {
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-cleanup-alias-run-dir-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth json metadata");

    let root = tempfile::tempdir().expect("temp root");
    let real_run_dir = root.path().join("real-run");
    let alias_run_dir = root.path().join("alias-run");
    std::fs::create_dir_all(real_run_dir.join("credentials")).expect("create real credentials");
    std::os::unix::fs::symlink(&real_run_dir, &alias_run_dir).expect("create run_dir alias");

    let lexical_target = alias_run_dir.join("credentials").join("auth.json");
    let profile = legacy_auth_file_profile(&lexical_target);
    seed_legacy_auth_file(&store, &profile);

    let error = crate::cleanup_ephemeral_credential_materializations(&store, &alias_run_dir, false)
        .expect_err("a link to the run directory cannot be vouched for");
    assert!(error.contains("without following a link"), "{error}");
    assert!(
        real_run_dir.join("credentials").join("auth.json").exists(),
        "the credential file was removed through the link"
    );
    let rows = store
        .list_state(crate::CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .expect("list managed credential metadata");
    assert_eq!(rows.len(), 1);
    assert_ne!(
        serde_json::from_str::<serde_json::Value>(&rows[0].value_json).expect("metadata JSON")
            ["cleanup_status"],
        serde_json::json!("cleaned"),
        "a refused run directory must leave its records open"
    );
}

/// A worker repoints the run directory's link before the cleanup runs, at
/// another directory that holds a file of the same name.
#[test]
fn credential_run_scoped_cleanup_refuses_a_repointed_run_dir_link() {
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-cleanup-repointed-link-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth json metadata");

    let root = std::fs::canonicalize(tempfile::tempdir().expect("temp root").path())
        .expect("canonical root");
    let real_run_dir = root.join("job");
    let alias_run_dir = root.join("current");
    std::fs::create_dir_all(real_run_dir.join("credentials")).expect("create real credentials");
    std::os::unix::fs::symlink(&real_run_dir, &alias_run_dir).expect("create run_dir alias");

    let lexical_target = alias_run_dir.join("credentials").join("auth.json");
    let profile = legacy_auth_file_profile(&lexical_target);
    seed_legacy_auth_file(&store, &profile);

    let other_run_dir = root.join("other");
    let sentinel = other_run_dir.join("credentials").join("auth.json");
    std::fs::create_dir_all(sentinel.parent().unwrap()).expect("create other credentials");
    std::fs::write(&sentinel, "another run's credential").expect("seed the other run");
    std::fs::remove_file(&alias_run_dir).expect("drop the link");
    std::os::unix::fs::symlink(&other_run_dir, &alias_run_dir).expect("worker repoints the link");

    let error = crate::cleanup_ephemeral_credential_materializations(&store, &alias_run_dir, false)
        .expect_err("a repointed link cannot be vouched for");
    assert!(error.contains("without following a link"), "{error}");
    assert_eq!(
        std::fs::read_to_string(&sentinel).ok().as_deref(),
        Some("another run's credential"),
        "cleanup followed the repointed link and deleted another run's file"
    );
}

/// A link nested in the run directory itself is the same case: the worker
/// can rewrite it, so the cleanup refuses the run directory.
#[test]
fn credential_run_scoped_cleanup_refuses_a_link_nested_inside_the_run_dir() {
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-cleanup-nested-alias-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth json metadata");

    let root = tempfile::tempdir().expect("temp root");
    let real_run_dir = root.path().join("job");
    let alias_run_dir = real_run_dir.join("current");
    std::fs::create_dir_all(real_run_dir.join("credentials")).expect("create real credentials");
    std::os::unix::fs::symlink(&real_run_dir, &alias_run_dir)
        .expect("create alias nested inside real_run_dir");

    let legacy_target = alias_run_dir.join("credentials").join("auth.json");
    let profile = legacy_auth_file_profile(&legacy_target);
    seed_legacy_auth_file(&store, &profile);

    let error = crate::cleanup_ephemeral_credential_materializations(&store, &alias_run_dir, false)
        .expect_err("a nested link cannot be vouched for");
    assert!(error.contains("without following a link"), "{error}");
    assert!(
        real_run_dir.join("credentials").join("auth.json").exists(),
        "the credential file was removed through the nested link"
    );
    let rows = store
        .list_state(crate::CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .expect("list managed credential metadata");
    assert_eq!(rows.len(), 1);
    assert_ne!(
        serde_json::from_str::<serde_json::Value>(&rows[0].value_json).expect("metadata JSON")
            ["cleanup_status"],
        serde_json::json!("cleaned"),
        "a refused run directory must leave its records open"
    );
}

#[test]
fn credential_run_scoped_cleanup_does_not_redirect_when_run_dir_alias_points_elsewhere() {
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-cleanup-retargeted-alias-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth json metadata");

    let root = std::fs::canonicalize(tempfile::tempdir().expect("temp root").path())
        .expect("canonical root");
    let dir_a = root.join("run-a");
    let dir_b = root.join("run-b");
    std::fs::create_dir_all(dir_a.join("credentials")).expect("dir_a credentials");
    std::fs::create_dir_all(dir_b.join("credentials")).expect("dir_b credentials");
    std::fs::write(dir_b.join("credentials/auth.json"), "keep-dir-b-file").expect("seed dir_b");

    let alias_run_dir = root.join("run-alias");
    std::os::unix::fs::symlink(&dir_b, &alias_run_dir).expect("alias -> dir_b");

    let profile = legacy_auth_file_profile(&dir_a.join("credentials/auth.json"));
    seed_legacy_auth_file(&store, &profile);

    let error = crate::cleanup_ephemeral_credential_materializations(&store, &alias_run_dir, false)
        .expect_err("a run directory reached through a link is refused outright");
    assert!(error.contains("without following a link"), "{error}");
    assert_eq!(
        std::fs::read_to_string(dir_b.join("credentials/auth.json"))
            .ok()
            .as_deref(),
        Some("keep-dir-b-file"),
        "cleanup must never delete a file in the swapped alias target"
    );
    assert!(
        dir_a.join("credentials/auth.json").exists(),
        "a refused run directory leaves every file alone"
    );
    // A target outside that run directory is read by its own path.
    match crate::cleanup::inspect_target_file(&dir_a.join("credentials/auth.json"), None, 1024) {
        crate::cleanup::TargetInspection::RegularFile { bytes, .. } => {
            assert_ne!(bytes, b"keep-dir-b-file", "inspection read dir_b");
        }
        _ => panic!("expected regular file at dir_a"),
    }
}

/// A cleanup for a run directory that is gone is still limited to that run
/// directory's records. Another run's open record must not be closed over a
/// credential file that is still in use.
#[test]
fn a_missing_run_directory_closes_only_its_own_records() {
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-cleanup-missing-run-dir-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth json metadata");

    let root = tempfile::tempdir().expect("temp root");
    let gone_target = root.path().join("gone/credentials/auth.json");
    let live_target = root.path().join("live/credentials/auth.json");
    seed_legacy_auth_file(&store, &legacy_auth_file_profile(&gone_target));
    seed_legacy_auth_file(&store, &legacy_auth_file_profile(&live_target));
    // The first run ended and took its directory with it.
    std::fs::remove_dir_all(root.path().join("gone")).expect("drop the finished run directory");

    let gone = gone_target.to_string_lossy().to_string();
    let live = live_target.to_string_lossy().to_string();
    for dry_run in [true, false] {
        let report = crate::cleanup_ephemeral_credential_materializations(
            &store,
            &root.path().join("gone"),
            dry_run,
        )
        .expect("a gone run directory is cleaned");
        assert_eq!(
            report.missing,
            vec![gone.clone()],
            "dry_run={dry_run}: {report:?}"
        );
        assert!(report.removed.is_empty(), "dry_run={dry_run}: {report:?}");
        assert!(
            report.skipped.contains(&live),
            "dry_run={dry_run}: {report:?}"
        );
    }

    assert_eq!(
        std::fs::read_to_string(&live_target).ok().as_deref(),
        Some(r#"{"token":"secret-token"}"#),
        "another run's credential file was removed"
    );
    let mut statuses = std::collections::BTreeMap::new();
    for row in store
        .list_state(crate::CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .expect("list managed credential metadata")
    {
        let metadata: serde_json::Value =
            serde_json::from_str(&row.value_json).expect("metadata JSON");
        statuses.insert(
            metadata["target"]
                .as_str()
                .expect("recorded target")
                .to_string(),
            metadata["cleanup_status"].as_str().map(str::to_string),
        );
    }
    assert_eq!(
        statuses.remove(&gone),
        Some(Some("cleaned".to_string())),
        "the finished run's record is closed"
    );
    assert_eq!(
        statuses.remove(&live),
        Some(None),
        "the live run's record stays open"
    );
}

/// `..` in a run directory is refused outright: resolving it by name would
/// skip whatever sits in front of it, a link included.
#[test]
fn a_run_directory_written_with_dot_dot_is_refused() {
    let root = std::fs::canonicalize(tempfile::tempdir().expect("temp root").path())
        .expect("canonical root");
    std::fs::create_dir_all(root.join("real/job/credentials")).expect("real run dir");
    std::os::unix::fs::symlink(root.join("real"), root.join("link"))
        .expect("link to the real tree");

    for run_dir in [root.join("link/../job"), root.join("real/../real/job")] {
        match crate::cleanup::open_run_root(&run_dir) {
            Err(error) => assert!(error.contains("'..'"), "{error}"),
            Ok(_) => panic!("'..' was resolved by name: {}", run_dir.display()),
        }
    }
    match crate::cleanup::open_run_root(&root.join("link/job")) {
        Err(_) => {}
        Ok(_) => panic!("a run directory behind a link opened"),
    }
    match crate::cleanup::open_run_root(&root.join("real/job")) {
        Ok(_) => {}
        Err(error) => panic!("the same directory named in full was refused: {error}"),
    }
}

/// Puts the test in a working directory and puts the previous one back on
/// every exit, a panic included.
struct WorkingDirectory(std::path::PathBuf);

impl WorkingDirectory {
    fn enter(directory: &std::path::Path) -> Self {
        let previous = Self(std::env::current_dir().expect("cwd"));
        std::env::set_current_dir(directory).expect("change the working directory");
        previous
    }
}

impl Drop for WorkingDirectory {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.0);
    }
}

/// An older record spells its target relative to a working directory that the
/// record does not keep. Read from any other directory, the daemon's own
/// included, that path names another run's file. So it belongs to no run
/// directory: a run-scoped cleanup neither closes the record nor touches a
/// file under that name.
#[test]
fn a_record_spelled_by_a_relative_path_belongs_to_no_run_directory() {
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-cleanup-relative-record-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth json metadata");

    let root = tempfile::tempdir().expect("temp root");
    let written_from = root.path().join("first");
    let cleaned_from = root.path().join("second");
    let target = std::path::PathBuf::from("run/credentials/auth.json");

    std::fs::create_dir_all(&written_from).expect("first run root");
    let _guard = WorkingDirectory::enter(&written_from);
    seed_legacy_auth_file(&store, &legacy_auth_file_profile(&target));
    // The same path, read from where the next cleanup runs, is another run's
    // file.
    let other_file = cleaned_from.join(&target);
    std::fs::create_dir_all(other_file.parent().expect("parent")).expect("second run dir");
    std::fs::write(&other_file, "another run's credential file").expect("other run's file");

    std::env::set_current_dir(&cleaned_from).expect("work from the second run's root");
    let report = crate::cleanup_ephemeral_credential_materializations(
        &store,
        &cleaned_from.join("run"),
        false,
    )
    .expect("the cleanup runs");
    assert!(
        report.removed.is_empty() && report.missing.is_empty(),
        "{report:?}"
    );
    assert!(
        report
            .skipped
            .iter()
            .any(|entry| entry.starts_with("run/credentials/auth.json (")),
        "{report:?}"
    );

    // Nor from the directory it was written in, with the other run's
    // directory gone.
    std::fs::remove_dir_all(cleaned_from.join("run")).expect("drop the second run directory");
    std::env::set_current_dir(&written_from).expect("work from the first run's root");
    let report = crate::cleanup_ephemeral_credential_materializations(
        &store,
        &written_from.join("run"),
        false,
    )
    .expect("the cleanup runs");
    assert!(
        report.removed.is_empty() && report.missing.is_empty(),
        "{report:?}"
    );
    assert!(
        report
            .skipped
            .iter()
            .any(|entry| entry.starts_with("run/credentials/auth.json (")),
        "{report:?}"
    );

    assert_eq!(
        std::fs::read_to_string(written_from.join(&target))
            .ok()
            .as_deref(),
        Some(r#"{"token":"secret-token"}"#),
        "the recorded file was removed"
    );
    let rows = store
        .list_state(crate::CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .expect("list managed credential metadata");
    assert_eq!(rows.len(), 1);
    assert_ne!(
        serde_json::from_str::<serde_json::Value>(&rows[0].value_json).expect("metadata JSON")
            ["cleanup_status"],
        serde_json::json!("cleaned"),
        "the record was closed"
    );
}

/// A daemon whose working directory has been deleted still cleans the
/// absolute run directories it is given. Nothing about them needs the working
/// directory.
#[test]
fn an_absolute_run_directory_is_cleaned_from_a_deleted_working_directory() {
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-cleanup-deleted-cwd-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth json metadata");

    let root = tempfile::tempdir().expect("temp root");
    let run_dir = std::fs::canonicalize(root.path())
        .expect("canonical root")
        .join("run");
    let target = run_dir.join("credentials/auth.json");
    seed_legacy_auth_file(&store, &legacy_auth_file_profile(&target));

    let gone = tempfile::tempdir().expect("a directory to work from");
    let _guard = WorkingDirectory::enter(gone.path());
    std::fs::remove_dir_all(gone.path()).expect("delete the working directory");
    assert!(
        std::env::current_dir().is_err(),
        "premise: the working directory is gone"
    );

    let report = crate::cleanup_ephemeral_credential_materializations(&store, &run_dir, false)
        .expect("an absolute run directory needs no working directory");
    assert_eq!(
        report.removed,
        vec![target.to_string_lossy().to_string()],
        "{report:?}"
    );
    assert!(!target.exists(), "the credential file is still there");
}

/// `doctor` refuses a run directory reached through a link and inspects none
/// of its targets: their recorded paths go through that same link.
#[test]
fn doctor_reports_a_refused_run_directory_and_reads_nothing_behind_it() {
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-doctor-alias-run-dir-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth json metadata");

    let root = tempfile::tempdir().expect("temp root");
    let real_run_dir = root.path().join("job");
    std::fs::create_dir_all(&real_run_dir).expect("real run dir");
    let alias = root.path().join("current");
    std::os::unix::fs::symlink(&real_run_dir, &alias).expect("run dir alias");
    let profile = crate::CredentialProfile {
        provider: None,
        description: None,
        entries: [("auth_json".to_string(), "CODEX_AUTH_JSON".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::AllowedConsumers::default(),
        materializers: vec![crate::CredentialMaterializer {
            kind: "config_overlay".to_string(),
            source: "auth_json".to_string(),
            target: "{credentials_dir}/auth.json".to_string(),
            chmod: None,
            template: None,
        }],
    };
    let secret_values = [(
        "CODEX_AUTH_JSON".to_string(),
        r#"{"token":"secret-token"}"#.to_string(),
    )]
    .into_iter()
    .collect();
    crate::apply_credential_materialization(
        "codex_shared",
        &profile,
        "codex_cli",
        &store,
        &secret_values,
        &crate::CredentialApplyOptions {
            allow_existing: false,
            run_dir: Some(real_run_dir.clone()),
        },
    )
    .expect("materialize into the real run directory");

    // Only a doctor that read the file can report its mode.
    let auth_json = std::fs::canonicalize(&real_run_dir)
        .expect("canonical run dir")
        .join("credentials/auth.json");
    std::fs::set_permissions(&auth_json, std::fs::Permissions::from_mode(0o644))
        .expect("widen the mode");

    let through_alias = crate::doctor_credential_profile(
        "codex_shared",
        &profile,
        "codex_cli",
        &store,
        Some(&alias),
    )
    .expect("doctor report");
    for issue in &through_alias.issues {
        assert!(
            !matches!(
                issue.code.as_str(),
                "target_permissions_too_broad"
                    | "plaintext_config_secret"
                    | "managed_target_hash_mismatch"
                    | "managed_target_untrusted_entry"
                    | "existing_target"
                    | "managed_target_unreadable"
                    | "target_config_unreadable"
            ),
            "issue '{}' needed the file behind the link: {:#?}",
            issue.code,
            through_alias.issues
        );
    }
    assert!(
        through_alias
            .issues
            .iter()
            .any(|issue| issue.code == "run_dir_link_unverifiable"),
        "{:#?}",
        through_alias.issues
    );

    // The same profile read through the real directory does report the mode,
    // so the check above is not passing for lack of something to report.
    let through_real = crate::doctor_credential_profile(
        "codex_shared",
        &profile,
        "codex_cli",
        &store,
        Some(&real_run_dir),
    )
    .expect("doctor report");
    assert!(
        through_real
            .issues
            .iter()
            .any(|issue| issue.code == "target_permissions_too_broad"),
        "{:#?}",
        through_real.issues
    );
}

/// `doctor` opens the run directory before it plans anything. Planning a file
/// target looks at what is there, so through a refused link the report is the
/// refusal alone, whatever sits behind the link.
#[test]
fn doctor_plans_nothing_behind_a_refused_run_directory() {
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-doctor-alias-plan-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth json metadata");

    let root = tempfile::tempdir().expect("temp root");
    let real_run_dir = root.path().join("job");
    // A directory holding the target's name: a plan that looks behind the
    // link has something to report about it.
    std::fs::create_dir_all(real_run_dir.join("credentials/auth.json"))
        .expect("a directory holding the target's name");
    let alias = root.path().join("current");
    std::os::unix::fs::symlink(&real_run_dir, &alias).expect("run dir alias");
    let alias_text = alias.display().to_string();
    let profile = crate::CredentialProfile {
        provider: None,
        description: None,
        entries: [("auth_json".to_string(), "CODEX_AUTH_JSON".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::AllowedConsumers::default(),
        materializers: vec![crate::CredentialMaterializer {
            kind: "config_overlay".to_string(),
            source: "auth_json".to_string(),
            target: "{credentials_dir}/auth.json".to_string(),
            chmod: None,
            template: None,
        }],
    };

    let report = crate::doctor_credential_profile(
        "codex_shared",
        &profile,
        "codex_cli",
        &store,
        Some(&alias),
    )
    .expect("doctor report");

    assert_eq!(report.issues.len(), 1, "{:#?}", report.issues);
    let [issue] = &report.issues[..] else {
        unreachable!("one issue was just asserted")
    };
    assert_eq!(issue.code, "run_dir_link_unverifiable");
    assert_eq!(issue.severity, "high");
    assert_eq!(issue.target.as_deref(), Some(alias_text.as_str()));
    assert_eq!(report.summary.issue_count, 1);

    // Planned against the real directory, the same profile does look, and
    // reports the directory that holds the target's name.
    let through_real = crate::doctor_credential_profile(
        "codex_shared",
        &profile,
        "codex_cli",
        &store,
        Some(&real_run_dir),
    )
    .expect("doctor report");
    assert!(
        through_real
            .issues
            .iter()
            .any(|issue| issue.code == "invalid_materializer_target"),
        "{:#?}",
        through_real.issues
    );
}
