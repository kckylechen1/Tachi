use super::*;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;

#[test]
fn credential_doctor_refuses_symlink_target_without_following_it() {
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-doctor-symlink-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("OPENAI_API_KEY", None))
        .expect("insert api key metadata");

    let run_dir = tempfile::tempdir().expect("temp run dir");
    let host_dir = tempfile::tempdir().expect("temp host dir");
    let outside_secret = host_dir.path().join("host-secret.json");
    std::fs::write(&outside_secret, r#"{"apiKey":"sk-outside-plaintext"}"#)
        .expect("write outside host file");
    std::fs::set_permissions(&outside_secret, std::fs::Permissions::from_mode(0o644))
        .expect("broad host permissions");

    let credentials_dir = run_dir.path().join("credentials");
    std::fs::create_dir_all(&credentials_dir).expect("create credentials dir");
    let target = credentials_dir.join("opencode.json");
    std::os::unix::fs::symlink(&outside_secret, &target).expect("plant target symlink");

    let profile = crate::CredentialProfile {
        provider: None,
        description: None,
        entries: [("api_key".to_string(), "OPENAI_API_KEY".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::AllowedConsumers::default(),
        materializers: vec![crate::CredentialMaterializer {
            kind: "config_overlay".to_string(),
            source: "api_key".to_string(),
            target: "{credentials_dir}/opencode.json".to_string(),
            chmod: Some("0600".to_string()),
            template: Some(serde_json::json!({"apiKey": "{{secret}}"})),
        }],
    };

    let report = crate::doctor_credential_profile(
        "opencode_shared",
        &profile,
        "opencode",
        &store,
        Some(run_dir.path()),
    )
    .expect("doctor report");
    let codes = report
        .issues
        .iter()
        .map(|issue| issue.code.as_str())
        .collect::<Vec<_>>();
    assert!(
        codes.contains(&"managed_target_untrusted_entry"),
        "{codes:?}"
    );
    assert!(
        !codes.contains(&"plaintext_config_secret"),
        "doctor must not read through the symlink: {codes:?}"
    );
    assert!(
        !codes.contains(&"target_permissions_too_broad"),
        "doctor must not stat the symlink target: {codes:?}"
    );
}

#[test]
fn credential_doctor_refuses_fifo_target_without_blocking() {
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-doctor-fifo-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth metadata");

    let out_dir = tempfile::tempdir().expect("temp output dir");
    let fifo = out_dir.path().join("auth.json");
    let fifo_c = CString::new(fifo.as_os_str().as_bytes()).expect("fifo CString");
    // SAFETY: fifo_c is NUL-terminated and mkfifo consumes the mode.
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);

    let profile = legacy_auth_file_profile(&fifo);
    let (sent, seen) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sent.send(crate::doctor_credential_profile(
            "codex_shared",
            &profile,
            "codex_cli",
            &store,
            None,
        ));
    });
    let report = seen
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("doctor must not block on a FIFO target")
        .expect("doctor report");
    let codes = report
        .issues
        .iter()
        .map(|issue| issue.code.as_str())
        .collect::<Vec<_>>();
    assert!(
        codes.contains(&"managed_target_untrusted_entry"),
        "{codes:?}"
    );
}
