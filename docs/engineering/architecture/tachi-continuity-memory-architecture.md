# Tachi Continuity Memory Architecture

**Status:** architecture design + first implementation slice
**Date:** 2026-06-23
**Related docs:**
- [`pattern-timeline-bonding-memory.md`](./pattern-timeline-bonding-memory.md) — original design + cold review
- [`../../wiki/agent/tachi/Tachi-图书馆架构设计.md`](../../wiki/agent/tachi/Tachi-图书馆架构设计.md) — Karpathy LLM Wiki mapping

---

## 1. Design intent: an alignment bridge

The base model is aligned by its vendor to a generic notion of helpfulness, honesty, and safety. That alignment is useful but not identical to the user's own alignment — the user's actual values, judgment habits, thresholds for evidence, and long-term interests.

The continuity memory system is therefore **not** a "remember more" cache. It is a **dynamic, auditable bridge between vendor alignment and user alignment**:

- **Pattern memory** learns the user's cognitive and judgment structures.
- **Timeline memory** records how those structures were discovered, defended, revised, and validated.
- **Bonding layer** accumulates a per-user shared protocol that compresses communication bandwidth.
- **Affect** remains a tone-only read model; it is not bonding and it is not an emotional state of the model.

Because this bridge can drift toward the user's short-term feedback at the expense of their long-term interests, the system keeps an **over-fit brake** and an **un-revocable cold seat** as structural safeguards.

### 1.1 Core invariant: one ledger, many read models

Continuity memory is an **append-only evidence substrate** plus projected read models. Pattern, timeline, bonding, affect, lorebook/world-book, eval, wiki, and skill crystallization should all be derived from the same evidence ledger, not built as isolated caches.

This means:

- The source of truth is `tachi_events`.
- Projections are read models used for search, prompt context, wiki drafts, skills, or evaluation.
- Affect/emotion projections can influence tone/reminder style only; they must not mutate facts, scores, routing, trading decisions, or execution.
- Pattern and bonding memory are not obedience knobs. They can compress communication and surface learned judgment structures, but cold-seat review and label calibration remain mandatory.
- A2A transport should share evidence, open questions, and provenance across agents; it should not force agents to inherit conclusions.

---

## 2. Core abstractions

### 2.1 Pattern memory — what the user recognizes

Pattern memory stores the user's **cognitive and judgment structures**: how they classify events, what evidence they trust, when they revise a belief, and what recurring shapes they have found in prior work.

It is a model of the user's thinking, not merely a catalog of world facts. A pattern such as `religious_leader_political_proxy` is valuable not because the world contains religious leaders, but because the user has learned to recognize a structural shape across multiple domains and can use it to predict and analyze new instances.

Current storage: `ProjectionKind::Pattern` materializes into `/user/patterns/<domain>/<hash>` in the memory DB. Human-readable slugs and wiki drafts are later crystallization targets, not the current projector path shape.
Metadata counters: `seen / hit / miss / confidence / last_seen`.
Authority: usually `CollectOnly` until promoted.

### 2.2 Timeline memory — why a conclusion is trustworthy

Timeline memory stores the **credibility history** of a judgment or pattern. It answers "why is this conclusion trustworthy?" rather than "what did we discuss on which day?".

The timeline is an evolution chain: discovered → defended → revised → externally validated. Each transition is a causal edge with temporal validity. The depth of adversarial testing and external verification is itself evidence for the conclusion's reliability.

Current storage: append-only `tachi_events` plus `ProjectionKind::Timeline` projections under `/timeline/<domain>/<hash>`. Causal graph edges and typed `TimelineEntry` records are design targets, not a committed runtime type yet.
Surface target: `open_threads` at next session start; `tachi_event action=context` already returns timeline projections when projected.

### 2.3 Bonding layer — shared communication protocol

Bonding is a **per-user shared communication protocol**: shorthand references, common investigative history, domain-specific vocabulary, and expected interaction rhythms.

It is **not** an emotional state of the model, nor is it the same thing as an RLHF warmth output. The model does not "feel" attached; it reads a structured read model that compresses bandwidth between user and system.

That said, **affect and warmth are legitimate carriers for bonding**. A warm delivery can make the shared protocol land more effectively, just as an inappropriate cold delivery can undermine it. The key distinction is:
- **Bonding** = the shared context and shorthand the system has accumulated with this user.
- **Warmth/affect** = one delivery carrier that can make bonding feel natural and trustworthy.
- A cold-carrier agent with strong bonding can still be effective; a warm-carrier agent without bonding is only performing generic RLHF politeness.

