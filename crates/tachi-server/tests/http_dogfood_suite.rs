#![cfg(unix)]

mod common;

#[test]
fn packaged_native_session_supervision_http_dogfood() {
    common::assert_dogfood_scenario("native_session_supervision_and_mailbox");
}

#[test]
fn packaged_shared_daemon_memory_and_binding_http_dogfood() {
    common::assert_dogfood_scenario(concat!(
        "capture_gate_warning_states_consequence,",
        "search_markdown_labels_scores_and_documents_the_order,",
        "global_library_rows_are_not_labeled_legacy,",
        "retired_ledger_snapshots_stay_out_of_default_recall,",
        "synthetic_recall_is_opt_in_and_real_lessons_remain_visible,",
        "checkpoint_freshness_respects_agent_and_project,",
        "two_identities_contend_for_a_scope,",
        "eval_receipts_preserve_requested_identity_and_canonical_replay,",
        "repository_short_name_addresses_the_library,",
        "project_mailbox_delivers_at_a_host_turn_boundary,",
        "retired_tools_are_not_listed_or_callable",
    ));
}

#[test]
fn packaged_shared_daemon_lifecycle_and_eval_http_dogfood() {
    common::assert_dogfood_scenario(concat!(
        "retired_hub_mcp_proxy_and_virtual_capabilities_are_rejected,",
        "continuity_projection_retirement,",
        "recall_eval_receipts_bind_inputs_and_reject_drift,",
        "recall_eval_contracts_fail_closed_and_status_is_private",
    ));
}

#[test]
fn packaged_private_daemon_search_and_maintenance_http_dogfood() {
    common::assert_dogfood_scenario(concat!(
        "enrichment_failures_use_the_vector_coverage_cohort,",
        "missing_vectors_do_not_claim_enrichment_queue_depth,",
        "voyage_model_selection_reaches_memory_search_and_rerank,",
        "exact_ties_within_top_k_reach_the_reranker,",
        "maintenance_candidates_preserve_active_pinned_and_references",
    ));
}

#[test]
fn packaged_private_daemon_recovery_and_staff_http_dogfood() {
    common::assert_dogfood_scenario(concat!(
        "interrupted_legacy_copy_refuses_then_recovers,",
        "clean_clone_manifest_init_defers_plan_c_alias,",
        "lost_staff_start_response_recovers_original_run,",
        "recalled_data_cannot_forge_worker_prompt_sections",
    ));
}
