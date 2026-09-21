---
title: "MCP Surface Death List"
summary: "Classifies Tachi's native MCP surface into keep, quarantine, and deletion candidates so small-surface work can delete capabilities instead of only hiding them behind admin."
category: "engineering/architecture"
organize: true
---
# MCP Surface Death List

Status: historical evidence ledger for #757, originally under #745; current product-boundary authority is #1467 and bounded leaves route to their native owners.
Batch C: executed 2026-07-07 — `tachi_dispatch`, `tachi_board`, and `approve_merge` MCP routes deleted; later contraction leaves `tachi_task(action='board')` and retires Task dispatch/merge paths.
Base reviewed: `2cd7a750` (`main`, 2026-07-07).

This document records the deletion queue as it stood at the named historical
base. Everything below is non-executable evidence: no row authorizes a new leaf,
profile change, compatibility removal, or delete. Reconcile current code and the
#1467 / #1319 disposition before deriving any new bounded contract.

## Decision Frame

The current system already hides most complexity from default clients. The
counts below were refreshed for the #1312 native-first cut on 2026-07-20;
#1319 owns the complete schema-byte/property census and further re-slice; closed
#1315 is retained only as historical design evidence:

The machine-readable baseline is
[`execution-surface-census-v1.fixture.json`](execution-surface-census-v1.fixture.json).
It is recomputed from the composed native router through the same production
profile and action-schema transforms as `tools/list`; observed values and
provisional budgets are deliberately separate. To inspect a candidate update,
run
`TACHI_PRINT_EXECUTION_SURFACE_CENSUS=1 cargo test -p tachi-server execution_surface_census -- --nocapture`,
review the printed census and budget rationale, then update the fixture
explicitly in the same PR. Normal tests never rewrite the fixture.

The deletion target for Task dispatch, Shell, and Arena is frozen separately in
[`external-staffing-contract-v1.fixture.json`](external-staffing-contract-v1.fixture.json):
one `TachiDispatchParams` adoption kernel and one canonical run receipt. Its
secondary-ledger roots, staffing-projection writer sites (owner-module documents
plus dispatch/completion flow projections), and linked-result-copy inventory
describe legacy debt and may only decrease as #1319 deletion leaves land. This
is a staffing projection census, not a census of unrelated lifecycle/GitHub
writes into the shared flow directory; none of these stores is an additional
authority.

- `standard` is the curated allow-list in `STANDARD_MINIMAL_TOOL_PATTERNS`;
  its `tachi_task` schema also omits operator-only `dispatch`.
- `delegate` is the worker-safe allow-list in `DELEGATE_MINIMAL_TOOL_PATTERNS`;
  source, rather than this historical document, owns its exact count.
- `admin` is not a curated management profile. `tool_visible()` returns true
  immediately for admin, so admin exposes the full native catalog plus any
  directly exposed proxy/skill tools.
- The source-owned routed `#[tool]` inventory is the current census. The old
  132-tool and 76/56 breakdown is retired; regenerate profile-level counts
  under #1319 rather than treating a prose snapshot as current truth.

The product goal is stricter than profile hiding: daily agents should see a
small canonical surface, and retired capabilities should disappear instead of
living forever as admin/backcompat clutter.

## CodeGraph Verification Pass

This ledger used a worktree-local CodeGraph index, not only string search:
`codegraph init -i .` indexed 1,370 files, 14,044 nodes, and 44,711 edges.
Relevant checks:

- `codegraph query tachi_dispatch --path .` showed (historical; tachi_dispatch is retired)
  `MemoryServer::tachi_dispatch` (retired) at
  `crates/tachi-server/src/tools/dispatch_facade.rs:18`, plus many
  `tachi_dispatch` (retired) crate/module imports. Deletion leaves must target the MCP
  wrapper route, not broad string matches against the dispatch implementation
  crate.
- `codegraph callers MemoryServer::tachi_dispatch` (retired) — historical: reported no direct
  Rust callers for the wrapper method, matching its deprecated/backcompat route
  classification.
- `codegraph impact MemoryServer::tachi_complete --path . --depth 2` (historical command; tachi_complete is retired) reports 20
  affected symbols, mostly completion/eval/dispatch tests. That is why
  `tachi_complete` (retired) was in the worker-escape-hatch batch (all since retired), not in an immediate
  delete batch.
