# Ship: the one-button contract-shipping verb

Status: design ratified by owner 2026-07-05. Phased delivery; each phase independently mergeable.

**Release / brew distribution** is a separate layer: after a tag lands, CI + the
public Homebrew tap publish installable binaries. That path is documented in
[`release-distribution.md`](./release-distribution.md) (#728 / #758 / #874).
`ship` opens PRs; it does **not** update formulas or restart daemons.

## The problem

Shipping a finished contract (issue / frozen spec → branch → PR) is fixed-cost ceremony:
collect commits, author a PR body, run the suite once, open the PR, record the evidence,
reap the worktree. Today that choreography lives in three inconsistent places:

1. **Agent memory.** The `tachi_task` tool description carries a 12-step "typical worker
   flow" that only well-briefed agents execute correctly. Everyone else fragments — the
   2026-07-04 incident produced ~50 one-commit PRs from a single vague goal.
2. **A private shim.** `codex-ship` (a bash script in `~/.codex/bin/`) solved the ceremony
   for exactly one harness: PR body generated from `git log` (zero prose tokens), full
   suite once, worktree reap. Claude/droid/opencode lanes get nothing.
3. **À la carte Tachi verbs.** `tachi_gh(ship)` stages an exact file list but demands a
   caller-authored commit message and PR body — the opposite of zero-prose. `tachi_verify`,
   `safe_merge`, `dispatch`, `close_loop` each work, but nothing composes them.

The goal: **any agent, any harness, presses `ship` once — Tachi does the rest.**

## The principle: model drives, code executes

The executor of a ship is a **cheap dispatched worker** (deepseek-flash / haiku tier,
routed by dispatch profile card) that follows the **ship skill SOP**. All determinism
lives in the mechanical tools the worker calls (`tachi_gh`, `tachi_verify`, `tachi_task`);
the worker contributes no judgment beyond following the checklist and reacting to failures
(a red suite, a missing base branch, a review BLOCK). This split is deliberate:

- The ceremony must not consume expensive-tier tokens — that is the whole point.
  A leader agent at high reasoning effort pressing `ship` hands off everything below
  its judgment line.
- The worker is disposable and re-dispatchable: state lives in
  `.tachi/runs/<flow_id>/ship.json` and the verify ledger, never in the worker's context.
  A crashed ship is re-dispatched and resumes from recorded state.
- Judgment-dense steps (adversarial review) are NOT the ship worker's job — it dispatches
  them to the right profile and consumes the structured verdict.

## What already exists (inventory, verified 2026-07-05)

| Piece | Location | Role in ship |
| --- | --- | --- |
| `tachi_gh ship` | `gh_ops/ship.rs` | mechanical stage/commit/push/PR-create (today: caller-authored prose) |
| `tachi_gh safe_merge` | `gh_safe_merge/` | gated GitHub merge: verification.json, protected labels, preview-unless-confirm |
| `tachi_verify` | `verify_ops/` | evidence ledger: seed required checks → runners record → merge gate consumes |
| `tachi_staff start` / `tachi_task complete` | `staffing_ops/` + `dispatch_ops/` | admit + spawn backend workers (claude/codex/grok/kimi/opencode, acpx/ACP), eval rows |
| `tachi_task intake/close_loop` | task facade | issue→flow binding, closure write-back (local worktree merge retired #1683 C1a) |
| dispatch profile cards | `dispatch_profile/` | authority flags (`write_code`/`merge`/`github_write`), evidence contracts, model routing |

Nothing here is replaced. `ship` composes these; it invents no parallel infrastructure.

## Architecture

### Front doors (one implementation, three surfaces)

- MCP: `tachi_gh(action='ship', issue=N, base?, wait?)` — the canonical surface.
- CLI: `tachi ship <issue#> [--base goal/N] [--wait]`.
- Harnesses call `tachi ship` directly — no shims (owner 2026-07-05: a wrapper whose
  only content is a pointer is a diverging wrapper waiting to happen). The coaching
  lines every harness needs ("this contract's ceremony is DONE — if your mandate has
  more contracts, start the next one now") live in ship's result text, shared by all
  callers. If Tachi is unreachable, shipping fails LOUDLY — there is no fallback path.

`ship` returns immediately with a `ship_id` (a flow-scoped run); `wait=true` blocks until
terminal state. Review-lane callers that require inline verdicts (codex two-layer
completion trap) pass `wait=true`.

### The pipeline (what the ship worker executes)

Given `issue` + current branch (or worktree) + optional `base`:

1. **Bind contract.** `intake` the issue → flow_id; refuse to ship from the base branch;
   refuse if no commits exist against the base.
2. **Seed verification.** `tachi_verify(start)` with the required checks for this repo
   (suite, clippy, gitleaks — repo-configurable).
3. **Verify.** Dispatch a test-runner worker to run the full suite ONCE; it records
   results via `tachi_verify(record)`. Red suite → ship halts in `verify_failed`,
   reports, does not open a PR.
4. **Adversarial review.** Dispatch a reviewer via `recommend` routing (a different
   model than the implementer; vendor diversity is optional defense-in-depth). The route
   receipt must show a distinct model identity; a new session, profile alias, persona, or
   reasoning-effort change on the same model does not count. The reviewer
   returns a STRUCTURED verdict (see frozen constraints) which is recorded as a required
   check in the verify ledger. BLOCK verdict → ship halts in `review_blocked`.
5. **Open the PR.** Contract mode: PR body generated from `git log <base>..HEAD`
   (commit list + verification tail + honest `Not-tested` section) — zero caller prose.
   Push branch, `pr create`, `link_pr` back to the flow.
6. **Record.** `complete` writes the eval row (feeds route evolution); ship.json reaches
   `shipped`.
7. **Reap.** If shipping from a linked worktree, remove it on terminal state
   (the reclaim-on-terminal-state hook shared with the disk governor, #484).

`safe_merge` stays a separate, human-triggered (or campaign-close) act. Ship opens;
the adjudicator merges. Ship never merges to the default branch.

### CI check-state ingest artifact

`tachi_gh safe_merge` preview mode records CI state as observation only. When a
`flow_id` is supplied it writes `.tachi/runs/<flow_id>/check_state.json` and stores a
small discovery pointer at `status.json::artifacts.check_state`; when `flow_id` is
missing, the response reports `check_state_ingest.non_auditable_reason` and no ledger
write is claimed.

The artifact schema is `tachi.github.check_state.v1`:

```json
{
  "schema": "tachi.github.check_state.v1",
  "flow_id": "flow_...",
  "repo": "owner/repo",
  "pr": { "number": 123, "ref": "owner/repo#123", "head_ref": null },
  "observed_at": "2026-07-07T00:00:00Z",
  "source": "safe_merge.dry_run",
  "dry_run": true,
  "aggregate": { "state": "failure", "status": "completed", "conclusion": "failure" },
  "buckets": { "success": 0, "failure": 1, "pending": 0, "skipped": 0, "other": 0, "total": 1 },
  "checks": [{ "name": "ci", "status": "completed", "conclusion": "failure" }],
  "failed_checks_recorded_only": true,
  "repair_attempted": false,
  "merge_attempted": false,
  "boundary": { "watch": "ingest", "repair": "dispatch", "adjudicate": "leader" }
}
```

Failed checks are ledger state, not authority. The boundary is: watch = ingest,
repair = dispatch, adjudicate = leader.

### Campaigns and `goal/*` integration branches

A multi-contract campaign (umbrella issue) gets an integration branch `goal/<issue>`:

- Slices ship with `base=goal/<issue>`, open a reviewable PR, and stop. Implementers never
  self-merge into the integration branch.
- The adjudicator personally reads and merges each accepted slice into `goal/*`; exactly
  one separately reviewed PR goes `goal/* → main` at campaign close, merged by the owner.
- The integration branch bounds campaign state and default-branch churn. It does not
  transfer merge authority or relax review freshness.

### Phases

1. **Contract mode (deterministic core).** Extend `gh_ops/ship.rs`: when `pr_body` is
   absent, derive commits from `<base>..HEAD` and generate the body from the git log
   (adopt the codex-ship format verbatim); accept `base`; optional worktree reap.
   No LLM, no new concepts. codex-ship is DELETED the same day; the codex constitution
   swaps `codex-ship <n>` for `tachi ship <n>`.
2. **Pipeline (the one-button).** The ship skill SOP + cheap-worker dispatch profile
   (`ship_runner` card: delegate tool profile, no recursion, github_write only);
   ship.json state machine in the run dir; verify + review steps wired as above.
3. **Campaign.** `goal/*` lifecycle on the flow: create on umbrella intake, slice
   accounting, campaign-close producing the single reviewed PR.

## Frozen constraints (spec law for all phases)

- **Structured verdicts only.** The reviewer dispatch prompt carries a verdict schema
  (numbered checkpoints, OK/CONCERN/BUG + evidence + Not-checked). Prose verdicts do not
  gate; an unparseable verdict is `review_blocked`, not a pass. This is the design's
  most fragile assumption made load-bearing: if verdicts aren't machine-readable, the
  gate is decorative.
- **Fail loud, never degrade silently.** Ledger DB down → ship fails with an Ops
  incident report; it does not fall back to ungated shipping.
- **Authority comes from the card.** Every pipeline step is bounded by the dispatch
  profile's authority flags; the ship worker runs the delegate tool profile and cannot
  dispatch sub-workers (no recursion).
- **Ship never weakens a gate.** No flag skips verification or review; the only bypass
  is the human running the mechanical `tachi_gh ship` with explicit prose — which is
  visible in the ledger as a manual ship.
- **One contract, one branch, one PR.** Ship refuses on the base branch, refuses empty
  commit ranges, and never opens a second PR for a branch that already has one open
  (it reports the existing PR instead).

## Convergence notes

- Shipping has one door: the canonical `tachi_gh ship` surface and its pipeline own
  this contract; the Shell runtime was deleted in #1319-B7 and no second door is planned.
- Worktree reap on terminal state is the same hook #484 (disk governor) needs; implement
  once, reference from both.
- `codex-ship`'s constitution clauses (mandate ≠ contract; turn ends when the ledger is
  empty) stay harness-side; ship's stderr/result text reinforces them ("this contract's
  ceremony is DONE — if your mandate has more contracts, start the next one now").

## Open questions

- Required-check set per repo: hardcode the tachi trio (suite/clippy/gitleaks) first, or
  read from repo config at Phase 2? (Leaning: hardcode first, config when a second repo
  needs it.)
- Reviewer routing when the implementer model is unknown (manual branches): resolve and
  record it before routing; otherwise the different-model gate cannot be proven.
- Whether `ship` on a dirty tree should auto-commit leftovers (leaning NO: refuse and
  list them — silent batching hides scope creep).
