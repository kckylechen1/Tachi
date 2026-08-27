# Tachi Continuity Memory Architecture

**Status:** architecture design + first implementation slice
**Date:** 2026-07-19
**Last code-alignment audit:** 2026-07-19
**Related docs:**
- [`pattern-timeline-bonding-memory.md`](./pattern-timeline-bonding-memory.md) — original design + cold review
- [`memory-soul-architecture.md`](./memory-soul-architecture.md) — reviewed operating-identity projection over continuity evidence
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

Continuity memory is an **append-only evidence substrate** plus projected read models. Pattern, timeline, bonding, affect, lorebook/world-book, eval, and wiki share one logical event protocol and provenance model, not isolated cache semantics. The event protocol itself survives; the skill-crystallization leg is **RETIRED by #1690 C3** — promotion emits wiki drafts and agent-profile proposals only, never skill candidates. Physically isolated trust-domain partitions are required for private user, relationship, and journal material; “one ledger” does not mean one readable database.

This means:

- `tachi_events` is the system of record for typed observations and their
  provenance. Current truth is a predicate-authorized reduction over those
  observations, not whatever an event claims.
- Projections are read models used for search, prompt context, wiki drafts, skills, or evaluation.
- Affect/emotion projections can influence tone/reminder style only; they must not mutate facts, scores, routing, trading decisions, or execution.
- Pattern and bonding memory are not obedience knobs. They can compress communication and surface learned judgment structures, but cold-seat review and label calibration remain mandatory.
- A2A transport should share evidence, open questions, and provenance across agents; it should not force agents to inherit conclusions.
- Host lifecycle events are evidence inputs, not a second control plane. Host
  adapters can emit session, prompt, tool, compact, stop, and end events, but
  project-cycle state and durable memory remain Tachi-owned.

### 1.2 Current truth is a revision-aware reduction, not the latest narrative

An append-only ledger preserves history, but append-only storage alone does not
answer "what is true now?" A continuity system MUST reduce typed assertions into
a revision-aware current-truth view.

The minimum assertion model is:

```text
assertion_id
subject_ref             # issue, PR, commit, deployment, decision, handoff, etc.
predicate               # opened, implemented_by, merged_as, deployed_to, supersedes...
object / value
asserted_by              # source system, owner/adjudicator, agent identity
source_revision_or_hash
observed_at
valid_from / valid_to
evidence_refs[]         # immutable object or artifact references
review_state             # observed | candidate | reviewed | rejected
authority_scope          # predicates this issuer may establish
host_scope?             # required for deployment/runtime truth
supersedes[]
correction_of?
retracted_by?
```

The reducer MUST:

1. preserve every observation rather than overwrite an earlier claim;
2. distinguish a correction from a later state transition;
3. follow explicit `supersedes`, reopen, revert, and retraction edges;
4. reconcile typed GitHub objects instead of inferring state from prose;
5. retain unresolved conflicts and provenance rather than selecting the most
   fluent account;
6. derive the current action queue from current truth, not copy a prior handoff's
   todo list;
7. mark projections stale when their evidence head is older than the reconciled
   object state.

Authority is predicate-scoped. At minimum:

| Predicate | Evidence that may establish it |
|---|---|
| issue/PR open, closed, merged, ref/SHA | the typed GitHub object at a recorded source revision |
| `implemented_by` | an explicit trusted relation or reviewed disposition, not title/body similarity |
| accepted | the named reviewer/owner decision required by that work contract |
| deployed | a host + service + binary/schema-scoped deployment receipt |
| owner-closed / owner-protected | an owner decision or typed repository label/state event |
| supersedes/corrects | the authority for the predicate being revised, with a backward reference |

Model-generated assertions remain `candidate` even when they cite evidence;
citation establishes provenance, not authority. The reducer admits a claim only
when the issuer and evidence satisfy that predicate's policy.

For delivery work these predicates are intentionally distinct:

