//! Capture gate — pre-write validation for `save_memory` and friends.
//! Extracted from tachi-server (#833 slice 4).
//!
//! Branch #4 of the memory governance series. Goals (per project owner):
//!   1. Reject saves with empty / missing `domain` UNLESS the path is under
//!      `/scratch/...` (scratch is the only legal home for un-domained memos).
//!   2. Reject paths that don't begin with one of the allowed top-level
//!      buckets — prevents the historical "/foo/bar" / "/" rogue captures.
//!   3. Reject "raw markdown dumps" — long text containing markdown structure
//!      (headers, code fences, bullet lists) suggests the agent is paste-saving
//!      a whole transcript instead of running `extract_facts` first.
//!   4. Enforce a global `captureMinChars` floor (default 200) to filter out
//!      noise like "ok", "yes", "thanks" that slip past `is_noise_text`.
//!
//! Behavior is **soft-enforce by default**: the gate returns a structured
//! `GateDecision` with `accept | reject | warn`, and callers decide whether
//! to fail or merely annotate the response. This lets us roll the gate out
//! gradually without breaking OpenClaw plugin or hyperion-tachi consumers.
//!
//! To switch to hard-enforce globally, set `TACHI_CAPTURE_GATE=enforce` in
//! the environment. Default mode is `warn` (structured warning attached to
//! the save response, write proceeds).

use serde::Serialize;
use std::sync::{Mutex, OnceLock};

/// Default minimum character count for a non-scratch capture.
pub const DEFAULT_CAPTURE_MIN_CHARS: usize = 200;

/// Allowed top-level path buckets. Saves to anything else are rejected.
/// Use `register_bucket()` to extend at runtime (e.g. from plugins).
static BUCKET_REGISTRY: OnceLock<Mutex<Vec<&'static str>>> = OnceLock::new();

const BASE_BUCKETS: &[&str] = &[
    "wiki",
    "scratch",
    "decisions",
    "facts",
    "experience",
    "preferences",
    "entities",
    "code-review",
    "trading",
    "agent",
    "sft",
    "eval",
    "precedents",
    // #1076: pending principle-level precedent candidates fanned out from
    // captured `rulings[]` (see `precedent_candidate_ops`) — first-class
    // sibling of `precedents`, registered here the same way #950 registered
    // `precedents` itself, not a test-only bypass.
    "precedent_candidates",
    // #1192 (D2 forge pilot): pending lesson candidates. Registered
    // alongside `precedent_candidates` by the same gate audit (#1183) since
    // both are pending-candidate buckets that were discovered missing from
    // the allowed set. The writer that motivated it
    // (`lesson_forge_ops::storage::persist_pending_lesson_candidate`) was
    // deleted unwired in #1564; the bucket stays registered because this is
    // an accept-list over paths that may already exist in a store, and
    // because the #1073 forge contract still owns this path shape. Removing
    // it would be a live behavior change under
    // `TACHI_CAPTURE_GATE=enforce`, not a dead-code cleanup.
    "lesson_candidates",
    "checkpoints",
    "foundry",
    "ghost",
    "openclaw",
    "handoff",
    "scoreboard",
    "tachi",
    "rules",
    "memory", // legacy, allowed for backward compat with older agents
    "antigravity",
    "hyperion",
    "quant",
    "hapi",
];

/// Register additional buckets at runtime. Thread-safe via Mutex.
#[cfg(test)]
pub fn register_bucket(bucket: &'static str) {
    let registry = BUCKET_REGISTRY.get_or_init(|| Mutex::new(BASE_BUCKETS.to_vec()));
    if let Ok(mut buckets) = registry.lock() {
        if !buckets.contains(&bucket) {
            buckets.push(bucket);
        }
    }
}

pub fn allowed_buckets() -> Vec<&'static str> {
    let registry = BUCKET_REGISTRY.get_or_init(|| Mutex::new(BASE_BUCKETS.to_vec()));
    registry
        .lock()
        .map(|b| b.clone())
        .unwrap_or_else(|_| BASE_BUCKETS.to_vec())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateMode {
    /// Validation runs, but failures only attach a `warning` to the save response.
    Warn,
    /// Validation runs and rejects the save with an error.
    Enforce,
    /// Validation is skipped entirely (escape hatch for migrations).
    Off,
}

