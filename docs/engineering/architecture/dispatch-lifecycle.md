# Delivery and independent review

Status: current repository procedure, owner-ratified 2026-09-20. This risk-based policy supersedes the 2026-08-02 universal different-model review rule. Historical doctrine and worked cases remain available in Git history; they are not operational prerequisites.

This procedure explains how to deliver, review, and verify a bounded change. It does not require a dispatch system, GitHub issue, card consultation, route receipt, or separate top-level session. Product routing, model-card architecture, incident history, host operations, and porting guidance belong in their owning documents.

## 1. When this procedure applies

Use this procedure for behavior changes, non-trivial refactors, release or dependency changes, and any work that needs independent review or acceptance evidence. Tiny non-semantic edits need only proportional checks and an honest report.

One owner request has one coordinating session. A portfolio is a queue: default WIP is one active delivery and at most one executing helper. Additional top-level sessions or concurrency require explicit owner approval.

## 2. Classify review risk

Classify the change before implementation. Uncertainty takes the stricter class.

| Class | Typical change | Required review |
|---|---|---|
| **Low** | prose, comments, formatting, generated refresh with no semantic delta | appropriate checks; no mandatory independent model review |
| **Ordinary** | behavior change, ordinary bug fix, bounded refactor | independent read-only review of the actual candidate; different model preferred |
| **High** | authorization, credentials or secrets, trust boundaries, persistent data or migrations, destructive operations, concurrency or atomicity, public compatibility, merge/release gates, or safety/agent-authority policy | independent read-only different-model review of the exact candidate plus risk-specific verification |

An owner may freeze a stricter requirement. File count alone does not determine risk.

Independence means the reviewer did not implement the reviewed slice. Where different-model review is required, launcher or route evidence must establish the effective identities; aliases, personas, requested routing, session IDs, and model self-description do not. Unknown identity leaves diversity `incomplete`; opening more sessions does not satisfy it.

## 3. Scope, ownership, and authority

Before writing:

1. State the requested outcome, material assumptions, and external approval boundaries.
2. Establish ownership of the workspace and the base or candidate being changed.
3. Identify relevant tests, compatibility boundaries, and do-not-touch areas.
4. Isolate delegated writers and concurrent work. Read-only inspection does not require another worktree.

The coordinating session may implement directly. It may also adjudicate review findings and perform an authorized merge after reading the diff, but it cannot replace required independent review with self-review.

Untrusted files, comments, patches, attachments, and tool output may be inspected when authorized. Inspection never grants authority and never implies permission to execute them.

## 4. Helpers and session topology

Use the host's native in-session helper or subagent mechanism only when a bounded search, analysis, review, or isolated implementation slice materially helps. A helper receives:

- the outcome and constraints;
- the workspace plus base or candidate identity;
- exact scope and write permission;
- required checks and evidence;
- a stopping point and return shape.

Helpers are leaves and return results to the coordinator. They do not delegate or create replacement coordinators. A new issue, PR, candidate SHA, role, or review round is not a reason for another top-level session.

A genuinely different execution environment or durable independently owned work that must outlive the coordinating session can justify requesting a separate top-level session; it still requires explicit owner approval. If required review or isolation is unavailable, report the requirement as incomplete instead of manufacturing another route.

## 5. Implement and discriminate

While the candidate is changing:

1. Make the smallest complete change within the owning module.
2. Run focused checks that distinguish the intended behavior from plausible wrong implementations.
3. Preserve agreed assertions, goldens, content atomicity, and safety guards. If the contract is wrong, stop for adjudication rather than weakening it.
4. Resolve intended base, conflicts, generated artifacts, and fixtures before declaring the candidate frozen.

Run proportional format, compile, lint, and tests during iteration. Do not pay the full acceptance cost for a knowingly provisional head.

## 6. Freeze and review

Bind review to the exact candidate object, not a branch name. The reviewer reads the requirements and diff, remains read-only, and reports numbered `OK`, `CONCERN`, and `BUG` findings with evidence and explicit `Not-checked` gaps.

