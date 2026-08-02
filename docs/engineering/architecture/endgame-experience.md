# The Endgame Experience (北极星)

> Owner-articulated 2026-07-10; AgentSoul clarification ratified 2026-07-19.
> **Discipline of this document**: it states *invariants of the finished experience*
> and their acceptance tests — nothing else. It contains **zero current-state
> claims** (those rot; see how fast #734's status sections aged). Mechanisms live
> in the issues/epics referenced at the end; this file is the single source for
> *what done feels like*, and only that.

## The one-line endgame

Anywhere, on any machine, with any model as the carrier, the experience is
identical — because every kind of work is a conversation with agents, and
everything that makes those agents *ours* (memory, language, judgment,
personality, crew, librarian) lives in Tachi, not in any vendor's weights or any
single machine's disk.

## The interaction invariant: conversation feels like a project manager

The human may speak in fragments, shorthand, corrections, or an unstructured
dump. They do not manually assemble task packets or read tool transcripts. One
project-aware conversation layer performs two grounded translations:

```text
human intent
  → resolve project/current-work context
  → explicit assumptions, unknowns, proposed scope, acceptance and next action
  → machine/crew execution
  → reconcile objects, verification and unresolved work
  → concise human status, decisions needed and next choices
```

This is not permission for a model to turn ambiguity into invented authority.
The original request remains verbatim evidence; inferred intent is labeled,
material ambiguity is surfaced, and irreversible/shared actions retain their
approval gates. On the return path, a fluent progress report cannot outrank PR,
test, deployment, or owner-decision evidence. Machine receipts remain available
for inspection, but the default interface speaks in project-aware human terms.

“Project manager” is a conversational projection over the same truth and ask
contracts, not a new truth store or workflow engine. #954 owns the single
user-facing ask/conversation lifecycle; current truth, timeline, task briefing,
and execution stay with their existing owners.

## The five pillars (experience invariants)

### 1. The memory substrate holds
Facts recalled are true-or-flagged (truth maintenance), digested rather than
hoarded (consolidation), and reachable by time and cause ("what happened around
X", "what overturned this"). The substrate is one organism — a typed graph
grown as a rebuildable projection over the event ledger — not a pile of
features.

### 2. The crew is the product
Work is done by seats — implementer, independent reviewer, verifier, mechanical
clerk — enforcing the invariants that were proven in live operation:
implementation and review never share a seat or model; the different-model
boundary applies equally to security, credentials, identity/authority, egress,
and merge/release gates, while cross-vendor routing remains optional defense-in-depth; no
self-report is trusted without independent verification (自报勿信), and change
lands only as a reviewable PR (呈单即止). The seats are the product; any
particular model staffing them is not. The leader seat is a *role*: its
judgment is fed by the precedent store (pillar 4), so swapping the leader model
preserves the judging.

### 3. Work circulates as issues and PRs
The unit of intent is an issue; the unit of change is a PR; state is evented.
The endgame loop runs unattended: issue in → seats dispatched → verified,
reviewed, fix-rounds → PR out — and stops there. **Merging is a human act, by
design.** Autonomy is not granted, it is *earned per lane* from accumulated
eval evidence, and every unattended loop has a watcher (a wedged lane is a
paged event, not a silent stall).

### 4. Memory is two-bodied
- **Project memory** — any agent, cold, can pick up any project: it reads one
  timeline whose spine is merged PRs (annotated between the vertebrae by
  checkpoints), generated from ground truth and never hand-written, and can
  state what happened yesterday and what is next.
- **The agent's own memory** — an internalization gradient, ascending in
  slowness-of-change and review cost, while preserving distinct authorities:

  ```
  events/facts ──┬──> knowledge (wiki/guide)
                 ├──> precedent (engineering rulings)
                 ├──> lane cards (role/carrier evidence)
                 └──> AgentSoul proposals ──review──> operating identity

  user-model (human values/goals/habits) ──separate sensitive authority
  ```

  - **Precedents (L3)**: principle-level records of adjudications — case,
    options, ruling, doctrine cited, outcome. Established automatically when a
    leader ruling is validated by results; the owner holds overturn power; the
    overturn chain is first-class (truth maintenance applies to judgments).
    Decomposition from verdict text to principles is a backend-model job.
    Repeatedly-validated precedents harden upward: 判例 → principle →
    constitutional clause, by the amendment threshold (a large consistent body
    of experience; never a single edit).
  - **AgentSoul (operating identity)**: the stable temperament, craft habits,
    judgment posture, and relationship protocol of one persistent
    `AgentIdentity`. It grows from reviewed, cross-event behavioral evidence;
    register, 梗, and warmth are only its conversational surface. The model may
    propose a revision but cannot install one. The owner can veto, suspend,
    reset, or overturn the active projection through append-only transitions;
    privacy erasure separately removes sensitive source/derived text and leaves
    only a non-sensitive tombstone where policy permits. Swap the carrier, keep
    the admitted agent and its reviewed operating identity.
  - **The user-model**: the agent understands its human — habits, goals, and
    the *why* behind preferences (values, from which tactics are derived).
    Values change slowly and are hand-ratified; habits are learned from
    corrections. It is the system's most sensitive store and carries the
    soul's tamper-proofing posture.

### 5. The librarian answers
Any agent can `ask`. The first beat answers from memory instantly; when depth
is wanted or confidence is low, a resident background agent fuses web search,
memory, engineering practice, and standard documentation into a cited answer —
and the distilled result flows back into the library (advisory tier), so the
library grows from being used. Cost is tiered: remembering is free, research
is deliberate.

## Acceptance (the four-question test)

1. **Swap test** — change carrier or machine while retaining the same admitted
   AgentIdentity: under comparable pressure, does it make the same evidenced
   choices without being reminded? Same 梗 or tone alone does not pass.
2. **Cold-start test** — a fresh agent on any project reads the timeline and
   states, unprompted, what was done yesterday and what is next?
3. **Unattended-loop test** — file an issue: it comes back as a reviewed,
   verified, open PR with no human step in between?
4. **Librarian test** — one `ask` returns a fused, cited answer drawing on all
   four sources?

Plus the standing discrimination law inherited from the soul design: swapping
any grown layer (precedents, AgentSoul, user-model) must *change behavior*;
if behavior is unchanged, that layer is a prompt costume, not memory.

## Owner rulings recorded (2026-07-10; Soul scope clarified 2026-07-19)

1. Precedent granularity is **per principle**, decomposed by a backend model.
2. Precedents establish automatically on ruling + validation; the **owner holds
   overturn power** (the propose/review/apply lifecycle is the veto surface).
3. Precedent, lane card, user-model, and AgentSoul consume a common evidence
   substrate but remain **distinct authorities**, not one editable profile.
4. **Soul = operating identity**, not personality alone. Engineering truth
   belongs to precedent; Soul can internalize a judgment posture (for example,
   seeking legacy-path evidence) but cannot establish the concrete ruling.
5. Review economics: **small tasks get one review round**; multi-round
   adversarial battles are reserved for security, authorization, external-input
   surfaces, and large structural change.

## Where the mechanisms live

Substrate #734 #773 #774 · crew/runtime #839 #894 · circulation #906 (+ the
autonomous-loop leaf, to be filed) · project timeline (leaf, to be filed) ·
precedents (leaf: filed with this document) · soul #855 #858 · user-model
(leaf, to be filed) · ask/project-manager conversation #954/#1071 · research
source adapter #530 under the #1467 product boundary · multi-device sync
(leaf, to be filed). Mechanism details belong in
those issues and their design docs — never here.
