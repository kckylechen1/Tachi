# tachi-server layering

Layer map for the top-level modules under `crates/tachi-server/src`, per the
#1467 cores and the #2077 stage-2 split. The rule: **a lower layer never
imports a higher one.** Imports between the four domain groups are horizontal
edges, not layer violations; #2077 tracks them for later boundary work.

Layer order, lowest to highest:

1. `core` — leaf utilities and shared types, no domain knowledge.
2. `infra` — shared services (Vault, Hub, Knowledge, Foundry) used by domains.
3. `domain` — Memory; Work Control (incl. GitHub lifecycle); Staffing; A2A.
   The four domains are unordered peers of each other.
4. `admin` — operator surfaces: status, repair, doctor, daemon client.
5. `shell` — server assembly: state, handlers, methods, tool registry,
   bootstrap, the `facade_*_ops`/`tune_ops` facades, `lib.rs`, `main.rs`.

| Layer | Modules |
|---|---|
| core | `utils` `path_utils` `tool_params` `shared_defs` `db_ownership` `session_identity` `provenance` `project_db_ops` `manifest` `host_profile` `action_effect` `build_info` `prompts` `server_instructions` `memory_row_defaults` `doctor_types` `build_resources` `kernel_policy_adapter` |
| Memory | `memory_search_ops` `memory_ops` `memory_resources` `enrichment` `vector_backfill` `vector_sweep` `continuity_ops` `continuity_projector` `precedent_ops` `precedent_candidate_ops` `feedback_rule_ops` `notes_ops` `event_ops` `research_ops` `web_search_ops` `dlq_ops` |
| Work Control | `task_lifecycle` `dispatch_ops` `dispatch_profile` `complete_ops` `claims_ops` `verify_ops` `delivery_ops` `workflow_closure` `kanban` `handoff_ops` `gh_ops` `copilot_ops` `orchestrator_ops` |
| Staffing | `staffing_ops` `exec_env` `exec_env_ops` `exec_env_postflight` `managed_run_control` `managed_run_epoch` `host_spawn_bridge` `executor_offload` `sandbox_ops` `verified_admission` `build_broker` |
| A2A | `a2a_ops` `peer_ops` `mcp_peer` `agent_registry` |
| infra | `vault_ops` `vault_crypto` `provider_config` `hub_ops` `builtins` `skill_chain_ops` `skill_policy` `skill_source_resolver` `mcp_pool` `mcp_connection` `mcp_proxy` `agent_markdown` `component_governance_ops` `domain_adapter_ops` `wiki_ops` `docs_ops` `foundry_runtime_ops` `foundry_scheduler` `pipeline_ops` `daily_pipeline` `agent_eval` `lesson_forge_ops` |
| admin | `status_ops` `repair` `doctor` `cli_client` |
| shell | `server_state` `server_handler` `server_methods` `tools` `bootstrap` `facade_memory_ops` `facade_save_ops` `facade_search_ops` `tune_ops` |

Known deviations in current code; removal is tracked in #2077:

- Most modules import `server_state::MemoryServer`, or a shell facade that
  takes it. That seam is why capability traits are being introduced; it does
  not move a module's row.
- Remaining upward edge: `gh_ops/handoff → status_ops` (snapshot cluster,
  not a pure move; tracked in #2077).
- Pipeline-style infra modules (`agent_eval`, `daily_pipeline`,
  `pipeline_ops`, `foundry_runtime_ops`, `wiki_ops`, `docs_ops`) still call
  domain modules. Their shared-infra placement is an owner decision (#1467);
  the calls narrow as the trait seams land.
- `daemon_lock`, `network_safety` and `physical_db_identity` (#2077 PR C)
  left `tachi-server` for the `tachi-runtime-core` workspace crate, joined by
  `lsof_stderr` from `tachi-clean` (#2088 PR 2). `db_ownership` stays in
  `tachi-server`: its `cfg(test)`-gated `set_ownership_inject_for_test` API is
  consumed by 31 test sites in `repair`/`doctor`, which a dependency crate's
  `cfg(test)` can never serve. The layer rule applies unchanged to the new
  home.
- Test-only top-level modules (`tests`, `test_support`,
  `model_call_census_tests`, `bootstrap_test_api`, `delivery_test_api`,
  `vault_test_api`) are outside the map.
