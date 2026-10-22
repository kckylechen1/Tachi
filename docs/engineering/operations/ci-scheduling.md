# CI applicability and PR scheduling

Status: owner-approved 2026-10-08; moved here from
[`dispatch-lifecycle.md`](../architecture/dispatch-lifecycle.md) §8 on
2026-10-18 without change in substance. That document owns review, acceptance,
and authority; this one only allocates CI execution.

## Execution seats

Required PR and main workflows use standard GitHub-hosted VMs. Full CI's
Rust workspace runs on `macos-15` (Darwin arm64); its setup, Node, secret scan
and acceptance jobs run on `ubuntu-24.04`. The Windows observation matrix
continues on `windows-latest`. Owner/event admission, scope profiles and
same-run platform evidence remain unchanged.

Since 2026-10-22 (owner decision) the main ruleset requires only `acceptance`,
and there is no Linux conformance workflow: the former `Linux acceptance`
check and its self-hosted seat (`tachi-conformance`) are removed. Linux-only
`cfg` paths are therefore not compiled or tested in CI.

Each Rust job owns its workspace target, with two build jobs and four test
threads. Dev/test debug symbols are disabled to fit standard runner storage;
debug assertions, test inventory and feature checks remain enabled. The VM
owns cleanup. Shared-machine Cargo leases, the installed `host-build` client,
resident process cleanup and the shared host's 60 GiB watermark are not
applicable to these disposable VMs; their implementation and runbooks remain
available for explicitly admitted shared-host work.

A public repository can use these standard hosted runners without consuming
private-repository minutes. Larger paid runners are not selected. A runner
image, tool download, capacity or storage failure remains a failed or blocked
check; it never reduces coverage or accepts missing evidence.

Rolling back execution seats requires restoring the complete shared-host
routing, installed admission runtime and lifecycle together. Do not point
these direct Cargo commands at a self-hosted runner. Required checks and the
protected-main ruleset remain in force during rollback.

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
| `node_cache` | Only the recognized npm-cache stanza in the existing `ci.yml` Node job; both endpoints recognized and all other bytes identical | Setup/policy checks, complete Node matrix and npm audit, secret scan, Windows observation and both acceptance summaries |
| `post_merge` | Protected `main` push; clean exact checkout, two parents, and push-before equals the first parent | Setup/version/policy checks, secret scan, unconditional Windows observation and both acceptance summaries; Rust workspaces and Node matrix are not applicable |

For PR content classification (`archive_prose`, `node_presentation` and `node_cache`),
active docs remain full because Rust contract tests consume and recursively
scan them. Archive prose is already excluded from that scan. Native Node,
transport, daemon, credentials/config, protocol and package/build inputs are
not presentation. New paths, deletes, renames, executable-mode or symlink
changes, dirty source, missing/shallow ancestry and invalid inputs never reduce
coverage. The cache allowlist admits the pinned setup-node remote npm cache stanza or
the local-cache stanza with a literal safe leaf under `$RUNNER_TOOL_CACHE/tachi/`;
new shell/YAML syntax, tool pins, versions, hooks and classifier edits stay full.
Expanding the positive scope requires source review of consumers.

The setup job publishes the profile and checkout tree; acceptance independently
recomputes them from its checkout and requires a matching successful producer
before admitting a reduced scope. A permitted skipped job is reported as
`not_applicable`, with the profile and Git identity, never as a test PASS.
Failures, cancellations, unknown outcomes, or missing/extra job rows still
refuse acceptance. Windows remains unconditional and observational with its
original raw outcome reporting and release restriction.

The protected `acceptance` check remains in place. Build and validation policy
changes themselves take `full`; the classifier never exempts its own edits.

## PR-authoritative scheduling (owner-approved 2026-10-08)

The owner selected fresh-base PR acceptance plus lightweight post-merge checks.
Before enabling `post_merge`, the main ruleset must require pull requests,
strict up-to-date status checks, and `acceptance` from GitHub Actions
(app 15368), with no bypass actors. `Linux acceptance` was originally also
required; the owner removed it, and its workflow, on 2026-10-22. Preserve deletion and
non-fast-forward protection. No additional human review count is imposed; the
independent review requirements in
`dispatch-lifecycle.md` remain in force.

Rollout order: first produce a green scheduling PR with a green `acceptance`
aggregate; then activate and read back the ruleset; refresh its exact head/base and checks;
then merge. Never remove main full validation before that protection is active.
The `GITHUB_REF_PROTECTED` check is an additional signal, not proof of all these
rules: weakening the external ruleset invalidates this scheduling policy.

This is an explicit allocation of validation to the PR, not reuse of a historical
PASS on a different SHA or proof that the final Git-derived binary/version was
executed. Deployment and release verification obligations remain unchanged.
Windows observation still executes on main. Multi-commit advances, squash/rebase
(single-parent) commits, other refs, dirty/mismatched/shallow-unknown checkouts
and manual runs keep full validation. `workflow_dispatch` remains the full lane
for Full CI. Strict mode may require rebuilding a PR when another merge
advances its base; this covers changed integration input.

Rollback without weakening protection: remove the `rust`/`node` exclusions from
`post_merge` in the acceptance plan. Restore full scheduling before any later
relaxation of branch rules.

## Retiring the former Linux seat

The owner retired the self-hosted `tachi-conformance` seat on 2026-10-22.
Drain it first: remove the `tachi-conformance` routing label, list queued and
in-progress runs that target it and cancel each (do not re-run them; a re-run
keeps the old workflow revision and its label, and the workflow no longer
exists), then wait for `busy=false` and no `Runner.Worker` process. Only then
stop and remove its service from the administrative account, then, as `gha`,
remove the registration with a short-lived removal token entered at the
interactive prompt (do not authenticate `gh` on the host):

```bash
cd <runner-root>
./config.sh remove
```

Preserve any required receipts before deleting `<runner-root>`. Nothing else
is runner-owned outside `<runner-root>`, `gha`'s toolchain caches
(`~/.rustup`, `~/.cargo`), and the installer state `~/.install-action/`.
