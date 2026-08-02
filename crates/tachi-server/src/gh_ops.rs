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
#[cfg(test)]
pub(crate) use transport::{
    github_command_runner_call_count, reset_github_command_runner_call_count,
};
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

/// One resolved `gh` executable plus explicit credential. This deliberately
/// implements neither `Debug` nor serialization: its token may only be passed
/// into a hardened child environment or used by the authority gate to derive
/// a one-way fingerprint/redact captured output.
pub(crate) struct GhApiContext {
    gh_path: String,
    token: String,
}

impl GhApiContext {
    /// `#[cfg(any(feature = "contract-leaves", test))]`: the production caller
    /// is `resolve_gh_api_context` below, gated per #1564 with the approver
    /// gate that consumes it; `for_test` keeps this reachable — and the
    /// empty-credential refusal covered — in a default `cfg(test)` build.
    #[cfg(any(feature = "contract-leaves", test))]
    fn new(gh_path: String, token: String) -> Result<Self, String> {
        if token.trim().is_empty() {
            return Err(
                "no explicit GitHub credential is available (Vault `GH_TOKEN`, or \
                 `GH_TOKEN`/`GITHUB_TOKEN` in the daemon environment). Approval authority \
                 must be bound to a credential context this gate can identify, and a `gh` \
                 keyring session cannot be pinned, so this is a refusal rather than an \
                 unpinned approval"
                    .to_string(),
            );
        }
        Ok(Self { gh_path, token })
    }

    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    #[cfg(test)]
    pub(crate) fn for_test(gh_path: String, token: String) -> Result<Self, String> {
        Self::new(gh_path, token)
    }
}

/// Resolve the executable and explicit credential once for a security-sensitive
/// GitHub probe. A caller must retain the returned context for the complete
/// issuance or revalidation round; resolving a new one is a new round.
///
/// `#[cfg(feature = "contract-leaves")]`: its only caller is
/// `approver_authority::GhApproverAuthorityProbe::new`, gated per #1564 with
/// the `governed_precedent_establishment` module at the top of that chain.
#[cfg(feature = "contract-leaves")]
pub(crate) fn resolve_gh_api_context(server: &MemoryServer) -> Result<GhApiContext, String> {
    let gh_path = resolve_gh_path()?;
    let token = resolve_gh_token(server)?.unwrap_or_default();
    GhApiContext::new(gh_path, token)
}

/// Build the same hardened `gh api` command used elsewhere, but from an
/// already-pinned context so no later request can splice in another token.
///
/// Ungated: the approver probe's request builders call this on every probe,
/// including in this crate's default-feature tests.
pub(crate) fn gh_api_command_for_context(context: &GhApiContext, args: &[&str]) -> Command {
    let mut cmd = build_gh_command_for_resolved_credential(&context.gh_path, Some(context.token()));
    cmd.arg("api");
    cmd.args(args);
    cmd
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
