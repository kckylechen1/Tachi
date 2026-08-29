# Trusted acceptance runner (single-lane) — operator runbook

Covers issue #1865 (owner-ratified C1): one trusted acceptance runner serving
exactly the four in-scope lanes of `ci.yml` — `build-seat-setup`, `rust`,
`node`, `gitleaks`. Census and sizing rationale: #1852.

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
- **Named coverage gap (review round 1, accepted):** Linux-only `cfg` paths —
  e.g. the seccomp containment in
  `crates/tachi-server/src/dispatch_ops/subprocess.rs` and the
  `renameat2`-dependent receipt logic in
  `crates/tachi-server/src/repair/receipt.rs` — no longer compile or test in
  the canonical lane while it runs on darwin-arm64. That coverage was last
  real on 2026-07-05 (the last hosted-ubuntu green run) and has been absent
  since the queue died; restoring it requires a genuine Linux host (census
  class A), which is an owner decision outside this lane.
- **Out of scope by law (#1865):** Windows, macOS release/bottles, native
  release matrix, memcore mirror publication, npm/Homebrew publication, all
  release/provider secrets. Those workflows keep their hosted `runs-on` labels
  and are not served by this runner.

## Trust boundary (fork PRs never get this runner)

Two structural layers, no racing:

1. **Label scoping** — only the four in-scope jobs name the runner labels; no
   other workflow in the repository uses `self-hosted`/`tachi-acceptance`.
2. **Job-level guard** — each of the four jobs carries a fail-closed
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
therefore trustworthy only while repository **write access is owner-only**,
which the private single-owner repo state guarantees today (true fork PRs
cannot exist, and only the owner can push branches). That is the ticket's
v1 trust model: same-repository owner-authored PRs are an admitted source
by definition.

**Before making this repository public, or adding any collaborator with
write access, stop the runner first** (`cd ~/runner-tachi && ./svc.sh
stop`) and re-derive the boundary — a modified `ci.yml` in a PR-controlled
merge ref could otherwise drop the guard and claim the runner. In a public
future, GitHub's outside-collaborator approval gate and a runner-group
review policy are the minimum re-work; do not carry this lane over
unchanged.

## GITHUB_TOKEN surface

`ci.yml` already declares workflow-level `permissions: contents: read`. The
four lanes need nothing beyond the auto `GITHUB_TOKEN` (gitleaks read,
artifact upload). No third-party publication credential (`MEMCORE_MIRROR_TOKEN`,
`NPM_TOKEN`, `HOMEBREW_TAP_GITHUB_TOKEN`, provider keys) is referenced by any
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
