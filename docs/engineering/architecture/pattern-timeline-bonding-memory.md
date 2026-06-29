# Pattern / Timeline / Bonding Memory — Design & Cold Review

This note converges a multi-session design (the `tachi-pattern-timeline-memory-spec`
and its derivation timeline) into an implementation plan **aligned to the existing
Tachi codebase**, with an independent review baked in as load-bearing constraints.

It is written from the cold-reviewer seat: every claim is anchored to a file/line
or marked as unverified. The goal is not to endorse the spec — it is to state what
is buildable, in what order, and where it breaks.

## Design intent: an alignment bridge

The base model is aligned by its vendor to a generic notion of helpfulness,
honesty, and safety. That alignment is useful but not identical to the user's
own alignment — the user's actual values, judgment habits, thresholds for
evidence, and long-term interests. The memory system is therefore not primarily
a "remember more" cache; it is a **dynamic, auditable bridge between vendor
alignment and user alignment**.

- **Pattern memory** learns the user's cognitive and judgment structures: how
they classify, evaluate, and revise beliefs.
- **Timeline memory** records the evolution and testing of those structures,
so conclusion credibility is grounded in adversarial history, not assertion.
- **Bonding layer** accumulates a per-user shared protocol that compresses
communication bandwidth, so later interactions can start from accumulated
context instead of rebuilding it every turn.

Because this bridge can drift toward the user's short-term feedback at the
expense of their long-term interests, the system keeps an **over-fit brake**
and an **un-revocable cold seat** as structural safeguards.

## Audit stance

This document is a code-aligned audit, not a value judgment. Each mechanism is
classified as implemented, partially implemented, or missing against the current
Tachi codebase. Two findings shape the implementation order:

1. **The outcome labeler is the linchpin and has no free signal** (see Constraint 1).
2. **The cold seat must be structurally un-revocable** (see Constraint 5).

## Why this is not a whiteboard spec (external anchor)

The same memory primitives have independently converged across three repos by one
author — evidence of cross-**domain** generality (not cross-**user**; see Constraint 7):

- **Sigil / tachi** (`crates/memory-core` + `memory-server`) — the agent-memory engine.
- **hypermemory** (`Quant_Analyzer_2026/hypermemory/crates/memory-core`) — the same
  engine, quant domain.
- **Quant `autoresearch_lab`** (Python) — a full pattern system: score-free feature
  vectors, three counters (IC / hit-rate / decay), positive/negative samples
  (`true_S`/`stale_S`/`neutral` in `fragility.py`), leakage guard, ablation, BCa
  bootstrap CI. This is the conversation pattern-memory abstraction, in a different
  language/codebase. **Independent convergence, not code reuse.**
- **zeroclaw** (in both `Quant_Analyzer_2026` and `RomanBath`) — the "you own the
  agent / data / machine" product; the shared gateway across products = the concrete
  A2A substrate.
- **RomanBath `characters/*.json`** — pattern-memory + bonding written in **prose**:
  the character card's `post_history_instructions` ("记忆锚点协议") records confirmed
  user preferences as `[核心法则]`, logs failed strategies, and overwrites old rules
  on conflict. This is `seen/hit/last_seen` + outcome + overwrite, in natural language,
  in production — **with no over-fit brake** (it updates toward whatever keeps the user
  engaged: the failure this spec exists to prevent, running live).

So the spec's job is **codifying prose/cross-domain patterns into one canonical schema
+ counters + brake**, not inventing them.

## Layers, mapped to existing code

### Pattern memory
- **What it stores:** the user's cognitive and judgment structures — how they
  classify events, what evidence they trust, when they revise a belief, and what
  recurring shapes they have found in prior work. It is a model of the user's
  thinking, not merely a catalog of world facts.
- **Storage:** implemented as `ProjectionKind::Pattern` rows under `/user/patterns/*`.
  Ordinary memory recall filters continuity projections; `tachi_search scope="patterns"`
  reads them explicitly.
- **Decay:** the `"pattern"` tier already exists with a much longer half-life and
  lower ACT-R decay than raw/consolidated memories.
- **Weight:** projection metadata currently stores `seen / hit / miss / confidence /
  last_seen`. Explicit feedback updates counters; pattern search records `seen`;
  `tachi_complete` can consume `pattern:<id>` evidence refs; `close_loop` records
  reviewed attached patterns as `hit`. Briefing/context runtime still does not decide
  hit/miss automatically.
- **Recall:** existing hybrid search supports pattern scope. Session-start preload is
  available through context/briefing surfaces, but the host-adapter contract is not yet
  packaged as one protocol.

