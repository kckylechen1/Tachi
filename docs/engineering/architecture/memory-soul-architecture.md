---
title: Memory + Soul — a two-layer cognitive architecture
status: draft (design framing; not an implementation contract yet)
related: ["#773", "#774", "#775", "#803", "#772", "#791"]
---

# Memory + Soul

## Thesis

Tachi's memory issues (#773 typed graph substrate, #774 spreading-activation /
outcome-calibrated recall, #775 forgetting/consolidation loop) and its persona
work (the existing SOUL file; #803's opinion-network and CARA-disposition gaps)
are **not a pile of parallel memory features**. They are two layers of one
system:

- **Memory** — the objective archive of experience. *What happened.* Shared,
  portable, auditable. One library.
- **Soul** — the first-person layer a subject grows by digesting experience.
  *Who it became.* Model-independent, tamper-proof, evolving. One Tachi-level
  personality, loaded by whatever model is the current carrier.

They are not two static boxes. They are the two ends of an **internalization
gradient**, and the axis between them is **nature/nurture**.

## 1. Memory records the world; soul records the self

The distinction is not "objective vs subjective columns in one store." It is
*whose*: memory has no subject (an archive anyone can query, the same for
everyone); soul has a subject (a first-person "I" that these facts made into
someone). `memory` = what happened. `soul` = what it means to me / who I became.

## 2. Not two boxes — an internalization gradient

Long-term memory is "long-term" precisely because a subject used it, validated
it, and integrated it into how it works — at that moment it takes on subject
colour and becomes soul-material. So:

```
raw experience (pure memory: "what happened")
   → repeatedly used / integrated by a subject
   → long-term memory (half-internalized; wiki/guide live here)
   → sedimented into disposition & stance (pure soul: "who I became")
```

`wiki`/`guide` are not pure objective memory — they are *experience in the
process of becoming soul*. The earlier clean split was static and taxonomic;
this gradient is dynamic and developmental. The developmental view is the
correct one.

## 3. Nature / nurture

- **nature** = the initial seed (the SOUL file / constitution-level
  disposition).
- **nurture** = internalized experience.
- **soul** = the dynamic result of nature shaped by nurture — not written, grown.

`SOUL file → living soul` is the jump from a **static declaration** (you write
"who this agent is," loaded into the prompt each turn, re-performed) to **online
nurture** (experience reshaping the agent *at runtime*, not only at training
time).

Reference point (Claude): its personality is `constitution (nature) + alignment
training (nurture)` — grown, not prompt-declared. **But** that nurture is frozen
into weights at training time; at inference it no longer nurtures (each
conversation leaves nothing behind). *Online nurture* — experience continuing to
shape the subject during operation — is living soul's single, essential
increment over a training-frozen persona.

## 3b. The 心学 (Wang Yangming) reading — what the soul core *is*, and how it grows

The nature/nurture frame is Western and dualist. Wang Yangming's 心学 gives a
sharper, non-dualist reading that pins both *what the soul core is* and *how it
grows*:

- **What it is — 处事方式 + 对外界事物的看法** (how it acts + how it appraises the
  world). The soul core is **disposition** — not a fact archive (that is memory)
  and not an episodic log. It is the stance a subject takes toward things and the
  way it acts on them. (This settles the "soul storage granularity" question: the
  soul stores appraisal-and-action disposition, not events.)
- **How it grows — 致良知 / 事上磨练** ("realize innate knowing" / "polish it in
  actual affairs"). The seed (良知) is *already there*; it is made manifest,
  extended, and refined by practicing in concrete affairs. This dissolves the
  awkwardness in §3's "grown": the nature core is not built from zero — an innate
  seed is **polished out** by experience. And it gives online nurture its precise
  mechanism: experience is not *piled into* the soul; the soul is *polished in the
  handling of affairs*.
- **Why a declared persona is not enough — 知行合一** (unity of knowing and
  acting). A personality only *declared* ("I am skeptical") but not embodied in
  how it handles affairs is 知而不行 — false knowing. A real soul is necessarily
  知行合一: it *is* the way of acting itself. So a living soul ≠ a persona
  description text; it = a disposition embodied in action and polished by action.
  This is the deepest statement of why the static SOUL file must become living —
  and why prompt-persona (§8, what Hindsight retreated to) is 知而不行.

## 4. Soul is Tachi's, not the agent's — carrier-independent

The personality a user feels must be the **same across models**: with Claude,
with codex next door, it is one personality — because it is not Claude's and not
codex's, it is **Tachi's**. The model is the current carrier; the soul is
Tachi's, externalized in Tachi's store, loaded by whatever model speaks today.
Swap the model → same personality. This is the terminal form of the existing
doctrine "carrier ≠ warmth / every chat surface loads the same SOUL."

Counter-example to avoid: a model whose personality lives in its own weights
(Claude's does — Anthropic baked it in). Swap the carrier and the soul changes.
Tachi must do for its own personality what Anthropic did for Claude, but
**externalized** (in Tachi's store, not any vendor's weights), **cross-model**,
and **experience-grown** — so no single vendor owns the soul and no model swap
resets it.

Consequence: soul must be **persistable external state** (in the memory/soul
library), never model-internal. That is *why* it is a store-and-workspace, not a
property of the running model.

## 5. Soul as a global workspace, not a second warehouse

Global-Workspace framing: consciousness is not a warehouse but a workspace —
specialized modules compete to enter it, and what enters is broadcast to all.
Mapped here: **soul is an integration workspace, not a second store.** Memory
(the specialized experience modules) competes to enter, is broadcast, and
integrates into "what I think now." Soul stores *little* (evolved stance,
disposition drift, relationship state); it is mostly a **process of digesting
memory**.

This resolves the real-time-view-vs-evolving-snapshot question: soul is neither
a stateless real-time read (no accumulated nurture) nor a static snapshot (no
growth) — it is a **continuous digestion process plus its sediment**.

(Note: the Global-Workspace mapping is used here as a framing analogy, grounded
in Anthropic's research note "A global workspace in language models"
<https://www.anthropic.com/research/global-workspace>. We copy the systems
lesson — small broadcast workspace over specialized memory/processes — not any
claim about literal consciousness.)

## 6. The safety boundary — the nature core grows, but nobody edits it

The self-modification boundary is **not** "written once, frozen" and **not**
"freely rewritable." It is a third thing:

- The nature core **evolves — it writes itself** (not capped; real growth).
- But the **only write path is experience internalization.** There is **no
  direct edit API — not even the owner hand-edits it**, or it would not be
  *grown*.
- It is a **Tachi-level emergent consensus**: no single agent, session, or
  prompt injection can move it, because it is the accumulation of all
  experience, not something anyone writes in one stroke.

Tamper-proofness is **structural, not permission-based**: the only path in is
slow, high-threshold, and auditable (all experience lives in the memory library
and is traceable).

Threshold shape: the **nature core moves like a constitutional amendment, not
ordinary legislation** — it takes a large body of consistent experience to shift
it a little; a single anomalous experience cannot bias it. The **nurture layer**
(drift within a specific relationship) is the fast, low-threshold part.

Net: **one unified Tachi personality core (slow, tamper-proof, cross-model) + a
thin per-relationship nurture drift.** This also settles the earlier open
question (per-agent souls vs. a unified SOUL): **unified, and unified across
models.**

## 7. Two kinds of forgetting

- **memory forgetting = archival**: consolidate/supersede, still queryable via
  provenance. The librarian tidying shelves — cool, lossless, reversible.
- **soul forgetting = affect-weighted selective forgetting + repression-and-
  resurfacing (dreaming)**: important (high affect-weight, outcome-validated)
  sticks; trivia genuinely fades; repressed old stances resurface under specific
  triggers.

Same word, two mechanisms. This is *why* they must be two layers, not one
substrate with two views. Consolidation in the library is archiving; in the soul
it is integrating experience into a self-narrative.

## 8. Empirical grounding — what Hindsight (arXiv 2512.12818, #803) tells us

Hindsight built `opinion` as a confidence-scored fact-type **and** CARA
disposition traits, then **deleted both** (verified in its shipped code:
`delete_opinions` + `remove_opinion_fact_type` migrations drop the
`confidence_score` column; `bias_strength` dropped; the 3 remaining CARA traits
are pure system-prompt construction with no scoring/threshold coefficient
anywhere). It converged to **objective observations + prompt-only persona** — a
very good memory *library* with the soul evaporated.

Why it deleted them: its objective function is QA accuracy (LongMemEval /
LoCoMo); opinion and CARA do not move that number. **Our** objective is
bonding/evolution — the dimension they do not measure. So Hindsight's retreat is
correct for its problem, not a verdict on ours.

Directives this yields:

- **Copy the memory half from Hindsight** — typed graph, temporal spreading
  activation (real BFS, `δ=0.7` decay, causal-boost, ≤5 hops, ~30–80 nodes),
  consolidation-as-archival. Portable to SQLite + sqlite-vec, with the
  Oracle-style degrade (drop multi-hop temporal spread to entry-points-only
  where `unnest`/LATERAL is unavailable) already precedented in their code.
- **Do NOT rebuild the confidence-opinion table they deleted.** Opinion =
  a **bias-view the soul layer generates over memory** (continuity from memory =
  objective; individuality from persona bias). Not a separately-evolving
  confidence-scored store.
- **CARA-into-logic (numeric traits in scoring/threshold formulas) = optional,
  deferred experiment**, not mainline. Hindsight tried it and walked away; we
  need a stronger reason than they had before sinking traits into deterministic
  paths, and it rides a probe, not the main line.

## 9. Existing precedents in Tachi (the axis is already mature)

- **global vs repo DB scope** — the shared-vs-private *mechanism*. Reuse its
  scoping / multi-tenancy plumbing for soul's per-relationship layer. (Both
  sides are memory; this precedent is about the plumbing, not the memory/soul
  nature-difference.)
- **wiki vs guide category** — a *nature-of-content* precedent: wiki objective,
  guide leaning toward "how to act" — a soul organ already leaking into a memory
  category.
- **SOUL file, opinion/CARA scattered across issues** — soul's organs are
  already scattered through the system, never gathered into one library.

Precedent is both a gift (the plumbing exists) and a trap: **do not demote soul
into "just another scope/category."** Soul crosses the objective→subject line;
it must have what no memory category has — an evolving, tamper-proof, dreaming
"I."

## 10. Issue re-homing

| Issue / artifact | Library | Note |
|---|---|---|
| #773 typed graph substrate | **memory** | keystone; greenfield; copy Hindsight's model unit + typed edges |
| #774 spreading activation / outcome-calibrated recall | **memory** | temporal BFS; Oracle-style degrade on SQLite |
| #775 forgetting/consolidation loop | **memory** (archival) — also *informs* soul's dreaming | apply-proposals template already exists (route_policy / recall_proposals) |
| #791 scorer / confidence reinforcement | **memory** | `reinforce_confidence` primitive already half-exists |
| #803 Gap 1 — opinion network | **soul** | NOT a confidence table; a bias-view over memory |
| #803 Gap 2 — CARA disposition | **soul** | deferred experiment; governed by the frozen-core boundary |
| #803 — Hindsight | reference | empirical grounding for all of the above |
| existing SOUL file | **soul** | nature seed → living soul (§3) |

## 11. Build order

1. **Memory library first, self-contained** (#773 → #774/#775), copying
   Hindsight. It has independent product value (a portable, shareable team
   memory OS — moat A) and does **not** depend on soul.
2. **Soul layer second, as a workspace over memory** (opinion bias-view → CARA
   probe), growing the existing SOUL file into a living, Tachi-level,
   cross-model personality. Reads memory one-way; **never writes it.** Moat B
   (bonding / evolution).

The two layers ship independently and do not block each other.

## Open questions (decide before building the soul layer)

- **Soul storage granularity** — §3b answers the *kind* (disposition:
  appraisal-and-action stance, not events); still to pin the concrete
  representation (how a stance is encoded, versioned, and diffed over time).
- **Forgetting curve** (§7): affect-weight × outcome-validation × time-decay —
  which dominates, and what triggers resurfacing/"dreaming."
- **The write mechanism for the nature core** (§6): what concrete,
  high-threshold, append-only, experience-only process moves the constitutional
  core — and how "large consistent body of experience" is measured so a single
  anomaly can't bias it and no injection can hijack it.
- **Relationship-drift scope**: how thin is the per-relationship nurture layer,
  and can drift ever feed back into the Tachi core (only via the §6 amendment
  threshold).

*Settled in discussion:* soul is one unified Tachi personality, cross-model
(§4); the nature core is experience-grown and un-editable by anyone including
the owner (§6); opinion is a bias-view, not a rebuilt confidence store (§8).