Current storage: `ProjectionKind::Bonding` materializes into `/user/patterns/bonding/<domain>/<hash>`. `SharedLexicon` is a design target for a richer read model.
Guardrails: `tone_and_reminder_only`; no execution, scoring, portfolio, or fact-mutation effects.

### 2.4 Affect — delivery tone only

Affect projections (`/user/affect/*`) estimate tone, urgency, or reminder intensity. They are explicitly denied scoring, execution, portfolio, and fact-mutation effects. Affect is what the model sounds like; bonding is what the model can assume the user already knows.

### 2.5 Over-fit brake and cold seat

- **Over-fit brake**: prevents the system from optimizing short-term user satisfaction at the expense of the user's long-term interests. Measured by `challenge_rate` and label-quality calibration.
- **Cold seat**: a retained generic-alignment reference. As pattern and bonding memory learn the user's personal alignment, the cold seat prevents unchecked drift. It checks facts and logic independently and never ingests warm-coalition conclusions.

---

## 3. Relationship to existing Tachi subsystems

### 3.1 Memory — pattern memory lives inside it

Pattern memory is already stored in the memory DB under `/user/patterns/*`. It uses the same hybrid search, scorer, and CRUD infrastructure as ordinary memories.

However, ordinary memory recall should **exclude** `/user/patterns/`, `/user/affect/`, and `/lorebook/` by default, just as it excludes `/sft/` training seeds. A dedicated `scope="patterns"` should be added to `tachi_search`.

Integration direction:
- `tachi_memory save` should emit a `memory.saved` continuity event.
- The distill lane can consume `memory.saved` events to discover or update patterns.
- Pattern hits can write concrete instance memories under `/memory/instances/<pattern_id>/`.

### 3.2 Wiki — pattern promotion path

Wiki is the **human-readable, reviewed crystallization** of mature patterns. The flow is:

```
pattern candidate → project to /user/patterns/* → mature (high hit, verified)
  → generate /wiki/drafts/patterns/<name>.md
  → human review
  → promote to /wiki/decision/ or /wiki/runbook/
```

A wiki page derived from a pattern must retain a `pattern_ref` link. It is **not** automatically project truth; it is a reviewed artifact built on pattern evidence.

` tachi_wiki_write ` should optionally recall relevant `/user/patterns` before composing content, so wiki pages can reference or challenge active patterns.

### 3.3 Skill — pattern execution path

Skill is the **executable crystallization** of patterns. Tachi already has a complete skill system (`hub_register`, `run_skill`, `recommend_skill`, `skill_evolve`). Pattern memory should become its **evidence-driven discovery layer**, not a replacement.

Two integration modes:

1. **Recommendation signal** (low risk): `recommend_skill` includes active patterns as context, improving skill matching without changing registration.
2. **Skill generation** (higher risk): a mature pattern can generate a skill definition and be registered. Generated skills should start as `discoverable` or pending review, with a `pattern_ref` field for traceability.

This mirrors the Karpathy LLM Wiki flow:

```
Raw notes / sessions → pattern memory → wiki → skill
```

Tachi maps this as:

```
Notes / memory / session → /user/patterns/* → /wiki/drafts/patterns/* → skill:<name>
```

### 3.4 Eval — the credibility loop

Eval records (`/eval/...`) and `tachi_complete` outcomes feed the continuity ledger. `session.outcome` labels are the conversation-domain substitute for quant's `fwd_return`.

`challenge_rate = ai_corrected / eligible_outcomes` is surfaced in `tachi_status`. Label-quality calibration (`tachi_event action=label_eval`) must pass before the over-fit brake can be trusted.

---

## 4. Backend model strategy

The current backend exposes four lanes: `extract`, `summary`, `distill`, `reasoning`. Available models include SiliconFlow Qwen (`Qwen3.5-27B`, `Qwen2.5-72B-Instruct`) and DeepSeek V4 (`Flash`, `Pro`).

Recommended mapping for continuity memory:

