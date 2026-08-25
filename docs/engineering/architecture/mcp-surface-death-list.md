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

- `standard` is a 16-tool allow-list in `STANDARD_MINIMAL_TOOL_PATTERNS` after
  removing `tachi_arena` and `tachi_agent_eval`; its `tachi_task` schema also
  omits operator-only `dispatch`.
- `delegate` is an 11-tool allow-list in `DELEGATE_MINIMAL_TOOL_PATTERNS`.
- `admin` is not a curated management profile. `tool_visible()` returns true
  immediately for admin, so admin exposes the full native catalog plus any
  directly exposed proxy/skill tools.
- The source currently contains 103 routed `#[tool]` methods. The old 132-tool
  and 76/56 breakdown is retired; regenerate profile-level counts under #1319
  rather than treating that historical split as current truth.

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

These tools are the public surface to optimize around. They should be small,
well-documented, and allowed in `standard` unless a security reason says
otherwise.

| Tool | Role | Notes |
| --- | --- | --- |
| `tachi_tools` | discovery | Must stay visible so agents stop guessing tool names. |
| `runtime_info` | routing identity | Cheap route/profile self-check. |
| `tachi_status` | health | Session-start health and readiness signal. |
| `tachi_memory` | memory facade | Canonical search/get/save/extract/briefing/checkpoint/alerts surface. |
| `tachi_task` | task lifecycle facade | Historical (2026-07-07 base) plan/dispatch/complete/status/board/wait surface, since narrowed further — `dispatch`/`cancel`/`wait` were removed by #1319-C2 and `plan`/`cycle_plan`/`recommend`/`refine_issues`/`merge`/`ux_matrix` were retired by #1683 C1a. Current action set lives in `TachiTaskAction::PRIMARY`, not in this historical row. |
| `tachi_tune` | route/recall tuning | Extracted from task/memory in #1426. Admin/operator only — never part of the standard keep-set. |
| `tachi_verify` | verification ledger | Keep as evidence ledger for dispatch and safe-merge workflows. |
| `tachi_wiki` | wiki facade | Canonical wiki search/browse/read/write facade. |
| `tachi_skill` | skill facade | Canonical discover/run facade. |
| `tachi_web_search` | web search intake | Keep only as a compatibility intake where a host lacks web search; #1467 leaves research reasoning with the host model. |
| `vault_status` | safe credential readiness | Read-only status only; write/get vault tools stay out of daily profiles. |
| `tachi_gh` | GitHub/evidence facade | Keep if GitHub remains part of ship/evidence workflows; move duplicated task PR actions here. |

`tachi_save` and `tachi_briefing` were convenience shorthands (both retired since), not separate
capabilities. Keep them only if dogfood shows the shorthand materially reduces
friction; otherwise fold them into `tachi_memory`.

## Historical Death List

### Batch A: Executed — Hard-Retire Old Direct Memory Names

The raw memory routes (`search_memory`, `save_memory`, `remember`, `get_memory`, `find_similar_memory`)
have been retired and removed from the MCP tool router. All operations are canonically unified under `tachi_memory`.

| Surface | Current evidence | Replacement | Decision | Next leaf |
| --- | --- | --- | --- | --- |
| `search_memory` | **DONE Batch A:** Retired from MCP tool router. | `tachi_memory(action="search")` | Hard-retired direct MCP name. | — |
| `save_memory` | **DONE Batch A:** Retired from MCP tool router. | `tachi_memory(action="save")` | Hard-retired direct MCP name. | — |
| `remember` | **DONE Batch A:** Retired from MCP tool router. | `tachi_memory(action="save")` | Hard-retired direct MCP name. | — |
| `get_memory` | **DONE Batch A:** Retired from MCP tool router. | `tachi_memory(action="get")` | Hard-retired direct MCP name. | — |
| `find_similar_memory` | **DONE Batch A:** Retired from MCP tool router and operate profile. | `tachi_memory(action="search")` | Hard-retired direct MCP name. | — |
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
| `tachi_browse` | Facade read tool remains in observe patterns at `patterns.rs:23`. | `tachi_wiki(action="browse")` if browse stays a wiki action. | Fold candidate, but not first cut. | Confirm delegate dogfood before removing; delegate currently exposes `tachi_browse`. |

