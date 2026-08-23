mod acp_native;
mod acpx;
mod board;
mod dispatch;
mod dispatch_v2;
mod harness;
mod kanban_helpers;
mod launcher;
mod mcp_config;
mod path_gate;
mod predicate;
mod prompt;
mod subprocess;

// Re-exports preserving the legacy public surface so external callers
// (`tools.rs`, `shell_ops.rs`, `complete_ops.rs`, `tests.rs`) keep
// resolving symbols via `crate::dispatch_ops::<name>`.
pub(crate) use acpx::run_acpx_control_from_status;
pub(crate) use board::{collect_run_task_for_server, handle_tachi_board, runs_dir_for_server};
#[cfg(test)]
pub(crate) use dispatch::apply_unlocked_vault_env;
#[cfg(test)]
pub(crate) use dispatch::background_dispatch_cleanup_complete;
pub(crate) use dispatch::dispatch_runs_root;
pub(crate) use dispatch::handle_tachi_dispatch;
pub(crate) use dispatch::launch_staff_assignment;
#[cfg(test)]
pub(crate) use dispatch::new_dispatch_id;
pub(crate) use dispatch::recover_orphaned_dispatch_runs;
pub(crate) use dispatch::{load_dispatch_identity_receipt_checked, DispatchReceiptLoad};
pub(crate) use dispatch_v2::stamp_route_decision_id;
pub(crate) use dispatch_v2::status_json_lock_for;
#[cfg(test)]
pub(crate) use dispatch_v2::write_status_json;
pub(crate) use harness::{
    harness_server_attach_ready, probe_harness_server_status, probe_harness_server_status_with_env,
};
#[cfg(test)]
pub(crate) use kanban_helpers::get_kanban_state;
#[cfg(test)]
pub(crate) use kanban_helpers::should_cleanup_run;
pub(crate) use kanban_helpers::update_kanban_state;
pub(crate) use prompt::seat_card::{
    card_kind_participates_in_seat_projection, complete_counter_clause_projection,
    resolve_exact_seat_card_readiness,
};
#[cfg(test)]
pub(crate) use subprocess::{
    install_managed_cancel_dequeue_barrier, ManagedCancelTryWaitObservation,
};
// tachi#1173 k2 fix: shared dispatch-id path-traversal gate (allowlist +
// canonicalize-and-confine), consumed by `board::runs`, `dispatch::dedupe`,
// `tools::dispatch_complete_defaults`, and `predicate` -- see `path_gate` for
// the full call-site inventory this closes.
pub(crate) use path_gate::{
    canonical_dir_is_within, ensure_descriptor_reads_supported, is_valid_dispatch_id,
    read_text_file_within, read_text_file_within_with_metadata, regular_file_len_within,
};
#[cfg(test)]
pub(crate) use path_gate::{install_secure_read_hook, SecureReadHookStage};
pub(crate) use predicate::{
    evaluate_completion_predicate_for_dispatch, execution_outcome_for_kanban_state,
    resolve_completion_predicate_context, resolve_completion_state, PredicateVerdict,
};
#[cfg(test)]
pub(crate) use prompt::{
    assemble_prompt, assemble_prompt_with_trace, assemble_resolved_prompt_with_trace,
};