### Timeline memory
- **What it stores:** the credibility history of a judgment or pattern — how it was
discovered, defended, revised, and externally validated. Timeline memory answers
"why is this conclusion trustworthy?" rather than "what did we discuss on which day?".
- **Evolution chain:** append-only `tachi_events` plus `ProjectionKind::Timeline`
  projections exist. Causal graph `add_edge` integration is still a target.
- **Generation:** `capture_session` emits `session.captured`; the optional continuity
  pipeline can emit candidates and `session.outcome`. Context output now carries a
  `TimelineEntry` schema marker and typed metadata; enforced Rust validation is still
  missing.

### Bonding layer
- **What it stores:** a per-user shared communication protocol — shorthand references,
  common investigative history, domain-specific vocabulary, and expected interaction
  rhythms that let later turns start from accumulated context instead of rebuilding it.
- **What it is not:** bonding is neither an emotional state of the model nor an RLHF
  warmth output. The model does not "feel" attached; it reads a structured read model
  that compresses bandwidth between user and system.
- **Storage:** bonding projections exist under `/user/patterns/bonding/*`.
  Context output now carries a `SharedLexicon` schema marker and lexicon metadata;
  enforced Rust validation is still missing.
- **Precedent:** the RomanBath character card's prose protocol is the working model;
  formalize it (origin, meaning, callback_hits, appropriate/inappropriate contexts).
- **Delivery carrier:** see Constraint 2 — bonding is **not** warmth.

### A2A (replaces the deleted "Ghost Whispers")
- **Substrate exists:** shared global DB + daemon + namespace (the process-lifecycle
  work) is the transport; zeroclaw's shared gateway is the cross-product precedent.
- **Partial:** `tachi_event action="context"` exposes a local read-only A2A evidence
  bundle with pattern refs, bonding refs, open threads, and compact event refs. A
  cross-process "subscribe to another agent's timeline" transport is still missing.
  See Constraint 5 for the hard rule on what A2A may and may not share.

## Load-bearing constraints (the review, as design decisions)

