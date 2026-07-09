# Tachi Continuity Memory Architecture

**Status:** architecture design + first implementation slice
**Date:** 2026-06-23
**Last code-alignment audit:** 2026-06-29
**Related docs:**
- [`pattern-timeline-bonding-memory.md`](./pattern-timeline-bonding-memory.md) — original design + cold review
- [`../../wiki/agent/tachi/Tachi-图书馆架构设计.md`](../../wiki/agent/tachi/Tachi-图书馆架构设计.md) — Karpathy LLM Wiki mapping
- [`host-adapter-lifecycle-v1.md`](./host-adapter-lifecycle-v1.md) — host runtime event contract

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
- Host lifecycle events are evidence inputs, not a second control plane. Host
  adapters can emit session, prompt, tool, compact, stop, and end events, but
  project-cycle state and durable memory remain Tachi-owned.

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

Current storage: append-only `tachi_events` plus `ProjectionKind::Timeline` projections under `/timeline/<domain>/<hash>`. The projector preserves a first typed metadata slice at `metadata.timeline`: `discoveries`, `decisions`, `open_threads`, `evolution`, `causal_edges`, `external_validations`, and temporal validity fields.
Current surface: `tachi_event action=context` returns a dedicated `timeline[]` read-model section with a `TimelineEntry` schema marker when timeline projections are available. Causal graph `add_edge` storage and fully enforced Rust `TimelineEntry` / `JudgmentEvolution` validation are still design targets.

### 2.3 Bonding layer — shared communication protocol

Bonding is a **per-user shared communication protocol**: shorthand references, common investigative history, domain-specific vocabulary, and expected interaction rhythms.

It is **not** an emotional state of the model, nor is it the same thing as an RLHF warmth output. The model does not "feel" attached; it reads a structured read model that compresses bandwidth between user and system.

That said, **affect and warmth are legitimate carriers for bonding**. A warm delivery can make the shared protocol land more effectively, just as an inappropriate cold delivery can undermine it. The key distinction is:
- **Bonding** = the shared context and shorthand the system has accumulated with this user.
- **Warmth/affect** = one delivery carrier that can make bonding feel natural and trustworthy.
- A cold-carrier agent with strong bonding can still be effective; a warm-carrier agent without bonding is only performing generic RLHF politeness.

Current storage: `ProjectionKind::Bonding` materializes into `/user/patterns/bonding/<domain>/<hash>`. The projector preserves a SharedLexicon-shaped metadata slice at `metadata.lexicon`: `origin_session`, `origin_context`, `meaning`, `shorthand_triggers`, `appropriate_contexts`, `inappropriate_contexts`, `callback_hits`, and `last_successful_use`.
Current surface: `tachi_event action=context` returns a dedicated `bonding[]` read-model section with a `SharedLexicon` schema marker. A first-class Rust `SharedLexicon` type and schema validator are still design targets.
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

Ordinary memory recall excludes `/user/patterns/`, `/user/affect/`, `/timeline/`, `/lorebook/`, and other continuity projection rows by default, while `tachi_search scope="patterns"` reads projected pattern rows explicitly.

Integration direction:
- `tachi_memory save` can emit a `memory.saved` continuity event when `emit_continuity=true`.
- The distill lane can consume `memory.saved` events to discover or update patterns.
- Pattern hits/misses can be written through pattern feedback events. `tachi_search scope="patterns"` emits `seen`; `tachi_complete` consumes `evidence_refs=["pattern:<id>"]` and maps successful completions to `hit`, failures to `miss`, and non-terminal outcomes to `seen`; `close_loop` attaches reviewed pattern refs to wiki writes and records them as `hit`. Concrete instance memories under `/memory/instances/<pattern_id>/` are still a target.

### 3.2 Wiki — pattern promotion path

Wiki is the **human-readable, reviewed crystallization** of mature patterns. The flow is:

```
pattern candidate → project to /user/patterns/* → mature (high hit, verified)
  → generate /wiki/drafts/patterns/<name>.md
  → human review
  → promote to /wiki/decision/ or /wiki/runbook/
```

A wiki page derived from a pattern must retain a `pattern_ref` link. It is **not** automatically project truth; it is a reviewed artifact built on pattern evidence.

`tachi_wiki_write include_patterns=true` recalls relevant `/user/patterns` before composing content, stores `metadata.pattern_refs`, and emits a reviewed `wiki.saved` continuity event. Automatic wiki draft generation from mature patterns is still a target.

### 3.3 Skill — pattern execution path

