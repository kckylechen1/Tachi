use super::*;

#[test]
fn credential_materialize_apply_records_managed_hash_metadata() {
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-materialize-managed-hash-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth json metadata");

    let out_dir = tempfile::tempdir().expect("temp output dir");
    let config_path = out_dir.path().join("credentials/config.json");
    let mut profile = legacy_auth_file_profile(&config_path);
    profile.materializers[0].kind = "config_overlay".to_string();
    profile.materializers[0].target = "{credentials_dir}/config.json".to_string();
    let values = [(
        "CODEX_AUTH_JSON".to_string(),
        r#"{"token":"secret-token"}"#.to_string(),
    )]
    .into_iter()
    .collect();
    let result = crate::apply_credential_materialization(
        "codex_shared",
        &profile,
        "codex_cli",
        &store,
        &values,
        &crate::CredentialApplyOptions {
            allow_existing: false,
            run_dir: Some(out_dir.path().to_path_buf()),
        },
    )
    .expect("apply temporary config materialization");
    assert_eq!(
        result.report.steps[0].target,
        std::fs::canonicalize(&config_path)
            .unwrap()
            .to_string_lossy()
            .to_string(),
    );

    let rows = store
        .list_state(crate::CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .expect("list managed credential metadata");
    assert_eq!(rows.len(), 1);
    assert!(rows[0].key.starts_with("managed:"));
    let metadata: serde_json::Value =
        serde_json::from_str(&rows[0].value_json).expect("metadata JSON");
    assert_eq!(metadata["profile"], serde_json::json!("codex_shared"));
    assert_eq!(metadata["consumer"], serde_json::json!("codex_cli"));
    assert_eq!(
        metadata["resolved_secret"],
        serde_json::json!("CODEX_AUTH_JSON")
    );
    assert_eq!(
        metadata["target"],
        serde_json::json!(std::fs::canonicalize(&config_path)
            .unwrap()
            .to_string_lossy()
            .to_string())
    );
    assert!(metadata["content_hash"]
        .as_str()
        .unwrap()
        .starts_with("stable-fnv1a:"));
    assert!(!rows[0].value_json.contains("secret-token"));

    let metadata_snapshot = || {
        store
            .list_state(crate::CREDENTIAL_MATERIALIZATION_NAMESPACE)
            .expect("managed metadata snapshot")
            .into_iter()
            .map(|row| (row.key, row.value_json, row.version, row.updated_at))
            .collect::<Vec<_>>()
    };
    let original_metadata = metadata_snapshot();
    let readonly_store = memcore::MemoryStore::open_read_only(db_path.to_str().unwrap())
        .expect("read-only doctor store");

    for edited in [false, true] {
        if edited {
            std::fs::write(&config_path, r#"{"token":"edited-outside-tachi"}"#)
                .expect("edit managed config outside Tachi");
        }
        let before_content = std::fs::read(&config_path).expect("config bytes before doctor");
        let before_modified = std::fs::metadata(&config_path).unwrap().modified().unwrap();
        #[cfg(unix)]
        let before_mode = std::fs::metadata(&config_path)
            .unwrap()
            .permissions()
            .mode();

        // Keep the original template target: doctor must resolve the same
        // run directory as apply to find this exact managed record.
        let doctor = crate::doctor_credential_profile(
            "codex_shared",
            &profile,
            "codex_cli",
            &readonly_store,
            Some(out_dir.path()),
        )
        .expect("doctor report from read-only store");
        let codes = doctor
            .issues
            .iter()
            .map(|issue| issue.code.as_str())
            .collect::<Vec<_>>();
        for unexpected in [
            "invalid_materializer_target",
            "existing_target",
            "managed_target_missing",
        ] {
            assert!(!codes.contains(&unexpected), "{codes:?}");
        }
        assert_eq!(
            codes.contains(&"managed_target_hash_mismatch"),
            edited,
            "{codes:?}"
        );
        if edited {
            let mismatch = doctor
                .issues
                .iter()
                .find(|issue| issue.code == "managed_target_hash_mismatch")
                .expect("edited managed file has drift");
            assert_eq!(
                mismatch.target.as_deref(),
                Some(result.report.steps[0].target.as_str())
            );
        }

        assert_eq!(
            std::fs::read(&config_path).unwrap(),
            before_content,
            "doctor must leave file content unchanged"
        );
        assert_eq!(
            std::fs::metadata(&config_path).unwrap().modified().unwrap(),
            before_modified,
            "doctor must not rewrite the file"
        );
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&config_path)
                .unwrap()
                .permissions()
                .mode(),
            before_mode,
            "doctor must not change file permissions"
        );
        assert_eq!(
            metadata_snapshot(),
            original_metadata,
            "doctor must not rewrite or mark managed metadata"
        );
    }
}