| Lane | Recommended model | Reason |
|---|---|---|
| **extract** | Qwen 3.5-27B or DeepSeek V4 Flash | Fast structured extraction and pattern matching at runtime |
| **summary** | Qwen 3.5-27B or DeepSeek V4 Flash | Compression, cheap and frequent |
| **distill** | DeepSeek V4 Pro or Qwen 72B | Abstracting patterns and timeline entries requires synthesis |
| **reasoning** | DeepSeek V4 Pro | Outcome labeling is the linchpin of the over-fit brake |
| **label eval** | DeepSeek V4 Pro | The judge that calibrates the labeler must itself be strong |

Configuration example:

```bash
export EXTRACT_MODEL="Qwen/Qwen3.5-27B"
export SUMMARY_MODEL="Qwen/Qwen3.5-27B"
export REASONING_MODEL="deepseek-ai/DeepSeek-V4-Pro"
export DISTILL_MODEL="deepseek-ai/DeepSeek-V4-Pro"

export DEEPSEEK_API_KEY="sk-..."
export REASONING_API_KEY="$DEEPSEEK_API_KEY"
export DISTILL_API_KEY="$DEEPSEEK_API_KEY"
export REASONING_BASE_URL="https://api.siliconflow.cn/v1"
export DISTILL_BASE_URL="https://api.siliconflow.cn/v1"
```

Or use the tier system:

```bash
export TACHI_BACKEND_EXTRACT_TIER=fast
export TACHI_BACKEND_SUMMARY_TIER=fast
export TACHI_BACKEND_REASONING_TIER=balanced
export TACHI_BACKEND_DISTILL_TIER=balanced
```

---

## 5. End-to-end data flow

```
User session
    ↓
capture_session emits session.captured              [implemented]
    ↓
TACHI_CONTINUITY_PIPELINE=1                         [implemented, opt-in]
distill lane    → pattern.candidate / bonding.candidate / timeline.candidate
reasoning lane  → session.outcome
    ↓
tachi_event action=project                          [implemented, explicit]
    → idempotently materialize eligible candidates into:
       /user/patterns/<domain>/<hash>
       /user/patterns/bonding/<domain>/<hash>
       /lorebook/<domain>/<hash>
       /user/affect/<domain>/<hash>
       /timeline/<domain>/<hash>
       /project-cycle/<domain>/<hash>
    → update counters (seen / hit / miss / confidence / last_seen)
    ↓
Next session start / explicit context request
    → tachi_event action=context
    → returns projected memories + patterns + lorebook + affect + metrics
    → dispatch can assemble prompt context from projections
```

Implemented integration slice:

```
save_memory emit_continuity=true → memory.saved event
tachi_search scope=patterns      → explicit /user/patterns recall
tachi_wiki_write include_patterns=true → wiki metadata.pattern_refs[]
tachi_skill action=from_pattern  → pending/disabled Hub skill candidate
tachi_domain_adapter lorebook_import → repo lorebook shape → world_book events
```

Target integration still to add:

```
wiki.saved event with optional pattern_ref
pattern hit/miss callback → pattern.hit / pattern.miss event
projection loop → background tachi_event action=project
pattern maturity → /wiki/drafts/patterns/<name>.md → reviewed wiki/runbook
pattern maturity → reviewed promotion of generated skill candidate
```

Adapter boundary:

`tachi_domain_adapter` belongs to the generic core only when it converts an external
repo shape into the neutral continuity ledger. The first implemented action,
`lorebook_import`, maps RomanBath/SillyTavern lorebook entries into `tachi_event`
`world_book` candidates and optional projection. Domain business logic such as
finance tickers, trading lesson paths, or Quant-specific defaults should live in a
Quant adapter pack/fork that emits the same neutral events or normal memory writes;
it should not be exposed by the generic agent surface.

The target runtime path is:

```
Runtime event
    → search recalls top-k projected patterns
    → extract/flash LLM or deterministic matcher confirms match
    → emit pattern.hit / pattern.miss event
    ↓
Pattern matures (hit_rate / confidence threshold + external validation + cold-seat check)
    → generate /wiki/drafts/patterns/<name>.md
    → generate skill:<name> candidate
    → human review
    → promote to wiki + hub skill
```

---

## 6. Current implementation status

### Implemented