Skill is the **executable crystallization** of patterns. Tachi already has a complete skill system (`hub_register`, `run_skill`, `recommend_skill`, `skill_evolve`). Pattern memory should become its **evidence-driven discovery layer**, not a replacement.

Two integration modes:

1. **Recommendation signal** (low risk): `recommend_skill` includes active patterns as context, improving skill matching without changing registration. Matching recommendations carry `pattern_refs`; `tachi_task`'s lightweight skill recommendations use the same bridge signal.
2. **Skill generation** (higher risk): `tachi_skill action="from_pattern"` can generate a disabled, pending-review, discoverable skill candidate with `pattern_ref` traceability. Human/maturity promotion to a listed skill is still a target.

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
    → returns projected memories + patterns + pattern_refs + bonding + timeline + lorebook + affect + local A2A evidence bundle + host_lifecycle + metrics
    → dispatch can assemble prompt context from projections
```

Implemented integration slice:

```
save_memory emit_continuity=true → memory.saved event
tachi_search scope=patterns      → explicit /user/patterns recall
tachi_wiki_write include_patterns=true → wiki metadata.pattern_refs[]
tachi_skill action=from_pattern  → pending/disabled Hub skill candidate
tachi_event action=promote       → wiki draft + pending skill + agent-profile proposal review artifacts
tachi_domain_adapter lorebook_import → repo lorebook shape → world_book events
tachi_event action=context       → read-only local A2A bundle with pattern refs, bonding refs, open threads, and compact event refs
tachi_event action=a2a           → read-only A2A evidence bundle without context feedback writes
```

Target integration still to add:

```
pattern maturity → external validation + cold-seat check
review artifact → human-approved wiki/runbook promotion
review artifact → human-approved generated skill promotion to listed/enabled
review artifact → human-approved Agent MD/profile write
```

Adapter boundary:

`tachi_domain_adapter` belongs to the generic core only when it converts an external
repo shape into the neutral continuity ledger. The first implemented action,
`lorebook_import`, maps RomanBath/SillyTavern lorebook entries into `tachi_event`
`world_book` candidates and optional projection. Domain business logic such as
finance tickers, trading lesson paths, or Quant-specific defaults should live in a
Quant adapter pack/fork that emits the same neutral events or normal memory writes;
it should not be exposed by the generic agent surface.

Generic Tachi therefore ships an empty routing configuration by default. Products
such as HyperTachi must opt in through their own `routing.json` or adapter pack
instead of relying on built-in finance/trading routes in the shared package.

The target runtime path is:

```
Runtime event
    → search recalls top-k projected patterns
    → extract/flash LLM or deterministic matcher confirms match
    → emit pattern.hit / pattern.miss event
    ↓
Pattern matures (hit_rate / confidence threshold + external validation + cold-seat check)
    → projection report includes review_artifacts
    → review artifact can create /wiki/drafts/patterns/<name>.md
    → review artifact can create skill:<name> candidate
    → human review
    → promote to wiki + hub skill
