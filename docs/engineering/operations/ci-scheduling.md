# CI applicability and PR scheduling

Status: owner-approved 2026-10-08; moved here from
[`dispatch-lifecycle.md`](../architecture/dispatch-lifecycle.md) §8 on
2026-10-18 without change in substance. That document owns review, acceptance,
and authority; this one only allocates CI execution.

## Execution seats

Required PR and main workflows use standard GitHub-hosted VMs. Full CI's
Rust workspace runs on `macos-15` (Darwin arm64); its setup, secret scan
and acceptance jobs run on `ubuntu-24.04`. The Windows observation matrix
continues on `windows-latest`. Owner/event admission, scope profiles and
same-run platform evidence remain unchanged.

The TypeScript CLI and OpenClaw plugin are retired. Their dedicated `node` job
family and `node_cache` applicability profile are removed. Native Node bindings
remain a separate package with their own manual build/publish workflow.

Since 2026-10-22 (owner decision) the main ruleset requires only `acceptance`,
and there is no Linux conformance workflow: the former `Linux acceptance`
check and its self-hosted seat (`tachi-conformance`) are removed. Linux-only
`cfg` paths are therefore not compiled or tested in CI.

Each Rust job owns its workspace target, with two build jobs and four test
threads. Dev/test debug symbols are disabled to fit standard runner storage;
debug assertions, test inventory and feature checks remain enabled.  The VM
owns cleanup.

The macOS Rust job caches Cargo registry/git sources and `target/debug`, excluding
incremental state. It does not cache Cargo credentials/configuration, installed
tools, JUnit reports or acceptance receipts. Cache keys separate runner OS,
architecture, VM image version, pinned toolchain, manifests/lockfile, Cargo
configuration and the owning workflow/setup action. A source-SHA suffix records
each successful build; fallback stays within the same build inputs. Cargo still
checks source freshness, and every required command runs on both hits and misses.
Only a successful job saves a cache. Cache transport or eviction never substitutes
for checks; Windows jobs keep their existing isolated cold-build behavior.

A public repository can use these standard hosted runners without consuming
private-repository minutes. Larger paid runners are not selected. A runner
image, tool download, capacity or storage failure remains a failed or blocked
check; it never reduces coverage or accepts missing evidence.

The Windows `physical-db-identity-windows` matrix is observational (owner
adjudication 2026-09-23): all three crate legs still run, and their raw
outcomes appear in the acceptance receipt as `observational_*_excluded`, but
they do not block the aggregate. `.github/acceptance-plan.json` records this
per job. Windows releases remain prohibited until #1963 is fixed.

## CI applicability (owner-approved 2026-10-08)

The required job inventory remains closed. `.github/acceptance-plan.json`
(schema 4) records which job families may be inapplicable, and
`.github/scripts/ci_scope.py` derives the profile from immutable Git objects.
Labels, PR descriptions and caller-written path lists never select a profile.

| Profile | Eligibility | Execution retained |
| --- | --- | --- |
| `full` | Default; every unclassified or mixed change, active docs outside the explicit operational allowlist, shared/build/security paths, unproven/non-merge pushes and all manual events | Existing complete matrix |
| `archive_prose` | Only modifications to existing regular `docs/archive/**/*.md` files | Setup policy, applicability/acceptance tests, release-version checks, secret scan, unconditional observational Windows matrix |
| `operational_prose` | Only modifications to the existing regular `docs/engineering/operations/recall-eval.md` and `staff-result-testing.md` files | Every job and non-workspace-test step; workspace nextest execution selects the existing retired-surface documentation contract |

The retired `node_presentation` and `node_cache` profiles are rejected. Changes
at former plugin/CLI paths or workflow cache settings receive the conservative
`full` profile.

Active docs outside that two-file allowlist remain full because Rust contract
tests consume and recursively scan them. Archive prose is already excluded
from that scan. Native Node,
transport, daemon, credentials/config, protocol and package/build inputs remain
full. New paths, deletes, renames, executable-mode or symlink
changes, dirty source, missing/shallow ancestry and invalid inputs never reduce
coverage. Workflow, shell/YAML, tool pin, version, hook and classifier edits
stay full.
Expanding the positive scope requires source review of consumers.

The operational allowlist's current repository-document consumer is
`tests::profile_tests::tool_profile_router_coverage::retired_surface_documentation_has_markers`.
That unchanged Rust test recursively scans active docs and validates retired
names against the live router and typed wire parser. Its exact nextest filter
uses `--no-tests=fail`; a missing or ignored test cannot become an empty pass.
`--workspace`, the CI test profile, JUnit upload, clippy, audit, portable tests,
doc tests and Windows jobs remain unchanged. No cold-build saving is
assumed. Measure compilation and test elapsed time separately.

This is an exact file allowlist, not an operations-directory exemption.
CI/review/authority policy, other runbooks, fixtures and new files stay full.
If an allowlisted document gains a build, runtime or test consumer, update the
selected coverage or remove its eligibility in that same change. Classifier,
acceptance-plan, workflow and scheduling-policy changes themselves stay full.

The setup job publishes the profile and checkout tree; acceptance independently
recomputes the profile from its checkout and always requires a matching
successful producer, including when the recomputed profile is `full`.
Non-full profiles also require the same valid checkout-tree identity; two
missing identities cannot match. Full/manual events retain full test execution
without requiring a narrowing tree. A permitted skipped job is reported as
`not_applicable`, with the profile and Git identity, never as a test PASS.
Failures, cancellations, unknown outcomes, or missing/extra job rows still
refuse acceptance. Windows remains unconditional and observational with its
original raw outcome reporting and release restriction.

The protected `acceptance` check remains in place. Build and validation policy
changes themselves take `full`; the classifier never exempts its own edits.

## Main validates merged results (owner decision 2026-10-31)

This replaces the 2026-10-08 PR-authoritative scheduling (strict up-to-date
PRs plus a lightweight `post_merge` profile on main).

- A PR's current head must pass `acceptance` before merge. A PR is **not**
  required to be up to date with `main`, so another merge advancing the base
  does not force a rebuild of every open PR.
- Every push to `main`, which is every merge, runs the full matrix. Main is
  where merged results are integration-tested, including semantic conflicts
  between PRs that were each green on their own base. There is no
  `post_merge` profile.
- A red `main` is fixed or reverted promptly. CI concurrency cancels an older
  in-progress `main` run when a newer merge lands; the newest run covers the
  cumulative tree, so a failure may need bisecting across the merges it spans.
- The main ruleset keeps requiring pull requests and `acceptance` from GitHub
  Actions (app 15368) with no bypass actors, and keeps deletion and
  non-fast-forward protection. The strict up-to-date requirement is turned
  off. No additional human review count is imposed; the independent review
  requirements in `dispatch-lifecycle.md` remain in force.

Rollout order: merge this change first so `main` runs full checks, then turn
off strict up-to-date in the ruleset. Never relax the ruleset while `main`
still skips applicable required jobs. `workflow_dispatch` remains a full lane, and
deployment and release verification obligations are unchanged.

## No self-hosted seats

The self-hosted seats existed while the repository was private, to save
hosted minutes. Once the repository went public, standard hosted runners cost
nothing. The `tachi-conformance` seat was retired on 2026-10-22. On
2026-11-08 the last registered runner was deregistered and its service
disabled, and the shared-host Cargo gate snapshot, runner hygiene scripts,
and runbook were deleted. Do not route PR code to a self-hosted runner on
this public repository.