```text
implemented != merged != accepted != deployed(host) != owner_closed
```

A merged PR is evidence that code entered a ref. It does not prove the behavior
was accepted, deployed to every host, or that an owner-protected issue should be
closed. Conversely, an open issue does not prove its implementation is missing.
Deployment claims are always host-scoped; "works on Codex" is not evidence that
the same adapter is live on Claude Code or another carrier.

Model-written summaries, journals, handoffs, and "what happened" narratives are
non-authoritative read models. They can propose assertions, but only cited
objects and reviewed decisions can change current truth. A handoff therefore
contains its evidence head and generated-at revision. On resume, the system
reconciles those refs before presenting any remaining work.

#### Worked example A — handoff continuity without stale narration

The chain #1205 → #1221 → #1248 → #1284 → #1285 shows why preserving a useful
handoff is not enough. Each issue refined the operational continuity contract;
PR #1291 then implemented the P0 slice for #1285. A stored narrative written
before that merge can still truthfully describe the earlier investigation while
being wrong about the next action.

The ledger keeps both observations. The reducer follows the later typed PR/merge
evidence, marks the old action item stale, and derives only the remaining #1285
work. It does not delete history or silently rewrite the old handoff.

#### Worked example B — GitHub open state is not implementation state

Issues #1288 and #1289 remained open after their implementation PRs had merged:
PR #1290 merged as `14e6b3a5`, and PR #1292 merged as `b69693ae`. Treating
`issue.state == open` as "not done" therefore recreated completed work in the
action queue.

A correct reducer records at least:

```text
#1288 implemented_by PR #1290
PR #1290 merged_as 14e6b3a5
#1288 owner_closed <later event>

#1289 implemented_by PR #1292
PR #1292 merged_as b69693ae
#1289 owner_closed <later event>
```

The period between merge and close is represented honestly: implementation is
merged, owner closure is pending. No single boolean flattens those facts.

#### Worked example C — experience can propose a disposition, not manufacture one

The schema-migration chain #1119 → PR #1124 → PR #1188 → #1289 → PR #1292
repeatedly exposed the difference between a fresh-install test and a legacy
migration proof. These events can support an AgentSoul proposal such as
"when persistence schemas change, actively seek a legacy-path discriminator."

The continuity ledger owns the events and outcomes. Engineering precedent owns
the concrete migration rule. AgentSoul may project the reviewed, cross-event
judgment disposition. Event frequency alone cannot promote it, and a later
counterexample must remain attached as counterevidence rather than disappear
behind the projection.

### 1.3 Success is measured by recovery quality, not retained token volume

Continuity changes require a cold-start A/B evaluation against an agent receiving
only the ordinary repository and task context. The primary measures are:

- **current-state accuracy** — correctly distinguishes active, superseded,
  merged, deployed, and owner-pending states;
- **next-step accuracy** — proposes the next unresolved action rather than a
  historically plausible one;
- **duplicate investigation count** — repeats already-settled searches or work;
- **unsupported claim rate** — presents a conclusion without an adequate
  evidence reference;
- **recovery cost** — elapsed time and tokens to regain an actionable state.

More recalled prose is not success. A smaller projection that improves these
measures is preferable to a larger transcript summary that cannot be corrected.

### 1.4 The project-manager projection is bidirectional and non-authoritative

Continuity becomes useful to a human through two translations. Both are views
over evidence; neither may silently create current truth.

```text
IntentBrief {
  verbatim_request
  resolved_project_and_object_refs[]
  inferred_intent_candidates[]
  assumptions[]
  unknowns_or_conflicts[]
  proposed_scope
  acceptance_and_approval_gates[]
  proposed_next_action
  evidence_head
}

OutcomeBrief {
  requested_outcome_ref
  reconciled_state_transitions[]
  verification_and_object_refs[]
  completed_scope[]
  unresolved_or_unverified[]
  decisions_needed[]
  proposed_next_choices[]
  evidence_head
}
```