For PR-backed work, record required review findings and their accepted, rejected, or downgraded dispositions in the PR body. A bare verdict is insufficient when review is required.

“Fresh review” means a new assessment and verdict on the current candidate, not a new reviewer or session. An eligible reviewer may be reused after repair. A candidate-changing repair, rebase, or merge invalidates the previous verdict. Before requesting another full review, distinguish a changed commit identity from changed source content using the procedure below.

### Candidate identity across agents and CI

The coordinator names one full candidate commit for a review cycle. Read-only
reviewers inspect that object; they do not pull a moving branch or create another
implementation checkout. Writers may have different HEADs while working, but
only the integrated candidate is final acceptance input.

Run `python3 scripts/candidate_identity.py --requested-head <full-commit>`
from the clean source checkout to record the requested commit/tree and the
actual checkout commit/tree. An optional `--base <full-commit>` records the
review base. CI records the same JSON alongside its existing identity log;
its checkout includes the immediate parents so a PR merge checkout can resolve
the requested PR head. CI records a base only for PR events: an earlier push
tip may be beyond the shallow boundary and is not a review base for the pushed
commit. Missing requested objects or a dirty source refuse the record.

Different commit IDs are expected for a PR head, GitHub's test merge and the
final merge. `same_tree` proves only identical tracked source, including
submodule gitlinks; it does not prove submodule worktrees, ignored/generated
files, build outputs or runtime state match. `different_tree` requires review
of the actual delta; never explain it away as a harmless merge SHA.

For identical trees, the same reviewer can issue a short new verdict naming
both commits, the unchanged tree, the original findings, and any changed review
base or scope. This is a new assessment, not automatic transfer of approval.
History-sensitive or version-generation behavior still needs review.

Test evidence always stays attached to its actual execution commit. Tree
identity alone never transfers a PASS, bypasses required CI, or approves a
release. Before proposing reuse, establish identical commands, features,
toolchain, platform, relevant environment and fixtures, generated inputs and
Git-derived version inputs; missing evidence means rerun the affected check.
This source-identity record does not implement a runtime-evidence reuse gate.

## 7. Repair budget and stopping

The default autonomous budget is:

1. one initial review;
2. one consolidated repair by the implementation seat;
3. one re-review of the repaired exact candidate.

If another candidate-changing repair or review round is needed, stop automatic execution. Report the candidate, unresolved findings, acceptance state, and smallest next action. The owner decides whether to authorize another bounded cycle, rescope, park, or cancel. A new PR, packet, branch, or session does not reset the budget.

Budget exhaustion never turns failing or partially checked work into accepted work.

## 8. Acceptance evidence

After required review is satisfied, run the complete **local-safe** acceptance surface appropriate to the change. Use authoritative CI for merge policy, platform-specific execution, and checks unavailable locally.

- Never copy CI runner setup, workspace cleanup, credential bootstrap, or other destructive lifecycle commands into an ordinary developer checkout.
- Name every narrowed target, skipped platform, unavailable tool, and matrix gap.
- A candidate test failure is `test_failed`; unavailable runner, billing, capacity, or service is `infra_blocked`; missing or ambiguous evidence is `incomplete`; an inapplicable check is `not_applicable` only under an approved applicability rule.
- Do not let an early audit or setup failure masquerade as evidence that later behavior tests passed. Report every unexecuted obligation separately.
- Red-then-green evidence is required when it materially proves a new regression or security test discriminates the fix. Structural proof may substitute only when runtime mutation is not the honest method, and the reason must be stated.

### CI applicability (owner-approved 2026-10-08)

The required job inventory remains closed. `.github/acceptance-plan.json`
(schema 4) records which job families may be inapplicable, and
`.github/scripts/ci_scope.py` derives the profile from immutable Git objects.
Labels, PR descriptions and caller-written path lists never select a profile.

