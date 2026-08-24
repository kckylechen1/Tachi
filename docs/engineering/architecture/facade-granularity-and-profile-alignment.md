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

- Facades are the right idea, but several are **overloaded**. `tachi_task` carries **10 actions**
  (28 before the 4 PR-lifecycle duplicates were removed to `tachi_gh` in #757);
  `tachi_memory` carries **9** actions and `tachi_gh` carries **19**; its former
  claim/release aliases are retired in #1688 in favor of `tachi_task`.
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
| `tachi_task` | 10 | Task lifecycle/read actions; PR duplication resolved (#757), route tuning extracted (#1426), closure moved to `tachi_gh` (#1713), and retired actions removed (#1683 C1a, #1712 C1b-1, #1687 C1c) |
| `tachi_memory` | 9 | Contracted in #1689 to search/get/save/briefing/checkpoint/alerts/ask/extract_facts/consolidate; health, maintenance, ingestion, and pattern evidence use their canonical status, operator, adapter, or internal owners. |
| `tachi_gh` | 19 | GitHub primitives plus PR lifecycle and `close_loop` |
| `tachi_tune` | 8 | Extracted in #1426 — admin/operator only, absent from every profile pattern array |
| `tachi_skill` | 2 | Contracted to pure static `discover` / `run` (#1690) |
| `tachi_wiki` / `tachi_verify` | 4 / 4 | Healthy |

`tachi_skill` and `tachi_wiki` demonstrate that a **medium-grained facade
(~5-7 actions) works well** (the 7-action `tachi_arena` was retired in #1319-D2). The problem is not facades; it is unbounded facades.

### Concrete confusion points

1. **PR lifecycle dual entry (resolved #757).** `link_pr` / `pr_status` / `pr_handoff` / `release_note`
   now live only on `tachi_gh`. The previous `tachi_task` compatibility aliases were deleted so agents
   no longer guess which entry point to call.
2. **`merge` was semantically split, then resolved — the `tachi_task` `merge` action was retired (#1683 C1a).** It used to mean
   local worktree merge, distinct from `tachi_gh(safe_merge)`'s GitHub PR merge — same word, different
   machine. `tachi_task(merge)` is retired; `tachi_gh(safe_merge)` is now the only `merge` on either facade.
3. **Briefing then appeared in three surfaces** (historical) — `tachi_briefing` (standalone, since retired),
   `tachi_memory(briefing)`, and the feature-scoped `tachi_task(brief)`. `save` is duplicated
   across `tachi_save` (retired shorthand) and `tachi_memory(save)`.
4. **Self-tuning actions were interleaved with execution (resolved #1426).** Eight of the nine —
   `route_simulate` / `proposals` / `review_proposal` / `apply_proposals` (task side) and
   `recall_simulate` / `recall_proposals` / `review_recall_proposal` / `apply_recall_proposals`
   (memory side) — left the daily surfaces for `tachi_tune`. Pattern evidence is now
   accepted only through the context-bound internal completion/close-loop seam; ordinary
   agents cannot submit feedback that changes counters or ranking.

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

> **Superseded by #1713.** The proposal below to introduce a separate `tachi_flow` lifecycle facade is
> retained only as historical analysis. #1713 relocated closure to the existing Coordinate-tier
> `tachi_gh(action='close_loop')`, removed `tachi_workflow`, and kept lifecycle status on `tachi_task`.

```diagram
Now                            Proposed
tachi_task (10) ─────┬──▶ tachi_task    execution core: complete/status/board (3)
                     ├──▶ tachi_gh      lifecycle closure: close_loop (1) [#1713]
                     ├──▶ tachi_gh      all PR lifecycle (already isolated, #757)
                     └──▶ tachi_tune    self-tuning: route_simulate/route_proposals/route_review/route_apply (DONE #1426)

tachi_memory (9) ────────▶ search/get/save/briefing/checkpoint/alerts/ask/extract_facts/consolidate
```

`tachi_tune` (self-optimization) stays out of the `standard` surface and is reached only via `admin`
profile or explicit `tachi_tools` discovery. As shipped in #1426 that is enforced by omission: the
name appears in none of the OBSERVE/REMEMBER/COORDINATE/OPERATE pattern arrays, and `tool_visible`
short-circuits to true only for admin/full profiles.

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
But a facade packs many capabilities behind one name (`tachi_task` = 10 actions), so a profile can
only allow or deny the *entire* `tachi_task` — it cannot deny just `dispatch` (the retired Task dispatch action).

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

**The `delegate` allow-list once omitted `tachi_task` entirely.** (Historical: the retired `dispatch` action could not be denied alone.) Today `delegate` carries an action-scoped `tachi_task` subset (`complete`/`status`/`board`/`brief` per `tool_profiles/patterns.rs` + `action_policy.rs`) — the coarse-facade problem this passage warned about was resolved by action-level policy.
the worker (recursive dispatch). The cost: workers cannot use `complete` or `status` from the
facade, and must fall back to standalone legacy tools (`tachi_task(action="complete")`, `tachi_unstick`) that never
moved into a facade.

> This is the smoking gun: **`delegate` still needs a pile of un-faceted legacy tools precisely because
> the facade is too coarse for `ToolProfile` to deny `dispatch` alone.**

## 4b. `handoff_ops` deprecated — memo vs. baton split (resolved #1016)

`handoff_ops` (#157-era `handoff_leave`/`handoff_check`/`tachi_handoff`) tried to cover two
different jobs with one loosely-addressed, read-then-write memo shape. Both jobs now have a
purpose-built home, and #1016 rules `handoff_ops` **deprecated, not deleted**:

- **Same-host advisory message** → `tachi_a2a(action='respond')` (#1751).
  Explicit AgentIdentity admission, idempotency, and delivery receipts replace the old memo loop.
- **Structured baton for a resumed/handed-off task** → `tachi_task(action='handoff')`.
  Its canonical WorkClaim fields carry the claimant, worktree, expected head, lease, and
  transition evidence a resuming session needs, which the old memo shape never had.

`promote_issue` (memo → GitHub issue) has no replacement yet and is unaffected. Deletion of
`handoff_ops` is a later, separately-audited cut (#757-style) once callers are confirmed migrated;
this ruling only marks the surface deprecated (module doc, facade tool descriptions, and a
`deprecated` field on `handoff_leave`/`handoff_check` responses) so agents see the pointer at call
time. Refs #1016.

**Update (#1099, 2026-07-17): that later cut landed.** The caller-sweep evidence gate this
paragraph was waiting on came back clean (zero live external callers), so `handoff_leave`,
`handoff_check`, and `tachi_handoff`'s 'leave'/'check' actions are deleted — not just
deprecated — along with the briefing "Cross-project (global handoffs)" projection and the
dedicated handoff-memory GC branch. `tachi_handoff` survives narrowed to `promote_issue`
only, which is now a wind-down capability over pre-existing `handoff:<id>` rows (no writer
remains); those rows are retained read-only rather than migrated or force-deleted. See
`handoff_ops.rs`'s module doc for the full evidence and data-policy record.

## 5. DispatchProfile — keep it

`DispatchProfile` (`claude_plan`, `codex_55_review`, `kimi_arch`, …) answers "which sub-agent/model to
hire and what evidence it must return." It is orthogonal to the tool surface, logically independent,
and actively used. **No change proposed.**

## 6. Recommendation

```diagram
╭─────────────────────────────────────────────────────────────────────╮
│ 1. Re-slice facades by cognitive domain; each action ≤ ~7;           │
│    eliminate cross-facade duplicate entry points (PR lifecycle).     │
│ 2. Extract self-tuning into a dedicated tachi_tune, kept out of the  │
│    standard surface. DONE — 8 actions, #1426.                        │
│ 3. Collapse ToolProfile to 3 tiers: standard / delegate / admin;    │
│    mark observe/remember/coordinate/operate deprecated.             │
│ 4. Upgrade profile filtering to the ACTION level → let delegate      │
│    expose tachi_task with only complete/status, deny dispatch.       │
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
3. Ship the low-risk wins first: delete the duplicate PR actions from `tachi_task` (done, #757),
   then extract `tachi_tune` (done, #1426).
4. Design and implement action-level profile filtering (larger change; needs its own spec).
