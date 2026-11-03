use super::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

struct Fixture {
    directory: tempfile::TempDir,
    run_dir: PathBuf,
    store: memcore::MemoryStore,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("fixture");
        let run_dir = directory.path().join("run");
        std::fs::create_dir(&run_dir).expect("run dir");
        let store =
            memcore::MemoryStore::open(directory.path().join("vault.sqlite").to_str().unwrap())
                .expect("store");
        store
            .vault_upsert_entry(&test_vault_entry("KEY", None))
            .expect("metadata");
        Self {
            directory,
            run_dir,
            store,
        }
    }

    fn profile(target: &str) -> crate::CredentialProfile {
        crate::CredentialProfile {
            provider: None,
            description: None,
            entries: Default::default(),
            allowed_consumers: Default::default(),
            materializers: vec![crate::CredentialMaterializer {
                kind: "config_overlay".to_string(),
                source: "KEY".to_string(),
                target: target.to_string(),
                chmod: None,
                template: Some(serde_json::json!({"token": "{{secret}}"})),
            }],
        }
    }

    fn apply(
        &self,
        target: &str,
        run_dir: Option<&Path>,
        allow_existing: bool,
    ) -> Result<crate::CredentialApplyResult, String> {
        let values: HashMap<_, _> = [("KEY".to_string(), "synthetic-secret-value".to_string())]
            .into_iter()
            .collect();
        crate::apply_credential_materialization(
            "worker",
            &Self::profile(target),
            "worker",
            &self.store,
            &values,
            &crate::CredentialApplyOptions {
                run_dir: run_dir.map(Path::to_path_buf),
                allow_existing,
            },
        )
    }
}

#[test]
fn config_overlay_requires_run_directory_and_rejects_escape_paths() {
    let fixture = Fixture::new();
    let missing_grant = fixture
        .apply("config.json", None, false)
        .expect_err("run grant required");
    assert!(
        missing_grant.contains("caller-granted run_dir"),
        "{missing_grant}"
    );
    let outside = fixture.directory.path().join("outside.json");
    let sibling = fixture.directory.path().join("run-other/config.json");
    for (target, expected) in [
        (
            outside.to_string_lossy().to_string(),
            "inside the caller-granted run_dir",
        ),
        (
            sibling.to_string_lossy().to_string(),
            "inside the caller-granted run_dir",
        ),
        (
            "{run_dir}/../outside.json".to_string(),
            "must not contain '..'",
        ),
        ("{run_dir}".to_string(), "must name a file"),
    ] {
        let error = fixture
            .apply(&target, Some(&fixture.run_dir), true)
            .expect_err("unsafe target");
        assert!(error.contains(expected), "{target}: {error}");
        assert!(!error.contains("synthetic-secret-value"));
    }
    assert!(!outside.exists());
    assert!(!sibling.exists());
    assert_eq!(std::fs::read_dir(&fixture.run_dir).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn config_overlay_rejects_symlink_ancestors_and_leaf_targets() {
    let fixture = Fixture::new();
    let outside = fixture.directory.path().join("outside");
    std::fs::create_dir(&outside).expect("outside dir");
    let outside_file = outside.join("config.json");
    std::fs::write(&outside_file, "original").expect("original outside config");
    std::os::unix::fs::symlink(&outside, fixture.run_dir.join("linked-dir"))
        .expect("parent symlink");
    std::os::unix::fs::symlink(&outside_file, fixture.run_dir.join("linked.json"))
        .expect("leaf symlink");
    std::os::unix::fs::symlink(
        outside.join("missing.json"),
        fixture.run_dir.join("dangling.json"),
    )
    .expect("dangling symlink");
    for target in ["linked-dir/new.json", "linked.json", "dangling.json"] {
        let error = fixture
            .apply(target, Some(&fixture.run_dir), true)
            .expect_err("symlink target");
        assert!(error.contains("symlink"), "{target}: {error}");
    }
    assert_eq!(std::fs::read_to_string(&outside_file).unwrap(), "original");
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
    assert!(fixture
        .store
        .list_state(crate::CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .unwrap()
        .is_empty());
}

#[cfg(unix)]
#[test]
fn config_overlay_rechecks_paths_after_planning() {
    let fixture = Fixture::new();
    let parent = fixture.run_dir.join("credentials");
    std::fs::create_dir(&parent).expect("original parent");
    let profile = Fixture::profile("{credentials_dir}/config.json");
    let plan = crate::plan_credential_materialization_with_run_dir(
        "worker",
        &profile,
        "worker",
        &fixture.store,
        Some(&fixture.run_dir),
    )
    .expect("plan");
    assert_eq!(plan.steps[0].status, "ready");
    std::fs::remove_dir(&parent).expect("replace planned directory");
    std::os::unix::fs::symlink(fixture.directory.path(), &parent).expect("late parent symlink");
    let error = fixture
        .apply(
            "{credentials_dir}/config.json",
            Some(&fixture.run_dir),
            false,
        )
        .expect_err("planning is not a capability to bypass current filesystem checks");
    assert!(error.contains("symlink"), "{error}");
    assert!(!fixture.directory.path().join("config.json").exists());
}

#[cfg(unix)]
#[test]
fn config_overlay_replacement_does_not_modify_an_outside_hardlink() {
    let fixture = Fixture::new();
    let original = fixture.directory.path().join("outside.json");
    let target = fixture.run_dir.join("config.json");
    std::fs::write(&original, "original").expect("outside config");
    std::fs::hard_link(&original, &target).expect("existing hardlink");
    let error = fixture
        .apply("config.json", Some(&fixture.run_dir), false)
        .expect_err("existing target needs explicit replacement");
    assert!(error.contains("already exists"), "{error}");
    fixture
        .apply("config.json", Some(&fixture.run_dir), true)
        .expect("replace only run-dir entry");
    assert_eq!(std::fs::read_to_string(&original).unwrap(), "original");
    assert!(std::fs::read_to_string(&target)
        .unwrap()
        .contains("synthetic-secret-value"));
    assert_eq!(
        std::fs::read_dir(&fixture.run_dir).unwrap().count(),
        1,
        "no temporary backups"
    );
}

#[cfg(unix)]
#[test]
fn granted_run_directory_alias_materializes_and_cleans_the_canonical_target() {
    let fixture = Fixture::new();
    let alias = fixture.directory.path().join("run-alias");
    std::os::unix::fs::symlink(&fixture.run_dir, &alias).expect("granted run-dir alias");
    let result = fixture
        .apply("{credentials_dir}/config.json", Some(&alias), false)
        .expect("caller may grant an existing directory alias");
    let target = std::fs::canonicalize(&fixture.run_dir)
        .unwrap()
        .join("credentials/config.json");
    assert_eq!(Path::new(&result.report.steps[0].target), target.as_path());
    assert!(target.exists());
    let cleanup =
        crate::cleanup_ephemeral_credential_materializations(&fixture.store, &alias, false)
            .expect("cleanup through the same granted run-dir alias");
    assert_eq!(cleanup.removed, vec![target.to_string_lossy().to_string()]);
    assert!(!target.exists());
}