The inbound path preserves the user's exact words, resolves known project
shorthand and current objects, and labels every interpretation. It asks only
when an ambiguity would materially alter scope, authority, or irreversible
action. The outbound path is generated after object reconciliation; it says
what changed, what evidence supports it, what remains unknown, and what choice
is needed without dumping the tool transcript by default.

#### Worked example D — “整理 open issues” as project management

Suppose the user says: “先把收尾的收尾一下，再把设计方向合并。” A weak
assistant paraphrases this into a todo list from issue titles. The project-manager
projection instead resolves live issue/PR relations, discovers that #1288 and
#1289 were open despite merged implementation PRs, separates owner-protected
design cards from close-ready work, and presents categories plus proposed
close/update actions. The language is concise, but each state remains traceable
to the reconciled evidence head.

After execution, it does not say merely “都整理好了.” It reports which issues
were closed, which protected cards were only updated, which new design owner was
created, which document changes remain uncommitted, and which assertions are
still unverified.

#### Existing partial and missing product wire

`tachi_task(action="brief")` accepts a natural-language task and returns memory/wiki
hits, a coarse intent, SOPs, tool plan, skills, and routing suggestions. This is
an input-side machine brief, not yet the full contract above. Missing pieces are:

- exact current-work resolution and revision-aware evidence from #1297;
- explicit assumptions/conflicts/approval gates instead of silent intent
  classification;
- one human-readable renderer shared by ask, briefing, timeline, and completion;
- a reconciled outbound `OutcomeBrief` rather than model-authored session summary;
- compact-default/detail-on-demand output with immutable receipt references.

#954 remains the sole user-facing ask/conversation owner. #1071 owns the grounded
ask/briefing evidence seam; #952 supplies the generated project timeline; #1297
supplies current truth. No second “project manager memory” store is introduced.

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
Current surface: `tachi_event action=context` returns a dedicated `timeline[]` read-model section with a `TimelineEntry` schema marker when timeline projections are available. Explicit causal edges with existing memory-id endpoints persist through `add_edge` (retired/internalized, not MCP-facing); natural-language endpoint resolution and fully enforced Rust `TimelineEntry` / `JudgmentEvolution` validation remain design targets.

### 2.3 Bonding layer — shared communication protocol

Bonding is a **private dyadic communication protocol keyed by `(user trust domain, agent_identity_id)`**: shorthand references, common investigative history, domain-specific vocabulary, and expected interaction rhythms. It is not shared automatically among agents that happen to use one Tachi instance.

It is **not** an emotional state of the model, nor is it the same thing as an RLHF warmth output. The model does not "feel" attached; it reads a structured read model that compresses bandwidth between user and system.

That said, **affect and warmth are legitimate carriers for bonding**. A warm delivery can make the shared protocol land more effectively, just as an inappropriate cold delivery can undermine it. The key distinction is:
- **Bonding** = the shared context and shorthand the system has accumulated with this user.
- **Warmth/affect** = one delivery carrier that can make bonding feel natural and trustworthy.
- A cold-carrier agent with strong bonding can still be effective; a warm-carrier agent without bonding is only performing generic RLHF politeness.