- `codegraph impact remap_daemon_tool --path . --depth 2` reports
  `call_daemon_tool` and `maybe_forward_tool`, confirming old-name retirement
  must update daemon forwarding compatibility intentionally.

## Canonical Keep Set

These five facades are the complete ordinary Lead/Worker discovery surface.
Action policy still distinguishes Lead from Worker permissions.

| Tool | Role | Notes |
| --- | --- | --- |
| `tachi_memory` | memory facade | Canonical search/get/save/extract/briefing/checkpoint/alerts surface. |
| `tachi_task` | task lifecycle facade | Historical (2026-07-07 base) plan/dispatch/complete/status/board/wait surface, since narrowed further — `dispatch`/`cancel`/`wait` were removed by #1319-C2 and `plan`/`cycle_plan`/`recommend`/`refine_issues`/`merge`/`ux_matrix` were retired by #1683 C1a. Current action set lives in `TachiTaskAction::PRIMARY`, not in this historical row. |
| `tachi_gh` | GitHub/evidence facade | Keep if GitHub remains part of ship/evidence workflows; move duplicated task PR actions here. |
| `tachi_staff` | staffing facade | Lead may start/cancel; Worker is restricted to status. |
| `tachi_a2a` | advisory messaging facade | Canonical bounded agent-to-agent messaging surface. |

Diagnostics and residual facades such as `tachi_tools`, `runtime_info`,
`tachi_status`, `tachi_verify`, `tachi_wiki`, `tachi_skill`,
`tachi_web_search`, `vault_status`, and `tachi_tune` remain physically
registered only for explicit Ops/admin compatibility. They are not ordinary
daily discovery, and this contraction does not claim their deletion.

`tachi_save` and `tachi_briefing` were convenience shorthands, both retired
and removed from current profile allow-lists. Their canonical replacements are
`tachi_memory(action="save")` and `tachi_memory(action="briefing")`.

## Historical Death List

### Batch A: Hard-Retire Old Direct Names

At the recorded base, owner policy from #566 was hard retire rather than alias
infrastructure. The table captured the then-proposed sequence: remove names from
ordinary profiles, migrate internal callers, then consider wrapper or CLI
compatibility removal. It is not a current action queue.

| Surface | Current evidence | Replacement | Decision | Next leaf |
| --- | --- | --- | --- | --- |
| `search_memory` | Still in observe patterns at `crates/tachi-hub/src/tool_profiles/patterns.rs:10`; daemon CLI remaps to `tachi_memory(action="search")` at `crates/tachi-server/src/cli_client/tool_map.rs:18`. | `tachi_memory(action="search")` | Hard-retire direct MCP name. | Profile-hide in #756, then migrate direct tests/callers and delete wrapper. |
| `save_memory` | Still in remember patterns at `patterns.rs:38`; daemon CLI remaps to `tachi_memory(action="save")` at `tool_map.rs:17`. | `tachi_memory(action="save")` | Hard-retire direct MCP name. | Same batch as `search_memory`. |
| `remember` | Still in remember patterns at `patterns.rs:39`; remaps with `save_memory` at `tool_map.rs:17`. | `tachi_memory(action="save")` | Hard-retire direct MCP name and keep only CLI prose if needed. | Same batch as `save_memory`. |
| `get_memory` | Already folded admin-only in `FOLDED_NATIVE_COMPAT_TOOLS` at `crates/tachi-server/src/tests/profile_tests/tool_profile_router_coverage.rs:155`; daemon CLI remaps to `tachi_memory(action="get")` at `tool_map.rs:19`. | `tachi_memory(action="get")` | Delete candidate after caller migration. | Remove direct wrapper once tests stop using it as public MCP. |
| `extract_facts` | Still a standalone remember tool and remaps to `tachi_memory(action="extract_facts")` at `tool_map.rs:16`. | `tachi_memory(action="extract_facts")` | Fold candidate, not first cut. | Decide whether high-frequency use justifies standalone entry. |

### Batch A2: Fold Wiki Duplicate Aliases

