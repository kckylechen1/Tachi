# Stage 3 — Result

## Goal
Split `crates/memory-server/src/tests.rs` (6205 lines, 114 test functions) into a
focused `crates/memory-server/src/tests/` directory with one `mod.rs` (shared
helpers + module declarations) and themed test submodules.

## Bucketing Plan

114 test functions distributed across 15 themed files. The plan's hub_tests
bucket was further subdivided into `hub_tests` (lifecycle/admin), `skill_tests`
(skill/recommend/ingest), and `vc_tests` (virtual-capability resolution) to keep
every file under the ~1000 line target.

| Bucket               | Tests | Notes                                                                 |
|----------------------|------:|-----------------------------------------------------------------------|
| `memory_tests`       |    16 | save/get/sync/gc/graph/stuck-detection, tachi_init/save_note, helpers |
| `wiki_tests`         |     8 | wiki write/browse/search/export/ingest/lint                           |
| `hub_tests`          |    16 | hub_call/register/quick_add/export/feedback/stats/disconnect          |
| `skill_tests`        |    16 | run_skill, recommend_*, ingest_source, distill, server_seeds, …      |
| `vc_tests`           |     3 | vc_resolve preference/version-mismatch + vc_register_and_bind         |
| `vault_tests`        |     9 | init/set/get/lock/unlock/rotation/auto-lock/audit/list/remove         |
| `sandbox_tests`      |     6 | sandbox check/set/policy, shell+section9 alias, process_runtime       |
| `kanban_tests`       |     6 | post_card/check_inbox/kanban_update                                   |
| `dispatch_tests`     |     3 | dispatch_run_cleanup, tachi_complete, tachi_task_brief                |
| `handoff_tests`      |     3 | handoff leave/check/persistence                                       |
| `profile_tests`      |     6 | agent_register, rate_limit_*, standard_profile_*                      |
| `pack_tests`         |    10 | test_pack_* + test_projection_list                                    |
| `facade_tests`       |     4 | cli_client_*                                                          |
| `proxy_tests`        |     5 | proxy_call_*, filter_mcp_tools, retry_dispatch, resolve_mcp_tool      |
| `bootstrap_tests`    |     3 | setup_report, tidy_report, tidy_apply                                 |
| **TOTAL**            | **114** |                                                                   |

## Line Counts (before vs after)

### Before
```
6205  crates/memory-server/src/tests.rs
```

### After
```
 247  crates/memory-server/src/tests/bootstrap_tests.rs
 163  crates/memory-server/src/tests/dispatch_tests.rs
 130  crates/memory-server/src/tests/facade_tests.rs
 148  crates/memory-server/src/tests/handoff_tests.rs
 622  crates/memory-server/src/tests/hub_tests.rs
 362  crates/memory-server/src/tests/kanban_tests.rs
 635  crates/memory-server/src/tests/memory_tests.rs
 269  crates/memory-server/src/tests/mod.rs
 586  crates/memory-server/src/tests/pack_tests.rs
 178  crates/memory-server/src/tests/profile_tests.rs
 222  crates/memory-server/src/tests/proxy_tests.rs
 309  crates/memory-server/src/tests/sandbox_tests.rs
 870  crates/memory-server/src/tests/skill_tests.rs
 636  crates/memory-server/src/tests/vault_tests.rs
 234  crates/memory-server/src/tests/vc_tests.rs
 625  crates/memory-server/src/tests/wiki_tests.rs
6236  total
```

The +31 line delta (6236 vs 6205) comes from the 16 themed file headers
(`use super::*;` + blank line) plus mod.rs's 13 `mod foo_tests;` declarations.
No test bodies were modified.

## Shared Helpers

`tests/mod.rs` (269 lines) contains the original preamble verbatim — the
`use` block and these helpers:

- `ensure_test_env`
- `home_test_lock`, `acquire_real_home_lock`, `TempHomeGuard` (Drop included)
- `make_server`, `make_server_with_temp_home`, `seed_wiki_project_entries`
- `make_entry`, `make_test_tool`, `make_mcp_capability`, `make_skill_capability`
- `call_tool_via_server`

Each themed file does `use super::*;` to access them.

## Ambiguity / Judgment Calls

- `tachi_complete_writes_eval_ledger_and_returns_review_bundle` → `dispatch_tests`
  (it exercises dispatch-completion lifecycle even though it touches eval).
- `tachi_task_brief_uses_wiki_hits_for_debug_checklist` → `dispatch_tests`
  (dispatch/task-flow surface, even though it consumes wiki hits).
- `wiki_lint_*` → `wiki_tests` (despite touching memory health).
- `server_seeds_builtin_capabilities_and_mcp_policies` → `skill_tests`
  (capability/skill seeding rather than bootstrap report).
- `standard_profile_direct_add_edge_call_is_rejected` → `profile_tests`
  (profile-policy enforcement, not generic hub).

## Commit

`46ce5c1aadc8c975985b1e51c72506adf2ff3803`

## Verification Summary

- Baseline: **270 passed**.
- After refactor: **270 passed**, 0 failed.
- `cargo check -p memory-server --tests` clean (no new warnings).
- Original `crates/memory-server/src/tests.rs` deleted.
- No external references (`crate::tests::` / `super::tests::`) found in the crate.
