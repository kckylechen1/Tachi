use super::*;

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let flow = tmp.path().join("flow_windows_legacy");
    std::fs::create_dir(&flow).unwrap();
    (tmp, flow)
}

#[test]
fn windows_unsealed_legacy_roundtrip_keeps_atomic_write_and_append() {
    let (_tmp, flow) = fixture();
    let status = flow.join("status.json");
    super::super::write_legacy(&status, br#"{"state":"legacy"}"#, false)
        .unwrap()
        .unwrap();
    assert!(super::super::legacy_read_allowed(&flow));
    assert_eq!(
        super::super::read_legacy_json(&status)
            .unwrap()
            .unwrap()
            .unwrap()["state"],
        "legacy"
    );
    super::super::write_legacy(&status, br#"{"state":"updated"}"#, false)
        .unwrap()
        .unwrap();
    assert_eq!(read_json(&status).unwrap().unwrap()["state"], "updated");
    super::super::write_legacy(&flow.join("instruction.md"), b"legacy instruction", false)
        .unwrap()
        .unwrap();
    super::super::write_legacy(&flow.join("events.jsonl"), b"{\"event\":1}\n", true)
        .unwrap()
        .unwrap();
    assert_eq!(
        std::fs::read(flow.join("events.jsonl")).unwrap(),
        b"{\"event\":1}\n"
    );
    assert_eq!(
        std::fs::read(flow.join("instruction.md")).unwrap(),
        b"legacy instruction"
    );
}

#[test]
fn windows_verification_only_is_the_fixed_missing_status_exception() {
    let (_tmp, flow) = fixture();
    super::super::legacy_verification_preflight(&flow).unwrap();
    write(
        &flow,
        &flow.join("verification.json"),
        br#"{"overall":"failed","source":"caller_asserted"}"#,
        false,
    )
    .unwrap();
    assert_eq!(
        super::super::read_legacy_verification(&flow)
            .unwrap()
            .unwrap()["overall"],
        "failed"
    );
    std::fs::write(flow.join("private.json"), br#"{"private":"sentinel"}"#).unwrap();
    assert!(read_json(&flow.join("private.json")).is_err());
    assert!(!super::super::legacy_read_allowed(&flow));
}

#[test]
fn windows_ownership_and_malformed_markers_never_fall_back_to_legacy() {
    let (_tmp, flow) = fixture();
    let private = flow.join("private.json");
    std::fs::write(&private, br#"{"private":"sentinel"}"#).unwrap();
    for marker in [
        "",
        "null",
        "[1]",
        "{",
        r#"{"ownership":null}"#,
        r#"{"ownership":{},"ownership":{}}"#,
        r#"{"ownership":{"unknown":true}}"#,
        r#"{"ownership":{"version":1,"claim_id":"c","canonical_project":"alpha","flow_id":"flow_windows_legacy"}}"#,
    ] {
        std::fs::write(flow.join("status.json"), marker).unwrap();
        assert!(!super::super::legacy_read_allowed(&flow), "{marker}");
        assert!(
            super::super::read_legacy_json(&private).unwrap().is_err(),
            "{marker}"
        );
        assert!(
            super::super::read_legacy_verification(&flow).is_err(),
            "{marker}"
        );
        assert!(
            super::super::write_legacy(&private, b"overwrite", false)
                .unwrap()
                .is_err(),
            "{marker}"
        );
        assert_eq!(
            std::fs::read_to_string(flow.join("status.json")).unwrap(),
            marker
        );
        assert!(!flow.join(".status-mutation.lock").exists());
    }
    assert_eq!(
        std::fs::read(&private).unwrap(),
        br#"{"private":"sentinel"}"#
    );
}

#[test]
fn windows_directory_and_status_sharing_have_distinct_boundaries() {
    let (tmp, flow) = fixture();
    std::fs::write(flow.join("status.json"), br#"{"state":"legacy"}"#).unwrap();
    let directory = Directory::open(&flow, false).unwrap().unwrap();
    assert!(std::fs::rename(&flow, tmp.path().join("moved")).is_err());
    // Pinning the directory does not make all child contents immutable.
    std::fs::write(flow.join("child.txt"), "child write remains allowed").unwrap();
    let status = directory.status().unwrap().unwrap();
    assert!(std::fs::write(flow.join("status.json"), "changed").is_err());
    assert!(std::fs::rename(flow.join("status.json"), flow.join("old.json")).is_err());
    assert_eq!(decode(status).unwrap()["state"], "legacy");
    // The same-handle read released its child sharing guard.
    std::fs::write(flow.join("status.json"), "{}").unwrap();
}

#[test]
fn windows_reparse_files_and_flow_directories_are_refused() {
    let (tmp, flow) = fixture();
    let target = tmp.path().join("target.json");
    std::fs::write(&target, "{}").unwrap();
    std::os::windows::fs::symlink_file(&target, flow.join("status.json")).unwrap();
    assert!(preflight(&flow).is_err());
    assert!(read_json(&flow.join("status.json")).is_err());
    assert!(write(&flow, &flow.join("private.json"), b"no", false).is_err());
    let alias = tmp.path().join("flow_alias");
    std::os::windows::fs::symlink_dir(&flow, &alias).unwrap();
    assert!(preflight(&alias).is_err());
    assert!(!flow.join("private.json").exists());
    assert_eq!(std::fs::read_to_string(target).unwrap(), "{}");
}

#[test]
fn windows_waiting_writer_rechecks_a_late_seal_under_the_mandatory_fence() {
    use crate::managed_run_control::test_hooks::{install_status_io_hook, StatusIoHookStage};
    let (_tmp, flow) = fixture();
    std::fs::write(flow.join("status.json"), "{}").unwrap();
    let directory = Directory::open(&flow, false).unwrap().unwrap();
    let fence = directory.fence().unwrap();
    let target = directory.path.clone();
    let worker_flow = flow.clone();
    let (waiting, observed) = std::sync::mpsc::sync_channel(0);
    let worker = std::thread::spawn(move || {
        install_status_io_hook(StatusIoHookStage::LockWouldBlock, target, move |_| {
            waiting.send(()).unwrap();
        });
        write(
            &worker_flow,
            &worker_flow.join("status.json"),
            br#"{"state":"must not replace seal"}"#,
            false,
        )
    });
    observed
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    std::fs::write(flow.join("status.json"), r#"{"ownership":null}"#).unwrap();
    drop(fence);
    assert!(worker.join().unwrap().is_err());
    assert_eq!(
        std::fs::read_to_string(flow.join("status.json")).unwrap(),
        r#"{"ownership":null}"#
    );
}

#[test]
fn optional_absence_never_bypasses_marker_or_events_gate() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("flow_events");
    std::fs::create_dir(&dir).unwrap();
    assert_eq!(read_events(&dir).unwrap(), None);
    let event = b"{\"kind\":\"legacy\"}\n";
    std::fs::write(dir.join("events.jsonl"), event).unwrap();
    assert!(read_events(&dir).is_err());
    std::fs::write(dir.join("status.json"), "{}").unwrap();
    assert_eq!(
        read_events(&dir).unwrap().as_deref(),
        Some(event.as_slice())
    );
    std::fs::remove_file(dir.join("events.jsonl")).unwrap();
    for status in [
        r#"{"ownership":null}"#,
        r#"{"ownership":{},"ownership":{}}"#,
        r#"{"ownership":{"version":1,"claim_id":"c","canonical_project":"alpha","flow_id":"flow_events"}}"#,
        "{",
    ] {
        std::fs::write(dir.join("status.json"), status).unwrap();
        assert!(
            read_events(&dir).is_err(),
            "absent events must not bypass marker denial"
        );
    }
}