The canonical wiki surface is `tachi_wiki`. Direct wiki aliases should follow
the same migration rule as raw memory names: hide first, migrate callers, then
delete wrappers.

| Surface | Current evidence | Replacement | Decision | Next leaf |
| --- | --- | --- | --- | --- |
| `tachi_wiki_search` | Direct wiki search remains live in observe patterns at `patterns.rs:4`; the CLI map routes both `tachi_wiki_search` and the retired `wiki_search` alias to `tachi_wiki(action="search")` at `tool_map.rs:20`. | `tachi_wiki(action="search")` | Fold/delete candidate; not retired. | Migrate tests/docs before a separate deletion leaf. |
| `wiki_search` | **DONE:** Retired and removed from MCP tool router and observe patterns. | `tachi_wiki(action="search")` | Hard-retired alias. | — |
| `tachi_wiki_write` | Direct wiki write remains in remember patterns at `patterns.rs:40`; CLI map routes `tachi_wiki_write` and `wiki_write` to `tachi_wiki(action="write")` at `tool_map.rs:21`. | `tachi_wiki(action="write")` | Fold/delete candidate. | Same wiki alias leaf. |
| `tachi_browse` | Facade read tool remains registered for explicit Ops/admin compatibility but is absent from ordinary Lead/Worker discovery. | `tachi_wiki(action="browse")` if browse stays a wiki action. | Fold candidate, but not first cut. | Decide physical retirement separately; profile hiding is not deletion. |

### Batch B: Retire Direct Kanban Tools

These routes are implementation details of the dispatch/card board. They should
not be a model-facing collaboration API if `tachi_task` owns the agent workflow
(`tachi_arena` was deleted in #1319-D2, so it is no longer an alternative home
for any of these).