Current storage is a known pre-migration implementation: `ProjectionKind::Bonding` materializes into unpartitioned `/user/patterns/bonding/<domain>/<hash>` paths. The projector preserves a SharedLexicon-shaped metadata slice at `metadata.lexicon`: `origin_session`, `origin_context`, `meaning`, `shorthand_triggers`, `appropriate_contexts`, `inappropriate_contexts`, `callback_hits`, and `last_successful_use`.
Current surface: `tachi_event action=context` returns a dedicated `bonding[]` read-model section with a `SharedLexicon` schema marker, and the local A2A bundle currently exposes bonding refs. The target private partition MUST exclude bonding refs/content from A2A, workers, and cold seats. Identity-bound migration, exclusion enforcement, a first-class Rust `SharedLexicon` type, and schema validator remain unbuilt.
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
- Ordinary public save facades do not emit `memory.saved`; admitted internal callers can opt into the internal save path (the retired standalone `save_memory`'s crate-internal route; the public `tachi_memory(action="save")` facade intentionally does not expose continuity emission).
- The distill lane can consume `memory.saved` events to discover or update patterns.
- Internal completion and workflow-closure pattern use is recorded through a crate-private append-only evidence seam. Each admitted event carries a real flow id, a source revision, an evidence digest, the exact pattern id, the `seen` / `hit` / `miss` / `stale` outcome, and a deterministic idempotency key. The fixed `tachi.pattern_evidence.v1` adapter writes `CollectOnly` + `EffectScope::None` events with no projection hints, so these receipts never project, promote, or update pattern counters. `tachi_task(action="complete")` and `tachi_gh(action="close_loop")` are admitted only with a non-empty flow id from their owning operational path. A missing identity is a typed skip, never a query/domain/comment fallback. The model-facing `tachi_search scope="patterns"` and `tachi_event action=context` surfaces remain read-only even when a caller supplies session text. There is no model-facing pattern-feedback workflow. Concrete instance memories under `/memory/instances/<pattern_id>/` are still a target.

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

Skill **was** the executable crystallization of patterns; that crystallization leg is **RETIRED by #1690 C3** — promotion emits wiki drafts and agent-profile proposals only, never skill candidates. This section is kept as a historical record of the retired path. Tachi has a complete skill system (`hub_register`, `tachi_skill(action='run'|'discover')`). The retired "second model brain" surfaces that used to ride here — `run_skill` (native alias), `recommend_skill`, `skill_evolve`, and `tachi_skill(action='from_pattern')` — are deleted end-to-end by #1690 C3 (delete list: "skill recommendation and auto-selection", "skill generation"). Pattern memory is a **read-model source for reviewed artifacts**, not an evidence-driven skill-selection layer.

The two integration modes once planned here are both retired:

1. **Recommendation signal** (low risk): **RETIRED by #1690 C3** — `recommend_skill` is deleted; no skill matching consumes pattern context.
2. **Skill generation** (higher risk): `tachi_skill(action="from_pattern")` is **RETIRED by #1690 C3**; patterns never mint skill candidates. Mature patterns promote to reviewed wiki/runbook artifacts only.

This mirrors the Karpathy LLM Wiki flow:

```
Raw notes / sessions → pattern memory → wiki → skill   (historical Karpathy mirror; Tachi's skill leg is retired — see §3.3)
```

Tachi maps this as:

```
Notes / memory / session → /user/patterns/* → /wiki/drafts/patterns/* → reviewed wiki/runbook   (no skill minting — retired by #1690 C3)
```

### 3.4 Eval — the credibility loop

Eval records (`/eval/...`) and `tachi_task(action="complete")` outcomes feed the continuity ledger. `session.outcome` labels are the conversation-domain substitute for quant's `fwd_return`.

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
internal save path (the retired standalone `save_memory`, now the crate-internal save route) with emit_continuity=true → memory.saved event — note: the public `tachi_memory(action="save")` facade intentionally does NOT expose emit_continuity
tachi_search scope=patterns      → explicit read-only /user/patterns recall
tachi_wiki_write include_patterns=true → wiki metadata.pattern_refs[]
tachi_skill action=discover|run  → static reviewed skill surface only (the retired from_pattern action was deleted by #1690 C3; no skill candidate is ever minted from a pattern)
tachi_event action=promote       → wiki draft + agent-profile proposal review artifacts (the disabled skill-candidate artifact is retired by #1690 C3)
tachi_domain_adapter lorebook_import → repo lorebook shape → world_book events
tachi_event action=context       → read-only local context bundle; caller session text is not evidence admission
tachi_event action=a2a           → read-only A2A evidence bundle without context feedback writes
```

Target integration still to add:

```
pattern maturity → external validation + cold-seat check
review artifact → human-approved wiki/runbook promotion
review artifact → human-approved generated skill promotion to listed/enabled   (moot — RETIRED by #1690 C3: no generated skills exist)
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
    → append collect-only pattern.evidence.hit / pattern.evidence.miss receipt
    ↓
Pattern matures (hit_rate / confidence threshold + external validation + cold-seat check)
    → projection report includes review_artifacts
    → review artifact can create /wiki/drafts/patterns/<name>.md
    → review artifact can create an agent_profile.proposal continuity event
      (the skill:<name> candidate leg is retired by #1690 C3 — no skill
      candidate is ever minted from a pattern)
    → human review
    → human-approved wiki/runbook promotion (hub-skill promotion is moot:
      no generated skills exist)
```

---

## 6. Current implementation status

### Implemented

- `memcore` typed `tachi_events` ledger: `TachiEventRecord`, `ProjectionKind`, `AuthorityLevel`, `EffectScope`.
- `tachi-server` `tachi_event` facade with eight actions: `emit`, `query`, `metrics`, `project`, `promote`, `context`, `a2a`, `label_eval`.
- `tachi_status` surfaces `challenge_rate` as a read-only continuity metric.
- `capture_session` emits `session.captured`; optional continuity pipeline emits candidates and `session.outcome`.
- `tachi_task(action="complete")` bridges subagent eval into `task.outcome` / `subagent.evaluated` events.
- `tachi_event action=project` idempotently materializes events into stable projections.
- `tachi_event action=context` returns projected memories plus `patterns`, `pattern_refs`, `bonding`, `timeline`, `lorebook`, `affect`, local `a2a`, and `host_lifecycle` read-model sections. It is read-only and never treats its model-supplied `session_id` as internal evidence admission.
- Pattern/bonding projections retain their legacy `seen / hit / miss / confidence / last_seen` fields for historical readability. New admitted internal pattern-evidence receipts do not update those counters or grant projection/promotion authority.
- Timeline projections carry typed metadata and a validated `TimelineEntry` schema marker for discoveries, decisions, open threads, evolution, causal edges, external validations, and validity fields. Explicit causal edges with existing memory-id endpoints are persisted into `memory_edges`; natural-language-only edges are skipped rather than creating orphans.
- Bonding projections carry SharedLexicon-shaped metadata and a validated `SharedLexicon` schema marker for origin, meaning, shorthand triggers, appropriate/inappropriate contexts, callback hits, and last successful use.
- Context responses expose a local read-only `a2a` evidence bundle that includes share policy, cold-seat constraints, poll-subscription metadata, pattern refs, bonding refs, timeline open threads, and compact event refs without raw payloads. `tachi_event action=a2a` returns the same evidence bundle without context feedback writes.
- Affect projections carry explicit guardrails plus local rule-based signals for language switches, known markers, input/output length, and IO ratio.
- `tachi_event action=label_eval` provides a label-quality harness.
- A held-out label-eval smoke fixture covers `session.outcome` vs `session.outcome.review` matching before `challenge_rate` is used as an over-fit signal.
- the internal save path (the retired standalone `save_memory`) supports explicit `emit_continuity=true`, appending a `memory.saved` event with path/category-derived projection hints.
- `tachi_search` supports `scope="patterns"` and excludes continuity projection rows from ordinary `memory` recall.
- `tachi_wiki_write` supports `include_patterns=true`, persisting active pattern references in wiki metadata.
- `tachi_wiki_write` emits `wiki.saved` continuity events for reviewed wiki writes.
- `tachi_skill(action="from_pattern")` **RETIRED by #1690 C3** — no skill candidate is ever minted from a pattern; the `from_pattern` action is typed-rejected.
- `recommend_skill` and feature/task lightweight skill recommendation **RETIRED by #1690 C3** — no skill matching consumes pattern context; the static task-brief intent map (`selected_sops`) is the only surviving advisory projection.
- `tachi_event action="promote"` executes conservative review-artifact creation for mature patterns: a pending wiki draft and an `agent_profile.proposal` continuity event (the disabled skill-candidate artifact is retired by #1690 C3). It supports `dry_run`, `force`, per-artifact skip flags, and a promotion gate that marks external-validation / cold-seat-review readiness before any final promotion.
- The daemon runs a scoped background continuity projection loop; projection reports include projected, skipped, and promotion candidate counters plus review artifacts for wiki drafts and agent-profile proposals (the `skill_candidate` review artifact is retired by #1690 C3).
- Complete skill system: `hub_register`, `tachi_skill(action='run'|'discover')`, builtin skills. The native `run_skill` alias, `recommend_skill`, and `skill_evolve` are retired by #1690 C3.

### Missing / gaps

1. **P0 current-truth reducer — first slice implemented (#1696), integration slices open**: `tachi-params::current_truth` now provides the §1.2 core — the typed `AssertionV1` vocabulary with predicate-scoped authority admission, the append-only SQLite assertion store with immutable-revision idempotency, the deterministic `current | superseded | conflicted | unknown` reducer, stale-handoff evaluation, the derived open-action projection, and the #1693 consumer read surface (fixtures only). Still missing, as deliberate follow-up slices: the live GitHub adapter over the bounded `gh` read path (today only test fakes implement `GithubRefreshAdapter`), the `tachi_events` observation→assertion bridge, the caller-admission surface binding issuers to authority classes, and any production consumer (#1693's Work Read Model is itself fixture-blocked). Current timeline projections are not a substitute for any of this.
2. **Agent MD crystallization is only first-slice**: the `tachi_profile` tool that used to import/render/context profile packs and target `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, Cursor, and OpenClaw files was retired from the MCP surface under #757, superseded by the memory-line promotion path (#950, #534). `tachi_event action="promote"` can emit an `agent_profile.proposal` event for a mature pattern, but it does not yet render or write host Agent MD files.
3. **No cross-process A2A transport**: a local read-only `a2a` evidence bundle and `action=a2a` poll surface exist, but there is no daemon pub/sub API, no independent cold-seat host profile, and no transport-level evidence/open-question feed.
4. **Label-quality calibration incomplete**: harness and smoke fixture exist, but calibration still needs a larger reviewed held-out corpus, thresholds, and an operator-visible calibration status.
5. **Maturity gates are partially implemented**: projection reports and `tachi_event action="promote"` expose external-validation / cold-seat-review gate status. The gate still does not auto-promote drafts or proposals to final wiki or Agent MD writes (the listed-skill promotion leg is **retired by #1690 C3** — no generated skills exist, so there is no listed-skill target to promote).
6. **Pattern evidence is append-only but downstream interpretation remains partial**: `tachi_task(action="complete")` can consume `pattern:<id>` evidence refs when it has a real flow id, and `tachi_gh(action="close_loop")` can append `hit` evidence for reviewed attached patterns when it has a real flow id. Missing flow identity produces a typed skip. `tachi_search scope="patterns"` and `tachi_event action=context` are read-only. These internal receipts do not project or change counters. The explicit legacy feedback action can still emit counter-mutating `hit` / `miss` / `stale` signals. Ordinary briefing/context use still does not automatically decide hit/miss without downstream outcome evidence.
7. **Timeline graph is partially wired**: timeline projections expose a typed `metadata.timeline` / `timeline[]` read-model slice with a `TimelineEntry` schema marker, and explicit causal edges with existing memory-id endpoints persist to `memory_edges`. Natural-language causal edges, typed `JudgmentEvolution`, and automatic endpoint resolution are still missing.
8. **Bonding privacy migration is unbuilt**: current `/user/patterns/bonding/*` projections are not physically partitioned by `(user trust domain, agent_identity_id)`, and current local A2A bundles expose bonding refs. Migrate to the private relationship partition, bind identity, exclude all bonding refs/content from A2A/workers/cold seats, and add migration/leakage goldens. The current `SharedLexicon` shape also lacks a standalone Rust domain type.
9. **Rule-based affect detector is first-slice**: affect projections extract language switch, known markers, IO ratio, and length signals, but response-latency-like signals are not yet available without host timing input.
10. **Session lifecycle contract is packaged as a read model**: `host_lifecycle` now documents startup preload, in-session buffer, feedback, session-end capture, and profile refresh. Per-host adapters still need to consume that contract consistently.

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
3. Done: the internal save path (the retired standalone `save_memory`) can emit `memory.saved` when `emit_continuity=true` — the public `tachi_memory(action="save")` facade does not expose this knob.
4. Done: `tachi_event action=context` returns `pattern_refs`, `bonding`, `timeline`, `lorebook`, `affect`, local `a2a`, and `host_lifecycle` read-model sections.
5. Done: `tachi_wiki_write` can recall and reference active patterns with `include_patterns=true`.
6. Done: `tachi_wiki_write` emits `wiki.saved` continuity events with reviewed `pattern_refs`.

### Phase 3 — Background projection loop

**Goal:** close the loop without requiring manual `tachi_event action=project`.

1. Done: daemon scheduler calls `project_auto_continuity_events_for_target` for scoped/manifest DBs.
2. Done: auto projection skips `Blocker` and `ExecutionGate` authority events.
3. Done: reports and scheduler logs include projected count, skipped count, promotion candidates, and review artifacts.

### Phase 4 — Skill crystallization (RETIRED by #1690 C3)

**Goal:** mature patterns become executable skills — **retired end-to-end** (delete list: "skill recommendation and auto-selection", "skill generation"; `recommend_skill`, `tachi_skill(action='from_pattern')`, and `skill_evolve` are deleted). The implemented items below describe the retired state, kept as history:

1. Done (retired): `recommend_skill` and lightweight task skill recommendation used active pattern bridge signals and exposed `pattern_refs` on matches.
2. Done (retired): `tachi_skill action=from_pattern` generated skill candidates from active patterns.
3. Done (retired): generated skills were `discoverable`, disabled, pending review, and carried `pattern_ref` metadata.
4. Done (retired): the maturity gate created review artifacts before promotion to `listed`.
5. Done (retired): `tachi_event action="promote"` materialized the pending wiki draft, disabled skill candidate, and agent-profile proposal event for an eligible or forced pattern — the skill-candidate artifact is gone post-#1690 C3.
6. Remaining (moot): generated skill candidates should reference the evidence chain used for promotion, not just the latest pattern projection.

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
- **Current behavior:** after building `memories`, active `/user/patterns` and bonding projections are returned under `"patterns"` with `pattern_ref`; bonding projections are also exposed through `"bonding"` with `SharedLexicon` schema markers and `lexicon`; timeline projections are exposed through `"timeline"` with `TimelineEntry` schema markers and typed timeline metadata; the local `"a2a"` section exposes evidence/open-thread refs without raw payloads; `"host_lifecycle"` exposes the adapter contract. Context returns a **read-only preview receipt** of `seen` feedback for returned pattern refs — the synthetic `seen` events are projected in-memory via `preview_auto_projection_with_events` (`dry_run=true`) and never durably written to the event ledger; `action=a2a` remains read-only as well.

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
- **Current behavior:** `tachi_task(action="complete")` records pattern feedback from `evidence_refs`; success defaults to `hit`, failure to `miss`, and partial/aborted to `seen`.
- **File:** `crates/tachi-server/src/workflow_closure.rs`
- **Current behavior:** `close_loop` writes wiki entries with `include_patterns=true` and records attached reviewed patterns as `hit`.

### 8.3 Memory save emits continuity event when requested

- **File:** `crates/tachi-server/src/memory_search_ops/save_memory/handler.rs` (the internal save path; the standalone `save_memory` tool name is retired)
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
- **Current behavior:** `pub(crate) fn list_active_patterns` is available for wiki integration (the skill-crystallization leg is retired by #1690 C3 — no skill caller exists).
- **File:** `crates/tachi-params/src/memory/wiki.rs`
- **Current behavior:** `WikiWriteParams` has `include_patterns`, `pattern_query`, and `pattern_top_k`.

### 8.5 Pattern → skill generator (RETIRED by #1690 C3)

This section described `crates/tachi-server/src/hub_ops/pattern_to_skill.rs` /
`handle_skill_from_pattern` — **deleted end-to-end in #1690 C3**. Historical
behavior (kept for record):

- `tachi_skill(action="from_pattern")` used to fetch active patterns, build a
  skill definition JSON (system, prompt, content, inputSchema, policy, tags,
  `pattern_ref`), persist via `store.hub_register`, and keep the generated
  skills disabled + pending review (never auto-exposed as listed tools).
- The action is now typed-rejected (`tachi_skill` accepts only `discover`/`run`;
  `crates/tachi-params/src/facade/action_inventory.rs`). Patterns never mint
  skill candidates; promotion creates reviewed wiki/runbook artifacts only.

---

## 9. Open questions

1. **Should any memory categories auto-emit `memory.saved` events?** Current behavior is explicit only (`emit_continuity=true`).
2. **What is the promotion threshold from pattern to wiki/runbook?** Pure hit-rate, or hit-rate + external validation + timeline depth? (The skill promotion leg is retired by #1690 C3 — promotion targets wiki/runbook and agent-profile proposals only.)
3. **How does the cold seat participate in cross-process A2A?** Does it subscribe to events but ignore timeline conclusions, or does it maintain a separate evidence stream?
4. **Pattern scope is authority-specific, not one global switch.** Engineering
   instances may be project-scoped; user-model and dyadic relationship patterns
   live in physically isolated private trust-domain partitions. Cross-project
   promotion requires explicit reviewed export, not implicit global recall.
5. **How should each host adapter consume the lifecycle contract?** `host_lifecycle` defines preload, local buffer, outcome label, pattern feedback, and writeback; Codex/Claude/Gemini/Cursor/OpenClaw still need per-host enforcement.

---

## 10. Summary

Tachi already has a continuity observation substrate: the `tachi_events` ledger, projection machinery, counters, guardrails, typed timeline/bonding read-model slices, wiki references, pattern-derived wiki/runbook + agent-profile review artifacts (skill candidates are retired by #1690 C3), and lifecycle/GitHub evidence surfaces. It does **not** yet have the revision-aware current-truth reducer or an exposed profile-pack rendering surface. Remaining work is reconciliation, loop closure, calibration, stronger schemas, causal endpoint resolution, and final promotion:

- Build the **predicate-authorized current-truth reducer** and derived action queue before treating timeline/handoff output as current state.
- Extend **automatic pattern hit/miss feedback** beyond search/context/complete/close_loop into briefing runtime and outcome-backed hit/miss classification.
- Extend **maturity gates** from explicit readiness reporting into final promotion enforcement for wiki/Agent MD writes (the listed-skill promotion leg is retired by #1690 C3 — no generated skills exist).
- Add **Agent MD crystallization** so mature continuity patterns become reviewed profile proposals that can be rendered for each host.
- Harden **timeline and bonding read models** from schema-marked/validated output into standalone Rust domain types and richer automatic causal endpoint resolution; explicit existing-ID causal edges already persist.
- Extend the local **A2A evidence/open-thread bundle** and poll surface into cross-process transport while preserving the cold seat.
- Keep the **over-fit brake and cold seat** as un-revocable safeguards.

The result is a system that learns the user's alignment, surfaces it when relevant, and turns it into durable knowledge and reviewed wiki/runbook + agent-profile artifacts — the skill-crystallization leg is retired by #1690 C3 — without losing the ability to be challenged or corrected.
