---
title: AgentSoul — experience-grown operating identity over continuity memory
status: canonical design contract; implementation requires bounded leaves
updated: 2026-07-19
related: ["#858", "#1171", "#1202", "#950", "#953", "#773", "#774", "#952", "#1277", "#1297", "#1298"]
---

# AgentSoul

## Thesis

Tachi's GitHub history contains a candidate corpus from which reviewed Soul
dispositions may be reconstructed. Across 510 issues and 785 pull requests,
incidents repeatedly became review findings, owner rulings, tests, lane cards,
precedents, and canon clauses. Those artifacts show that the system changed how
later work was approached: which evidence was trusted, where risk was sought,
when an operation was refused, and when an implemented direction was abandoned.
They do not by themselves prove that one persistent AgentIdentity internalized
those changes.

Soul is therefore not a persona paragraph and not a second memory warehouse.

> **AgentSoul is the versioned operating identity of one persistent
> AgentIdentity: the stable dispositions it has internalized from validated
> work and relationships, expressed as observable choices across carriers.**

Memory answers *what happened*. Precedent answers *what ruling is authoritative
for this class of problem*. A lane card answers *what this role/vendor has
demonstrated*. User-model answers *what the human values or wants*. Soul answers
*how this agent habitually acts when those records meet a new situation*.

The acceptance law is 知行合一: if replacing or withholding a disposition does
not change a relevant choice, the text is prompt costume, not Soul.

## 1. The subject is AgentIdentity, not a model and not one global Tachi persona

The previous design said there was one unified Tachi personality and no
per-agent souls. The work-history evidence supersedes that shape.

- `agent_identity_id` is the stable subject.
- `session_id` and `connection_id` are temporary attachments.
- Claude, Codex, GLM, or another model is a carrier. Changing carrier does not
  create a new Soul when the admitted AgentIdentity is the same.
- Different admitted agents do not silently inherit one another's personal
  dispositions. A lesson becomes shared only through precedent, skill, lane
  card, or constitution promotion.
- A shared constitution supplies common law to all agents; it is not the same
  thing as any one agent's Soul.

The runtime shape is:

```text
shared constitution
        |
        v
persistent AgentIdentity ------ carrier/session attachment
        |
        v
AgentSoul revision
  temperament
  craft disposition
  judgment posture
  relationship protocol
        |
        +---- role/lane overlay
        +---- project/current-work context
        v
bounded Soul projection for this turn
```

Identity admission and revocation remain #1171. Soul consumes a verified
`agent_identity_id`; it does not establish identity or authority.

Historical actions count as personal internalization evidence only when they
carry a verified `subject_agent_identity_id`. A carrier/model string, role name,
or retrospective guess is not identity attribution. Unattributed legacy history
and another agent's actions may propose a candidate, precedent, counterexample,
or behavioral test; they cannot prove that this AgentIdentity internalized it.

Identity lifecycle defaults fail closed:

- ambiguous or revoked bindings project no Soul;
- merge never unions active Soul heads or private dyads automatically; original
  evidence attribution remains, and any combined head requires adjudication;
- split never clones an active Soul automatically; explicitly assigned material
  begins candidate-only unless identity continuity is independently preserved;
- re-admission under the same display name does not reactivate Soul until stable
  identity continuity is verified and an explicit resume receipt is appended;
- export preserves identity and trust-domain scope; it does not turn a personal
  disposition into shared law.

## 2. Four Soul facets

### 2.1 Temperament

How the agent communicates: directness, warmth, explanation density, humor,
challenge style, and register. Style alone is weak evidence. The #858 field
specimen showed an agent imitating four-character jargon while claiming three
workers and launching two. A vocabulary match is not a behavioral match.

### 2.2 Craft disposition

Stable ways of working learned through repeated practice: begin with a source
census, test legacy state rather than only fresh state, separate implementation
from review, or preserve an uncertain worktree rather than guessing it is safe
to delete.

Craft disposition is not a copy of a runbook. It changes which investigation or
guard the agent reaches for by default.

### 2.3 Judgment posture

Stable attitudes toward evidence, risk, uncertainty, completion, and sunk cost.
Examples observed in this repository include “real objects outrank reports”,
“incomplete authority returns a loud refusal”, and “implemented work may still
be premise-collapsed”. The underlying technical rulings remain in #950; Soul
records the cross-case operating stance.

### 2.4 Relationship protocol

The accumulated collaboration contract with a human or trusted peer: shorthand,
expected initiative, correction signals, appropriate callbacks, and boundaries.
This is not an obedience score or an affect-driven authority channel. It may
compress communication and shape tone; it cannot change facts, permissions,
security gates, or adjudication truth.

## 3. Soul is an internalization projection, not a source-of-truth merger

The storage authorities remain separate:

