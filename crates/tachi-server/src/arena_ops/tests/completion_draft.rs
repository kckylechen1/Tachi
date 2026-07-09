use crate::arena_ops::dispatch_bridge::infer_completion_outcome;
use std::path::Path;

#[test]
fn infer_completion_outcome_reads_exit_code_from_result_md() {
    let dir = tempfile::tempdir().expect("tempdir");
    let success = dir.path().join("success.md");
    std::fs::write(&success, "exit_code: 0\nall good").expect("write");
    assert_eq!(infer_completion_outcome(&success), Some("success"));

    let failure = dir.path().join("failure.md");
    std::fs::write(&failure, "exit_code: 1\nfailed").expect("write");
    assert_eq!(infer_completion_outcome(&failure), Some("failure"));
}

#[test]
fn infer_completion_outcome_returns_none_when_ambiguous() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ambiguous.md");
    std::fs::write(&path, "worker finished without explicit status").expect("write");
    assert_eq!(infer_completion_outcome(Path::new(&path)), None);
}