### Batch B: Executed — Retire Direct Kanban Tools

These routes are implementation details of the dispatch/card board and have been
removed from the model-facing MCP router (`kanban_tool_router` retired). Internal
storage/handlers remain for test and internal orchestration.

| Surface | Current evidence | Replacement | Decision | Next leaf |
| --- | --- | --- | --- | --- |
| `check_inbox` | **DONE Batch B:** Removed from MCP tool router; tests migrated to internal handlers. | `tachi_task(action="board")` / `tachi_a2a`. | Deleted direct MCP route. | — |
| `post_card` | **DONE Batch B:** Removed from MCP tool router; tests migrated to internal handlers. | `tachi_staff(action="start")` / `tachi_a2a`. | Deleted direct MCP route. | — |
| `update_card` | **DONE Batch B:** Removed from MCP tool router; tests migrated to internal handlers. | `tachi_task(action="status")` / `tachi_a2a`. | Deleted direct MCP route. | — |

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
| `tachi_save` shorthand (retired) | Standard allow-list includes it at `patterns.rs:130`. | `tachi_memory(action="save")`. | Retired (executed). | Dogfood decision after Batch A. |
| `tachi_briefing` shorthand | Standard allow-list includes it at `patterns.rs:128`; separate briefing also exists in `tachi_memory` and the feature-scoped `tachi_task(action="brief")`. | `tachi_memory(action="briefing")`. | Retired (executed). | Dogfood decision after action-level profile design. |

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

### Batch F: Worker Escape Hatches, Do Not Delete Before Action-Level Filtering

These tools look like standalone clutter, but they currently compensate for a
real facade/profile mismatch: `delegate` omits `tachi_task` to prevent recursive
dispatch, so workers need separate completion, rescue, and skill execution
entrypoints.

| Surface | Current evidence | Decision | Blocker before deletion |
| --- | --- | --- | --- |
| `tachi_complete` | Delegate allow-list includes it at `patterns.rs:158`; facade granularity notes explain delegate cannot expose all of `tachi_task` because that would expose dispatch. | Retired (executed). | Let delegate call `tachi_task(action="complete")` without `dispatch`. |
| `tachi_unstick` | Delegate allow-list includes it at `patterns.rs:157`; observe patterns include it at `patterns.rs:22`. | Keep as worker self-rescue. | Provide equivalent rescue path in a worker-safe facade. |
| `run_skill` | Delegate allow-list includes it at `patterns.rs:161`; remember patterns include it at `patterns.rs:43`. | Retired (executed; skills are static reviewed now). | Replace with action-scoped `tachi_skill(action="run")` that is safe for delegates. |
| `tachi_event` | Delegate allow-list includes it at `patterns.rs:153`. | Keep while continuity events are worker-facing. | Decide whether event append/query folds into memory/task. |
| `runtime_info` and `tachi_tools` | Standard/delegate allow-lists include both at `patterns.rs:115-117` and `:148-149`; unknown-tool errors route users to `tachi_tools`. | Keep. | None; these are readiness/discovery, not product clutter. |
| `tachi_verify` | Standard allow-list includes it at `patterns.rs:125`; dispatch law requires verification evidence. | Keep. | None until verification ledger is absorbed elsewhere. |

## Proposed Leaf Queue

1. **DONE PR #756 — profile-hid the first old direct names.**
   Raw memory and direct kanban names were removed from non-admin profiles while
   routes remained available for admin/backcompat at that migration stage.
2. **Migrate raw memory direct callers.**
   Convert tests, CLI text, and internal dogfood to `tachi_memory` actions.
   Then retired (executed): `search_memory`, `save_memory`, `remember`, and possibly
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
7. **Introduce action-level filtering for delegates.**
   Only after this could `tachi_complete`, `tachi_unstick`, `run_skill`, and (all since retired)
   similar worker escape hatches be folded safely.
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