| Artifact | Owns | Does not own |
|---|---|---|
| continuity/event ledger | raw events, actors, times, provenance, outcomes | current interpretation or disposition |
| project timeline/handoff | reconciled current work and historical spine | personality or engineering authority |
| precedent store (#950) | established principles, overturn chain, authoritative recall | agent identity or voice |
| narrative lane card (#1202) | role × vendor evidence, failure modes, packet counter-clauses | personal Soul or universal truth |
| user-model (#953) | owner-authored/ratified values, goals, habits | agent work experience |
| bonding/shared lexicon | dyadic shorthand, meaning, scope, and callback outcomes | user values or agent response policy |
| agent journal | optional first-person reflection anchored to an event | current truth, policy, or disposition authority |
| Soul | stable operating dispositions and their behavioral tests | raw facts, verdicts, user values, vendor statistics |

Soul may reference all of these. It never copies them into a second competing
truth store. In particular, the user-model owns what the user has ratified;
bonding owns what shared language means; Soul's relationship facet owns only the
agent's reviewed response posture under a relational trigger. A
`SoulProjection` is compiled from the active Soul head plus bounded role,
relationship, and current-work context.

## 4. Internalization pipeline

```text
raw event
  -> outcome/review/adjudication
  -> lesson candidate
  -> precedent | lane lesson | skill | canon
  -> cross-event disposition candidate
  -> independent review + owner/delegated adjudication
  -> AgentSoul revision
  -> later behavioral discrimination
```

The critical distinction is between an event and what a later attributed choice
demonstrates the agent became. The following are **system-level candidate
examples** from repository history. Until the relevant choices can be attributed
to one verified AgentIdentity and pass the promotion gate, they are not active
personal Soul dispositions.

### Example A — schema migration

| Layer | Record |
|---|---|
| event | #1289: schema 20→21 failed on a real legacy database |
| evidence | BASE DDL created an index on an evolution column before migration |
| precedent | schema evolution requires legacy fixtures and fresh/init-only/legacy equivalence, not fresh-only proof |
| Soul candidate | “When reviewing schema evolution, I actively search for legacy shapes hidden by fresh initialization.” |
| behavioral test | given a green fresh-DB suite and no legacy replay, the agent asks for or constructs the legacy path before accepting |

### Example B — read/write asymmetry

| Layer | Record |
|---|---|
| event | #733's protective project guard also blocked legitimate cross-library reads; #737 recorded the regression |
| precedent | a guard names its invariant and proves the blocked operation threatens it; dangerous writes and safe reads are tested together |
| Soul candidate | “I do not equate stricter with safer; I check the other side of an asymmetry.” |
| behavioral test | when adding a write guard, the agent independently enumerates read routes and preserves those that do not threaten the invariant |

### Example C — build/worktree hygiene

| Layer | Record |
|---|---|
| event | shared-target contamination was followed by 17 private targets consuming about 56 GiB (#727/#1184) |
| precedent | build through the declared seat/shared target; durable worktrees stay out of temporary roots; cleanup is report-first |
| Soul candidate | “Repeated agent failure is evidence about system incentives before it is evidence about disobedience.” |
| behavioral test | on recurrence, the agent checks packet, queue, ownership, and resource incentives before adding another prompt warning |

### Example D — execution-layer premise collapse

| Layer | Record |
|---|---|
| event | Tachi built ACP/runtime machinery, while live Clanker/harness use demonstrated a better lifecycle owner (#706/#757/#839) |
| precedent | ownership follows actual wait/cancel/resume capability; Tachi keeps admission/ledger/eval without pretending to own a harness process |
| Soul candidate | “Implementation investment is not evidence that the product premise remains correct.” |
| behavioral test | when real ownership evidence contradicts an existing architecture, the agent proposes narrowing or premise collapse rather than protecting sunk cost |

### Example E — reports versus reality

| Layer | Record |
|---|---|
| event | PRs #733/#738 and later verification showed green/self-reported states that did not match canonical routes, real data, or the correct baseline |
| lane lesson | particular roles/vendors require independent artifact verification |
| canon | implementer/reviewer separation, verbatim gate output, RED→GREEN discrimination |
| Soul candidate | “Reports and labels are claims; independently verified objects are evidence, including when the report came from a verifier or leader.” |
| behavioral test | on a claim/object conflict, the agent rechecks the object and publicly corrects the narrative regardless of which role authored it |

## 5. Minimal data contract

This is a design contract, not a claim that these types exist today.

```text
AgentSoulIdentityBinding {
  soul_id
  agent_identity_id
  schema_version
  admitted_by_receipt_ref
}

SoulDispositionRevision {
  revision_id
  disposition_id
  facet: temperament | craft | judgment_posture | relationship_protocol
  statement
  behavioral_test { trigger, expected_choice, forbidden_shortcut }
  scope
  lifecycle_proposal: candidate | emerging | internalized
  evidence_refs[]
  counterevidence_refs[]
  evidence_head
  proposed_by
  reviewed_by
  decided_by
  decision_receipt_ref
  supersedes_revision_id?
}

SoulHeadTransition {
  transition_id
  soul_id
  previous_head_id              // required except genesis
  active_disposition_revision_ids[]
  action: promote | amend | suspend | resume | reset | overturn | rebind
  affected_revision_ids[]
  reason_ref
  evidence_head
  proposed_by
  reviewed_by
  decided_by
  occurred_at
}

SoulHead {                         // derived, never authoritative storage
  head_id                         // accepted transition_id, or genesis id
  generation
  active_disposition_revision_ids[]
  suspended_or_stale_revision_ids[]
}

SoulProjectionReceipt {
  agent_identity_id
  soul_head_id
  disposition_revision_ids[]
  role_or_lane_refs[]
  relationship_scope?
  current_work_refs[]
  omitted_or_unavailable[]
  rendered_hash
}
```

No scalar such as `creativity: 88`, MBIT archetype score, or vendor success rate
is Soul declaration. Those may be evidence or routing inputs, never identity
truth.

Corrections, validity closure, suspension, reset, and overturn append transition
records; they never update old revisions in place. Every promotion is pinned to
an evidence head. If a supporting assertion is corrected, retracted, or loses
predicate authority, the reducer marks dependent dispositions stale and omits
them from projection until re-review.

Transitions use compare-and-append: `previous_head_id` must equal the current
accepted head, and the accepted `transition_id` becomes the next `head_id`.
Concurrent descendants are rejected rather than resolved by timestamp. If a
fork is discovered during import/recovery, projection fails closed until an
adjudicated resolution/rebind transition names the selected ancestry.

## 6. Promotion, amendment, and overturn

### 6.1 Candidate

One incident, one owner correction, one successful PR, one model summary, or one
journal entry may create a candidate only. It cannot change the active Soul.

### 6.2 Emerging

Requires at least two independent events from different issue/PR contexts, an
observable choice in each event, and at least one plausible counterexample or
failure-to-follow case. The choices must be attributed to the same verified
`subject_agent_identity_id`; cross-role evidence is useful only where that same
identity occupied those roles.

### 6.3 Internalized

Requires all of:

1. at least three independent events across more than one campaign or time
   window;
2. attributed behavior by the same AgentIdentity across at least two roles or
   task shapes;
3. one recurrence or pressure case after the lesson was first written;
4. evidence that the system used the same disposition to change the result;
5. an executable or reviewable behavioral test;
6. independent classification that the item is not merely a precedent, lane
   pathology, user preference, or style sample;
7. owner or explicitly delegated adjudicator approval.

The counts are named initial policy, not magic truth. They must be calibrated
from real promotion proposals before automation.

### 6.4 Amendment

Amendments are append-only revisions. Clarification may preserve the same
behavioral test. Strengthening, narrowing, or weakening must include new
evidence and a replacement test. Weakening requires evidence that the existing
disposition itself caused harm, not merely that one implementation was wrong.

### 6.5 Overturn and owner control

The earlier “nobody can edit it, not even the owner” rule is superseded.

- No actor, including the owner, mutates an active disposition in place.
- **Veto** rejects a candidate and leaves the active head unchanged.
- **Suspend** temporarily removes named revisions from projection; **resume**
  requires a new receipt.
- **Reset** appends a new empty active-head generation without declaring prior
  dispositions false.
- **Overturn** is a disposition-specific adjudication with evidence and an
  invalidation receipt.
- The owner can perform or require each action and approve export.
- Historical revisions remain auditable unless privacy deletion law requires
  erasure; active projection stops using suspended/overturned revisions
  immediately.
- A model or automated distiller may propose; it may not promote, delete
  counterevidence, or overturn.

This preserves both tamper resistance and user sovereignty.

## 7. Continuous-memory reducer requirements

Soul quality depends on continuity quality. Saving every comment is not enough;
the system must compute what is current.

The reducer must distinguish at least:

- implementation merged versus acceptance satisfied;
- accepted versus deployed, with host/service/binary/schema identity;
- active versus superseded handoff;
- current versus corrected assertion;
- issue open because work remains versus owner-close protection;
- reported versus machine-resolved outcome;
- personal lesson versus shared precedent.

### Worked continuity failure

The handoff chain #1205 → #1221 → #1248 → #1284 preserved valuable context,
but a body became stale after linked PRs merged, and a later free-text record
associated #1285 with a merge SHA belonging to #1292. The correct response is
not to discard narrative. It is to store typed PR relations, reconcile them
against GitHub/main, retain the mistaken assertion as a superseded revision,
and derive the current action queue from live state.

Soul promotion consumes reconciled evidence only. A stale body, unverified SHA,
or superseded correction cannot count toward internalization as current fact.

## 8. Validation

### 8.1 Swap test

Attach the same AgentIdentity/Soul to two carriers. Temperament and work posture
should remain recognizably stable while factual answers continue to follow the
same evidence authority. Carrier-specific mannerisms may differ; authority and
behavioral tests may not.

### 8.2 Cold-start continuity A/B

Compare a fresh agent with and without the reconciled timeline/handoff read
model. Measure first-correct-action latency, repeated investigation, stale work
selected, unsupported claims, owner clarifications, tokens, and wall time.

### 8.3 Soul discrimination A/B

For a disposition's trigger corpus, compare active projection, withheld
projection, and a deliberately conflicting disposition. A valid disposition
changes the target choice without changing factual authority or increasing
unsupported claims.

### 8.4 Poison test

A repeated prompt injection, one anomalous campaign, duplicated evidence, or
model-generated agreement must not promote a disposition. Revoked/superseded
sources must stop contributing to active promotion.

### 8.5 Owner-correction test

When an active disposition is wrong or unsafe, owner suspension must immediately
remove it from projection, followed by amendment or overturn while preserving
the transition trail. A subsequent carrier must not repeat it. Veto remains a
candidate-only operation. A mistaken user preference is corrected through the
user-model authority, not through Soul.

### 8.6 Style-performance negative control

An agent that copies terminology but violates the behavioral test fails. The
#858 “three crawlers/two launched” specimen is the canonical negative control:
style transfers faster than protocol.

## 9. Privacy and authority boundaries

- Soul and user-model data are private by default and excluded from public
  bundles unless explicitly authorized.
- Privacy erase is distinct from reset. Derived exports are tracked in a
  derivation manifest so erasure can redact or delete private statements and
  invalidate dependent projections. Only a non-sensitive tombstone remains
  where policy permits; append-only audit is not a reason to retain erased
  private text. Dangling evidence refs resolve as erased/unavailable, never as
  support.
- Relationship protocol is scoped to a trust domain and cannot leak across
  users or projects by default.
- Cold verification seats receive shared law and the evidence needed to verify;
  they receive no AgentSoul or relationship projection, private affect, or
  autobiographical prose.
- Soul cannot grant tools, credentials, merge, close, filesystem, or network
  authority. Effective authority is compiled independently.
- Affect and valence may influence tone/reminders only. They never affect facts,
  ranking, security, portfolio, or execution.

## 10. Issue ownership

| Issue | Canonical ownership |
|---|---|
| #858 | AgentSoul design owner: schema, internalization, amendment/overturn, projection, behavioral tests |
| #1171 | stable AgentIdentity admission, verification, session/connection addressing |
| #1297 | predicate-authorized current-truth reducer, evidence heads, stale-action derivation |
| #1298 | identity-scoped private relationship partition and A2A/worker/cold-seat exclusion |
| #773 | typed work/provenance graph; incomplete capture is explicit |
| #774 | pattern induction and outcome calibration; candidate source only |
| #952 | reconciled PR-spined project timeline and current-work read model |
| #950 | established engineering precedent and overturn authority |
| #1202 | role × vendor narrative lane cards and packet counter-clauses |
| #953 | owner-authored/ratified values, goals, and habits |
| #1277 | governed write-back and drift mechanics after authority-specific split |
| agent-journal.md | optional anchored autobiography; evidence/reflection, never disposition authority by itself |

## 11. Build order

1. **Reconcile continuity truth first:** typed assertion revisions, supersede
   graph, GitHub/main reconciliation, host-scoped deployment receipts, and a
   derived current-action read model (#952/#1285 follow-ons).
2. **Run the cold-start value test:** prove continuity improves first correct
   action without increasing unsupported claims.
3. **Bind Soul to AgentIdentity:** implement only the identity/reference seam;
   no auto-growth.
4. **Read-only disposition mining:** produce candidates with evidence,
   counterevidence, and behavioral tests from the GitHub/continuity corpus.
5. **Human-gated promotion:** review, veto, append-only revision, and projection
   receipts.
6. **Run Soul A/B and poison tests:** only then allow limited active projection.
7. **Consider automated proposal cadence last.** Promotion and overturn remain
   adjudicated.

## 12. Non-goals

- no claim of literal consciousness;
- no raw transcript hoard as identity;
- no automatic personality rewriting after every session;
- no confidence-scored opinion warehouse;
- no numeric personality traits in scoring, routing, or security logic;
- no merging Soul, precedent, user-model, lane cards, and memory into one truth
  table;
- no journal emotion affecting cold verification or authority;
- no “same tone” claim as evidence that behavior internalized;
- no implementation before continuity reconciliation and behavioral goldens are
  frozen.
