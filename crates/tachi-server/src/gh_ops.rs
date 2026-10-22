use crate::task_lifecycle::{append_github_event, merge_github_status, run_dir_for_flow_id};
use crate::tool_params::{
    GhCommentParams, GhIssueCreateParams, GhIssueListParams, GhIssueReadParams, GhLabelParams,
    GhPrCommentsParams, GhPrListParams, GhPrReadParams, GhRepoViewParams, TachiGhParams,
    TachiVerifyParams,
};
use crate::verify_ops::{evaluate_verification_gate, record_items as record_verification_items};
use crate::MemoryServer;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use tachi_gh_safe_merge::{
    evaluate_merge_gate_with_policy, CheckRun, ChecksState, ClosingIssueLabels, GhClient, GhError,
    MergeDecision, MergeGatePolicy, MergeGatePolicyMode, MergeResult, MergeStrategy, Mergeable,
    PrLifecycleState, PrState, PrViewSnapshot, ReviewDecision,
};

const DEFAULT_REVIEW_AUTHOR_FILTER: &str = "gemini";
type GhPrCommentsBundle = (Vec<Value>, Vec<Value>, Vec<Value>);

mod comments;
mod current_truth_refresh;
mod handoff;
mod issue_freshness;
mod issues;
mod labels;
mod prs;
mod repo;
mod review_digest;
mod router;
#[cfg(test)]
pub(crate) use crate::gh_read::{
    github_command_runner_call_count, reset_github_command_runner_call_count,
};
#[cfg(test)]
pub(crate) use router::worktree_holder_gate;
mod safe_merge;
mod ship;

#[cfg(test)]
mod safe_merge_tests;
#[cfg(test)]
mod ship_tests;

use self::current_truth_refresh::handle_current_truth_refresh;
use self::handoff::{handle_gh_handoff_draft, handle_gh_handoff_publish, handle_gh_handoff_repair};
use self::issues::*;
use self::labels::*;
use self::prs::*;
use self::repo::*;
use self::review_digest::*;
use self::safe_merge::*;
use self::ship::*;
use crate::gh_read::*;

pub(crate) use self::comments::{gh_comment_marker_present, handle_gh_comment};
pub(crate) use self::issue_freshness::{
    fetch_and_scan_same_surface_churn, fetch_and_scan_stale_candidates, fetch_and_scan_zombies,
    fetch_merged_prs_since, reap_stale_kind_rows, save_freshness_row, FreshnessRow,
    KIND_CHURN_CANDIDATE, KIND_STALE_CANDIDATE, KIND_ZOMBIE, STALE_CANDIDATE_NS, ZOMBIE_NS,
};
pub(crate) use self::router::handle_tachi_gh;
pub(crate) use self::safe_merge::gh_client_for_server;
#[cfg(test)]
pub(crate) use self::safe_merge::handle_github_safe_merge;
