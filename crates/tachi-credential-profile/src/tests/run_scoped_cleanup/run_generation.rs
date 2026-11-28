use super::*;
use crate::{
    apply_credential_materialization, cleanup_ephemeral_credential_materializations_for_run,
    cleanup_managed_credential_materializations, CredentialApplyOptions, CredentialCleanupOptions,
    CREDENTIAL_MATERIALIZATION_NAMESPACE,
};
use memcore::MemoryStore;
use serde_json::Value;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

const TARGET: &str = "credentials/auth.json";

struct Fixture {
    root: tempfile::TempDir,
    store: MemoryStore,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(root.path().join("vault.sqlite").to_str().unwrap()).unwrap();
        store
            .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
            .unwrap();
        Self { root, store }
    }

    fn create_run(&self, relative: &str) -> PathBuf {
        let run = self.root.path().join(relative);
        fs::create_dir_all(&run).unwrap();
        run
    }

    fn apply(
        &self,
        run: &Path,
        profile: &str,
        generation: &str,
        replace: bool,
    ) -> Result<(), String> {
        self.apply_using(&self.store, run, profile, generation, replace)
    }

    fn apply_using(
        &self,
        store: &MemoryStore,
        run: &Path,
        name: &str,
        generation: &str,
        replace: bool,
    ) -> Result<(), String> {
        let mut profile = legacy_auth_file_profile(Path::new("{credentials_dir}/auth.json"));
        profile.materializers[0].kind = "config_overlay".to_owned();
        let values = [("CODEX_AUTH_JSON".to_owned(), format!("fixture-{name}"))].into();
        apply_credential_materialization(
            name,
            &profile,
            "codex_cli",
            store,
            &values,
            &CredentialApplyOptions {
                run_dir: Some(run.to_path_buf()),
                run_generation: Some(generation.to_owned()),
                allow_existing: replace,
            },
        )
        .map(|_| ())
    }

    fn cleanup(&self, run: &Path, generation: &str) -> crate::CredentialCleanupReport {
        cleanup_ephemeral_credential_materializations_for_run(
            &self.store,
            run,
            Some(generation),
            false,
        )
        .unwrap()
    }

    fn rows(&self) -> Vec<Value> {
        self.store
            .list_state(CREDENTIAL_MATERIALIZATION_NAMESPACE)
            .unwrap()
            .into_iter()
            .map(|row| serde_json::from_str(&row.value_json).unwrap())
            .collect()
    }
}

#[test]
fn bound_overlay_survives_tachi_home_alias() {
    let fixture = Fixture::new();
    let real_run = fixture.create_run("home-real/dispatch_runs/run-1");
    let alias_home = fixture.root.path().join("home-alias");
    symlink(fixture.root.path().join("home-real"), &alias_home).unwrap();
    let run = alias_home.join("dispatch_runs/run-1");
    fixture.apply(&run, "P", "run-1", false).unwrap();

    let rows = fixture.rows();
    assert_eq!(rows.len(), 1);
    let canonical = real_run.canonicalize().unwrap();
    assert_eq!(rows[0]["version"], 2);
    assert_eq!(rows[0]["run_generation"], "run-1");
    assert_eq!(rows[0]["run_root_canonical"], canonical.to_str().unwrap());
    assert_eq!(rows[0]["target_rel"], TARGET);

    let report = fixture.cleanup(&run, "run-1");
    assert_eq!(
        report.removed,
        vec![canonical.join(TARGET).to_str().unwrap()]
    );
    assert!(report.errors.is_empty(), "{report:?}");
    assert!(!real_run.join(TARGET).exists());
    assert_eq!(fixture.rows()[0]["cleanup_status"], "cleaned");
}

#[test]
fn bound_overlay_cleanup_preserves_both_targets_after_alias_retarget() {
    let fixture = Fixture::new();
    let dir_a = fixture.create_run("run-a");
    let dir_b = fixture.create_run("run-b");
    let alias = fixture.root.path().join("run-alias");
    symlink(&dir_a, &alias).unwrap();
    fixture.apply(&alias, "P", "run-1", false).unwrap();

    fs::remove_file(&alias).unwrap();
    symlink(&dir_b, &alias).unwrap();
    fs::create_dir_all(dir_b.join("credentials")).unwrap();
    fs::write(dir_b.join(TARGET), "unmanaged").unwrap();

    let report = fixture.cleanup(&alias, "run-1");
    assert!(!report.errors.is_empty(), "{report:?}");
    assert_eq!(fs::read_to_string(dir_a.join(TARGET)).unwrap(), "fixture-P");
    assert_eq!(fs::read_to_string(dir_b.join(TARGET)).unwrap(), "unmanaged");
    assert!(fixture.rows()[0]["cleanup_status"].is_null());
}

