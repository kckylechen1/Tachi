//! GitHub workflow state tracking for Tachi Shell flows.
//!
//! Manages the `github` block in `status.json` and emits typed events
//! to `events.jsonl` for GitHub-related automation (PR gates, merge
//! state tracking, etc.).

use super::*;
use chrono::Utc;
use serde_json::{json, Value};
use std::path::Path;

// ─── GitHub workflow state (status.json `github` block + events.jsonl) ──────
//
// Convoy / Touchy automation tracks GitHub-side state for a flow under a
// dedicated `github` block in `status.json`, and emits typed events to
// `events.jsonl`. Schema (all fields optional — partial updates are merged
// into whatever is already present):
//
// ```json
// {
//   "github": {
//     "repo": "owner/repo",
//     "issue_number": 123,
//     "issue_url": "https://github.com/owner/repo/issues/123",
//     "pr_number": 456,
//     "pr_url": "https://github.com/owner/repo/pull/456",
//     "merge_state": "pending|blocked|ready|merged",
//     "checks": { "state": "pending|success|failure", "updated_at": "..." },
//     "review": { "state": "pending|approved|changes_requested|blocked",
//                 "updated_at": "..." }
//   }
// }
// ```
//
// Events use the existing `event: "<kind>"` discriminator already used by
// `flow_created` / `stage_entered`, so downstream filters can grep by prefix:
// every GitHub-related event begins with `github_`.

/// Allow-list of GitHub event kinds that may be appended via
/// `append_github_event`. Centralised so the `tachi_gh safe_merge` flow,
/// future webhook bridges, and tests cannot drift.
//
// `dead_code` allow: production callers land in the follow-up commit that
// wires `tachi_gh safe_merge` through this helper. Tests already exercise
// every branch, and the helper is intentionally stable API surface.
pub(crate) const GITHUB_EVENT_KINDS: &[&str] = &[
    "github_issue_created",
    "github_issue_linked",
    "github_issue_commented",
    "github_pr_created",
    "github_pr_updated",
    "github_checks_polled",
    "github_review_gate_passed",
    "github_merge_blocked",
    "github_pr_merged",
];

/// Allow-list of `merge_state` values surfaced in `status.github.merge_state`.
/// Matches the section 五 schema. State machine intent:
///
/// - `pending`  — PR exists, gates still resolving (CI / review / mergeable)
/// - `blocked`  — at least one gate is red or a bot review requested changes
/// - `ready`    — all gates green, safe to merge (no automated merge yet)
/// - `merged`   — `gh pr merge` (any strategy) succeeded
pub(crate) const GITHUB_MERGE_STATES: &[&str] = &["pending", "blocked", "ready", "merged"];

/// Recursively merge `patch` into `target` in-place. Object values are merged
/// key-by-key (so a partial `{"checks": {"state": "success"}}` does not wipe
/// `checks.updated_at`); non-object values are replaced wholesale; `null`
/// values in `patch` clear the corresponding key in `target`.
fn deep_merge(target: &mut Value, patch: Value) {
    match (target, patch) {
        (Value::Object(t), Value::Object(p)) => {
            for (k, v) in p {
                if v.is_null() {
                    t.remove(&k);
                } else if let Some(existing) = t.get_mut(&k) {
                    deep_merge(existing, v);
                } else {
                    t.insert(k, v);
                }
            }
        }
        (slot, replacement) => {
            *slot = replacement;
        }
    }
}

/// Merge a GitHub-state patch into `status.json`'s `github` block. Returns the
/// resulting merged block so callers can echo it back to the agent. Creates
/// the block (and `status.json` itself) if absent.
///
/// `patch` MUST be a JSON object; non-object input is rejected to avoid
/// accidentally wiping the block with e.g. `Value::Null`.
pub(crate) fn merge_github_status(run_dir: &Path, patch: Value) -> Result<Value, String> {
    if !patch.is_object() {
        return Err(format!(
            "merge_github_status: patch must be a JSON object, got {}",
            match &patch {
                Value::Null => "null",
                Value::Bool(_) => "bool",
                Value::Number(_) => "number",
                Value::String(_) => "string",
                Value::Array(_) => "array",
                Value::Object(_) => unreachable!(),
            }
        ));
    }
    if let Some(state) = patch.get("merge_state").and_then(|v| v.as_str()) {
        if !GITHUB_MERGE_STATES.contains(&state) {
            return Err(format!(
                "merge_github_status: invalid merge_state '{}' (allowed: {:?})",
                state, GITHUB_MERGE_STATES
            ));
        }
    }
    let mut status = read_status(run_dir);
    if !status.is_object() {
        status = json!({});
    }
    let obj = status.as_object_mut().expect("ensured object above");
    let github = obj.entry("github".to_string()).or_insert_with(|| json!({}));
    deep_merge(github, patch);
    let merged = github.clone();
    obj.insert("updated_at".into(), json!(Utc::now().to_rfc3339()));
    crate::utils::write_run_status_file(run_dir, &status)?;
    Ok(merged)
}

/// Append a GitHub workflow event to `events.jsonl`. `kind` MUST be one of
/// `GITHUB_EVENT_KINDS`; unknown kinds are rejected so we don't silently
/// pollute the event stream. `payload` is merged into the event object after
/// the standard `event`/`flow_id`/`timestamp` fields, but those three keys
/// are reserved and cannot be overridden by the caller.
pub(crate) fn append_github_event(
    run_dir: &Path,
    flow_id: &str,
    kind: &str,
    payload: Value,
) -> Result<(), String> {
    if !GITHUB_EVENT_KINDS.contains(&kind) {
        return Err(format!(
            "append_github_event: unknown kind '{}' (allowed: {:?})",
            kind, GITHUB_EVENT_KINDS
        ));
    }
    let mut event = match payload {
        Value::Object(mut p) => {
            p.remove("event");
            p.remove("flow_id");
            p.remove("timestamp");
            p
        }
        _ => serde_json::Map::new(),
    };
    event.insert("event".into(), json!(kind));
    event.insert("flow_id".into(), json!(flow_id));
    event.insert("timestamp".into(), json!(Utc::now().to_rfc3339()));
    crate::utils::append_run_event(run_dir, Value::Object(event))
}
