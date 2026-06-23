# Tachi Continuity Memory Architecture

**Status:** architecture design  
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

---

## 2. Core abstractions

### 2.1 Pattern memory — what the user recognizes

Pattern memory stores the user's **cognitive and judgment structures**: how they classify events, what evidence they trust, when they revise a belief, and what recurring shapes they have found in prior work.

It is a model of the user's thinking, not merely a catalog of world facts. A pattern such as `religious_leader_political_proxy` is valuable not because the world contains religious leaders, but because the user has learned to recognize a structural shape across multiple domains and can use it to predict and analyze new instances.

Storage: `/user/patterns/*` in the memory DB.  
Metadata counters: `seen / hit / miss / confidence / last_seen`.  
Authority: usually `CollectOnly` until promoted.

### 2.2 Timeline memory — why a conclusion is trustworthy

Timeline memory stores the **credibility history** of a judgment or pattern. It answers "why is this conclusion trustworthy?" rather than "what did we discuss on which day?".

The timeline is an evolution chain: discovered → defended → revised → externally validated. Each transition is a causal edge with temporal validity. The depth of adversarial testing and external verification is itself evidence for the conclusion's reliability.

Storage: causal graph edges + `TimelineEntry` projections.  
Surface: `open_threads` at next session start; `tachi_event action=context` returns evolution metadata.

### 2.3 Bonding layer — shared communication protocol

Bonding is a **per-user shared communication protocol**: shorthand references, common investigative history, domain-specific vocabulary, and expected interaction rhythms.

It is **not** an emotional state of the model, nor is it the same thing as an RLHF warmth output. The model does not "feel" attached; it reads a structured read model that compresses bandwidth between user and system.

That said, **affect and warmth are legitimate carriers for bonding**. A warm delivery can make the shared protocol land more effectively, just as an inappropriate cold delivery can undermine it. The key distinction is:
- **Bonding** = the shared context and shorthand the system has accumulated with this user.
- **Warmth/affect** = one delivery carrier that can make bonding feel natural and trustworthy.
- A cold-carrier agent with strong bonding can still be effective; a warm-carrier agent without bonding is only performing generic RLHF politeness.

Storage: `/user/patterns/bonding/*` and `SharedLexicon` entries.  
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
capture_session emits session.captured
    ↓
(TACHI_CONTINUITY_PIPELINE=1)
distill lane    → pattern.candidate / bonding.candidate / timeline.candidate
reasoning lane  → session.outcome
    ↓
tachi_memory save → memory.saved event
tachi_wiki_write  → wiki.saved event (optionally referencing patterns)
    ↓