#[test]
fn cleanup_matches_renamed_directory_by_identity_and_preserves_reused_path() {
    let fixture = Fixture::new();
    let run = fixture.create_run("run");
    fixture.apply(&run, "P", "run-1", false).unwrap();

    let renamed = fixture.root.path().join("renamed");
    fs::rename(&run, &renamed).unwrap();
    fs::create_dir(&run).unwrap();
    fixture.apply(&run, "P", "run-1", false).unwrap();
    assert_eq!(
        fixture.rows().len(),
        2,
        "different root inodes need distinct keys"
    );

    let report = fixture.cleanup(&renamed, "run-1");
    assert_eq!(report.removed.len(), 1, "{report:?}");
    assert!(!renamed.join(TARGET).exists());
    assert!(run.join(TARGET).is_file());
}

#[test]
fn generation_mismatch_preserves_overlay_in_same_directory() {
    let fixture = Fixture::new();
    let run = fixture.create_run("run");
    fixture.apply(&run, "P", "run-1", false).unwrap();

    let report = fixture.cleanup(&run, "run-2");
    assert_eq!(report.skipped.len(), 1, "{report:?}");
    assert!(run.join(TARGET).exists());
    assert!(fixture.rows()[0]["cleanup_status"].is_null());

    fixture.apply(&run, "P", "run-2", true).unwrap();
    let rows = fixture.rows();
    assert_eq!(rows.len(), 2, "different generations need distinct keys");
    assert!(rows
        .iter()
        .any(|row| row["run_generation"] == "run-1" && row["cleanup_status"] == "superseded"));
    assert!(rows
        .iter()
        .any(|row| row["run_generation"] == "run-2" && row["cleanup_status"].is_null()));
    assert!(fixture.cleanup(&run, "run-1").removed.is_empty());
    assert!(run.join(TARGET).exists());
}

