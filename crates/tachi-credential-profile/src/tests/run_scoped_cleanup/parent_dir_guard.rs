use super::*;

#[test]
fn credential_run_scoped_cleanup_rejects_parent_dir_targets() {
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-cleanup-parent-dir-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth json metadata");

    let parent = tempfile::tempdir().expect("temp parent dir");
    let run_dir = parent.path().join("run");
    std::fs::create_dir_all(&run_dir).expect("create run dir");
    let outside_path = parent.path().join("outside-auth.json");
    let profile = crate::CredentialProfile {
        provider: None,
        description: None,
        entries: [("auth_json".to_string(), "CODEX_AUTH_JSON".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::AllowedConsumers::default(),
        materializers: vec![crate::CredentialMaterializer {
            kind: "file_copy".to_string(),
            source: "auth_json".to_string(),
            target: run_dir
                .join("../outside-auth.json")
                .to_string_lossy()
                .to_string(),
            chmod: None,
            template: None,
        }],
    };
    // Reproduce metadata written by the retired implementation. New apply
    // rejects this target; cleanup must remain safe for old database rows.
    seed_legacy_auth_file(&store, &profile);
    assert!(outside_path.exists());

    let cleanup = crate::cleanup_ephemeral_credential_materializations(&store, &run_dir, false)
        .expect("run-scoped cleanup");

    assert!(
        cleanup.removed.is_empty(),
        "parent-dir target must not be removed as run-scoped cleanup: {cleanup:?}"
    );
    assert!(
        cleanup
            .skipped
            .iter()
            .any(|target| target.ends_with("../outside-auth.json")),
        "{cleanup:?}"
    );
    assert!(
        outside_path.exists(),
        "cleanup must not delete files outside the run dir"
    );
}

#[test]
fn cleanup_and_doctor_reject_parent_dir_in_run_scope() {
    let parent = tempfile::tempdir().unwrap();
    let store =
        memcore::MemoryStore::open(parent.path().join("vault.sqlite").to_str().unwrap()).unwrap();
    let run = parent.path().join("run");
    let target = run.join("auth.json");
    let profile = legacy_auth_file_profile(&target);
    seed_legacy_auth_file(&store, &profile);
    let unsafe_run = run.join("../run");

    for mark_only in [false, true] {
        let error = crate::cleanup_managed_credential_materializations(
            &store,
            &crate::CredentialCleanupOptions {
                run_dir: Some(unsafe_run.clone()),
                mark_only,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(error.contains("run_dir must not contain '..'"), "{error}");
    }
    assert!(target.is_file());
    let row = &store
        .list_state(crate::CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .unwrap()[0];
    assert!(
        serde_json::from_str::<serde_json::Value>(&row.value_json).unwrap()["cleanup_status"]
            .is_null()
    );

    let error = crate::doctor_credential_profile(
        "codex_shared",
        &profile,
        "codex_cli",
        &store,
        Some(&unsafe_run),
    )
    .unwrap_err();
    assert!(error.contains("run_dir must not contain '..'"), "{error}");
}

#[test]
fn run_scope_skips_relative_legacy_target_even_with_mark_only() {
    let run = tempfile::tempdir().unwrap();
    let store =
        memcore::MemoryStore::open(run.path().join("vault.sqlite").to_str().unwrap()).unwrap();
    let target = std::path::Path::new("credentials/auth.json");
    let profile = legacy_auth_file_profile(target);
    seed_legacy_row(
        &store,
        "codex_shared",
        "codex_cli",
        &profile,
        "legacy fixture",
    );
    let report = crate::cleanup_managed_credential_materializations(
        &store,
        &crate::CredentialCleanupOptions {
            run_dir: Some(run.path().to_path_buf()),
            mark_only: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(report.skipped.len(), 1, "{report:?}");
    assert!(
        report.skipped[0].contains("legacy target is relative"),
        "{report:?}"
    );
    let row = &store
        .list_state(crate::CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .unwrap()[0];
    assert!(
        serde_json::from_str::<serde_json::Value>(&row.value_json).unwrap()["cleanup_status"]
            .is_null()
    );
}
