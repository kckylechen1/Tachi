//! Shared path-traversal gate for caller-supplied dispatch/run ids.
//!
//! tachi#1173 board autopsy (codex cold-review of uc-u-k2-board-autopsy,
//! eb473fd0) found `board::runs::collect_run_task_by_id` joining a
//! caller-controlled `dispatch_id` (from `tachi_task(action='wait'|'status'|
//! 'cancel')`) directly onto `runs_dir` with no validation -- a
//! `../../../../etc/passwd`-shaped id could read arbitrary files outside
//! `~/.tachi/runs`. The follow-up grep sweep (tachi#1173 k2 fix, this module)
//! found the identical join-with-no-validation shape reused at three more
//! call sites, all fed straight from a caller-supplied `dispatch_id`:
//!
//! - `dispatch::dedupe::load_dispatch_identity_receipt_checked` (from
//!   `TachiCompleteParams::dispatch_id` on `tachi_complete`)
//! - `tools::dispatch_complete_defaults::read_dispatch_defaults_for_complete`
//!   (same param, same call)
//! - `dispatch_ops::predicate::resolve_completion_predicate_context` (same
//!   param, same call)
//!
//! Centralized here so the character-class allowlist and the
//! canonicalize-and-confine defense-in-depth layer are defined once instead
//! of re-derived (and potentially re-drifted) per call site.

use std::path::Path;

/// Every real dispatch id minted by `dispatch::dedupe::new_dispatch_id` is a
/// single path component drawn from `[A-Za-z0-9_-]`
/// (timestamp-agent-suffix), so anything outside that allowlist is rejected
/// fail-closed -- treated identically to "run not found" rather than
/// surfaced as an error, so a probe gets no signal about what does or
/// doesn't exist on disk.
pub(crate) fn is_valid_dispatch_id(dispatch_id: &str) -> bool {
    !dispatch_id.is_empty()
        && dispatch_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

/// Defense in depth on top of [`is_valid_dispatch_id`]: canonicalize
/// `candidate_dir` (which must already exist) and confirm the resolved path
/// still lives under `root_dir`. Catches anything the character allowlist
/// alone might miss -- e.g. a symlinked run directory planted inside
/// `root_dir` that points elsewhere.
pub(crate) fn canonical_dir_is_within(candidate_dir: &Path, root_dir: &Path) -> bool {
    let (Ok(canonical_candidate), Ok(canonical_root)) =
        (candidate_dir.canonicalize(), root_dir.canonicalize())
    else {
        return false;
    };
    canonical_candidate.starts_with(&canonical_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_valid_dispatch_id_accepts_normal_shapes() {
        for ok in [
            "20260718T101010Z-claude-abc12345",
            "abc",
            "a-b_c9",
            "SIMPLE",
        ] {
            assert!(is_valid_dispatch_id(ok), "{ok:?} should be valid");
        }
    }

    #[test]
    fn is_valid_dispatch_id_rejects_traversal_and_degenerate_shapes() {
        for bad in ["../decoy", "..", "", "/etc/passwd", "a/../../decoy", "a/b"] {
            assert!(!is_valid_dispatch_id(bad), "{bad:?} should be rejected");
        }
    }
}