```

---

## 6. Current implementation status

### Implemented

- `memcore` typed `tachi_events` ledger: `TachiEventRecord`, `ProjectionKind`, `AuthorityLevel`, `EffectScope`.
- `tachi-server` `tachi_event` facade with eight actions: `emit`, `query`, `metrics`, `project`, `promote`, `context`, `a2a`, `label_eval`.
- `tachi_status` surfaces `challenge_rate` as a read-only continuity metric.
- `capture_session` emits `session.captured`; optional continuity pipeline emits candidates and `session.outcome`.
- `tachi_complete` bridges subagent eval into `task.outcome` / `subagent.evaluated` events.
- `tachi_event action=project` idempotently materializes events into stable projections.
- `tachi_event action=context` returns projected memories plus `patterns`, `pattern_refs`, `bonding`, `timeline`, `lorebook`, `affect`, local `a2a`, and `host_lifecycle` read-model sections. It also records pattern/bonding `seen` feedback for returned pattern refs.
- Pattern/bonding projections maintain `seen / hit / miss / confidence / last_seen` counters.
- Timeline projections carry typed metadata and a validated `TimelineEntry` schema marker for discoveries, decisions, open threads, evolution, causal edges, external validations, and validity fields. Explicit causal edges with existing memory-id endpoints are persisted into `memory_edges`; natural-language-only edges are skipped rather than creating orphans.
- Bonding projections carry SharedLexicon-shaped metadata and a validated `SharedLexicon` schema marker for origin, meaning, shorthand triggers, appropriate/inappropriate contexts, callback hits, and last successful use.
- Context responses expose a local read-only `a2a` evidence bundle that includes share policy, cold-seat constraints, poll-subscription metadata, pattern refs, bonding refs, timeline open threads, and compact event refs without raw payloads. `tachi_event action=a2a` returns the same evidence bundle without context feedback writes.
- Affect projections carry explicit guardrails plus local rule-based signals for language switches, known markers, input/output length, and IO ratio.
- `tachi_event action=label_eval` provides a label-quality harness.
- A held-out label-eval smoke fixture covers `session.outcome` vs `session.outcome.review` matching before `challenge_rate` is used as an over-fit signal.
- `save_memory` supports explicit `emit_continuity=true`, appending a `memory.saved` event with path/category-derived projection hints.
- `tachi_search` supports `scope="patterns"` and excludes continuity projection rows from ordinary `memory` recall.
- `tachi_wiki_write` supports `include_patterns=true`, persisting active pattern references in wiki metadata.
- `tachi_wiki_write` emits `wiki.saved` continuity events for reviewed wiki writes.
- `tachi_skill(action="from_pattern")` registers a disabled, pending-review, discoverable skill candidate with a `pattern_ref`.
- `recommend_skill` and feature/task lightweight skill recommendation can use active patterns as a bridge between the user's query and the skill surface; recommendations attach `pattern_refs` when a pattern contributed to the score.
- `tachi_event action="promote"` executes conservative review-artifact creation for mature patterns: a pending wiki draft, a disabled skill candidate, and an `agent_profile.proposal` continuity event. It supports `dry_run`, `force`, per-artifact skip flags, and a promotion gate that marks external-validation / cold-seat-review readiness before any final promotion.
- The daemon runs a scoped background continuity projection loop; projection reports include projected, skipped, and promotion candidate counters plus review artifacts for wiki drafts, skill candidates, and agent-profile proposals.
- Complete skill system: `hub_register`, `run_skill`, `recommend_skill`, `skill_evolve`, builtin skills.

### Missing / gaps

1. **Agent MD crystallization is only first-slice**: `tachi_profile` can import/render/context profile packs and target `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, Cursor, and OpenClaw files. `tachi_event action="promote"` can emit an `agent_profile.proposal` event for a mature pattern, but it does not yet render or write host Agent MD files.
2. **No cross-process A2A transport**: a local read-only `a2a` evidence bundle and `action=a2a` poll surface exist, but there is no daemon pub/sub API, no independent cold-seat host profile, and no transport-level evidence/open-question feed.
3. **Label-quality calibration incomplete**: harness and smoke fixture exist, but calibration still needs a larger reviewed held-out corpus, thresholds, and an operator-visible calibration status.
4. **Maturity gates are partially implemented**: projection reports and `tachi_event action="promote"` expose external-validation / cold-seat-review gate status. The gate still does not auto-promote drafts/candidates to final wiki, listed skill, or Agent MD writes.
5. **Pattern hit/miss feedback is partial but now connected to task closure and context**: `tachi_search scope="patterns"` and `tachi_event action=context` emit `seen`, explicit feedback can emit `hit` / `miss` / `stale`, `tachi_complete` can consume `pattern:<id>` evidence refs, and `close_loop` records reviewed attached patterns as `hit`. Ordinary briefing/context use still does not automatically decide hit/miss without downstream outcome evidence.
6. **Timeline graph is partially wired**: timeline projections expose a typed `metadata.timeline` / `timeline[]` read-model slice with a `TimelineEntry` schema marker, and explicit causal edges with existing memory-id endpoints persist to `memory_edges`. Natural-language causal edges, typed `JudgmentEvolution`, and automatic endpoint resolution are still missing.
7. **Bonding schema is partially enforced**: bonding projections expose `metadata.lexicon` / `bonding[]` with a `SharedLexicon` schema marker and validation issues, but there is no standalone Rust `SharedLexicon` domain type.
8. **Rule-based affect detector is first-slice**: affect projections extract language switch, known markers, IO ratio, and length signals, but response-latency-like signals are not yet available without host timing input.
9. **Session lifecycle contract is packaged as a read model**: `host_lifecycle` now documents startup preload, in-session buffer, feedback, session-end capture, and profile refresh. Per-host adapters still need to consume that contract consistently.

---

## 7. Architectural roadmap

### Phase 1 — Calibration and model tiering

**Goal:** make the over-fit brake trustworthy.

