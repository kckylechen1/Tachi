use crate::gh_safe_merge::{
    evaluate_merge_gate_with_policy, ChecksState, GhClient, GhError, MergeDecision,
    MergeGatePolicy, MergeGatePolicyMode, MergeResult, MergeStrategy, Mergeable, PrLifecycleState,
    PrState, ReviewDecision,
};
use crate::shell_ops::{append_github_event, merge_github_status, run_dir_for_flow_id};
use crate::tool_params::{
    GhCommentParams, GhIssueCreateParams, GhIssueListParams, GhIssueReadParams, GhPrCommentsParams,
    GhPrListParams, GhPrReadParams, GhRepoViewParams, TachiGhParams,
};
use crate::vault_ops::read_unlocked_vault_secret;
use crate::verify_ops::evaluate_verification_gate;
use crate::MemoryServer;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

const DEFAULT_REVIEW_AUTHOR_FILTER: &str = "gemini";
type GhPrCommentsBundle = (Vec<Value>, Vec<Value>, Vec<Value>);

mod comments;
mod issues;
mod prs;
mod repo;
mod review_digest;
mod router;
mod safe_merge;
mod transport;

#[cfg(test)]
mod safe_merge_tests;

use self::issues::*;
use self::prs::*;
use self::repo::*;
use self::review_digest::*;
use self::safe_merge::*;
use self::transport::*;

pub(crate) use self::comments::{gh_comment_marker_present, handle_gh_comment};
pub(crate) use self::router::handle_tachi_gh;
pub(crate) use self::safe_merge::{handle_github_safe_merge, CliGhClient};
