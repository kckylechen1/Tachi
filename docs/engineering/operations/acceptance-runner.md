# Trusted acceptance runner (single-lane) — operator runbook

The default macOS acceptance workflow now uses standard GitHub-hosted VMs;
see [CI execution seats](ci-scheduling.md#execution-seats). This runbook
covers the retained shared-host seat for explicit operator use and rollback.
Its installed hooks, locks, budgets and lifecycle are required only when
work is admitted to that shared host, not on disposable hosted VMs.

Covers issue #1865 (owner-ratified C1): one trusted acceptance runner serving
exactly the four in-scope lanes of `ci.yml` — `build-seat-setup`, `rust`,
`node`, `gitleaks` — plus the `acceptance` aggregate job, which runs on the
same runner under the same guard (five self-hosted job definitions in
total). Census and sizing rationale: #1852.

## Machine Cargo gate (#2052)

The shared host mechanism is maintained in `agent-control-plane/host/build`.
The tracked runtime files in `.github/scripts/` are a pinned vendor snapshot;
`.github/runner-config/host-build-upstream.json` records its source revision
and hashes. Update the upstream first and export it; do not independently
maintain a second implementation here. They are not automatically deployed
hooks. The host's existing `.sh` adapters invoke the
installed start/end files in `~/bin`. A green source CI run therefore does
not certify that the host has installed this revision.

For Cargo-capable jobs the start hook must establish a live `Runner.Worker`
ancestor, open the existing empty regular `/tmp/cargo-gate/lock`, and receive
a job/worker/holder/inode-bound acknowledgement while that same flock is
held. Failures return 70 with `GATE_E_*`; there is no environment bypass,
automatic lock creation, or ungated fallback. The four audited no-Cargo job
IDs (`node`, `gitleaks`, `acceptance`, `build-seat-setup`) in `Full CI` are
declared in this project's `.github/runner-config/cargo-gate-policy.json`.
Install that policy beside the host hooks with the runtime update. Exemptions
match the full repository and workflow identity; absent policy exempts nothing,
malformed policy refuses, and unknown job IDs take the lock. The hook does not
invoke Cargo. This project's 900/1800-second budgets remain in
`cargo_gate_step.py`; the host package does not own Tachi execution policy.

### Full CI command leases (#2065)

The Full CI Rust job has separate cumulative budgets: 900 seconds queued for
Cargo admission and 1800 seconds of admitted execution. Its 50-minute outer
timeout allows those 45 minutes plus five minutes for setup, metadata and
cleanup. After pinned Rust setup, `cargo_gate_step.py release` initializes the
job ledger from the installed holder's validated `snapshot`: initial queue
time and all elapsed setup time since acquisition are charged to their
respective budgets. Setup itself remains under the initial job lease. Its
execution time is checked retrospectively at release; if setup hangs before
that point, the 50-minute outer job timeout is its limit. The managed commands
below enforce their remaining execution deadline while running.

Clippy, audited absolute-path nextest, portable-kernel tests and doc tests use
`cargo_gate_step.py run -- COMMAND`. Each admission passes only the remaining
queue allowance to the installed start hook. Queue exhaustion reports
`GATE_E_QUEUE_TIMEOUT`, `infra_blocked` and exit 75 without starting a command;
execution exhaustion reports `GATE_E_EXEC_TIMEOUT` and exit 124. Ordinary
command failures keep their exit status. Neither queue waiting nor intervening
formatting, static script checks, audit metadata, audited downloads or artifact
uploads consume the admitted-execution budget. All existing workflow conditions
and the always-run workspace cleanup remain in place.

The mode-0600 budget record in `RUNNER_TEMP` binds the repository, workflow,
job, run ID, attempt and live `Runner.Worker` identity. A separate private flock
serializes that ledger only; it never grants machine build admission. Missing,
damaged, mismatched, duplicate-initialized or unfinished state refuses later
commands. Timeouts and cancellation are terminal for this job's budget; later
phases cannot restart or enlarge it through environment variables. The helper
requires the installed snapshot and bounded-start capabilities; an old helper
cannot silently supply a fresh or default budget. Source changes do not install
these capabilities on the host. Older workflows retain their job-wide lease.

A live anchor pins each command process group. At execution timeout the helper
sends TERM only to that group, then bounded KILL if needed. It does not reap the
anchor before inspection (an unreaped zombie also pins the group identity), and
releases only after proving that no live same-group descendants remain. Failed
or unavailable inspection retains the job lease and unfinished ledger for runner
cleanup. Other cancellation or lingering-descendant failures also drain owned
children while retaining the lease for cleanup. Deliberate session escapes
remain outside this guarantee; local wrapper custody is described below.
The helper never signals foreign processes, installs hooks, or changes runner
registration, `.env` or service state.

The existing local `~/bin/cargo` wrapper uses the same inode and its legacy
holder metadata is recognized only for a live wrapper process. Without sidecars, a known Hyperion verification guardian with this lock inode
open permits continued waiting. An open descriptor is not ownership proof;
admission still requires acquiring the real flock. CI waits on that same inode
with a cancellable acquisition child using blocking flock. Observer polling only
checks worker, stop and deadline state; it does not compete to acquire the lock.
A parent-death watchdog reclaims an orphan waiter. This removes the disadvantage
of sleeping between acquisition attempts, but does not promise FIFO admission. Malformed or conflicting
sidecars and other unknown held ownership refuse after a bounded grace period. C1
leases record PID plus process-start identity; completion requests the
matching lease to stop rather than killing a bare PID. A live worker's lock
has no elapsed-time expiry. Uncertain active-worker state and stray holder
signals retain the lock; operator inspection may be needed after an
unrecoverable holder crash. Cleanup removes only byte-matching owned
sidecars. Never remove or replace the lock inode while any owner may exist.

The tracked `.github/scripts/cargo` is the host wrapper installation source.
Heavy commands refuse missing/unsafe locks, acquisition errors and wrapper
exceptions; `CARGO_GATE=off` no longer bypasses admission. A job borrows only
its worker/job/inode-bound installed hook lease. Nested Cargo requires a known
live ancestor and a held kernel lock, retaining compatibility with Hyperion's
verification guardian. This is cooperative same-account coordination, not an
adversarial security boundary.

A separate guardian retains the inherited lock after wrapper death until Cargo
and its same-process-group descendants exit. Cargo/cache daemons do not inherit
the lock descriptor. Interrupted commands preserve their failure status;
uncertain process inspection retains the lease. `cargo run` and `cargo watch`
now hold admission for the whole invocation, including repeated builds; launch
already-built long-lived services directly. Full-path invocation outside gated
CI, deliberate session escapes, and killing the guardian itself remain outside
this guarantee. The single-account/public-runner risk decision is unchanged.

The wrapper is installed independently by hash-checked atomic replacement of
`~/bin/cargo`, preserving its mode and the existing lock inode. The companion
`cargo-gate-hold` update requires the idle-runner installation procedure below.
Retain exact backups. Do not replace active hook protocols or kill foreign owners. Validate
with the local wrapper, hook, step and interoperability fixture suites; these
use private lock paths and fake Cargo, never the live build gate. Installation
of source changes alone does not establish a new full CI acceptance receipt.

### Approved installation and rollback

1. Record the reviewed candidate SHA, SHA-256 and mode of all three source
   files, the installed three files and both `.sh` adapters, the filtered
   `ACTIONS_RUNNER_HOOK_JOB_STARTED`/`COMPLETED` paths, listener owner/service,
   and gate directory/lock device, inode and modes. Do not dump runner env or
   credential files. Run the isolated probes with
   `python3 -m unittest discover -s .github/scripts -p 'test_cargo_gate.py' -v`.
2. Obtain operator authority for the exact host installation. Pause the
   registered C1 runner service using its verified host service definition,
   after its current job finishes. Confirm no `Runner.Worker`, CI holder or
   local Cargo owner remains. Open the existing lock and acquire a
   nonblocking exclusive flock; refusal stops installation. Retain that
   descriptor throughout replacement. Quiescence also prevents an old
   completion hook from acting on the new protocol.
3. Compare installed hashes against the recorded precondition. Copy all
   three installed files to a private, dated backup directory; preserve
   their modes. For each reviewed source file create a mode-0755 temporary
   sibling in `~/bin`, then atomically rename it over the corresponding
   `cargo-gate-ci-start`, `cargo-gate-ci-end`, or `cargo-gate-hold` file.
   Keep both existing `.sh` adapters and the machine lock inode unchanged.
   Remove only temporary files created by this install. Any failure restores
   all three originals while the runner remains paused and the lock held.
4. Verify all installed hashes/modes and unchanged adapter/lock identity,
   release the installation flock, and resume the same registered service.
   Run one fresh owner-admitted Full CI Rust job plus its final acceptance;
   retain the exact run/job URL and hook's successful ownership log. The
   source tests alone do not satisfy this post-install receipt.
5. Rollback uses the same pause/quiescence/exclusive-lock procedure, restores
   the exact three backed-up files through temporary siblings and atomic
   renames, verifies original hashes, then resumes the service. Restoring the
   old fail-open hooks is a deliberate rollback, not acceptance of #2052.

An absent/damaged lock, unknown sidecar or unavailable service control is a
stop for operator inspection. This procedure neither bootstraps a new lock
nor silently deletes unknown holder state.

## What this lane is (and is not)

- **One self-hosted runner** registered against `kckylechen1/tachi` with the
  custom label `tachi-acceptance` (plus the implicit `self-hosted`). The four
  in-scope jobs declare `runs-on: [self-hosted, tachi-acceptance]` — both
  labels are required, so the jobs can only ever land on this runner.
- **Host class (owner-acknowledged deviation):** the census proposed an
  ubuntu-24.04-parity Linux class, but the only trusted build host available
  is this Apple-silicon macOS machine and no container runtime is usable on
  it. The four lanes are platform-neutral (bash/ruby policy checks, cargo
  clippy/fmt/audit/nextest, npm build/test/audit, gitleaks binary scan — none
  uses Linux-only semantics, no `apt`, no docker, no `services:`). The lane is
  therefore *macOS-arm64 executing the Linux-class acceptance surface*; the
  `physical-db-identity-windows` job is **NOT-COVERED** (its `cfg(not(unix))`
  path is dead on any Unix) and stays on `windows-latest` (out of scope,
  #1865).
- **Observational Windows classification (owner adjudication 2026-09-23,
  current delivery only):** the `physical-db-identity-windows` matrix remains
  defined and unconditional on `windows-latest` — all three crate legs still
  execute, and the aggregate still collects and reports their raw status —
  but it is classified observational in `.github/acceptance-plan.json`, so a
  red leg does not block this delivery's CI acceptance. Visible, not
  required: the exclusion is dated, named per job in the plan, and not
  retroactive. Windows releases remain prohibited until #1963 is fixed, and
  this macOS-arm64 lane still certifies no Linux or Windows platform
  behavior.
- **Named coverage gap (review round 1, accepted):** Linux-only `cfg` paths —
  e.g. the seccomp containment in
  `crates/tachi-server/src/dispatch_ops/subprocess.rs` and the
  `renameat2`-dependent receipt logic in
  `crates/tachi-server/src/repair/receipt.rs` — no longer compile or test in
  the canonical lane while it runs on darwin-arm64. That coverage was last
  real on 2026-07-05 (the last hosted-ubuntu green run) and has been absent
  since the queue died. This gap is specific to this C1 lane: Linux
  coverage is now provided by the separate C2 Linux conformance lanes on
  their own host (see [`conformance-runner.md`](conformance-runner.md)).
- **Out of scope by law (#1865):** Windows, macOS release/bottles, native
  release matrix, npm/Homebrew publication (the memcore mirror was retired in
  #1902), all
  release/provider secrets. Those workflows keep their hosted `runs-on` labels
  and are not served by this runner.

## Trust boundary

> **Current posture (owner risk acceptance, 2026-09-27, #2015).** The
> repository is public. This runner stays online for **owner-authored
> workloads only**, on the owner's own account, on the premise that an
> admitted job runs the same code the owner already builds locally. What
> actually keeps non-owner code off this host is not the job guard below
> but three controls outside the workflow file: GitHub's fork-run approval
> policy (`all_external_contributors`), owner-only write access, and the
> operating rule **never approve a fork PR run, and add no collaborator while
> this lane is online**. Accepted residual risk: admitted jobs execute
> with the owner account's access to its credentials and writable state
> (SSH keys, the `gh` login, `~/.tachi`, other repositories), so approval is
> the remaining barrier against a fork-controlled merge-ref workflow. The
> two layers below are defence in depth: on
> `pull_request` the workflow comes from the PR's merge ref, so a PR can
> rewrite its own guard or add a job that names these labels. Deferred host
> hardening (dedicated non-admin account, immutable hook chain, cargo-gate
> cross-account protocol) is tracked in #2015; re-derive this section before
> adding a collaborator, approving any fork run, or adding a
> `pull_request_target`/`workflow_run`/self-hosted workflow.

Two layers inside the workflow files (defence in depth, see above):

1. **Label scoping** — in the checked-in workflows only the five C1 job
   definitions in `ci.yml` (the four lanes plus the `acceptance` aggregate)
   name `tachi-acceptance`; the C2 jobs in `conformance-linux.yml` run on
   GitHub-hosted `ubuntu-24.04` (the self-hosted C2 seat is retired). A PR can add
   its own job naming these labels, so this is routing, not authorization.
2. **Job-level guard** — each of the five C1 jobs carries a fail-closed
   admission expression requiring BOTH `github.actor` (the original event
   actor) AND `github.triggering_actor` (the current operator — on a re-run
   these can differ) to be the repository owner:

   ```yaml
   if: >-
     ((github.event_name == 'push' || github.event_name == 'workflow_dispatch') &&
     github.actor == github.repository_owner &&
     github.triggering_actor == github.repository_owner) ||
     (github.event_name == 'pull_request' &&
     github.event.pull_request.head.repo.full_name == github.repository &&
     github.event.pull_request.user.login == github.repository_owner &&
     github.actor == github.repository_owner &&
     github.triggering_actor == github.repository_owner)
   ```

   Requiring both actor fields closes the two re-run bypasses: a
   collaborator re-running an owner-triggered job (blocked by
   `triggering_actor`), and the owner re-running a collaborator-pushed head
   on an owner-opened PR (blocked by `actor`, which keeps the original
   pusher). Only the enumerated sources run: owner `push`/`dispatch`, and
   same-repo `pull_request` whose opener, original actor, and triggering
   actor are all the repository owner. Fork PRs, non-owner operators, and
   any event type not listed (a future `pull_request_target`, `schedule`,
   or `merge_group` trigger) evaluate to false and the job is **skipped
   before any runner claim is made**. This does not depend on
   `close-external-prs.yml` winning a race against the scheduler.

### Visibility contract (owner rule, review R5)

The guard lives in `ci.yml`, and `pull_request` runs execute the workflow
from the **merge ref** — the PR's own version of the file. The guard is
therefore trustworthy only while repository **write access is owner-only**.
*Historical (v1, while the repository was private):* the private
single-owner state guaranteed that true fork PRs could not exist and only
the owner could push branches, so same-repository owner-authored PRs were an
admitted source by definition. That guarantee no longer holds: in a public
repository anyone can open a fork PR carrying a modified workflow, and
owner-only write access does not prevent that.

The repository has since been made public. The owner re-derived this
boundary on 2026-09-27 and accepted the risk for owner-authored workloads
(see *Current posture* above and #2015). Runner groups with a
selected-workflow restriction are unavailable for a user-owned repository,
so the fork-run approval policy plus the never-approve rule is the
remaining barrier on the merge-ref path. **Before adding any collaborator
with write access, or before approving any fork run, stop the runner
first** (`cd ~/runner-tachi && ./svc.sh stop`) and re-derive the boundary.

## GITHUB_TOKEN surface

`ci.yml` already declares workflow-level `permissions: contents: read`. The
four lanes need nothing beyond the auto `GITHUB_TOKEN` (gitleaks read,
artifact upload). No third-party publication credential (`NPM_TOKEN`, `HOMEBREW_TAP_GITHUB_TOKEN`, provider keys) is referenced by any
job served by this runner.

## Installation (host, once)

```bash
# Prerequisite: the setup-rust composite action runs under pwsh. If the
# brew stable cask is unavailable, stage the official osx-arm64 tarball:
#   mkdir -p ~/runner-tachi/pwsh && cd ~/runner-tachi/pwsh
#   curl -sL -o pwsh.tar.gz https://github.com/PowerShell/PowerShell/releases/download/<ver>/powershell-<ver>-osx-arm64.tar.gz
#   tar xzf pwsh.tar.gz && rm pwsh.tar.gz
# and add ~/runner-tachi/pwsh to the runner .path below.

mkdir -p ~/runner-tachi && cd ~/runner-tachi
# Fetch the latest actions-runner release for osx-arm64 and unpack it here.
# Registration token: single-use, short-lived; pipe straight from gh into
# config.sh via command substitution -- never echoed, never stored.
./config.sh --url https://github.com/kckylechen1/tachi \
  --token "$(gh api repos/kckylechen1/tachi/actions/runners/registration-token --jq .token)" \
  --name tachi-acceptance-1 --labels tachi-acceptance --ephemeral=no
```

`--ephemeral=no` (default persistent registration) keeps the single-slot
behavior: the runner takes one job at a time.

**PATH for the launchd service**: the service environment does not inherit the
interactive shell PATH. Create `~/runner-tachi/.path` (one entry per line) so
job steps can resolve `rustup`/`cargo`/`python3`/`git`:

```
/opt/homebrew/bin
/usr/local/bin
/Users/<user>/.cargo/bin
/usr/bin
/bin
/usr/sbin
/sbin
```

(Adjust to the real home; entries are appended to the service PATH. `ruby` and
`git` resolve from `/usr/bin`; `python3` and `pwsh` come from `/opt/homebrew/bin`.)

Then install and start the launchd service:

```bash
./svc.sh install && ./svc.sh start
```

## Disk watermarks (frozen owner lines)

- **First activation (enable Actions + register + first smoke) requires
  >= 80 GiB free** on the data volume. Poll with
  `df -h /System/Volumes/Data`.
- **Heavy-job start watermark 60 GiB**: the `rust` job's first post-checkout
  step is `runner_hygiene.sh preflight 60`, which fails the job loudly
  (`exit 3` with `::error::`) below the watermark. Light lanes use a 10 GiB
  sanity watermark.

## Job hygiene (tracked, reviewable)

- `.github/scripts/runner_hygiene.sh preflight <gib>` — every lane, first
  step after checkout: terminates processes a cancelled predecessor left
  holding the workspace (delegating to the tracked killer with
  `SCAN_ROOT=$GITHUB_WORKSPACE`; refusing loudly if they cannot be
  terminated), removes file residue (`target/`, `node_modules/`), reports
  the disk ledger, and enforces the watermark. The `gitleaks` lane adds an
  inline pre-checkout disk gate because its full-history checkout precedes
  script availability.
- `.github/scripts/runner_hygiene.sh cleanup` — every lane, last step with
  `if: always()`: wipes the entire workspace contents (including `.git`) so
  the runner holds no per-job disk between jobs; every job pays a fresh
  checkout. Target storage is therefore **per-job ephemeral** by
  construction; there is deliberately no shared unbounded `target/` and no
  private `CARGO_TARGET_DIR` (#1184 law).
- Caches that live outside the disposable workspace and are bounded by the
  toolchain/dependency universe, not by job count:
  `~/.rustup` (pinned 1.97.0 toolchain — shared with the developer seat),
  `~/.cargo` (registry + `cargo-audit`/nextest binaries — reinstall is
  idempotent), `~/runner-tachi/_work/_tool` (node 20 toolcache),
  `~/.npm` (setup-node cache). If `~/.cargo/registry` ever grows past a few
  GB, prune with `rm -rf ~/.cargo/registry/cache/*` between jobs.

## Queue hygiene (before first activation)

1. Enumerate every queued run — paginate (review R5): `gh api --paginate
   'repos/kckylechen1/tachi/actions/runs?status=queued&per_page=100'`.
2. Cancel every queued run (the 34-day stale queue, including the
   2026-08-10 memcore-mirror dispatch — cancellation is safe; re-running it is
   the owner's call and it must not execute on the new lane).
3. Then enable Actions:
   `gh api -X PUT repos/kckylechen1/tachi/actions/permissions -f enabled=true`.
4. `fmt.yml` is disabled as a workflow (redundant with the `rust` job's
   `cargo fmt --all --check` step — #1852 Finding 6, #1865 "prefer proving it
   is redundant"): `gh api -X PUT repos/kckylechen1/tachi/actions/workflows/fmt.yml/disable`.
   Re-enable explicitly if a separate fmt lane is ever wanted again.

## Cancellation protocol (acceptance 7)

After cancelling a run from the UI or `gh run cancel`, run the tracked
stray-killer from the developer checkout (any cwd is safe — the script
moves its own working directory out of the scan domain and never signals
its own process tree):

```bash
bash ~/Projects/Sigil/.github/scripts/runner_kill_strays.sh list   # inspect first
bash ~/Projects/Sigil/.github/scripts/runner_kill_strays.sh kill   # SIGTERM, then SIGKILL survivors
```

The kill set is the processes whose working directory sits under the
runner's `_work` root, plus processes executing a binary mapped from
under it (chdir-escaped compiled builds); processes that merely mention a
`_work` path in their arguments (editors, indexers) are listed as foreign
and left alone. If lsof discovery fails the script exits non-zero with
UNKNOWN state instead of claiming success.

Automatic `cancel-in-progress` does not invoke this script, but every
lane's preflight does (with `SCAN_ROOT` narrowed to that job's workspace):
it terminates leftover workspace-holding processes before building and
refuses loudly if they survive, then wipes file residue — so a cancelled
build cannot silently poison its successor.

Known limitation (reviews R5/R6): stray detection uses three signals --
cwd under the workspace, executable mapped from the workspace (catches
chdir-escaped compiled test/build binaries via `lsof -d txt`), and argv
references (reported, not signalled). A descendant that daemonizes,
changes its cwd, AND executes a binary copied outside the workspace with
a scrubbed argv is invisible to all three; no post-hoc scanner can catch
a process that deliberately erases every trace, and the runner's own
cancellation tree-kill is the first line of defence for that class. The
realistic cargo/nextest/rustc/test-binary set is covered by the cwd and
executable signals; argv-only matches are surfaced by preflight as a
WARNING block and by `list` under FOREIGN for operator judgement.

## Rollback / decommission

```bash
cd ~/runner-tachi && ./svc.sh stop && ./svc.sh uninstall
./config.sh remove --token "$(gh api repos/kckylechen1/tachi/actions/runners/remove-token --jq .token)"
gh api -X PUT repos/kckylechen1/tachi/actions/permissions -f enabled=false
```

The workflow edits revert with the merge commit; nothing else on the host is
runner-owned outside `~/runner-tachi` and the tool caches listed above.
