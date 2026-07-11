use crate::gh_safe_merge::{
    evaluate_merge_gate_with_policy, CheckRun, ChecksState, ClosingIssueLabels, GhClient, GhError,
    MergeDecision, MergeGatePolicy, MergeGatePolicyMode, MergeResult, MergeStrategy, Mergeable,
    PrLifecycleState, PrState, ReviewDecision,
};
use crate::shell_ops::{append_github_event, merge_github_status, run_dir_for_flow_id};
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
mod issue_freshness;
mod issues;
mod labels;
mod prs;
mod repo;
mod review_digest;
mod router;
mod safe_merge;
mod ship;
mod transport;

#[cfg(test)]
mod safe_merge_tests;
#[cfg(test)]
mod ship_tests;

use self::issues::*;
use self::prs::*;
use self::repo::*;
use self::review_digest::*;
use self::safe_merge::*;
use self::ship::*;
use self::transport::*;

pub(crate) use self::ci_watch::{daemon_ci_reader, spawn_ci_watch};
pub(crate) use self::comments::{gh_comment_marker_present, handle_gh_comment};
pub(crate) use self::issue_freshness::{
    briefing_freshness_queues, extract_file_line_anchors, fetch_and_scan_stale_candidates,
    fetch_and_scan_zombies, list_freshness_verdicts, save_freshness_verdict, FreshnessVerdict,
};
pub(crate) use self::issues::handle_gh_issue_read;
pub(crate) use self::labels::handle_gh_label;
pub(crate) use self::router::handle_tachi_gh;
pub(crate) use self::safe_merge::{gh_client_for_server, handle_github_safe_merge};