| Surface | Current evidence | Replacement | Decision | Next leaf |
| --- | --- | --- | --- | --- |
| `check_inbox` | Direct tool in `crates/tachi-server/src/tools.rs:109`; in coordinate profile at `patterns.rs:61`. | `tachi_task(action="board")`. | Profile-retire, then delete public route. | Move kanban tests to handlers or canonical facade; remove MCP route. |
| `post_card` | Direct tool in `tools.rs:101`; in coordinate profile at `patterns.rs:64`. | `tachi_staff(action="start")` or an internal board write (`tachi_task(action="dispatch")` and `tachi_arena(action="spawn")` were both deleted, #1319-C2/D2). | Profile-retire, then delete public route. | Same kanban route deletion leaf. |
| `update_card` | Direct tool in `tools.rs:117`; in coordinate profile at `patterns.rs:65`. | `tachi_task(action="complete"|"status")`, `tachi_staff(action="cancel")`, or an internal board update. | Profile-retire, then delete public route. | Same kanban route deletion leaf. |

Do not delete the kanban storage/handler code in this batch. Only delete the MCP
route after the canonical task/staff flows cover the same dogfood path.

### Batch C: Executed — deprecated dispatch facades removed (PR #822; tracked by #757)

These entries record the completed compatibility-route deletion. They are not a current work queue; the internal dispatch/board handlers remain behind canonical facades.

| Surface | Current evidence | Replacement | Decision | Next leaf |
| --- | --- | --- | --- | --- |
| `tachi_dispatch` | **DONE Batch C (PR #822):** the model-facing route and folded-compat entry are absent; the internal handler remains reachable through the canonical task path. Open migration tracker #757 owns later reconciliation, not this deletion again. | `tachi_staff` (the Task dispatch action itself was retired later, #1319-C2) | Deleted. | — |
| `tachi_board` | **DONE Batch C (PR #822):** the model-facing route and folded-compat entry are absent; the internal board handler remains behind the canonical task path. Open migration tracker #757 owns later reconciliation, not this deletion again. | `tachi_task(action="board")` | Deleted. | — |
| `approve_merge` | **DONE Batch C (PR #822):** the direct route is absent; PR merge remains on `tachi_gh(action="safe_merge")`. Open migration tracker #757 remains active for other surfaces. | `tachi_gh(action="safe_merge")` for PRs. | Deleted direct route; Task merge retired later. | — |

### Batch D: Split Overloaded Facades Before Deleting Actions

These are not route deletions yet. They reduce cognitive load by moving actions
to the correct facade, then deleting duplicate action aliases.

| Surface | Current evidence | Replacement | Decision | Next leaf |
| --- | --- | --- | --- | --- |
| `tachi_task` PR actions | **DONE under open #757 — PR-action slice only:** removed from `tachi_task` enum/router; canonical only on `tachi_gh`. Shared lifecycle handlers remain under `task_lifecycle` for `tachi_gh` / ship. | `tachi_gh` for PR/GitHub work. | Deleted dual entry. | — |
| `tachi_task` tuning actions | **DONE under #1426:** `route_simulate` (all retired)/`proposals`/`review_proposal`/`apply_proposals` are gone from the `TachiTaskAction` enum and router; `FromStr` rejects them with a pointer at the new surface. Handlers live at `tune_ops/route_policy/`. | `tachi_tune(action='route_simulate'\|'route_proposals'\|'route_review'\|'route_apply')`, admin-only by omission from every profile pattern array. | Extracted. | — |
| `tachi_memory` tuning actions | **DONE under #1426:** `recall_simulate`/`recall_proposals`/`review_recall_proposal`/`apply_recall_proposals` are gone from `TACHI_MEMORY_ACTIONS`, the action schema, and the router; the handlers moved to `tune_ops/recall_*`. | `tachi_tune(action='recall_simulate'\|'recall_proposals'\|'recall_review'\|'recall_apply')`, admin-only. | Extracted. | — |
| `tachi_save` shorthand (retired) | Absent from current profile allow-lists and the live router. | `tachi_memory(action="save")`. | Retired (executed). | — |
| `tachi_briefing` shorthand (retired) | Absent from current profile allow-lists and the live router; canonical briefing remains on `tachi_memory` and feature-scoped `tachi_task(action="brief")`. | `tachi_memory(action="briefing")`. | Retired (executed). | — |

### Batch E: Admin Quarantine, Not Immediate Deletion

These groups are too dangerous to delete without a separate owner decision. They
should remain hidden from default agents, but they are legitimate operator or
runtime escape hatches.

| Group | Current evidence | Decision | Reason |
| --- | --- | --- | --- |
| Vault write/get tools | `vault_*` admin names are listed in router coverage at `tool_profile_router_coverage.rs:97-105`; only `vault_status` is in daily patterns at `patterns.rs:139`. | Keep admin-only. | Credential management and diagnostics; do not expose secrets to daily agents. |
| Sandbox policy tools | `sandbox_*` admin names at `tool_profile_router_coverage.rs:84-89`. | Keep admin-only. | Security policy management and audit. |
| Hub governance and VC tools | `hub_*` and `vc_*` admin names at `tool_profile_router_coverage.rs:64-72` and `:106-109`; recommendation/read surfaces remain non-admin. | Keep admin-only; do not expand a model-facing hub facade. | Retain only the operator enforcement required by #1467; broad capability intelligence is not a product surface. |
| Pack/projection tools | `pack_*` and `projection_list` admin names at `tool_profile_router_coverage.rs:77-82`. | Keep admin-only. | Pack lifecycle is operator/admin, not daily MCP. |
| ~~Raw domain CRUD~~ | Deleted in #757 (#972): `register_domain`/`get_domain`/`list_domains`/`delete_domain` and the `domains` registry table/store layer are gone. The free-text `memories.domain` filter field remains. | Deleted. | Registry was feature-dead (0 production rows, zero external callers). |
| Graph/state primitives | **DONE under open #757 — graph/state slice only; dead-code layer removed #913:** MCP registration removed for `add_edge` / `set_state` / `get_state` / `get_edges` / `memory_graph`; the tachi-server-side `MemoryServer` facade helpers that used to wrap them (`graph_state_ops.rs`, `tools/graph_state_facade.rs`) were deleted outright once #913 found zero remaining in-crate callers. Only the `memcore::MemoryStore` boundary (`store/graph.rs`, `store/state.rs`) remains, called directly by in-crate consumers (auto-link, contradiction detection, wiki lint, orchestrator/foundry state, etc.). | Internal only; agents use facades. | Internalized off MCP surface. |
| Runtime/adapter primitives | `recall_context`, `capture_session`, compaction, and section tools live in operate patterns at `patterns.rs:84-108`. | Keep out of standard; do not delete yet. | Host adapters may own these calls. |

### Batch F: Worker Escape Hatches After Action-Level Filtering

Action-level filtering resolves the facade/profile mismatch. Lead and Worker
discovery now expose only the five product facades; Worker action policy keeps
recursive staffing and GitHub mutation denied. Retained diagnostic and
compatibility routes remain physically registered for explicit Ops/admin use.

| Surface | Current evidence | Decision | Remaining work |
| --- | --- | --- | --- |
| `tachi_complete` (retired) | The direct route is retired; delegates complete work through the action-scoped `tachi_task(action="complete")` path. | Retired (executed). | None. |
| `tachi_unstick` | The route remains registered but is absent from ordinary Lead/Worker discovery. | Retain for explicit Ops/admin compatibility; do not claim deletion. | Decide physical retirement separately. |
| `run_skill` (retired) | The direct route is retired; `tachi_skill` is also absent from ordinary Lead/Worker discovery. | Retired (executed; skills are static reviewed now). | None. |
| `tachi_event` | The route remains registered but is absent from ordinary Lead/Worker discovery. | Retain for explicit Ops/admin compatibility; do not claim deletion. | Decide whether event append/query folds into memory/task. |
| `runtime_info` and `tachi_tools` | Routes remain registered but ordinary Lead/Worker discovery no longer exposes them. | Retain for explicit Ops/admin compatibility; do not claim deletion. | Decide physical retirement separately. |
| `tachi_verify` | The route remains registered for explicit Ops/admin compatibility; verification law does not require model-facing default discovery. | Retain outside ordinary profiles. | None until verification evidence is absorbed elsewhere. |

## Proposed Leaf Queue

1. **DONE PR #756 — profile-hid the first old direct names.**
   Raw memory and direct kanban names were removed from non-admin profiles while
   routes remained available for admin/backcompat at that migration stage.
2. **Migrate raw memory direct callers.**
   Convert tests, CLI text, and internal dogfood to `tachi_memory` actions.
   Then retired (executed): native `search_memory`, `save_memory`, and `remember` MCP wrappers, and possibly
   `get_memory` (retired) MCP wrappers.
3. **Retire direct kanban MCP routes.**
   Move direct tests to handlers or task/arena facades, then remove
   `check_inbox`, `post_card`, and `update_card` from MCP (executed, all retired).
4. **DONE Batch C via PR #822; open #757 tracks later migration work.**
   (all retired) `tachi_dispatch`, `tachi_board`, and the direct `approve_merge` route are no
   longer model-facing; GitHub merge behavior remains on `tachi_gh`.
5. **Fold direct wiki aliases.**
   Move docs/tests/callers to `tachi_wiki` actions, then delete direct wiki
   search/write aliases that remain only for compatibility.
6. **Split overloaded task/memory tuning actions.**
   Move self-tuning actions to an admin-only `tachi_tune` or equivalent before
   deleting duplicate action aliases.
7. **DONE — introduce action-level filtering for delegates.**
   Delegates discover exactly the five product facades and use their
   action-scoped Worker policy. The direct route `tachi_complete` is retired;
   `run_skill` is retired too. `tachi_skill` and `tachi_unstick` remain only for
   explicit Ops/admin compatibility.
8. **Decide admin facade shape.**
   Either keep admin as full bypass for emergency use only, or replace it with
   explicit admin facades (`tachi_admin`, `tachi_hub`, `tachi_vault`) and a
   separate emergency `full` profile.

## Non-Goals For Deletion PRs

- Do not delete vault, sandbox, hub, pack, or runtime adapter internals in a
  surface cleanup PR.
- Do not delete worker escape hatches until delegate profile can filter facade
  actions, not just tool names.
- Do not keep old names only because an old client might exist. #566 policy is
  hard retire unless there is a live owner-controlled caller that cannot be
  migrated in the same campaign.
- Do not hide a tool and call the product simplified. Hiding is a transition
  step; the end state is fewer callable entrypoints and fewer duplicated
  actions.
