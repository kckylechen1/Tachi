# Research: from search to knowledge creation

Status: design ratified by owner 2026-07-05. Depends on #515 (context7 through the hub)
and #527 principle 5 (extract/distill lanes doing the work).

## The verb taxonomy (settled 2026-07-05)

| Verb | Question it answers | Contract | Cost |
| --- | --- | --- | --- |
| `search` (memory) | "what evidence do we hold?" | sync, ranked hits, deterministic-ish | ms, ~zero LLM |
| `ask` (memory) | "what do we already know about X?" | sync, cited answer over INTERNAL evidence only | one LLM call |
| `research` (new) | "what does the world know, and what does it change for us?" | async dispatch, multi-step, external+internal, verified, cited report artifact | a pipeline |

One-liners: search returns evidence; ask turns existing evidence into an answer;
research creates new evidence. In unknowns language: search fetches known knowns,
ask composes them, research attacks known unknowns.

Boundary rules that keep the trio composable without fusing:
- **ask's honesty clause:** insufficient internal evidence → say so and SUGGEST research;
  never auto-escalate (a boolean hiding a 100× cost jump betrays the caller — same law
  as #527's response economics).
- **research's first step is always search/ask** — check our own head before going out.
- ask stays in `tachi_memory`; folding it into search as `synthesize=true` was
  considered and REJECTED: the two promise different contracts at different cost
  classes, and cost-opaque flags are how facades rot.

## Pipeline (the manual rehearsal was 2026-07-05: two pasted articles → spec changes)

```
question / URL (+ optional issue_ref)
  → plan queries
  → fan-out retrieval: web (tachi_web_search backends) + external docs (context7 via
    hub, #515) + internal (memory / wiki / docs / issues via doc_index)
  → fetch + extract (background extract lane — #527 principle 5)
  → COLD verification of load-bearing claims (web content is untrusted input per
    security-model T3; researcher runs read-only delegate profile)
  → synthesize cited report → report.md in the flow run dir (the evidence original)
  → wiki draft with citations + fetched_at freshness (the distilled conclusion;
    machine-drafted → advisory tier, per the three-tier knowledge doctrine)
  → IMPACT ROUTING: "findings → affected specs/issues/wiki + suggested actions",
    leader-adjudicated before anything lands (generalizes pr_review_digest's
    leader-verdict routing plan)
```

Impact routing is the load-bearing stage: a report that doesn't route into specs is
shelf-ware. The 2026-07-05 rehearsal's value was exactly the manual routing of two
articles' findings into #495/#516/#468 and the pattern-memory spec.

## Lifecycle integration (research is a node type in the issue↔flow↔PR↔wiki graph)

- research → issue: routed findings land as issue comments / new issues (the "工作进
  issue" discipline, mechanized).
- issue → research: pre-spec blind-spot pass — intake/cycle_status may suggest a light
  research when a leaf touches unfamiliar territory (the Fable field guide's
  blind-spot pass, in-pipeline).
- PR → research: review packets may attach relevant research (official docs digest
  for an unfamiliar upstream API) — ammunition for the 2-gate.
- research → wiki → briefing: distilled reports surface in later flows' briefings —
  external knowledge enters the compounding loop instead of evaporating in one session.

## Trigger modes

v1: **question mode** (`tachi research "<q>"`, full pipeline) and **feed mode** (drop a
URL → fetch + digest + impact routing, no fan-out). NOT v1: ambient monitoring; sole
exception is #497's drift detector optionally researching "what changed upstream and
what does it touch".

## Execution shape

Research is a dispatched pipeline, not a query: researcher dispatch profile
(read-only, delegate tier, write authority = its own report artifact only), retrieval
on cheap models / synthesis on a strong model (SP6 tiering), eval row per run (was the
report adopted?) — research quality joins the same flywheel as routes and skills.
Zero new infrastructure: dispatch/verify/eval/wiki/doc_index all exist.

## Non-goals

Auto-editing specs (impact list is proposals; the leader ratifies — 2-gate).
Replacing `ask` or `search`. Ambient feed-watching. A general crawler.