impl GateMode {
    pub fn from_env() -> Self {
        match std::env::var("TACHI_CAPTURE_GATE")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "enforce" | "strict" | "hard" => Self::Enforce,
            "off" | "disable" | "disabled" => Self::Off,
            _ => Self::Warn,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GateDecision {
    /// True if the save should proceed (in Warn mode this is always true unless mode=Off bypassed).
    pub accept: bool,
    pub mode: GateMode,
    pub violations: Vec<GateViolation>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GateViolation {
    pub code: GateViolationCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateViolationCode {
    DomainRequired,
    PathBucketDisallowed,
    PathTooShallow,
    BelowMinChars,
    LooksLikeMarkdownDump,
}

/// Inputs needed to evaluate the gate. Mirrors the relevant fields of
/// `SaveMemoryParams` without forcing the gate module to depend on it.
pub struct GateInput<'a> {
    pub text: &'a str,
    pub path: &'a str,
    pub domain: Option<&'a str>,
    pub force: bool,
    pub min_chars: usize,
}

impl<'a> GateInput<'a> {
    pub fn new(text: &'a str, path: &'a str, domain: Option<&'a str>, force: bool) -> Self {
        Self {
            text,
            path,
            domain,
            force,
            min_chars: DEFAULT_CAPTURE_MIN_CHARS,
        }
    }
}

/// Evaluate a save against the gate. The gate runs even when `force=true`
/// but in that case all violations are downgraded to warnings (force still
/// bypasses hard-enforce). `mode=Off` short-circuits with accept=true.
pub fn evaluate(input: &GateInput<'_>, mode: GateMode) -> GateDecision {
    if matches!(mode, GateMode::Off) {
        return GateDecision {
            accept: true,
            mode,
            violations: vec![],
        };
    }

    let mut violations = Vec::new();
    let path = input.path.trim();
    let bucket = first_bucket(path);

    // Rule 2 — path bucket gating.
    if bucket.is_empty() {
        violations.push(GateViolation {
            code: GateViolationCode::PathTooShallow,
            message: format!(
                "path '{path}' has no bucket; expected '/<bucket>/...' (allowed: {})",
                allowed_buckets().join(", ")
            ),
        });
    } else if !allowed_buckets().contains(&bucket.as_str()) {
        violations.push(GateViolation {
            code: GateViolationCode::PathBucketDisallowed,
            message: format!(
                "path bucket '/{bucket}' is not in the allowed set; expected one of: {}",
                allowed_buckets().join(", ")
            ),
        });
    }

    let is_scratch = bucket == "scratch";

    // Rule 1 — domain requirement (scratch exempt).
    let domain_present = input
        .domain
        .map(|d| !d.trim().is_empty() && d.trim() != "<none>")
        .unwrap_or(false);
    if !domain_present && !is_scratch {
        violations.push(GateViolation {
            code: GateViolationCode::DomainRequired,
            message: "domain is required for non-scratch captures (use '/scratch/...' for un-domained memos)".to_string(),
        });
    }

    // Rule 4 — min chars (scratch exempt; force exempt).
    let len = input.text.chars().count();
    if !is_scratch && !input.force && len < input.min_chars {
        violations.push(GateViolation {
            code: GateViolationCode::BelowMinChars,
            message: format!(
                "text length {} below capture floor {}; consider raising importance, batching, or saving to /scratch/...",
                len, input.min_chars
            ),
        });
    }

    // Rule 3 — markdown-dump heuristic (scratch and force exempt).
    if !is_scratch && !input.force && looks_like_markdown_dump(input.text) {
        violations.push(GateViolation {
            code: GateViolationCode::LooksLikeMarkdownDump,
            message: "text appears to be a raw markdown dump; run `extract_facts` first or save to /scratch/raw/...".to_string(),
        });
    }

    let accept = match mode {
        GateMode::Off => true,
        GateMode::Warn => true,
        GateMode::Enforce => violations.is_empty() || input.force,
    };

    GateDecision {
        accept,
        mode,
        violations,
    }
}

/// Extract the first non-empty path segment (the "bucket"). Returns empty string
/// for "/", "", or "//".
fn first_bucket(path: &str) -> String {
    path.trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("")
        .to_string()
}

/// Heuristic: does the text look like a raw markdown transcript paste?
/// Signals (any 2+ ⇒ flag):
///   - contains a code fence (```)
///   - contains a markdown header line (`\n# `, `\n## `, `\n### `)
///   - contains 3+ bullet lines (`\n- ` or `\n* `)
///   - length > 800 chars (a real fact extraction is typically tighter)
fn looks_like_markdown_dump(text: &str) -> bool {
    if text.len() < 200 {
        return false;
    }
    let mut score = 0;
    if text.contains("```") {
        score += 1;
    }
    if text.contains("\n# ") || text.contains("\n## ") || text.contains("\n### ") {
        score += 1;
    }
    let bullet_count = text
        .matches("\n- ")
        .count()
        .saturating_add(text.matches("\n* ").count());
    if bullet_count >= 3 {
        score += 1;
    }
    if text.len() > 800 {
        score += 1;
    }
    score >= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evaluate_warn(text: &str, path: &str, domain: Option<&str>) -> GateDecision {
        evaluate(
            &GateInput::new(text, path, domain, false),
            GateMode::Enforce,
        )
    }

    #[test]
    fn rejects_missing_domain_outside_scratch() {
        let d = evaluate_warn("a".repeat(300).as_str(), "/decisions/foo", None);
        assert!(!d.accept);
        assert!(d
            .violations
            .iter()
            .any(|v| v.code == GateViolationCode::DomainRequired));
    }

    #[test]
    fn allows_missing_domain_under_scratch() {
        let d = evaluate_warn("hello", "/scratch/raw/note", None);
        assert!(d.accept);
        assert!(d.violations.is_empty());
    }

    #[test]
    fn rejects_unknown_bucket() {
        let d = evaluate_warn(&"x".repeat(300), "/random/path", Some("equity_trading"));
        assert!(!d.accept);
        assert!(d
            .violations
            .iter()
            .any(|v| v.code == GateViolationCode::PathBucketDisallowed));
    }

    #[test]
    fn allows_sft_and_eval_buckets_with_domain() {
        let sft = evaluate_warn(
            &"x".repeat(300),
            "/sft/v4/strict/engineering/1",
            Some("coding"),
        );
        assert!(sft.accept, "SFT bucket should be accepted: {sft:?}");
        assert!(!sft
            .violations
            .iter()
            .any(|v| v.code == GateViolationCode::PathBucketDisallowed));

        let eval = evaluate_warn(&"x".repeat(300), "/eval/2026-06-05/task", Some("coding"));
        assert!(eval.accept, "eval bucket should be accepted: {eval:?}");
        assert!(!eval
            .violations
            .iter()
            .any(|v| v.code == GateViolationCode::PathBucketDisallowed));
    }

    #[test]
    fn registered_bucket_is_allowed() {
        register_bucket("plugin-test-bucket");
        let d = evaluate_warn(&"x".repeat(300), "/plugin-test-bucket/path", Some("plugin"));
        assert!(d.accept, "registered bucket should be accepted: {d:?}");
        assert!(!d
            .violations
            .iter()
            .any(|v| v.code == GateViolationCode::PathBucketDisallowed));
    }

    #[test]
    fn rejects_root_or_empty_path() {
        let d = evaluate_warn(&"x".repeat(300), "/", Some("equity_trading"));
        assert!(d
            .violations
            .iter()
            .any(|v| v.code == GateViolationCode::PathTooShallow));
    }

    #[test]
    fn enforces_min_chars_outside_scratch() {
        let d = evaluate_warn("too short", "/facts/lesson", Some("coding"));
        assert!(d
            .violations
            .iter()
            .any(|v| v.code == GateViolationCode::BelowMinChars));
    }

    #[test]
    fn min_chars_skipped_in_scratch() {
        let d = evaluate_warn("ok", "/scratch/note", None);
        assert!(d.accept);
    }

    #[test]
    fn force_bypasses_enforcement() {
        let d = evaluate(
            &GateInput {
                text: "tiny",
                path: "/decisions/x",
                domain: None,
                force: true,
                min_chars: DEFAULT_CAPTURE_MIN_CHARS,
            },
            GateMode::Enforce,
        );
        assert!(d.accept, "force=true must bypass enforce");
    }

    #[test]
    fn flags_markdown_dump() {
        let dump = format!(
            "# Title\n\n## Section\n\n```rust\nfn main() {{}}\n```\n\n- item 1\n- item 2\n- item 3\n{}",
            "x".repeat(900)
        );
        let d = evaluate_warn(&dump, "/facts/notes", Some("coding"));
        assert!(d
            .violations
            .iter()
            .any(|v| v.code == GateViolationCode::LooksLikeMarkdownDump));
    }

    #[test]
    fn warn_mode_accepts_with_violations() {
        let d = evaluate(
            &GateInput::new("short", "/decisions/x", None, false),
            GateMode::Warn,
        );
        assert!(d.accept);
        assert!(
            !d.violations.is_empty(),
            "warn mode still surfaces violations"
        );
    }

    #[test]
    fn off_mode_short_circuits() {
        let d = evaluate(
            &GateInput::new("", "/garbage/here", None, false),
            GateMode::Off,
        );
        assert!(d.accept);
        assert!(d.violations.is_empty());
    }

    #[test]
    fn empty_domain_string_is_treated_as_missing() {
        let d = evaluate_warn(&"x".repeat(300), "/facts/x", Some(""));
        assert!(d
            .violations
            .iter()
            .any(|v| v.code == GateViolationCode::DomainRequired));
    }

    #[test]
    fn none_literal_domain_is_treated_as_missing() {
        let d = evaluate_warn(&"x".repeat(300), "/facts/x", Some("<none>"));
        assert!(d
            .violations
            .iter()
            .any(|v| v.code == GateViolationCode::DomainRequired));
    }
}