1. Configure backend lanes:
   - `extract/summary` → fast model (Qwen 27B / DeepSeek V4 Flash)
   - `distill/reasoning` → strong model (Qwen 72B / DeepSeek V4 Pro)
2. Expand held-out transcript fixtures and run `tachi_event action=label_eval`.
3. Establish label-quality acceptance criteria before trusting `challenge_rate`.

### Phase 2 — Memory / Wiki / Pattern integration

**Goal:** pattern memory becomes a first-class search and recall citizen.

1. Done: `scope="patterns"` exists on `tachi_search`.
2. Done: `tachi_event action=context` returns a dedicated `patterns` section.
3. Done: `save_memory` can emit `memory.saved` when `emit_continuity=true`.
4. Done: `tachi_event action=context` returns `pattern_refs`, `bonding`, `timeline`, `lorebook`, `affect`, local `a2a`, and `host_lifecycle` read-model sections.
5. Done: `tachi_wiki_write` can recall and reference active patterns with `include_patterns=true`.
6. Done: `tachi_wiki_write` emits `wiki.saved` continuity events with reviewed `pattern_refs`.

### Phase 3 — Background projection loop

**Goal:** close the loop without requiring manual `tachi_event action=project`.

1. Done: daemon scheduler calls `project_auto_continuity_events_for_target` for scoped/manifest DBs.
2. Done: auto projection skips `Blocker` and `ExecutionGate` authority events.
3. Done: reports and scheduler logs include projected count, skipped count, promotion candidates, and review artifacts.

### Phase 4 — Skill crystallization

**Goal:** mature patterns become executable skills.

1. Done: `recommend_skill` and lightweight task skill recommendation use active pattern bridge signals and expose `pattern_refs` on matches.
2. Done: `tachi_skill action=from_pattern` generates skill candidates from active patterns.
3. Done: generated skills are `discoverable`, disabled, pending review, and carry `pattern_ref` metadata.
4. Done: maturity gate creates review artifacts before promotion to `listed`.
5. Done: `tachi_event action="promote"` can materialize the pending wiki draft, disabled skill candidate, and agent-profile proposal event for an eligible or forced pattern.
6. Remaining: generated skill candidates should reference the evidence chain used for promotion, not just the latest pattern projection.

### Phase 5 — A2A and cold seat transport

**Goal:** share evidence and open questions across agents and processes without homogenizing conclusions.

1. Done: context responses and `tachi_event action=a2a` include a local read-only A2A evidence bundle with share policy, poll-subscription metadata, and cold-seat constraints.
2. Wire a pub/sub or subscription transport on top of `tachi_events`.
3. Guarantee at least one cold-seat agent per coalition that does not ingest shared timeline conclusions.
4. Enforce at the transport/client layer: A2A shares facts + open questions, never conclusions.

---

## 8. Concrete file-level integration points

### 8.1 `patterns` section in `tachi_event action=context`

- **File:** `crates/tachi-server/src/continuity_ops/context.rs`
- **Function:** `build_continuity_context`
- **Current behavior:** after building `memories`, active `/user/patterns` and bonding projections are returned under `"patterns"` with `pattern_ref`; bonding projections are also exposed through `"bonding"` with `SharedLexicon` schema markers and `lexicon`; timeline projections are exposed through `"timeline"` with `TimelineEntry` schema markers and typed timeline metadata; the local `"a2a"` section exposes evidence/open-thread refs without raw payloads; `"host_lifecycle"` exposes the adapter contract. Context records `seen` feedback for returned pattern refs, while `action=a2a` remains read-only.

### 8.2 `scope="patterns"` in `tachi_search`

- **File:** `crates/tachi-server/src/facade_search_ops.rs`
- **Function:** `collect_tachi_search_sections`
- **Current behavior:** scope match includes `"patterns"`; pattern recall runs with `path_prefix = "/user/patterns"`, ordinary `memory` recall filters projection rows, and pattern rows include `pattern_ref` for later feedback.
- **File:** `crates/tachi-server/src/agent_markdown/search.rs`
- **Function:** `format_search_sections`
- **Current behavior:** the generic section renderer handles the `Patterns` section.

### 8.2.1 Pattern feedback loop

- **File:** `crates/tachi-server/src/continuity_ops/feedback.rs`
- **Current behavior:** normalizes `pattern:<id>` / `pattern-hit:<id>` / `pattern-miss:<id>` / `pattern-stale:<id>` evidence refs into continuity feedback events.
- **File:** `crates/tachi-server/src/complete_ops/handler.rs`
- **Current behavior:** `tachi_complete` records pattern feedback from `evidence_refs`; success defaults to `hit`, failure to `miss`, and partial/aborted to `seen`.
- **File:** `crates/tachi-server/src/workflow_closure.rs`
- **Current behavior:** `close_loop` writes wiki entries with `include_patterns=true` and records attached reviewed patterns as `hit`.