**1. Labeler-first; find the conversation `fwd_return`.**
The quant over-fit guard works because the label is free and external:
`fragility.py:112` computes `true_S/stale_S/neutral` from `fwd_return` — the market
realizes it, no judge needed. The conversation domain has **no `fwd_return`**:
`UserCorrect/AiCorrected` must be *judged* by a fallible labeler with no market to
check against. Therefore: build the **session-end LLM labeler first**, validate label
quality (inter-rater agreement on held-out transcripts) **before** trusting any
counter, and explicitly tag each label `externally-anchored` (the AI's factual claim
survived verification; the user's prediction resolved) vs `testimonial`. The brake is
only as good as its labels; the labels are the whole ballgame.

**2. Carrier axis: bonding ≠ warmth.**
bonding is orthogonal to warmth and to honesty. Abrasive/cold-carrier bonding (a
"roast" register) *raises* the user's scrutiny; warm-carrier bonding (豆包/flattery)
*lowers* it. The over-fit risk is **warm-carrier bonding**, not bonding. Design:
delivery has a `carrier` dimension; a warm-carrier active delivery must ship the
over-fit brake **in the same commit** (never a later phase). Cold-carrier delivery is
self-braking.

**3. Over-fit detection = challenge_rate, as a signal not a verdict.**
Read-only ratio of `AiCorrected` (AI challenged, user accepted) over a window. The
deeper purpose is not merely to make the AI "disagree with the user" occasionally;
it is to prevent the system from optimizing short-term user satisfaction at the
expense of the user's long-term interests. Two caveats it must encode: (a) it cannot
distinguish "user accepted because AI was right" from "user deferred / tired" — needs
an inverse guard; (b) a low rate may be a domain-expert user, not over-fitting.
Threshold and window are unjustified until calibrated on real data.

**4. Outcome enum is too coarse; `adversarial_tested` needs a second axis.**
Resolved in the current type layer: `SessionOutcomeKind` now includes partial/reframe
style outcomes such as `PartialReframe` and `MutualCorrection`, and
`OutcomeEvidenceBasis` splits external evidence from interlocutor argument. The
remaining design requirement is to keep these axes visible in labels, metrics, and
future promotion gates.

**5. The cold seat must be un-revocable; A2A shares evidence, not conclusions.**
The cold seat is the system's retained generic-alignment reference. As pattern
and bonding memory learn the user's personal alignment, the cold seat prevents
unchecked drift: when user alignment and vendor alignment conflict, that tension
must be visible, not silently resolved in favor of the user. The cold seat must
not ingest warm coalition conclusions; it checks facts and logic independently.

The ecosystem is currently all-warm (Jayne, zeroclaw personas) with **zero cold
agents**. A2A that broadcasts a warm coalition's conclusions homogenizes judgment and
destroys the independent calibration reference. Rule: A2A shares **facts + open
questions**, never conclusions; keep one agent that does not ingest the shared
timeline's conclusions; and **checkable findings (a logic fact, a `file:line`) may not
be overruled by "you don't understand bonding."** Bonding governs judgment calls; it
does not govern whether the enum is too coarse.

**6. Security/ownership model for the profile.**
The pattern+bonding corpus is the most sensitive store in the system (a psychological
profile of the user). "Collection is zero-risk" is false in general; it is low-risk
**only** under local-first + user-inspectable + deletable. Specify that model before
high-recall collection.

**7. n=1 USER.**
All evidence is one author. Validated cross-domain (quant / conversation / character),
**not** cross-user. The pattern layer's generalization across users is unproven; treat
per-user calibration as the unit until there is a second user.

## Phasing (corrected — labeler-first, not detection-first)

1. **Session-end labeler + label-quality eval** (Constraint 1). Nothing downstream is
   trustworthy without this.
2. **Over-fit detection** — read-only `challenge_rate` metric, surfaced in
   `tachi_status` (Constraint 3). Zero risk, highest strategic value.
3. **Pattern memory** — `/user/patterns/*`, `tier="pattern"`, three-counter weight;
   session-start preload / session-end writeback.
4. **Bonding collection** — high-recall / low-precision capture + counter-based
   retroactive promotion (you cannot detect an inside-joke at birth; significance is
   only knowable later). Under Constraint 6's security model.
5. **Timeline memory** — `capture_session` emits `TimelineEntry` + causal edges
   (Constraint 4's adversarial split).
6. **Agent MD crystallization** — generate `system.md` + `agent.{role}.md` from pattern
   memory; rails exist (`dispatch_profile.rs` roles + `hub_ops/export.rs`).
7. **A2A** — share evidence + open threads, keep a cold seat (Constraint 5).
8. **Warm-carrier active delivery (callback timing)** — LAST, and only with the brake
   in the same commit (Constraint 2).

## Current implementation status

Implemented substrate:

- `memory-core` now has a typed `tachi_events` ledger for continuity events, including
  authority level, effect scope, projection hints, outcome labels, evidence basis,
  and continuity metrics.
- `memory-server` exposes the `tachi_event` facade for manual/agent event emission,
  query, and continuity metric reads.
- `tachi_status` surfaces read-only continuity metrics, including `challenge_rate`.
- `capture_session` emits a raw `session.captured` event and can optionally run a
  disabled-by-default continuity pipeline with `TACHI_CONTINUITY_PIPELINE=1`.
- `tachi_complete` bridges the existing subagent eval path into `task.outcome` and
  `subagent.evaluated` events while keeping the eval ledger as the source of truth.
- The optional distill lane produces candidate `timeline` / `worldbook` projections
  with `CollectOnly` authority. The optional reasoning lane produces a read-only
  `session.outcome` label with `ReviewSignalOnly` authority.
- `tachi_event action="project"` now idempotently materializes projectable events into
  stable memory projections. Pattern/bonding/worldbook keys map to stable memory ids;
  rerunning projection does not double-count the same event.
- `tachi_event action="context"` now returns projected continuity memories plus typed
  `patterns`, `pattern_refs`, `bonding`, `timeline`, `lorebook`, `affect`, and `a2a`
  read-model sections for prompt/runtime consumers. It records `seen` feedback for
  returned pattern refs; `tachi_event action="a2a"` returns the local evidence bundle
  without feedback writes.
- Pattern/bonding projections maintain basic `seen / hit / miss / last_seen` counters
  in metadata. Promotion remains conservative: `CollectOnly` does not become an
  execution/scoring authority.
- Timeline projections preserve a first typed metadata slice:
  `discoveries`, `decisions`, `open_threads`, `evolution`, `causal_edges`,
  `external_validations`, validity fields, and a validated `TimelineEntry` schema
  marker. Explicit causal edges with existing memory-id endpoints persist to
  `memory_edges`; natural-language-only edges are skipped.
- Bonding projections preserve a SharedLexicon-shaped metadata slice:
  `origin_session`, `origin_context`, `meaning`, `shorthand_triggers`,
  `appropriate_contexts`, `inappropriate_contexts`, `callback_hits`, and
  `last_successful_use`, plus a validated `SharedLexicon` schema marker.
- Context responses expose a local read-only `a2a` evidence bundle with share policy,
  cold-seat constraints, poll-subscription metadata, pattern refs, bonding refs,
  timeline open threads, and compact event refs without raw payloads.
- Affect/emotion projections carry explicit guardrails: `tone_and_reminder_only`,
  `execution_effect=none`, `score_effect=none`, and `portfolio_effect=none`. They also
  expose first-slice local signals for language switch, known markers, length, and IO
  ratio.
- `tachi_event action="label_eval"` provides a read-only label-quality harness:
  compare `session.outcome` events against `session.outcome.review` gold labels by
  `target_event_id` or `session_id`.
- A held-out label-eval smoke fixture exercises that harness before `challenge_rate`
  is treated as an over-fit signal.
- `save_memory` can opt into `memory.saved` events with `emit_continuity=true`.
- `tachi_search scope="patterns"` searches projected `/user/patterns` rows without
  mixing them into ordinary memory recall.
- `tachi_wiki_write include_patterns=true` persists reviewed `pattern_refs` and emits
  `wiki.saved` events.
- `tachi_skill action="from_pattern"` registers disabled, pending-review,
  discoverable skill candidates carrying `pattern_ref` metadata.
- `recommend_skill` and `tachi_task` lightweight skill recommendation use active
  pattern bridge signals so a query can route through remembered project/user
  patterns into relevant skills. Matching recommendations expose `pattern_refs`.
- `tachi_event action="promote"` can materialize conservative review artifacts for
  an eligible or forced mature pattern: a pending wiki draft, a disabled skill
  candidate, and an `agent_profile.proposal` event. Promotion responses and reports
  include external-validation / cold-seat-review gate status.
- The daemon runs a background continuity projection loop. Projection reports expose
  `projected_count`, `skipped_count`, `promotion_candidate_count`, and review
  artifacts for wiki drafts, skill candidates, and agent-profile proposals.

Still missing:

- `Agent MD` crystallization is only first-slice: profile import/render/context exists,
  but mature continuity patterns do not automatically synthesize reviewed profile
  proposals.
- No cross-process A2A subscription transport has been wired on top of these events;
  only the local read-only `a2a` evidence bundle and poll surface exist through
  `tachi_event action="context"` / `action="a2a"`.
- Label-quality calibration is not complete; the harness and smoke fixture exist, but
  it still needs a larger reviewed held-out corpus.
- Runtime recall feedback is partial: pattern search and context emit `seen`, supplied
  callbacks update counters, `tachi_complete` consumes `pattern:<id>` evidence refs,
  and `close_loop` records reviewed attached patterns as `hit`; ordinary briefing use
  and outcome-backed automatic hit/miss classification are still missing.
- Maturity gates produce and can execute conservative review artifacts, but they do
  not yet promote drafts/candidates into final wiki, listed skills, or host Agent MD
  writes. External-validation / cold-seat readiness is exposed as gate status.
- Timeline projections expose `metadata.timeline` / `timeline[]` with a validated
  `TimelineEntry` schema marker; explicit memory-id causal edges persist to the graph,
  but enforced Rust validation, typed `JudgmentEvolution`, and automatic endpoint
  resolution are not wired.
- Bonding projections expose `metadata.lexicon` / `bonding[]` with a validated
  `SharedLexicon` schema marker; fields are not enforced as a standalone Rust domain
  type.
- A local rule-based affect detector extracts language switches, known markers, length,
  and IO ratio; response-latency-like signals still need host timing input.

## Lorebook and emotion mapping

RomanBath's Lorebook UI is useful as a shape reference, not as an authority source:
entries have trigger keys, optional secondary keys, position, priority, token budget,
enabled/selective/recursive flags, and content. In Tachi this maps to
`ProjectionKind::WorldBook` and paths under `/lorebook/{domain}/{key_hash}`. The
projection stores the trigger machinery in `metadata.lorebook`; consumers decide when
to inject it. This keeps "world book" as a read model, not a blanket memory preload.

Quant's emotion modules are useful mainly for the authority boundary. They estimate
states such as FOMO/panic/hesitation and attach counterweights/delivery modes, but the
important invariant is: affect can change tone/reminder intensity only. In Tachi this
maps to `ProjectionKind::Affect`, paths under `/user/affect/{domain}/{key_hash}`, and
metadata guardrails that explicitly deny scoring, execution, portfolio, and fact
mutation effects.

The shared abstraction is therefore not "roleplay memory" or "trading emotion." It is
`projection = candidate/event -> bounded read model`, with authority/effect metadata
deciding what a downstream agent may do with it.

## Open questions

- What is the conversation-domain `fwd_return`? Candidates: did a cited fact survive
  later verification; did a user prediction resolve; did a saved pattern's `hit` hold
  on re-encounter. All are sparse. Without a real external label, the brake is ungrounded.
- Is "three agents" the right decomposition, or is it one capability with orthogonal
  `(carrier, honesty, bonding)` dials? The best bonding moment in review came from the
  cold agent on an abrasive carrier — suggesting modes, not separate agents.
- Should user-level patterns live globally with project-scoped instances, or should
  each project DB own its own pattern namespace and only selected patterns promote
  globally?
- How should every CLI/IDE adapter consume the shared lifecycle contract:
  startup preload, in-session buffer, session-end capture, outcome label, pattern
  feedback, and profile refresh?