tachi_event action=project
    → idempotently materialize candidates into:
       /user/patterns/*
       /user/patterns/bonding/*
       /lorebook/*
       /user/affect/*
    → update counters (seen / hit / miss / last_seen)
    → concrete instances written to /memory/instances/<pattern_id>/
    ↓
Next session start
    → tachi_event action=context
    → returns active patterns + lorebook + affect + memories
    → dispatch assembles prompt with patterns
    ↓
Runtime event
    → vector search recalls top-k patterns
    → extract/flash LLM confirms match
    → emit pattern.matched event
    ↓
Pattern matures (hit_rate / confidence threshold + external validation)
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
- `tachi_event action=context` returns projected memories + `lorebook` + `affect` read-model sections.
- Pattern/bonding projections maintain `seen / hit / miss / confidence / last_seen` counters.
- Affect projections carry explicit guardrails.
- `tachi_event action=label_eval` provides a label-quality harness.
- Complete skill system: `hub_register`, `run_skill`, `recommend_skill`, `skill_evolve`, builtin skills.

### Missing / gaps

1. **No background auto-apply loop**: projection is explicit via `tachi_event action=project`.
2. **No Agent MD crystallization**: the ledger is not yet read when generating agent system prompts.
3. **No cross-process A2A transport**: only a local read model exists.
4. **Label-quality calibration incomplete**: harness exists, needs reviewed held-out data.
5. **Memory save does not emit continuity events**: `handle_save_memory` writes the DB row but does not append to `tachi_events`.
6. **Wiki write does not recall patterns**: `handle_tachi_wiki_write` builds `SaveMemoryParams` directly without consulting `/user/patterns`.
7. **`tachi_search` has no `scope="patterns"`**: pattern memories can only be reached via `path_prefix`.
8. **`tachi_event action=context` does not return a `patterns` section**: active patterns are mixed into `memories`.
9. **No pattern → skill generator**: the hub can register and evolve skills, but no code consumes `/user/patterns` to emit a `HubCapability`.

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

1. Add `scope="patterns"` to `tachi_search`.
2. Return a dedicated `patterns` section from `tachi_event action=context`.
3. Emit `memory.saved` continuity event from `handle_save_memory`.
4. Let `handle_tachi_wiki_write` optionally recall and reference active patterns.

### Phase 3 — Background projection loop

**Goal:** close the loop without requiring manual `tachi_event action=project`.

1. Add a scheduler/cron that calls `project_continuity_events` for eligible candidates.
2. Respect authority levels: only `CollectOnly` candidates are auto-projected; higher authority remains explicit.
3. Add observability: projected count, skipped count, promotion candidates.

### Phase 4 — Skill crystallization

**Goal:** mature patterns become executable skills.

1. Add `recommend_skill` pattern signal (Phase 4a).
2. Add `tachi_skill action=from_pattern` to generate skill definitions from mature patterns (Phase 4b).
3. Register generated skills as `discoverable` with `pattern_ref` metadata.
4. Human review before promoting to `listed`.

### Phase 5 — A2A and cold seat transport

**Goal:** share evidence and open questions across agents and processes without homogenizing conclusions.

1. Wire a pub/sub or subscription transport on top of `tachi_events`.
2. Guarantee at least one cold-seat agent per coalition that does not ingest shared timeline conclusions.
3. Enforce: A2A shares facts + open questions, never conclusions.

---

## 8. Concrete file-level integration points

### 8.1 Add `patterns` section to `tachi_event action=context`

- **File:** `crates/memory-server/src/continuity_ops.rs`
- **Function:** `build_continuity_context` (≈918)
- **Change:** after building `memories`, filter entries where `path.starts_with("/user/patterns")` or `metadata.projection_kind == "pattern"`, and include `"patterns": patterns` in the returned JSON.

### 8.2 Add `scope="patterns"` to `tachi_search`

- **File:** `crates/memory-server/src/facade_search_ops.rs`
- **Function:** `collect_tachi_search_sections` (≈59)
- **Change:** extend scope match to include `"patterns"`; run `search_memory_rows_with_access` with `path_prefix = "/user/patterns"`.
- **File:** `crates/memory-server/src/agent_markdown.rs`
- **Function:** `format_search_sections` (≈311)
- **Change:** add markdown rendering for the `Patterns` section.

### 8.3 Memory save emits continuity event

- **File:** `crates/memory-server/src/memory_search_ops/save_memory.rs`
- **Function:** `handle_save_memory` (≈288)
- **Change:** after `upsert_save_entry`, call a new helper `emit_memory_saved_event`.
- **File:** `crates/memory-server/src/continuity_ops.rs`
- **Change:** add `pub(crate) fn emit_memory_saved_event` that writes a `memory.saved` event with `projection_hints = [ProjectionKind::Pattern]`.
- **File:** `crates/memory-server/src/tool_params/memory.rs`
- **Change:** add optional `#[serde(default)] emit_continuity: bool` to `SaveMemoryParams`.

### 8.4 Wiki write recalls patterns

- **File:** `crates/memory-server/src/copilot_ops.rs`
- **Function:** `handle_tachi_wiki_write` (≈690)
- **Change:** before building `SaveMemoryParams`, call a new helper that queries active patterns and merges them into wiki metadata or entry text.
- **File:** `crates/memory-server/src/continuity_ops.rs`
- **Change:** expose `pub(crate) fn list_active_patterns`.
- **File:** `crates/memory-server/src/tool_params/memory.rs`
- **Change:** add `include_patterns`, `pattern_query`, `pattern_top_k` to `WikiWriteParams`.

### 8.5 Pattern → skill generator

- **New file:** `crates/memory-server/src/hub_ops/pattern_to_skill.rs`
- **Function:** `handle_skill_from_pattern`
- **Responsibilities:**
  1. Fetch active patterns.
  2. Build skill definition JSON (system, prompt, content, inputSchema, policy, tags, `pattern_ref`).
  3. Persist via `store.hub_register`.
  4. Optionally expose via `register_skill_tool`.
- **File:** `crates/memory-server/src/hub_ops/mod.rs`
- **Change:** re-export the new handler.
- **File:** `crates/memory-server/src/tool_params/facade.rs`
- **Change:** add `from_pattern` action to `TachiSkillParams` schema.
- **File:** `crates/memory-server/src/tools.rs`
- **Change:** route `tachi_skill(action="from_pattern")` to the new handler.
- **File:** `crates/memory-server/src/prompts.rs`
- **Change:** add `PATTERN_TO_SKILL_PROMPT`.

---

## 9. Open questions

1. **Which memory categories should auto-emit `memory.saved` events?** All saves, or only `preference`, `experience`, `decision`?
2. **Should pattern-derived skills be auto-approved or pending review?** Auto-approval risks spam; pending review adds friction.
3. **What is the promotion threshold from pattern to wiki/skill?** Pure hit-rate, or hit-rate + external validation + timeline depth?
4. **How does the cold seat participate in cross-process A2A?** Does it subscribe to events but ignore timeline conclusions, or does it maintain a separate evidence stream?
5. **Should pattern memory be per-project or global?** User judgment structures are likely global, but pattern instances may be project-specific.

---

## 10. Summary

Tachi already has the substrate for continuity memory: the `tachi_events` ledger, projection machinery, counters, and guardrails. The remaining work is integration, not invention:

- Connect **memory save** to the event ledger so patterns can grow from ordinary saves.
- Connect **wiki** to pattern memory so mature patterns can be reviewed and crystallized.
- Connect **skill** to pattern memory so verified patterns become executable capabilities.
- Add a **background projection loop** so the system closes the loop automatically.
- Keep the **over-fit brake and cold seat** as un-revocable safeguards.

The result is a system that learns the user's alignment, surfaces it when relevant, and turns it into durable knowledge and executable skills — without losing the ability to be challenged or corrected.
