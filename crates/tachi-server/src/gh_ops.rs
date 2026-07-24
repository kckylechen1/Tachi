use crate::gh_safe_merge::{
    evaluate_merge_gate_with_policy, CheckRun, ChecksState, ClosingIssueLabels, GhClient, GhError,
    MergeDecision, MergeGatePolicy, MergeGatePolicyMode, MergeResult, MergeStrategy, Mergeable,
    PrLifecycleState, PrState, ReviewDecision,
};
use crate::shell_ops::{append_github_event, merge_github_status};
use crate::task_lifecycle::run_dir_for_flow_id;
use crate::tool_params::{
    GhCommentParams, GhIssueCreateParams, GhIssueListParams, GhIssueReadParams, GhLabelParams,
    GhPrCommentsParams, GhPrListParams, GhPrReadParams, GhRepoViewParams, TachiGhParams,
    TachiVerifyParams,
};
use crate::vault_ops::read_unlocked_vault_secret;
use crate::verify_ops::{evaluate_verification_gate, record_items as record_verification_items};
use crate::MemoryServer;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

const DEFAULT_REVIEW_AUTHOR_FILTER: &str = "gemini";
type GhPrCommentsBundle = (Vec<Value>, Vec<Value>, Vec<Value>);

mod ci_watch;
mod comments;
mod handoff;
mod issue_freshness;
mod issues;
mod labels;
mod prs;
mod repo;
mod review_digest;
mod router;
#[cfg(test)]
pub(crate) use router::worktree_holder_gate;
mod safe_merge;
mod ship;
mod transport;

#[cfg(test)]
mod safe_merge_tests;
#[cfg(test)]
mod ship_tests;

use self::handoff::{handle_gh_handoff_draft, handle_gh_handoff_publish, handle_gh_handoff_repair};
use self::issues::*;
use self::labels::*;
use self::prs::*;
use self::repo::*;
use self::review_digest::*;
use self::safe_merge::*;
use self::ship::*;
use self::transport::*;

/// #1382: the approver-authority gate (`crate::approver_authority`) needs the
/// SAME hardened `gh` invocation every other GitHub caller here uses —
/// `env_clear` plus an allowlisted environment, the Vault-or-env token
/// injected as `GH_TOKEN`, prompts disabled — rather than a second,
/// less-guarded command builder of its own. This returns that command with
/// `api <args...>` already appended, plus the resolved token, which the gate
/// uses for output redaction and for the receipt's one-way, non-secret
/// credential fingerprint. The token value never leaves that gate.
pub(crate) fn gh_api_command(
    server: &MemoryServer,
    args: &[&str],
) -> Result<(Command, String), String> {
    let (mut cmd, token) = build_gh_command(server)?;
    cmd.arg("api");
    cmd.args(args);
    Ok((cmd, token))
}

/// #1382: reuse this module's token/auth-header redaction on any captured
/// GitHub output the approver gate is about to put into a denial message.
pub(crate) fn gh_redact(text: &str, token: &str) -> String {
    sanitize_output(text, token)
}

pub(crate) use self::ci_watch::{daemon_ci_reader, spawn_ci_watch};
pub(crate) use self::comments::{gh_comment_marker_present, handle_gh_comment};
pub(crate) use self::issue_freshness::{
    briefing_freshness_queues, extract_referenced_issue_numbers, fetch_and_scan_same_surface_churn,
    fetch_and_scan_stale_candidates, fetch_and_scan_zombies, fetch_merged_prs,
    fetch_merged_prs_since, reap_stale_kind_rows, save_freshness_row, FreshnessRow, MergedPr,
    KIND_CHURN_CANDIDATE, KIND_STALE_CANDIDATE, KIND_ZOMBIE, STALE_CANDIDATE_NS, ZOMBIE_NS,
};
pub(crate) use self::issues::read_issue_snapshot_bounded;
pub(crate) use self::router::handle_tachi_gh;
pub(crate) use self::safe_merge::{gh_client_for_server, handle_github_safe_merge};
