use super::*;

mod check_state;
mod cli;
mod error;
mod handler;
mod http;
mod parser;
mod policy;

pub(crate) use check_state::{write_check_state_artifact, CheckStateArtifactInput};
pub(crate) use cli::CliGhClient;
pub(in crate::gh_ops) use error::{classify_gh_error, is_no_checks_reported};
pub(crate) use handler::handle_github_safe_merge;
pub(crate) use http::gh_client_for_server;
pub(in crate::gh_ops) use parser::parse_pr_view_json;
#[cfg(test)]
pub(in crate::gh_ops) use policy::parse_merge_gate_policy;
pub(in crate::gh_ops) use policy::{
    apply_verification_gate_to_decision, effective_safe_merge_dry_run,
    merge_gate_policy_from_params, merge_strategy_flag, parse_merge_strategy,
    verification_satisfies_head_consistency,
};
