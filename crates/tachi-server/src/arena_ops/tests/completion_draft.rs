use crate::arena_ops::dispatch_bridge::infer_completion_outcome;

#[test]
fn infer_completion_outcome_reads_descriptor_read_result_content() {
    assert_eq!(
        infer_completion_outcome("exit_code: 0\nall good"),
        Some("success")
    );
    assert_eq!(
        infer_completion_outcome("exit_code: 1\nfailed"),
        Some("failure")
    );
}

#[test]
fn infer_completion_outcome_returns_none_when_ambiguous() {
    assert_eq!(
        infer_completion_outcome("worker finished without explicit status"),
        None
    );
}
