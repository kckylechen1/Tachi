use super::*;

#[test]
fn vault_replace_api_key_pool_rolls_back_when_rotation_write_fails() {
    let dir = tempfile::tempdir().expect("vault rollback temp dir");
    let path = dir.path().join("memory.db");
    let mut store = MemoryStore::open(&path.to_string_lossy()).expect("open test store");
    let offline = rusqlite::Connection::open(&path).expect("open offline trigger fixture");
    offline
        .execute_batch(
            "CREATE TRIGGER fail_pool_rotation
             BEFORE INSERT ON vault_key_rotations
             WHEN NEW.prefix = 'FAIL_API_KEY'
             BEGIN
               SELECT RAISE(ABORT, 'forced rotation failure');
             END;",
        )
        .expect("install failure trigger");
    drop(offline);

    let err = store
        .vault_replace_api_key_pool(
            "FAIL_API_KEY",
            &[test_entry("FAIL_API_KEY_1"), test_entry("FAIL_API_KEY_2")],
            &test_rotation("FAIL_API_KEY", 2),
        )
        .expect_err("rotation failure should abort replacement");

    assert!(
        err.to_string().contains("forced rotation failure"),
        "unexpected error: {err}"
    );
    assert!(
        store
            .vault_get_entry("FAIL_API_KEY_1")
            .expect("read first member")
            .is_none(),
        "pool member inserted before the failing rotation must roll back"
    );
    assert!(
        store
            .vault_get_entry("FAIL_API_KEY_2")
            .expect("read second member")
            .is_none(),
        "pool member inserted before the failing rotation must roll back"
    );
    assert!(
        store
            .vault_get_rotation("FAIL_API_KEY")
            .expect("read rotation")
            .is_none(),
        "failed replacement must not leave a rotation row"
    );
}
