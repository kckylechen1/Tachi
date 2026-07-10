---
title: "Facade Granularity & Profile Alignment"
summary: "Diagnoses facade action overload and the tool-vs-action granularity mismatch between facades and ToolProfile, and proposes cognitive-domain facade re-slicing plus action-level profile filtering."
category: "engineering/architecture"
organize: true
---
# Facade Granularity & Profile Alignment

This document is a design analysis, not yet an approved implementation plan. It is the
follow-up companion to [Tool Surface Bundle Plan](./tool-profile-plan.md) and
[Kernel Surface V1](./kernel-surface-v1.md).

It answers three questions raised during a facade review:

1. Do our facades expose too many actions, to the point that agents get lost?
2. Should we flatten everything into many single-purpose tools, or keep facades?
3. The `ToolProfile` layer was designed early — is it still doing anything useful?

## TL;DR

- Facades are the right idea, but several are **overloaded**. `tachi_task` carries **24 actions**
  (28 before the 4 PR-lifecycle duplicates were removed to `tachi_gh` in #757);
  `tachi_memory` and `tachi_gh` carry **16 each**.
- Neither extreme (flatten to 100+ tools, or keep stuffing mega-facades) is correct. The fix is
  **re-slicing facades along agent cognitive domains**, keeping each facade at roughly **7±2 actions**.
- The `ToolProfile` bundle system (`observe/remember/coordinate/operate/admin`) is **largely dead**:
  `standard_minimal` / `delegate_minimal` hard-coded allow-lists bypass the bundles for every default path.
- More fundamentally, **facades broke `ToolProfile`'s granularity**. `ToolProfile` filters by *tool
  name*, but a facade hides many capabilities behind one name, so a profile can only allow or deny an
  entire facade — never a single action.
- The `delegate`/worker surface is the clearest victim of this mismatch, and it proves the point.
- `DispatchProfile` (which agent/model to hire) is unrelated and healthy — keep it.

## 1. Facade action inventory

| Facade | Actions | Verdict |
| :--- | ---: | :--- |
| `tachi_task` | 24 | Overloaded — lifecycle + self-tuning still bundled (PR duplication resolved, #757) |
| `tachi_memory` | 16 | Overloaded — `recall_*` tuning mixed with daily ops |
| `tachi_gh` | 16 | Duplicates task's PR lifecycle |
| `tachi_arena` | 7 | Healthy |
| `tachi_skill` | 5 | Healthy |
| `tachi_wiki` / `tachi_verify` | 4 / 4 | Healthy |

`tachi_arena`, `tachi_skill`, and `tachi_wiki` demonstrate that a **medium-grained facade
(~7 actions) works well**. The problem is not facades; it is unbounded facades.

### Concrete confusion points

1. **PR lifecycle dual entry (resolved #757).** `link_pr` / `pr_status` / `pr_handoff` / `release_note`
   now live only on `tachi_gh`. The previous `tachi_task` compatibility aliases were deleted so agents
   no longer guess which entry point to call.
2. **`merge` is semantically split.** `tachi_task(merge)` = local worktree merge;
   `tachi_gh(safe_merge)` = GitHub PR merge. Same word, different machine.
3. **`briefing` appears in three places** — `tachi_briefing` (standalone), `tachi_memory(briefing)`,
   `tachi_task(briefing)`. `save` is duplicated across `tachi_save` and `tachi_memory(save)`.
4. **Self-tuning actions are interleaved with execution.** `route_simulate` / `proposals` /
   `review_proposal` / `apply_proposals` (task side) plus `recall_simulate` / `recall_proposals` /
   `review_recall_proposal` / `apply_recall_proposals` / `pattern_feedback` (memory side) — **9 actions**
   that ordinary task-executing agents almost never use, occupying prime real estate in the default surface.

## 2. Flatten vs. facade — pick the middle

- **Flatten to 100+ tools** is exactly what facades were created to avoid. `tool-profile-plan.md`
  explicitly warns against showing "the full admin catalog (100+ tools)". Flattening blows up context
  and paralyzes tool selection.
- **Keep stuffing mega-facades** just relocates the problem: the "which tool?" difficulty becomes a
  "which action?" difficulty *inside* one tool, plus an exploding schema description.

The design rule:

> **One facade = one coherent noun/cognitive domain, action count ≤ ~7±2.**

Today's facades are sliced by *backend module* (the `task` module → one facade). They should be sliced
by the **agent's mental task**.

```diagram
Now                            Proposed
tachi_task (24) ─────┬──▶ tachi_task    execution core: plan/dispatch/complete/status/board/wait (6)
                     ├──▶ tachi_flow    lifecycle: intake/cycle_status/cycle_plan/close_loop/ux_matrix (5)
                     ├──▶ tachi_gh      all PR lifecycle (already isolated, #757)
                     └──▶ tachi_tune    self-tuning: route_simulate/proposals/review/apply (isolated)

tachi_memory (16) ───┬──▶ tachi_memory  daily: search/get/save/ask/checkpoint/alerts (~7)
                     └──▶ tachi_tune    recall_*/pattern_feedback (isolated)
```

`tachi_tune` (self-optimization) stays out of the `standard` surface and is reached only via `admin`
profile or explicit `tachi_tools` discovery.

## 3. ToolProfile status — mostly hollowed out

There are two unrelated "profile" concepts. This section is about **`ToolProfile`** (tool-surface
trimming), defined in [`profiles/types.rs`](../../../crates/tachi-server/src/profiles/types.rs).

The early design had five additive bundles: `observe / remember / coordinate / operate / admin`.
But v1.0 introduced the facade surface and, with it, `standard_minimal` — a hard-coded 14-tool
allow-list ([`profiles/patterns.rs`](../../../crates/tachi-server/src/profiles/patterns.rs) → `STANDARD_MINIMAL_TOOL_PATTERNS`).
The net effect:

- **default = `standard` = the hard allow-list**, bypassing bundles
- **worker = `delegate` = another hard allow-list**
- **only `admin` actually walks the bundles**

→ `observe/remember/coordinate/operate` have almost no live code path. They activate only when
someone hand-types `--profile observe+coordinate`. That is dead design.

### The deeper problem: facades broke tool-level filtering

`ToolProfile` trims by **tool name** via glob matching
([`profiles/matching.rs#L77-L112`](../../../crates/tachi-server/src/profiles/matching.rs)).
But a facade packs many capabilities behind one name (`tachi_task` = 24 actions), so a profile can
only allow or deny the *entire* `tachi_task` — it cannot deny just `dispatch`.

## 4. Case study: the `delegate`/worker surface proves the mismatch

`delegate` is the `ToolProfile` given to dispatched **worker sub-agents**. The full wiring:

```diagram
DispatchProfileDef.tool_profile="delegate"
   │  (dispatch_ops/dispatch.rs#L165 passes params.tool_profile)
   ▼
generate_mcp_config → writes child env TACHI_PROFILE=delegate
   │  (dispatch_ops/mcp_config.rs#L29-L30)
   ▼
worker boot → parse_tool_profile("delegate") = ToolProfile::delegate()
   │  (profiles/matching.rs#L21: "delegate"|"worker"|"subagent" are aliases)
   ▼
DELEGATE_MINIMAL_TOOL_PATTERNS (7-tool allow-list) filters worker's visible tools
```

Two important nuances:

**Not every worker is `delegate`.** The tool surface depends on the worker's role, per the
`DispatchProfileDef` table in [`dispatch_profile/mod.rs`](../../../crates/tachi-server/src/dispatch_profile/mod.rs):

| dispatch profile | worker's tool_profile |
| :--- | :--- |
| claude_plan / glm_impl / opencode_builder | `delegate` (doers) |
| codex_55_review | `standard` (needs to see more) |
| kimi_arch / deepseek_explore / kimi_ux | `observe` (read-only review/exploration) |

**The `delegate` allow-list deliberately omits `tachi_task`.** Including it would hand `dispatch` to
the worker (recursive dispatch). The cost: workers cannot use `plan`, `complete`, or `status` from the
facade, and must fall back to standalone legacy tools (`tachi_complete`, `tachi_unstick`) that never
moved into a facade.

> This is the smoking gun: **`delegate` still needs a pile of un-faceted legacy tools precisely because
> the facade is too coarse for `ToolProfile` to deny `dispatch` alone.**

## 5. DispatchProfile — keep it

`DispatchProfile` (`claude_plan`, `codex_55_review`, `kimi_arch`, …) answers "which sub-agent/model to
hire and what evidence it must return." It is orthogonal to the tool surface, logically independent,
and actively used. **No change proposed.**

## 6. Recommendation

```diagram
╭─────────────────────────────────────────────────────────────────────╮
│ 1. Re-slice facades by cognitive domain; each action ≤ ~7;           │
│    eliminate cross-facade duplicate entry points (PR lifecycle).     │
│ 2. Extract self-tuning (~9 actions) into a dedicated tachi_tune,     │
│    kept out of the standard surface.                                 │
│ 3. Collapse ToolProfile to 3 tiers: standard / delegate / admin;    │
│    mark observe/remember/coordinate/operate deprecated.             │
│ 4. Upgrade profile filtering to the ACTION level → let delegate      │
│    expose tachi_task with only plan/complete/status, deny dispatch.  │
│ 5. Keep DispatchProfile unchanged.                                   │
╰─────────────────────────────────────────────────────────────────────╯
```

Item 4 is what makes `ToolProfile` useful again: **the profile's filtering granularity must match the
facade's granularity.** Once facades push granularity down to the action, profiles must be able to
filter by `tool.action`; otherwise the two systems keep undercutting each other, and workarounds like
the `delegate` legacy-tool pile persist.

### Suggested sequencing

1. Stress-test this design (oracle / review).
2. Land this document as the agreed direction.
3. Ship the low-risk wins first: delete the duplicate PR actions from `tachi_task`, then extract
   `tachi_tune`.
4. Design and implement action-level profile filtering (larger change; needs its own spec).