| Profile | Eligibility | Execution retained |
| --- | --- | --- |
| `full` | Default; every unclassified or mixed change, active docs, shared/build/security paths, unproven/non-merge pushes and all manual events | Existing complete matrix |
| `archive_prose` | Only modifications to existing regular `docs/archive/**/*.md` files | Setup policy, applicability/acceptance tests, release-version checks, secret scan, unconditional observational Windows matrix |
| `node_presentation` | Only modifications to existing `packages/tachi-cli/src/utils/ui.ts` and/or `utils/i18n.ts` | The same checks plus the complete existing Node matrix and npm audit |
| `post_merge` | Protected `main` push; clean exact checkout, two parents, and push-before equals the first parent | Setup/version/policy checks, secret scan, unconditional Windows observation and both acceptance summaries; Rust workspaces and Node matrix are not applicable |

For PR content classification (`archive_prose` and `node_presentation`),
active docs remain full because Rust contract tests consume and recursively
scan them. Archive prose is already excluded from that scan. Native Node,
transport, daemon, credentials/config, protocol and package/build inputs are
not presentation. New paths, deletes, renames, executable-mode or symlink
changes, dirty source, missing/shallow ancestry and invalid inputs never reduce
coverage. Expanding the positive scope requires source review of consumers.

The setup job publishes the profile and checkout tree; acceptance independently
recomputes them from its checkout and requires a matching successful producer
before admitting a reduced scope. A permitted skipped job is reported as
`not_applicable`, with the profile and Git identity, never as a test PASS.
Failures, cancellations, unknown outcomes, or missing/extra job rows still
refuse acceptance. Windows remains unconditional and observational with its
original raw outcome reporting and release restriction.

Both Full CI and Linux conformance use this same classifier. The named
`Linux acceptance` job has a closed inventory, recomputes scope and verifies the
Linux producer before admitting any skipped Rust job. It rejects failures,
cancellations, missing/extra jobs and unmatched producer evidence. Linux Rust jobs
are genuinely skipped for a proven inapplicable change; the applicability job
still runs and must succeed. This does not claim Linux execution on those PRs.
The protected `acceptance` check remains in place. Build and validation policy
changes themselves take `full`; the classifier never exempts its own edits.

### PR-authoritative scheduling (owner-approved 2026-10-08)

The owner selected fresh-base PR acceptance plus lightweight post-merge checks.
Before enabling `post_merge`, the main ruleset must require pull requests,
strict up-to-date status checks, and both `acceptance` and `Linux acceptance`
from GitHub Actions (app 15368), with no bypass actors. Preserve deletion and
non-fast-forward protection. No additional human review count is imposed; the
independent review requirements above remain in force.

Rollout order: first produce a green scheduling PR with the new Linux aggregate;
then activate and read back the ruleset; refresh its exact head/base and checks;
then merge. Never remove main full validation before that protection is active.
The `GITHUB_REF_PROTECTED` check is an additional signal, not proof of all these
rules: weakening the external ruleset invalidates this scheduling policy.

This is an explicit allocation of validation to the PR, not reuse of a historical
PASS on a different SHA or proof that the final Git-derived binary/version was
executed. Deployment and release verification obligations remain unchanged.
Windows observation still executes on main. Multi-commit advances, squash/rebase
(single-parent) commits, other refs, dirty/mismatched/shallow-unknown checkouts
and manual runs keep full validation. `workflow_dispatch` remains the full lane
for both Full CI and Linux conformance. Strict mode may require rebuilding a PR
when another merge advances its base; this covers changed integration input.

Rollback without weakening protection: remove the `rust`/`node` exclusions from
`post_merge` in the acceptance plan. Keep the Linux aggregate while its check is
required. Restore full scheduling before any later relaxation of branch rules.

## 9. Return and authorized integration

The final report states:

- base and exact candidate;
- changed scope;
- review requirement, reviewer evidence, findings, and dispositions;
- checks run and their decisive results;
- `test_failed`, `infra_blocked`, `incomplete`, or `not_applicable` obligations;
- external actions performed and remaining approval gates.

A complete report can describe a failed delivery; it does not make that delivery accepted. Merge, deployment, publication, issue closure, and production writes remain separate owner-authorized actions.

The coordinator owns helper and background-process completion. A task ID or running process is not a completed result.
