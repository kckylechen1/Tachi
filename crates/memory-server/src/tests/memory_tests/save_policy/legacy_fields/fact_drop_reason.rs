use super::*;

#[test]
fn fact_to_entry_with_reason_surfaces_drop_cause() {
    use crate::tool_params::{
        fact_to_entry, fact_to_entry_with_reason, FACT_DROP_EMPTY, FACT_DROP_TOO_SHORT,
    };

    // Empty text -> empty_text reason.
    assert_eq!(
        fact_to_entry_with_reason(&json!({"text": ""}), "extraction", json!({})).err(),
        Some(FACT_DROP_EMPTY)
    );

    // A faithfully-compressed short fact is dropped by the MIN_FACT_CHAR_COUNT
    // floor, and the reason is now surfaced instead of vanishing silently.
    let short = json!({"text": "端口会漂移", "topic": "t", "scope": "project"});
    assert_eq!(
        fact_to_entry_with_reason(&short, "extraction", json!({})).err(),
        Some(FACT_DROP_TOO_SHORT)
    );
    // The lossy wrapper still collapses both to None (off/default path unchanged).
    assert!(fact_to_entry(&short, "extraction", json!({})).is_none());

    // force=true bypasses the gate, so the same short fact builds an entry.
    assert!(fact_to_entry_with_reason(&short, "extraction", json!({"force": true})).is_ok());
}