### 8.3 Memory save emits continuity event when requested

- **File:** `crates/tachi-server/src/memory_search_ops/save_memory/handler.rs`
- **Function:** `handle_save_memory`
- **Current behavior:** after `upsert_save_entry`, `emit_continuity=true` calls `emit_memory_saved_event`.
- **File:** `crates/tachi-server/src/continuity_ops/emit.rs`
- **Current behavior:** `emit_memory_saved_event` writes a `memory.saved` event with projection hints inferred from category/path metadata.
- **File:** `crates/tachi-params/src/memory.rs`
- **Current behavior:** `SaveMemoryParams` has `#[serde(default)] emit_continuity: bool`.

### 8.4 Wiki write recalls patterns

- **File:** `crates/tachi-server/src/copilot_ops/wiki_facade.rs`
- **Function:** `handle_tachi_wiki_write`
- **Current behavior:** when `include_patterns=true`, active patterns are queried and merged into `metadata.pattern_refs`.
- **File:** `crates/tachi-server/src/continuity_ops/context.rs`
- **Current behavior:** `pub(crate) fn list_active_patterns` is available for wiki/skill integration.
- **File:** `crates/tachi-params/src/memory/wiki.rs`
- **Current behavior:** `WikiWriteParams` has `include_patterns`, `pattern_query`, and `pattern_top_k`.

### 8.5 Pattern → skill generator

- **New file:** `crates/tachi-server/src/hub_ops/pattern_to_skill.rs`
- **Function:** `handle_skill_from_pattern`
- **Current behavior:**
  1. Fetch active patterns.
  2. Build skill definition JSON (system, prompt, content, inputSchema, policy, tags, `pattern_ref`).
  3. Persist via `store.hub_register`.
  4. Keep generated skills disabled and pending review; do not auto-expose as listed tools.
- **File:** `crates/tachi-server/src/hub_ops/mod.rs`
- **Current behavior:** re-exports the new handler.
- **File:** `crates/tachi-server/src/tools/skill_facade.rs`
- **Current behavior:** routes `tachi_skill(action="from_pattern")` to the new handler.
- **File:** `crates/tachi-params/src/facade.rs`
- **Current behavior:** `from_pattern` is in the `TachiSkillParams` action schema.

---

## 9. Open questions

1. **Should any memory categories auto-emit `memory.saved` events?** Current behavior is explicit only (`emit_continuity=true`).
2. **What is the promotion threshold from pattern to wiki/skill?** Pure hit-rate, or hit-rate + external validation + timeline depth?
3. **How does the cold seat participate in cross-process A2A?** Does it subscribe to events but ignore timeline conclusions, or does it maintain a separate evidence stream?
4. **Should pattern memory be per-project or global?** User judgment structures are likely global, but pattern instances may be project-specific.
5. **How should each host adapter consume the lifecycle contract?** `host_lifecycle` defines preload, local buffer, outcome label, pattern feedback, and writeback; Codex/Claude/Gemini/Cursor/OpenClaw still need per-host enforcement.

---

## 10. Summary

Tachi already has the substrate for continuity memory: the `tachi_events` ledger, projection machinery, counters, guardrails, typed timeline/bonding read-model slices, wiki references, pending pattern-derived skill candidates, profile-pack rendering, and lifecycle/GitHub surfaces. Remaining work is loop closure, calibration, enforced schemas, causal graph storage, and final promotion:

- Extend **automatic pattern hit/miss feedback** beyond search/context/complete/close_loop into briefing runtime and outcome-backed hit/miss classification.
- Extend **maturity gates** from explicit readiness reporting into final promotion enforcement for wiki/listed skill/Agent MD writes.
- Add **Agent MD crystallization** so mature continuity patterns become reviewed profile proposals that can be rendered for each host.
- Harden **timeline and bonding read models** from schema-marked/validated output into standalone Rust domain types and richer automatic causal endpoint resolution.
- Extend the local **A2A evidence/open-thread bundle** and poll surface into cross-process transport while preserving the cold seat.
- Keep the **over-fit brake and cold seat** as un-revocable safeguards.

The result is a system that learns the user's alignment, surfaces it when relevant, and turns it into durable knowledge and executable skills — without losing the ability to be challenged or corrected.