#[test]
fn dispatch_cleanup_preserves_manual_v2_overlay_in_same_directory() {
    let fixture = Fixture::new();
    let run = fixture.create_run("run");
    let mut profile = legacy_auth_file_profile(Path::new("{credentials_dir}/manual.json"));
    profile.materializers[0].kind = "config_overlay".into();
    apply_credential_materialization(
        "manual",
        &profile,
        "codex_cli",
        &fixture.store,
        &[("CODEX_AUTH_JSON".into(), "manual contents".into())].into(),
        &CredentialApplyOptions {
            run_dir: Some(run.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    let manual = fixture.rows().pop().unwrap();
    assert_eq!(manual["version"], 2);
    assert!(manual["run_generation"].is_null());
    assert!(manual["cleanup_status"].is_null());
    fixture.apply(&run, "P", "dispatch-1", false).unwrap();

    let report = fixture.cleanup(&run, "dispatch-1");
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(report.removed.len(), 1, "{report:?}");
    assert_eq!(report.skipped, vec![manual["target"].as_str().unwrap()]);
    assert!(!run.join(TARGET).exists());
    assert_eq!(
        fs::read_to_string(run.join("credentials/manual.json")).unwrap(),
        "manual contents"
    );
    let rows = fixture.rows();
    assert!(rows.contains(&manual));
    assert!(rows
        .iter()
        .any(|row| row["run_generation"] == "dispatch-1" && row["cleanup_status"] == "cleaned"));
}

#[test]
fn colliding_profile_replacement_supersedes_open_owner() {
    let fixture = Fixture::new();
    let run = fixture.create_run("run");
    fixture.apply(&run, "P", "run-1", false).unwrap();

    // The metadata itself must block an unapproved collision, even after the
    // physical target is removed outside Tachi.
    fs::remove_file(run.join(TARGET)).unwrap();
    let error = fixture.apply(&run, "Q", "run-2", false).unwrap_err();
    assert!(
        error.contains("already exists") && error.contains("allow_existing"),
        "{error}"
    );
    assert!(!run.join(TARGET).exists());
    fs::write(run.join(TARGET), "fixture-P").unwrap();
    fixture.apply(&run, "Q", "run-2", true).unwrap();

    let rows = fixture.rows();
    let old = rows.iter().find(|row| row["profile"] == "P").unwrap();
    assert_eq!(old["cleanup_status"], "superseded");
    assert!(old["cleaned_at"].is_string() && old["expires_at"].is_string());

    let report = cleanup_managed_credential_materializations(
        &fixture.store,
        &CredentialCleanupOptions {
            profile: Some("P".to_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        report.removed.is_empty() && report.errors.is_empty(),
        "{report:?}"
    );
    assert_eq!(fs::read_to_string(run.join(TARGET)).unwrap(), "fixture-Q");
}

#[test]
fn absolute_run_scope_with_deleted_cwd() {
    const CHILD: &str = "TACHI_TEST_CREDENTIAL_DELETED_CWD";
    if std::env::var_os(CHILD).is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(
                module_path!().split_once("::").unwrap().1.to_owned()
                    + "::absolute_run_scope_with_deleted_cwd",
            )
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            result.status.success() && String::from_utf8_lossy(&result.stdout).contains("1 passed"),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }

    let fixture = Fixture::new();
    let run = fixture.create_run("run").canonicalize().unwrap();
    let cwd = fixture.create_run("cwd");
    fixture.apply(&run, "P", "run-1", false).unwrap();

    std::env::set_current_dir(&cwd).unwrap();
    fs::remove_dir(cwd).unwrap();
    assert!(
        std::env::current_dir().is_err(),
        "test requires a deleted cwd"
    );
    let report = fixture.cleanup(&run, "run-1");
    assert_eq!(report.removed.len(), 1, "{report:?}");
    assert!(!run.join(TARGET).exists());
}

#[test]
fn apply_and_cleanup_share_exclusion_across_sqlite_connections() {
    let fixture = Fixture::new();
    let run = fixture.create_run("run");
    let other =
        MemoryStore::open(fixture.root.path().join("vault.sqlite").to_str().unwrap()).unwrap();
    other
        .connection()
        .busy_timeout(std::time::Duration::ZERO)
        .unwrap();
    fixture.apply(&run, "P", "run-1", false).unwrap();

    fixture
        .store
        .with_state_transaction(|_| {
            let cleanup = cleanup_ephemeral_credential_materializations_for_run(
                &other,
                &run,
                Some("run-1"),
                false,
            );
            let cleanup_error = cleanup.unwrap_err();
            assert!(cleanup_error.contains("locked"), "{cleanup_error}");
            let apply_error = fixture
                .apply_using(&other, &run, "Q", "run-2", true)
                .unwrap_err();
            assert!(apply_error.contains("locked"), "{apply_error}");
            assert_eq!(fs::read_to_string(run.join(TARGET)).unwrap(), "fixture-P");
            Ok(())
        })
        .unwrap();

    assert_eq!(fixture.cleanup(&run, "run-1").removed.len(), 1);
}

#[test]
fn cleanup_refuses_reappeared_run_path_before_closing_rows() {
    let fixture = Fixture::new();
    let run = fixture.create_run("run").canonicalize().unwrap();
    fixture.apply(&run, "P", "run-1", false).unwrap();
    fs::remove_dir_all(&run).unwrap();
    symlink(fixture.root.path().join("missing"), &run).unwrap();

    let error = cleanup_managed_credential_materializations(
        &fixture.store,
        &CredentialCleanupOptions {
            run_dir: Some(run.clone()),
            run_generation: Some("run-1".into()),
            mark_only: true,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(
        error.contains("run_dir reappeared during cleanup"),
        "{error}"
    );
    assert!(fixture.rows()[0]["cleanup_status"].is_null());

    fs::remove_file(&run).unwrap();
    let report = cleanup_managed_credential_materializations(
        &fixture.store,
        &CredentialCleanupOptions {
            run_dir: Some(run.clone()),
            run_generation: Some("run-1".into()),
            mark_only: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(report.missing, vec![run.join(TARGET).to_str().unwrap()]);
    assert_eq!(fixture.rows()[0]["cleanup_status"], "cleaned");
}

#[test]
fn apply_unlinks_publication_if_recording_metadata_fails() {
    let fixture = Fixture::new();
    let run = fixture.create_run("run");
    fixture.apply(&run, "P", "run-1", false).unwrap();
    // Exhaust the SQLite page budget while recording the replacement intent.
    let conn = fixture.store.connection();
    let pages: u32 = conn
        .pragma_query_value(None, "page_count", |row| row.get(0))
        .unwrap();
    conn.pragma_update(None, "max_page_count", pages + 4)
        .unwrap();
    let profile = "Q".repeat(512 * 1024);

    let error = fixture.apply(&run, &profile, "run-2", true).unwrap_err();
    assert!(
        error.contains("record credential materialization metadata")
            && error.contains("database or disk is full"),
        "{error}"
    );
    assert_eq!(fs::read_to_string(run.join(TARGET)).unwrap(), "fixture-P");
    let rows = fixture.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["profile"], "P");
    assert!(
        rows[0]["cleanup_status"].is_null(),
        "supersession must roll back"
    );
}

#[test]
fn absent_run_path_does_not_close_a_renamed_directory() {
    let fixture = Fixture::new();
    let run = fixture.create_run("run");
    fixture.apply(&run, "P", "run-1", false).unwrap();
    let renamed = fixture.root.path().join("renamed");
    fs::rename(&run, &renamed).unwrap();

    let report = fixture.cleanup(&run, "run-1");
    assert!(!report.errors.is_empty(), "{report:?}");
    assert_eq!(
        fs::read_to_string(renamed.join(TARGET)).unwrap(),
        "fixture-P"
    );
    assert!(fixture.rows()[0]["cleanup_status"].is_null());

    let report = fixture.cleanup(&renamed, "run-1");
    assert_eq!(report.removed.len(), 1, "{report:?}");
    assert!(!renamed.join(TARGET).exists());
    assert_eq!(fixture.rows()[0]["cleanup_status"], "cleaned");
}

#[test]
fn operator_cleanup_does_not_close_a_renamed_directory() {
    let fixture = Fixture::new();
    let run = fixture.create_run("run");
    fixture.apply(&run, "P", "run-1", false).unwrap();
    let renamed = fixture.root.path().join("renamed");
    fs::rename(&run, &renamed).unwrap();

    let mut options = CredentialCleanupOptions {
        profile: Some("P".into()),
        ..Default::default()
    };
    let report = cleanup_managed_credential_materializations(&fixture.store, &options).unwrap();
    assert!(
        report
            .errors
            .iter()
            .any(|error| error.contains("recorded run_dir is absent")),
        "{report:?}"
    );
    assert!(fixture.rows()[0]["cleanup_status"].is_null());
    assert_eq!(
        fs::read_to_string(renamed.join(TARGET)).unwrap(),
        "fixture-P"
    );

    options.run_dir = Some(renamed.clone());
    let report = cleanup_managed_credential_materializations(&fixture.store, &options).unwrap();
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(report.removed.len(), 1, "{report:?}");
    assert!(!renamed.join(TARGET).exists());
    assert_eq!(fixture.rows()[0]["cleanup_status"], "cleaned");
}

#[test]
fn failed_second_replacement_restores_first_file_and_active_row() {
    use std::os::unix::fs::MetadataExt;

    let fixture = Fixture::new();
    let run = fixture.create_run("run");
    fixture.apply(&run, "P", "run-1", false).unwrap();
    let target = run.join(TARGET);
    let inode = fs::metadata(&target).unwrap().ino();
    let before = fixture.rows();

    let mut profile = legacy_auth_file_profile(Path::new("{credentials_dir}/auth.json"));
    profile.materializers[0].kind = "config_overlay".into();
    let mut second = profile.materializers[0].clone();
    second.target = "{credentials_dir}/second.json".into();
    profile.materializers.push(second);
    crate::apply::AFTER_OVERLAY_WRITE.with(|hook| {
        hook.set(Some(|root| {
            fs::write(root.join("credentials/second.json"), "foreign").unwrap();
        }));
    });
    let error = apply_credential_materialization(
        "Q",
        &profile,
        "codex_cli",
        &fixture.store,
        &[("CODEX_AUTH_JSON".into(), "replacement".into())].into(),
        &CredentialApplyOptions {
            allow_existing: true,
            run_dir: Some(run),
            run_generation: Some("run-2".into()),
        },
    )
    .unwrap_err();
    assert!(error.contains("already exists"), "{error}");
    assert_eq!(fs::read_to_string(&target).unwrap(), "fixture-P");
    assert_eq!(fs::metadata(&target).unwrap().ino(), inode);
    assert_eq!(fixture.rows(), before);
    let second = target.with_file_name("second.json");
    assert_eq!(fs::read_to_string(&second).unwrap(), "foreign");
    assert_eq!(fs::read_dir(target.parent().unwrap()).unwrap().count(), 2);
}

#[test]
fn nested_run_replacement_supersedes_owner_and_cleanup_checks_inode() {
    let fixture = Fixture::new();
    let run = fixture.create_run("run");
    fixture.apply(&run, "P", "old", false).unwrap();
    let nested = run.join("credentials");

    let mut profile = legacy_auth_file_profile(Path::new("{run_dir}/auth.json"));
    profile.materializers[0].kind = "config_overlay".into();
    apply_credential_materialization(
        "Q",
        &profile,
        "codex_cli",
        &fixture.store,
        &[("CODEX_AUTH_JSON".into(), "replacement".into())].into(),
        &CredentialApplyOptions {
            allow_existing: true,
            run_dir: Some(nested.clone()),
            run_generation: Some("new".into()),
        },
    )
    .unwrap();

    let row = fixture
        .store
        .list_state(CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .unwrap()
        .into_iter()
        .find(|row| row.value_json.contains("\"run_generation\":\"old\""))
        .unwrap();
    let mut old: Value = serde_json::from_str(&row.value_json).unwrap();
    assert_eq!(old["cleanup_status"], "superseded");
    let report = fixture.cleanup(&run, "old");
    assert!(
        report.errors.is_empty() && report.removed.is_empty(),
        "{report:?}"
    );

    // Even if an old active record survives, its inode does not grant
    // authority to unlink the newer file occupying its target path.
    old["cleanup_status"] = Value::Null;
    fixture
        .store
        .set_state(
            CREDENTIAL_MATERIALIZATION_NAMESPACE,
            &row.key,
            &old.to_string(),
        )
        .unwrap();
    let report = fixture.cleanup(&run, "old");
    assert!(
        !report.errors.is_empty() && report.removed.is_empty(),
        "{report:?}"
    );
    assert_eq!(fs::read_to_string(run.join(TARGET)).unwrap(), "replacement");
    assert_eq!(fixture.cleanup(&nested, "new").removed.len(), 1);
}

#[test]
fn nested_replacement_supersedes_pre_inode_binding_record_after_rename() {
    let fixture = Fixture::new();
    let run = fixture.create_run("run");
    fixture.apply(&run, "P", "old", false).unwrap();
    let mut metadata = fixture.rows().pop().unwrap();
    metadata.as_object_mut().unwrap().remove("target_dev");
    metadata.as_object_mut().unwrap().remove("target_ino");
    let row = fixture
        .store
        .list_state(CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .unwrap()
        .pop()
        .unwrap();
    fixture
        .store
        .set_state(
            CREDENTIAL_MATERIALIZATION_NAMESPACE,
            &row.key,
            &metadata.to_string(),
        )
        .unwrap();

    let renamed = fixture.root.path().join("renamed");
    fs::rename(&run, &renamed).unwrap();
    let run = renamed;
    let mut profile = legacy_auth_file_profile(Path::new("{run_dir}/auth.json"));
    profile.materializers[0].kind = "config_overlay".into();
    apply_credential_materialization(
        "Q",
        &profile,
        "codex_cli",
        &fixture.store,
        &[("CODEX_AUTH_JSON".into(), "replacement".into())].into(),
        &CredentialApplyOptions {
            allow_existing: true,
            run_dir: Some(run.join("credentials")),
            run_generation: Some("new".into()),
        },
    )
    .unwrap();
    let report = fixture.cleanup(&run, "old");
    assert!(
        report.errors.is_empty() && report.removed.is_empty(),
        "{report:?}"
    );
    assert_eq!(fs::read_to_string(run.join(TARGET)).unwrap(), "replacement");
    assert!(fixture
        .rows()
        .iter()
        .any(|row| row["run_generation"] == "old" && row["cleanup_status"] == "superseded"));
}

#[test]
fn hard_link_replacement_keeps_the_original_name_tracked() {
    let fixture = Fixture::new();
    let run = fixture.create_run("run");
    fixture.apply(&run, "P", "old", false).unwrap();
    let second = run.join("credentials/second.json");
    fs::hard_link(run.join(TARGET), &second).unwrap();
    let mut profile = legacy_auth_file_profile(Path::new("{credentials_dir}/second.json"));
    profile.materializers[0].kind = "config_overlay".into();
    apply_credential_materialization(
        "Q",
        &profile,
        "codex_cli",
        &fixture.store,
        &[("CODEX_AUTH_JSON".into(), "replacement".into())].into(),
        &CredentialApplyOptions {
            allow_existing: true,
            run_dir: Some(run.clone()),
            run_generation: Some("new".into()),
        },
    )
    .unwrap();
    assert_eq!(fixture.cleanup(&run, "old").removed.len(), 1);
    assert!(!run.join(TARGET).exists());
    assert_eq!(fs::read_to_string(&second).unwrap(), "replacement");
}

#[test]
fn committed_replacement_cleanup_discards_backup_for_a_distinct_hard_link() {
    let fixture = Fixture::new();
    let run = fixture.create_run("run");
    fixture.apply(&run, "P", "old", false).unwrap();
    let owner = fixture.rows().pop().unwrap();
    let second = run.join("credentials/second.json");
    let backup_path = run.join("credentials/.tachi-bak-interrupted");
    fs::hard_link(run.join(TARGET), &second).unwrap();
    fs::hard_link(&second, &backup_path).unwrap();
    let mut profile = legacy_auth_file_profile(Path::new("{credentials_dir}/second.json"));
    profile.materializers[0].kind = "config_overlay".into();
    apply_credential_materialization(
        "Q",
        &profile,
        "codex_cli",
        &fixture.store,
        &[("CODEX_AUTH_JSON".into(), "replacement".into())].into(),
        &CredentialApplyOptions {
            allow_existing: true,
            run_dir: Some(run.clone()),
            run_generation: Some("new".into()),
        },
    )
    .unwrap();
    // Reconstruct the journal row left between publication commit and backup removal.
    let mut backup = fixture
        .rows()
        .into_iter()
        .find(|row| row["profile"] == "Q")
        .unwrap();
    backup["target"] = backup_path.canonicalize().unwrap().to_str().unwrap().into();
    backup["target_rel"] = "credentials/.tachi-bak-interrupted".into();
    backup["restore_rel"] = "credentials/second.json".into();
    backup["target_dev"] = owner["target_dev"].clone();
    backup["target_ino"] = owner["target_ino"].clone();
    fixture
        .store
        .set_state(
            CREDENTIAL_MATERIALIZATION_NAMESPACE,
            "publication:interrupted:backup",
            &backup.to_string(),
        )
        .unwrap();

    let report = fixture.cleanup(&run, "new");
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(report.removed.len(), 2, "{report:?}");
    assert!(!second.exists());
    assert!(!backup_path.exists());
    assert!(fixture.rows().contains(&owner));
    assert_eq!(fs::read_to_string(run.join(TARGET)).unwrap(), "fixture-P");
}

#[test]
fn cleanup_retries_an_interrupted_inode_capture() {
    let fixture = Fixture::new();
    let run = fixture.create_run("run");
    fixture.apply(&run, "P", "old", false).unwrap();
    let row = fixture
        .store
        .list_state(CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .unwrap()
        .pop()
        .unwrap();
    let target = run.join(TARGET);
    let capture = target.with_file_name(crate::cleanup::cleanup_capture_name(&row.key));
    fs::rename(&target, &capture).unwrap();

    let error = fixture.apply(&run, "Q", "new", true).unwrap_err();
    assert!(error.contains("interrupted cleanup"), "{error}");
    assert!(!target.exists());
    assert_eq!(fs::read_to_string(&capture).unwrap(), "fixture-P");
    let report = fixture.cleanup(&run, "old");
    assert_eq!(report.removed.len(), 1, "{report:?}");
    assert!(!capture.exists());
    assert_eq!(fixture.rows()[0]["cleanup_status"], "cleaned");
}

#[test]
fn killed_publisher_leaves_a_committed_cleanup_record() {
    run_publisher_crash_child(false);
    let fixture = Fixture::new();
    let run = fixture.create_run("run");
    kill_publisher_after_write(
        &fixture,
        "killed_publisher_leaves_a_committed_cleanup_record",
    );
    assert_eq!(fs::read_to_string(run.join(TARGET)).unwrap(), "fixture-P");
    let report = fixture.cleanup(&run, "run-1");
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(report.removed.len(), 1, "{report:?}");
    assert!(!run.join(TARGET).exists());
}

#[test]
fn killed_replacement_publisher_restores_active_owner() {
    run_publisher_crash_child(true);
    assert_killed_replacement_recovery("killed_replacement_publisher_restores_active_owner", false);
}

#[test]
fn interrupted_replacement_recovery_closes_intent_on_retry() {
    run_publisher_crash_child(true);
    assert_killed_replacement_recovery(
        "interrupted_replacement_recovery_closes_intent_on_retry",
        true,
    );
}

fn assert_killed_replacement_recovery(test_name: &str, interrupted_cleanup: bool) {
    use std::os::unix::fs::MetadataExt;

    let fixture = Fixture::new();
    let run = fixture.create_run("run");
    fixture.apply(&run, "P", "run-1", false).unwrap();
    let target = run.join(TARGET);
    let original = fs::metadata(&target).unwrap();
    let owner = fixture.rows().pop().unwrap();

    kill_publisher_after_write(&fixture, test_name);
    assert_eq!(fs::read_to_string(&target).unwrap(), "replacement");
    if interrupted_cleanup {
        let backup = fixture
            .rows()
            .into_iter()
            .find(|row| row["restore_rel"] == TARGET)
            .unwrap();
        // The filesystem recorded the rename but SQLite still has its pre-cleanup rows.
        fs::rename(backup["target"].as_str().unwrap(), &target).unwrap();
    }

    let report = fixture.cleanup(&run, "run-2");
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(fs::read_to_string(&target).unwrap(), "fixture-P");
    let restored = fs::metadata(&target).unwrap();
    assert_eq!(
        (restored.dev(), restored.ino()),
        (original.dev(), original.ino())
    );
    assert!(fixture.rows().contains(&owner));
    assert!(fixture
        .rows()
        .iter()
        .filter(|row| row["run_generation"] == "run-2")
        .all(|row| row["cleanup_status"] == "cleaned"));
    assert_eq!(fs::read_dir(target.parent().unwrap()).unwrap().count(), 1);
}

const KILL_ROOT: &str = "TACHI_TEST_CREDENTIAL_KILL_ROOT";

fn run_publisher_crash_child(replace: bool) {
    if let Some(root) = std::env::var_os(KILL_ROOT) {
        let root = PathBuf::from(root);
        let store = MemoryStore::open(root.join("vault.sqlite").to_str().unwrap()).unwrap();
        let mut profile = legacy_auth_file_profile(Path::new("{credentials_dir}/auth.json"));
        profile.materializers[0].kind = "config_overlay".into();
        crate::apply::AFTER_OVERLAY_WRITE.with(|hook| {
            hook.set(Some(|_| {
                // SAFETY: signal only this dedicated test subprocess.
                unsafe { libc::raise(libc::SIGKILL) };
            }))
        });
        let (name, generation, content) = if replace {
            ("Q", "run-2", "replacement")
        } else {
            ("P", "run-1", "fixture-P")
        };
        let result = apply_credential_materialization(
            name,
            &profile,
            "codex_cli",
            &store,
            &[("CODEX_AUTH_JSON".into(), content.into())].into(),
            &CredentialApplyOptions {
                allow_existing: replace,
                run_dir: Some(root.join("run")),
                run_generation: Some(generation.into()),
            },
        );
        panic!("publisher unexpectedly returned: {result:?}");
    }
}

fn kill_publisher_after_write(fixture: &Fixture, test_name: &str) {
    use std::os::unix::process::ExitStatusExt;
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg(format!(
            "{}::{test_name}",
            module_path!().split_once("::").unwrap().1
        ))
        .env(KILL_ROOT, fixture.root.path())
        .output()
        .unwrap();
    assert_eq!(child.status.signal(), Some(libc::SIGKILL), "{child:?}");
}

#[test]
fn cleanup_does_not_restore_an_unrelated_capture_name() {
    let fixture = Fixture::new();
    let run = fixture.create_run("run");
    fixture.apply(&run, "P", "old", false).unwrap();
    let row = fixture
        .store
        .list_state(CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .unwrap()
        .pop()
        .unwrap();
    let target = run.join(TARGET);
    let capture = target.with_file_name(crate::cleanup::cleanup_capture_name(&row.key));
    // Retain the original inode so the foreign file cannot reuse its number.
    fs::rename(&target, target.with_file_name("original.json")).unwrap();
    fs::write(&capture, "foreign").unwrap();
    let report = fixture.cleanup(&run, "old");
    assert!(!report.errors.is_empty(), "{report:?}");
    assert!(!target.exists());
    assert_eq!(fs::read_to_string(&capture).unwrap(), "foreign");
    assert!(fixture.rows()[0]["cleanup_status"].is_null());
}

#[test]
fn killed_replacement_publisher_restores_legacy_v1_owner() {
    use std::os::unix::fs::MetadataExt;

    run_publisher_crash_child(true);
    let fixture = Fixture::new();
    let run = fixture.create_run("run");
    fixture.apply(&run, "P", "run-1", false).unwrap();
    let target = run.join(TARGET);
    let original = fs::metadata(&target).unwrap();

    // Downgrade P's record to legacy v1 (no target_dev/target_ino/run_generation).
    let row = fixture
        .store
        .list_state(CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .unwrap()
        .pop()
        .unwrap();
    let mut legacy: serde_json::Value = serde_json::from_str(&row.value_json).unwrap();
    legacy["version"] = serde_json::json!(1);
    for field in [
        "run_root_canonical",
        "run_root_dev",
        "run_root_ino",
        "target_rel",
        "target_dev",
        "target_ino",
        "run_generation",
    ] {
        legacy.as_object_mut().unwrap().remove(field);
    }
    fixture
        .store
        .set_state(
            CREDENTIAL_MATERIALIZATION_NAMESPACE,
            &row.key,
            &legacy.to_string(),
        )
        .unwrap();

    kill_publisher_after_write(
        &fixture,
        "killed_replacement_publisher_restores_legacy_v1_owner",
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "replacement");

    // Clean only Q by profile: must unlink Q's uncommitted file and restore P's v1 original.
    let report = cleanup_managed_credential_materializations(
        &fixture.store,
        &CredentialCleanupOptions {
            profile: Some("Q".into()),
            run_dir: Some(run.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(fs::read_to_string(&target).unwrap(), "fixture-P");
    let restored = fs::metadata(&target).unwrap();
    assert_eq!(
        (restored.dev(), restored.ino()),
        (original.dev(), original.ino())
    );
    assert!(fixture
        .rows()
        .iter()
        .any(|r| r["profile"] == "P" && r["cleanup_status"].is_null()));
}

#[test]
fn publish_refuses_subdirectory_moved_outside_run_dir_between_stage_and_write() {
    let fixture = Fixture::new();
    let run = fixture.create_run("run");
    crate::apply::BEFORE_OVERLAY_WRITE.with(|hook| {
        hook.set(Some(|root| {
            let escaped = root.parent().unwrap().join("escaped-creds");
            fs::rename(root.join("credentials"), escaped).unwrap();
        }))
    });
    let err = fixture.apply(&run, "P", "run-1", false).unwrap_err();
    assert!(
        err.contains("moved outside run_dir"),
        "expected moved-outside-run_dir error, got: {err}"
    );
    let escaped = run.parent().unwrap().join("escaped-creds");
    assert!(escaped.is_dir());
    for entry in fs::read_dir(&escaped).unwrap() {
        let path = entry.unwrap().path();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "",
            "no plaintext may be written into a moved subdirectory: {path:?}"
        );
    }
}
