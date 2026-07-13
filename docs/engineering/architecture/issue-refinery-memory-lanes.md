# Issue Refinery and Memory Knowledge Lanes

**Status:** proposed canonical architecture, owner-approved direction 2026-07-13

**Contracts:** [#1002](https://github.com/kckylechen1/tachi/issues/1002), [#1043](https://github.com/kckylechen1/tachi/issues/1043), [#1059](https://github.com/kckylechen1/tachi/issues/1059), [#950](https://github.com/kckylechen1/tachi/issues/950)

**Related:** [`project-cycle-memory-spine.md`](./project-cycle-memory-spine.md), [`dispatch-lifecycle.md`](./dispatch-lifecycle.md), [`recall-quality-architecture.md`](./recall-quality-architecture.md)

## 1. Decision

Issue refinement and long-term memory use one evidence lifecycle, but they do not
share authority.

```text
GitHub issue / PR snapshot       repo doc/spec @ blob SHA       code + verification
           \                            |                            /
            +---------------- Evidence bundle ---------------------+
                                      |
                              proposal + refutation
                                      |
                             leader/owner adjudication
                                      |
              +-----------------------+------------------------+
              |                       |                        |
       GitHub disposition       active wiki/guide       lesson/precedent candidate
                                                               |
                                                     #950 establishment gate
```

The durable rule is:

- issue/PR owns current work state and priority;
- a repo doc/spec at an immutable revision reachable from an owner-controlled,
  adjudicated ref owns accepted design, API, and invariants;
- a commit plus verification receipt owns implementation evidence; delivery state
  is `implemented_unmerged | merged | released`, and `shipped` additionally
  requires reachability from an owner-controlled main/release ref;
- memory owns atomic evidence and working context;
- wiki owns reviewed, human-readable knowledge projections;
- guides own reusable playbooks;
- #950 alone establishes or overturns precedents.

Memory, wiki, model output, and GitHub merged/closed state cannot promote
themselves into a higher authority class.

## 2. Why the current lane model is retired

`Extract`, `Reasoning`, `Distill`, and `Summary` currently mix three unrelated
concerns:

1. provider/model configuration;
2. authority to make a judgment;
3. the shape of the output.

The names therefore promise separation that the runtime does not provide. A
distill configuration may resolve to the same endpoint/model as extract;
`generate_distill` may use the summary route; reasoning may fall back to a
different HTTP lane; and a lane label does not prove which engine actually ran.

The static chat-lane names may remain as compatibility aliases during migration,
but no product contract may use them as an authority boundary.

### 2.1 Replacement capability seats

| Seat | Responsibility | May not |
|---|---|---|
| **Evidence compiler** | Resolve immutable sources, atomize claims, preserve spans/hashes, build typed relations | decide whether a claim is true, truncate a source, or supersede raw evidence |
| **Proposal reasoner** | Produce a disposition, lesson, or doc-delta proposal from an evidence bundle | establish truth, claim HEAD verification without tools, or apply a write |
| **Independent verifier** | Try to refute claims and proposals against source/code/tests; report missing checks | share the proposal seat's effective engine when independence is required, or self-apply findings |
| **Projection renderer** | Deterministically render approved records into comments, labels, wiki, guides, or briefing views | change claims, disposition, authority, or approval state |

`Summary` is an output format of the renderer, not a seat. Model-assisted prose is
allowed, but the renderer must preserve the approved typed fields exactly.

### 2.2 Parser first, model second

The evidence compiler uses deterministic parsers for issue refs, file anchors,
SHAs, test names, relation markers, and source spans. A model may propose
additional atoms, but every atom carries source coverage and the compiler reports
uncovered input. No issue workflow may reuse the daily distill behavior that
truncates each source or drops an over-budget tail.

## 3. One typed evidence envelope

Every downstream object uses the same envelope and a discriminated payload.
All `V1` shapes in this document are target contracts, not callable runtime
schemas, until their named leaf lands.

```text
SourceKindV1 = issue | comment | pr | commit | canonical_doc |
               episodic_memory | wiki | guide | precedent | eval | runtime

EvidenceEnvelopeV1<T> {
  schema_version
  packet_id
  payload_kind
  authority_class: current_work | canonical | verification | advisory | playbook | precedent
  adjudication_status: observation | proposal | approved | established
  evidence_refs: EvidenceRefV1[]
  source_bundle_hash
  observed_at
  repo_revisions: RepoRevisionV1[]
  engine_receipt?
  review_receipt?
  payload: T
}

EvidenceRefV1 {
  relation: derived_from | supports | contradicts | supersedes | applies_to
  target_kind: SourceKindV1
  ref
  immutable_revision:
    issue_snapshot_hash | issue_body_hash |
    { comment_id, updated_at, body_hash } |
    pr_snapshot_hash | pr_head_sha | blob_sha | memory_revision
  section_or_span?
  captured_at
}

RepoRevisionV1 {
  repo
  ref
  commit_sha
  verified_at
}

CanonicalDocRefV1 {
  repo
  trusted_ref
  commit_sha
  path
  blob_sha
  section
  authority_receipt
  verified_reachable_at
}

ApprovalReceiptV1 {
  packet_id
  proposal_hash
  source_bundle_hash
  approver
  decision
  decided_at
  source_snapshot_hashes: String[]
  repo_revisions: RepoRevisionV1[]
}

FreezeReceiptV1 {
  issue_ref
  issue_body_hash
  issue_snapshot_hash
  canonical_doc_ref: CanonicalDocRefV1
  derived_source_revisions: EvidenceRefV1[]
  captured_at
}
```

`SourceKindV1` is the one source-artifact vocabulary used by refs and recall.
`authority_class` is deliberately separate: it describes what a source may
decide, not what kind of source it is.

Current-work snapshot hashes cover semantic state, not only prose or code:
`IssueSnapshotV1` includes body, state, labels, milestone, dependency refs,
selected comment revisions, and `updated_at`; `PullRequestSnapshotV1` includes
body, state, head/base SHAs, reviews, checks, merge state, and `updated_at`.
`pr_head_sha` remains a code revision, not a PR-state revision.

The allowed authority/adjudication combinations are closed, not free-form:

| authority class | allowed adjudication status |
|---|---|
| current_work | observation |
| canonical | approved |
| verification | observation, approved |
| advisory | observation, proposal, approved; never established |
| playbook | proposal, approved; never established |
| precedent | proposal, established; establishment only through #950 |

An `EngineReceiptV1` records requested role, effective provider, effective model
and version, fallback chain, degraded state, latency, and tool access. Unknown
identity, an undeclared fallback, provider timeout, or missing tool access makes
the result preview-only. A lane name is not an engine receipt.

## 4. Issue Refinery (#1002)

Issue Refinery is a manually triggered, read-only semantic refinement workflow.
It is not a resident curator agent and it does not invent another execution
backend.

### 4.1 Input order

1. immutable issue body and selected comment snapshots;
2. linked canonical doc/spec sections at an owner-controlled trusted commit and
   blob SHA;
3. repository HEAD and verification evidence;
4. related issue/PR state;
5. advisory memory/wiki expansion.

If a requested issue/PR/doc anchor cannot be resolved, the packet is
`grounding_status=missing_anchor`; neither reasoning nor the number of advisory
hits may upgrade it to high confidence.

### 4.2 Packet payloads

```text
IssueEvidenceV1 {
  issue_ref
  issue_body_hash
  issue_snapshot_hash
  issue_updated_at
  claims: ClaimV1[] { claim_id, text, source_span, anchors: AnchorV1[], verification }
  relations: IssueRelationV1[] {
    kind: blocks | depends_on | duplicate_of | supersedes | parent_of | related
    target_ref
    evidence_refs: EvidenceRefV1[]
  }
  linked_specs: CanonicalDocRefV1[]
  coverage { source_bytes, covered_bytes, omitted_spans: SourceSpanV1[] }
}

IssueDispositionProposalV1 {
  issue_ref
  based_on_repo_revisions: RepoRevisionV1[]
  based_on_issue_snapshot_hash
  disposition
  evidence_refs: EvidenceRefV1[]
  contradictions: ContradictionV1[]
  proposed_comment?
  proposed_labels: String[]
  proposed_doc_deltas: DocDeltaProposalV1[] { path, blob_sha, section, reason, summary }
}
```

The disposition vocabulary is:

```text
KEEP | NARROW | ROUTER | DORMANT | BLOCKED | MERGE_CANDIDATE |
CLOSE_FIXED | CLOSE_SUPERSEDED | HISTORICAL | DECISION_REQUIRED
```

This vocabulary covers the failure classes observed in the 2026-07-13 manual
cleanup: stale bodies, child-state drift, scope collision, superseded-but-not-
shipped work, protected routers, missing prerequisites, and incomplete dispatch
packets. A proposal may include a close action, but only an explicit approval
receipt authorizes the existing GitHub write surface to apply it. Apply rechecks
the proposal hash, source bundle hash, issue/PR snapshot hashes, trusted doc
revision, and repository revision; a stale approval cannot be replayed.

### 4.3 Safety boundary

The refinery never, by itself:

- closes or reopens an issue;
- edits an issue body or canonical doc;
- establishes a precedent;
- treats an issue attachment or external download as evidence;
- treats a model-only opinion as repository verification;
- runs from a campaign/umbrella body as a dispatch packet.

The first version is on-demand only. Cron and resident-service scheduling remain
out of scope until manual batches have measured accuracy, cost, owner overturn
rate, and failure recovery.

## 5. Issue-to-doc contract

A frozen leaf issue contains three pins once the #1002 source-resolver and
`FreezeReceiptV1` slice lands:

```text
Spec-Ref: owner/repo:docs/path.md@<commit_sha>/<blob_sha>#<section>
Derived-From: <owner ruling/comment ids@body_hash>
Freeze-Receipt: <FreezeReceiptV1 artifact id>
```

The append-only receipt contains `{issue_ref, issue_body_hash,
issue_snapshot_hash, canonical_doc_ref, derived_source_revisions, captured_at}`.
The body-hash basis
is SHA-256 over the exact UTF-8 issue body after CRLF is normalized to LF and any
line beginning exactly `Freeze-Receipt:` is removed, preserving all other bytes.
This makes the hash reproducible without making the body self-referential. The
resolver verifies that the doc commit was reachable from the named owner-controlled
trusted ref at capture time; a SHA alone does not grant canonical authority.
The semantic snapshot hash is SHA-256 over canonical JSON with lexicographically
sorted object keys, array order preserved, and all string line endings normalized
to LF.

Migration window: before that slice lands, the leader freezes the exact issue body
and linked doc revision in the dispatch run artifact and records the computed hash
in the packet. `Spec-Ref` and `Derived-From` are recommended immediately;
`Freeze-Receipt` becomes mandatory only when the typed receipt store and resolver
are available. Existing leaves are not retroactively invalidated.

The issue freezes a bounded delivery packet; it does not copy the entire
architecture. The canonical doc owns durable invariants. A proposed doc delta is
an ordinary reviewed PR, never a memory/wiki write. A changed blob SHA or issue
body hash makes the packet stale and requires explicit re-freezing.

Field ownership matters more than a single global precedence rule:

| Field | Owner |
|---|---|
| active priority, status, dependency | issue/PR snapshot |
| accepted architecture/API/invariant | repo doc/spec at a trusted commit + blob SHA + authority receipt |
| implementation evidence | commit + verification receipt |
| delivery state | reachability from owner-controlled main/release ref + release evidence |
| explanatory lesson/runbook | reviewed wiki/guide projection |
| precedent establishment/overturn | #950 adjudication record |

## 6. Recall and ask are evidence composition

The current physical table may remain shared, but retrieval eligibility must be
typed before scoring. Memory, wiki, guide, eval, runtime state, kanban, recall
cache, and generated distills do not compete in one undifferentiated top-k.

### 6.1 Retrieval order

1. exact resolver for issue/PR refs, paths, ids, SHAs, and titles;
2. task-intent and required-authority plan;
3. independent candidate budgets per artifact kind;
4. lifecycle and scope filters;
5. lexical/vector/graph ranking within each eligible partition;
6. optional rerank;
7. evidence-pack assembly with coverage and contradictions.

Each result exposes both the raw retrieval score and an evidence confidence.
Normalizing the best result in each section to `1.0` must not be presented as
confidence.

### 6.2 Evidence envelope for recall

```text
RecallEvidenceV1 {
  kind: SourceKindV1
  authority
  lifecycle
  source_ref
  source_revision
  valid_at
  retrieval_score
  claim_coverage
  contradictions: ContradictionV1[]
}
```

Generic memory search excludes drafts, eval rows, runtime state, cache rows, and
compound foundry distills unless the query plan asks for them. Old compound
distills may support exploration; they cannot crowd out an exact current-work
anchor.

`ask` is evidence-pack composition plus optional synthesis. Its confidence comes
from required-anchor coverage, claim coverage, authority, and contradiction
state. An LLM may lower confidence or summarize the pack; it may not upgrade the
evidence confidence. Truncated synthesis returns `partial`, never
`completed/high/gaps=[]`.

Feature briefing with an issue ref must resolve the issue snapshot or return
`ref_only/missing_anchor` without a dispatch recommendation. It must not infer a
feature from token overlap with unrelated memory, wiki, guide, or eval rows.

## 7. Wiki is a reviewed projection

Wiki is valuable for durable explanations, architecture rationale, lessons, and
runbooks. It is advisory project knowledge, not a project truth store.

```text
KnowledgeArtifactV1 {
  artifact_kind: draft | wiki | guide
  authority: advisory | playbook
  lifecycle: candidate | pending_review | active | stale | superseded | rejected
  scope
  valid_from?
  valid_until?
  source_bundle_hash
  engine_receipt?
  review_receipt?
}
```

Required behavior:

- default search returns `active` artifacts only; drafts have a separate scope;
- exact path/title/id resolution precedes semantic search;
- read/search expose authority, lifecycle, revision, source refs, typed evidence
  refs, and review
  receipt;
- browse facets are derived from real paths, not a hard-coded category list;
- filtering happens before top-k, or the search refills after filtering;
- semantic/applicability drift is separate from retention age;
- permanent retention does not exempt content from semantic staleness;
- source revision drift, `supersedes`/`contradicts` relations, and applicability
  windows can mark an artifact stale;
- wiki evolution and closure synthesis produce `ClosureProposalV1` candidates;
  an independent review and approval receipt are required before invoking
  `close_loop`, writing active wiki, or posting GitHub writeback.

A wiki entry can explain a canonical doc. It cannot replace the doc, approve a
deviation, close an issue, or establish precedent.

### 7.1 Reference compatibility and closure state

The existing wiki metadata contract keeps `source_refs: Vec<String>`. Typed refs
land in a separate versioned `evidence_refs_v1` field. Writers dual-write the
human-readable string refs and typed refs; readers prefer typed refs and fall back
to strings; exporters render the typed `ref` field when present. Do not change the
type of `source_refs` in place.

`close_loop` remains the final apply action. Candidate generation does not write
`close_loop.json` and does not call `mark_task_close_loop`. The target proposal
lifecycle is `closure_candidate → pending_approval → applied`; only the applied
transition writes the closure artifact, active wiki projection, and optional
GitHub comment. Apply renders only the approved proposal hash; it must not reread
mutable `result.md` or notes and synthesize different content. Until that lifecycle
lands, candidate producers must not call the current `close_loop` action.

## 8. Distill campaign and precedent boundary

#1043 remains a router for three separately observable contracts:

- **D2:** forge selected, grounded source bundles into
  `situation → proposed ruling → why → how to apply → refs` candidates; run a
  50-row old-vs-new A/B pilot and a cold-start behavior discrimination test.
- **D4:** turn recalled-but-unhelpful, contradicted, stale, overturned, or
  outcome-mismatched evidence into `ReforgeRequestV1`; append a new candidate
  rather than editing history.
- **D5:** add `recall_reason` and `pattern_context` after ranking only, under
  #774's frozen contract. It does not change query, pool, score, or authority.

#1059 is one read-only GitHub source adapter after #1041. It preserves issue,
PR, comment, merge, revert, and verification provenance and emits lesson or
precedent candidates. It does not mirror GitHub as a second truth store and it
does not establish a precedent.

#950 owns free-text principle decomposition, pending-to-established promotion,
outcome validation, owner veto/overturn, and adjudication-time recall. A lesson
candidate maps into #950's existing ruling record only at that gate.

## 9. Evaluation surface and the native/Tachi line

Tachi needs a first-class evaluation intake for work it did not dispatch. This
is already frozen in #1066 as an extension of `tachi_agent_eval`; do not create a
second ledger or a parallel `tachi_task` verb:

```text
EvalPacketV1 {
  eval_run_id
  frozen_contract_ref
  execution_origin
  native_child_id?
  artifacts: ArtifactRefV1[]
  tests_run: TestRunV1[]
  producer_engine_receipt?
  verifier_engine_receipt?
  claims: ClaimV1[]
  verdicts: VerdictV1[]
  not_checked: NotCheckedV1[]
}
```

The #1066 `register → observe → adjudicate → get` lifecycle records carrier facts
separately from leader/reviewer judgment. `tachi_task(action="complete")` may
project referenced `eval_run_ids` into the existing append-only eval path. The
surface does not invent a pass when a native host cannot expose its model
identity, and only adjudicated, evidence-usable rows feed routing/card evolution.

Use a native subagent when the host already owns the bounded task, low-latency
parallelism matters, and model identity is not part of the proof. Use Tachi
dispatch when the run needs an explicit provider/model, durable resumability,
cross-host pickup, a controlled tool/sandbox profile, or a reproducible ledger.
For cross-model verification, an unknown native model is not an independent
seat; use an explicitly identified Tachi/external lane.

Evaluation is by capability and artifact, not legacy lane name:

- evidence compiler: source coverage, atom precision/recall, provenance loss;
- proposal reasoner: grounding, contradiction handling, disposition accuracy;
- verifier: refutation catch rate, independence receipt, false-pass rate;
- renderer: typed-field fidelity and deterministic rebuild;
- recall: exact-anchor hit, authority precision, claim coverage, MRR/recall;
- D2/D4/D5: cold-start behavior change, usefulness feedback, and no ranking
  mutation by D5.

## 10. Delivery sequence and kill gates

1. #1002: immutable source resolver, typed evidence/disposition packets, manual
   reference replay, proposal-only apply boundary.
2. #1071: recall evidence envelopes and RED corpus — exact #1002 anchor, old-distill
   crowding, missing snapshot, same-results-but-wrong-source, provider timeout,
   and truncated synthesis.
3. #1072: wiki lifecycle/provenance visibility and candidate-review-apply boundary.
4. #1073 plus the #1059 pilot after #1041.
5. #1076 principle decomposition, then #1077 establishment/overturn gate.
6. #1074 D4 re-forge queue.
7. #1078 adjudication recall, then #1075 D5 post-rank decoration.
8. #1066 `tachi_agent_eval` external/native intake after engine receipts are
   available.

### 10.1 Frozen RED corpus

| Case | RED on current behavior | Required GREEN |
|---|---|---|
| exact `#1002` current-work query | advisory memories/wiki rank without a #1002 snapshot while confidence is high | resolved #1002 snapshot is present; advisory rows cannot outrank the required anchor |
| missing issue/doc anchor | high confidence or a dispatch recommendation is still possible | `grounding_status=missing_anchor`, confidence is not high, no dispatch recommendation |
| old compound distill crowding | an old multi-topic distill wins over an exact current-work ref | current-work partition wins; distill remains advisory expansion |
| same candidate ids, wrong authority | ask/search parity passes despite no required-authority source | claim coverage fails and response reports the missing authority |
| synthesis provider timeout/fallback | top-level remains `completed` while nested synthesis timed out; effective engine and degraded state are absent | evidence survives, but overall result is `partial/preview-only` with timeout/fallback receipt and unchanged evidence confidence |
| truncated synthesis | `completed/high/gaps=[]` | `partial`, explicit truncation gap, unchanged evidence confidence |

The D2 pilot uses 50 preselected source rows and a predeclared target decision per
row. For each candidate, an independent adjudicator blind to arm identity scores
three cold runs with the candidate and three without it. A candidate passes only
when at least two of three treated runs make the reference-aligned material choice,
the baseline does not already do so in at least two of three runs, and unsupported
claim rate does not increase. Failing candidates are rejected, not averaged away;
the pilot reports yield and cost before any bulk run.

If D2 cannot prove that a cold-start agent changes a material decision, stop D2
expansion, bulk #1059 ingestion, and the D2-derived parts of D4/D5. Do not scale
those paths to compensate for a candidate shape that has no behavioral value.
#950 remains valid for directly captured leader rulings and is not killed by a
D2 failure.

Explicitly abandoned:

- reopening PR #1005;
- redoing landed D1/D3 or reviving #552 as a separate campaign;
- automatic issue close/body edit/doc edit;
- free-text substring verdict classifiers;
- `/gh/...` as a second GitHub truth store;
- closed/merged implying established precedent;
- Summary participating in judgment, ranking, or authority;
- D5 changing retrieval;
- a cron/resident curator before bounded manual evidence exists.