- `memory-core` typed `tachi_events` ledger: `TachiEventRecord`, `ProjectionKind`, `AuthorityLevel`, `EffectScope`.
- `memory-server` `tachi_event` facade with six actions: `emit`, `query`, `metrics`, `project`, `context`, `label_eval`.
- `tachi_status` surfaces `challenge_rate` as a read-only continuity metric.
- `capture_session` emits `session.captured`; optional continuity pipeline emits candidates and `session.outcome`.
- `tachi_complete` bridges subagent eval into `task.outcome` / `subagent.evaluated` events.
- `tachi_event action=project` idempotently materializes events into stable projections.
- `tachi_event action=context` returns projected memories + `patterns` + `lorebook` + `affect` read-model sections.
- Pattern/bonding projections maintain `seen / hit / miss / confidence / last_seen` counters.
- Affect projections carry explicit guardrails.
- `tachi_event action=label_eval` provides a label-quality harness.
- `save_memory` supports explicit `emit_continuity=true`, appending a `memory.saved` event with path/category-derived projection hints.
- `tachi_search` supports `scope="patterns"` and excludes continuity projection rows from ordinary `memory` recall.
- `tachi_wiki_write` supports `include_patterns=true`, persisting active pattern references in wiki metadata.
- `tachi_skill(action="from_pattern")` registers a disabled, pending-review, discoverable skill candidate with a `pattern_ref`.
- Complete skill system: `hub_register`, `run_skill`, `recommend_skill`, `skill_evolve`, builtin skills.

### Missing / gaps

1. **No background auto-apply loop**: projection is explicit via `tachi_event action=project`.
2. **No Agent MD crystallization**: the ledger is not yet read when generating agent system prompts.
3. **No cross-process A2A transport**: only a local read model exists.
4. **Label-quality calibration incomplete**: harness exists, needs reviewed held-out data.
5. **Wiki writes do not emit `wiki.saved` events**: wiki entries can carry `pattern_refs`, but the write itself is not yet a ledger event.
6. **Maturity gates are not implemented**: `from_pattern` can register candidates, but hit-rate / validation thresholds do not auto-promote to wiki or approved skills.
7. **Pattern hit/miss feedback is not wired**: counters update from event types if supplied, but runtime recall does not emit hit/miss callbacks.
8. **Pattern recommendation signal for skills is not wired**: `recommend_skill` does not yet use active patterns as context.

---

## 7. Architectural roadmap

### Phase 1 — Calibration and model tiering

**Goal:** make the over-fit brake trustworthy.

1. Configure backend lanes:
   - `extract/summary` → fast model (Qwen 27B / DeepSeek V4 Flash)
   - `distill/reasoning` → strong model (Qwen 72B / DeepSeek V4 Pro)
2. Run `tachi_event action=label_eval` on held-out transcripts.
3. Establish label-quality acceptance criteria before trusting `challenge_rate`.

### Phase 2 — Memory / Wiki / Pattern integration

**Goal:** pattern memory becomes a first-class search and recall citizen.

1. Done: `scope="patterns"` exists on `tachi_search`.
2. Done: `tachi_event action=context` returns a dedicated `patterns` section.
3. Done: `save_memory` can emit `memory.saved` when `emit_continuity=true`.
4. Done: `tachi_wiki_write` can recall and reference active patterns with `include_patterns=true`.
5. Remaining: emit `wiki.saved` events for reviewed wiki writes.

### Phase 3 — Background projection loop

**Goal:** close the loop without requiring manual `tachi_event action=project`.

1. Add a scheduler/cron that calls `project_continuity_events` for eligible candidates.
2. Respect authority levels: only `CollectOnly` candidates are auto-projected; higher authority remains explicit.
3. Add observability: projected count, skipped count, promotion candidates.

### Phase 4 — Skill crystallization

**Goal:** mature patterns become executable skills.

1. Add `recommend_skill` pattern signal (Phase 4a).
2. Done: `tachi_skill action=from_pattern` generates skill candidates from active patterns.
3. Done: generated skills are `discoverable`, disabled, pending review, and carry `pattern_ref` metadata.
4. Remaining: human/maturity review before promoting to `listed`.

### Phase 5 — A2A and cold seat transport

**Goal:** share evidence and open questions across agents and processes without homogenizing conclusions.

1. Wire a pub/sub or subscription transport on top of `tachi_events`.
2. Guarantee at least one cold-seat agent per coalition that does not ingest shared timeline conclusions.
3. Enforce: A2A shares facts + open questions, never conclusions.

---

## 8. Concrete file-level integration points

### 8.1 `patterns` section in `tachi_event action=context`

