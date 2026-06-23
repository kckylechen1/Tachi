use super::*;

mod close_loop;
mod dispatch_markers;
mod github;
mod refs;

pub(crate) use close_loop::mark_task_close_loop;
pub(crate) use dispatch_markers::{mark_task_dispatch, mark_task_dispatch_completion};
pub(crate) use github::{write_intake_flow_artifacts, write_link_pr_artifacts};
pub(crate) use refs::{flow_status_doc_refs, resolve_link_pr_issue_ref};

pub(in crate::task_lifecycle) use github::intake_briefing_params;
#[cfg(test)]
pub(in crate::task_lifecycle) use refs::{initial_merge_state_for_pr, normalize_issue_ref};
