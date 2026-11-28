use super::*;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileTypeExt;
use std::time::Duration;

const AUTH_JSON: &str = r#"{"token":"secret-token"}"#;

fn cleanup_codex_shared(
    store: &memcore::MemoryStore,
) -> Result<crate::CredentialCleanupReport, String> {
    crate::cleanup_managed_credential_materializations(
        store,
        &crate::CredentialCleanupOptions {
            run_dir: None,
            run_generation: None,
            profile: Some("codex_shared".to_string()),
            consumer: Some("codex_cli".to_string()),
            dry_run: false,
            mark_only: false,
        },
    )
}

fn cleanup_statuses(store: &memcore::MemoryStore) -> Vec<serde_json::Value> {
    store
        .list_state(crate::CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .expect("list managed credential metadata")
        .into_iter()
        .map(|row| {
            serde_json::from_str::<serde_json::Value>(&row.value_json).expect("metadata JSON")
                ["cleanup_status"]
                .clone()
        })
        .collect()
}

// Resolving a target symlink would read, and then report cleaned, secret
// bytes that live somewhere other than the managed path.
#[test]
fn credential_manual_cleanup_reports_a_target_symlink_instead_of_resolving_it() {
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-cleanup-target-symlink-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth json metadata");

    let host = tempfile::tempdir().expect("host dir");
    let out_dir = tempfile::tempdir().expect("temp output dir");
    let relocated = out_dir.path().join("relocated-auth.json");
    let dangling = out_dir.path().join("dangling-auth.json");
    for target in [&relocated, &dangling] {
        seed_legacy_auth_file(&store, &legacy_auth_file_profile(target));
    }
    // The managed bytes moved away and left a link behind; the other target
    // now points at nothing.
    let host_auth = host.path().join("auth.json");
    std::fs::rename(&relocated, &host_auth).expect("move managed bytes out");
    std::os::unix::fs::symlink(&host_auth, &relocated).expect("plant relocated link");
    std::fs::remove_file(&dangling).expect("drop managed bytes");
    std::os::unix::fs::symlink(out_dir.path().join("nowhere.json"), &dangling)
        .expect("plant dangling link");

    let report = cleanup_codex_shared(&store).expect("manual cleanup");

    assert!(
        report.removed.is_empty() && report.missing.is_empty() && report.marked.is_empty(),
        "{report:?}"
    );
    for target in [&relocated, &dangling] {
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.contains(target.to_string_lossy().as_ref())
                    && error.contains("it is a symlink")),
            "{} not reported as a symlink: {report:?}",
            target.display()
        );
        assert!(std::fs::symlink_metadata(target)
            .expect("link kept")
            .file_type()
            .is_symlink());
    }
    assert_eq!(
        std::fs::read_to_string(&host_auth).ok().as_deref(),
        Some(AUTH_JSON),
        "the relocated secret bytes must not be disturbed by cleanup"
    );
    assert_eq!(
        cleanup_statuses(&store),
        vec![serde_json::Value::Null, serde_json::Value::Null]
    );
}

// A path-based hash read blocks forever once the entry holding the target
// name is a FIFO.
#[test]
fn credential_manual_cleanup_refuses_a_fifo_instead_of_blocking_on_it() {
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-cleanup-fifo-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let out_dir = tempfile::tempdir().expect("temp output dir");
    let auth_path = out_dir.path().join("auth.json");
    {
        let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
        store
            .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
            .expect("insert auth json metadata");
        seed_legacy_auth_file(&store, &legacy_auth_file_profile(&auth_path));
    }
    std::fs::remove_file(&auth_path).expect("drop managed bytes");
    let fifo = CString::new(auth_path.as_os_str().as_bytes()).expect("path has no NUL");
    // SAFETY: fifo is a valid terminated path and mkfifo consumes the mode.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);

    // The blocked thread would never return; the channel keeps that outcome
    // visible as a failure instead of a hung test binary.
    let (sent, seen) = std::sync::mpsc::channel();
    let worker_db_path = db_path.clone();
    std::thread::spawn(move || {
        let verdict = memcore::MemoryStore::open(worker_db_path.to_str().unwrap())
            .map_err(|error| error.to_string())
            .and_then(|store| cleanup_codex_shared(&store));
        let _ = sent.send(verdict);
    });
    let report = seen
        .recv_timeout(Duration::from_secs(30))
        .expect("manual cleanup blocked on a FIFO target")
        .expect("manual cleanup");

    assert!(
        report
            .errors
            .iter()
            .any(|error| error.contains("not a regular file")),
        "{report:?}"
    );
    assert!(
        report.removed.is_empty() && report.missing.is_empty(),
        "{report:?}"
    );
    let kind = std::fs::symlink_metadata(&auth_path)
        .expect("fifo kept")
        .file_type();
    assert!(kind.is_fifo(), "{kind:?}");
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("reopen store");
    assert_eq!(cleanup_statuses(&store), vec![serde_json::Value::Null]);
}