- **File:** `crates/memory-server/src/continuity_ops.rs`
- **Function:** `build_continuity_context`
- **Current behavior:** after building `memories`, active `/user/patterns` and bonding projections are returned under `"patterns"`.

### 8.2 `scope="patterns"` in `tachi_search`

- **File:** `crates/memory-server/src/facade_search_ops.rs`
- **Function:** `collect_tachi_search_sections`
- **Current behavior:** scope match includes `"patterns"`; pattern recall runs with `path_prefix = "/user/patterns"`, while ordinary `memory` recall filters projection rows.
- **File:** `crates/memory-server/src/agent_markdown/search.rs`
- **Function:** `format_search_sections`
- **Current behavior:** the generic section renderer handles the `Patterns` section.

### 8.3 Memory save emits continuity event when requested

- **File:** `crates/memory-server/src/memory_search_ops/save_memory/handler.rs`
- **Function:** `handle_save_memory`
- **Current behavior:** after `upsert_save_entry`, `emit_continuity=true` calls `emit_memory_saved_event`.
- **File:** `crates/memory-server/src/continuity_ops.rs`
- **Current behavior:** `emit_memory_saved_event` writes a `memory.saved` event with projection hints inferred from category/path metadata.
- **File:** `crates/memory-server-params/src/memory.rs`
- **Current behavior:** `SaveMemoryParams` has `#[serde(default)] emit_continuity: bool`.

### 8.4 Wiki write recalls patterns

- **File:** `crates/memory-server/src/copilot_ops/wiki_facade.rs`
- **Function:** `handle_tachi_wiki_write`
- **Current behavior:** when `include_patterns=true`, active patterns are queried and merged into `metadata.pattern_refs`.
- **File:** `crates/memory-server/src/continuity_ops.rs`
- **Current behavior:** `pub(crate) fn list_active_patterns` is available for wiki/skill integration.
- **File:** `crates/memory-server-params/src/memory.rs`
- **Current behavior:** `WikiWriteParams` has `include_patterns`, `pattern_query`, and `pattern_top_k`.

### 8.5 Pattern → skill generator

- **New file:** `crates/memory-server/src/hub_ops/pattern_to_skill.rs`
- **Function:** `handle_skill_from_pattern`
- **Current behavior:**
  1. Fetch active patterns.
  2. Build skill definition JSON (system, prompt, content, inputSchema, policy, tags, `pattern_ref`).
  3. Persist via `store.hub_register`.
  4. Keep generated skills disabled and pending review; do not auto-expose as listed tools.
- **File:** `crates/memory-server/src/hub_ops/mod.rs`
- **Current behavior:** re-exports the new handler.
- **File:** `crates/memory-server/src/tools/skill_facade.rs`
- **Current behavior:** routes `tachi_skill(action="from_pattern")` to the new handler.
- **File:** `crates/memory-server-params/src/facade.rs`
- **Current behavior:** `from_pattern` is in the `TachiSkillParams` action schema.

---

## 9. Open questions

1. **Should any memory categories auto-emit `memory.saved` events?** Current behavior is explicit only (`emit_continuity=true`).
2. **What is the promotion threshold from pattern to wiki/skill?** Pure hit-rate, or hit-rate + external validation + timeline depth?
3. **How does the cold seat participate in cross-process A2A?** Does it subscribe to events but ignore timeline conclusions, or does it maintain a separate evidence stream?
4. **Should pattern memory be per-project or global?** User judgment structures are likely global, but pattern instances may be project-specific.

---

## 10. Summary

Tachi already has the substrate for continuity memory: the `tachi_events` ledger, projection machinery, counters, and guardrails. The first integration slice is now in place: explicit memory-save events, pattern search/context, wiki pattern references, and pending pattern-derived skill candidates. Remaining work is loop closure, calibration, and promotion:

- Add **pattern hit/miss feedback** so counters reflect runtime recall outcomes.
- Add **wiki.saved** events so reviewed wiki writes return to the ledger.
- Add **maturity gates** so only validated patterns promote to wiki/skills.
- Add a **background projection loop** so the system closes the loop automatically.
- Keep the **over-fit brake and cold seat** as un-revocable safeguards.

The result is a system that learns the user's alignment, surfaces it when relevant, and turns it into durable knowledge and executable skills — without losing the ability to be challenged or corrected.
