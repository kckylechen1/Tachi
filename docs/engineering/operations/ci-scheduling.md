# CI applicability and PR scheduling

Status: owner-approved 2026-10-08; moved here from
[`dispatch-lifecycle.md`](../architecture/dispatch-lifecycle.md) §8 on
2026-10-18 without change in substance. That document owns review, acceptance,
and authority; this one only allocates CI execution.

## CI applicability (owner-approved 2026-10-08)

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

## PR-authoritative scheduling (owner-approved 2026-10-08)

The owner selected fresh-base PR acceptance plus lightweight post-merge checks.
Before enabling `post_merge`, the main ruleset must require pull requests,
strict up-to-date status checks, and both `acceptance` and `Linux acceptance`
from GitHub Actions (app 15368), with no bypass actors. Preserve deletion and
non-fast-forward protection. No additional human review count is imposed; the
independent review requirements in
`dispatch-lifecycle.md` remain in force.

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
