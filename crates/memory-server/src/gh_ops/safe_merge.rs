use super::*;

mod cli;
mod error;
mod handler;
mod parser;
mod policy;

pub(crate) use cli::CliGhClient;
pub(in crate::gh_ops) use error::{classify_gh_error, is_no_checks_reported};
pub(crate) use handler::handle_github_safe_merge;
pub(in crate::gh_ops) use parser::parse_pr_view_json;
pub(in crate::gh_ops) use policy::{
    apply_verification_gate_to_decision, effective_safe_merge_dry_run, merge_strategy_flag,
    parse_merge_gate_policy, parse_merge_strategy, verification_satisfies_head_consistency,
};
