use super::*;

#[test]
fn arena_ids_reject_traversal() {
    for invalid in ["../../x", "arena_../x", "arena_bad/name", "notarena_x"] {
        assert!(validate_arena_id(invalid).is_err(), "{invalid}");
    }
    assert!(validate_arena_id("arena_20260606T000000Z_demo_deadbeef").is_ok());
    assert!(validate_mission_id("mission_explore_deadbeef").is_ok());
    let err = validate_mission_id("bad/name").unwrap_err();
    assert!(err.contains("Expected prefix 'mission_'"));
}

#[test]
#[cfg(unix)]
fn append_event_writes_synced_and_owner_only_file() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    crate::utils::append_run_event(tmp.path(), json!({"event": "test"})).unwrap();

    let path = tmp.path().join("events.jsonl");
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(contents.contains("\"event\":\"test\""));

    let meta = std::fs::metadata(&path).unwrap();
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "events.jsonl should be owner-readable only");
}
