#[test]
fn cleaner_bridge_uses_explicit_cleaner_binary_override() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let old_value = std::env::var_os("TACHI_CLEAN_BIN");
    std::env::set_var("TACHI_CLEAN_BIN", "/tmp/custom-tachi-clean");

    let resolved = tachi_merge_ops::resolve_tachi_clean_bin();

    match old_value {
        Some(value) => std::env::set_var("TACHI_CLEAN_BIN", value),
        None => std::env::remove_var("TACHI_CLEAN_BIN"),
    }
    assert_eq!(
        resolved,
        std::path::PathBuf::from("/tmp/custom-tachi-clean")
    );
}
