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
- **Out of scope by law (#1865):** Windows, macOS release/bottles, native
  release matrix, memcore mirror publication, npm/Homebrew publication, all
  release/provider secrets. Those workflows keep their hosted `runs-on` labels
  and are not served by this runner.

## Trust boundary (fork PRs never get this runner)

Two structural layers, no racing:

1. **Label scoping** — only the four in-scope jobs name the runner labels; no
   other workflow in the repository uses `self-hosted`/`tachi-acceptance`.
2. **Job-level guard** — each of the four jobs carries:

   ```yaml
   if: >-
     github.event_name != 'pull_request' ||
     (github.event.pull_request.head.repo.full_name == github.repository &&
     github.event.pull_request.user.login == github.repository_owner)
   ```

   A `pull_request` event whose head repo is a fork (or whose author is not
   the repository owner) evaluates the guard to false and the job is **skipped
   before any runner claim is made**. This does not depend on
   `close-external-prs.yml` winning a race against the scheduler. Same-repo
   branches authored by the owner, `push` to `main`, and owner
   `workflow_dispatch` (private repo: dispatch requires write access) are the
   only admitted sources.

The repository is private, so true fork PRs cannot exist today; the guard is
the standing boundary for any future visibility/collaborator change.

## GITHUB_TOKEN surface

`ci.yml` already declares workflow-level `permissions: contents: read`. The
four lanes need nothing beyond the auto `GITHUB_TOKEN` (gitleaks read,
artifact upload). No third-party publication credential (`MEMCORE_MIRROR_TOKEN`,
`NPM_TOKEN`, `HOMEBREW_TAP_GITHUB_TOKEN`, provider keys) is referenced by any
job served by this runner.

## Installation (host, once)

```bash
# Prerequisite: the setup-rust composite action runs under pwsh.
brew install --cask powershell

mkdir -p ~/runner-tachi && cd ~/runner-tachi
# Fetch the latest actions-runner release for osx-arm64 and unpack it here.
# Registration token (treat as a secret; never echo it):
gh api repos/kckylechen1/tachi/actions/runners/registration-token \
  --jq .token > /dev/null   # pipe straight into config, do not store
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
  step after checkout: removes residue a cancelled predecessor left
  (`target/`, `node_modules/` inside the workspace), reports the disk ledger,
  enforces the watermark.
- `.github/scripts/runner_hygiene.sh cleanup` — every lane, last step with
  `if: always()`: removes the same residue and reports disk again. Target
  storage is **per-job ephemeral** (in-workspace `target/`, deleted at job
  end); there is deliberately no shared unbounded `target/` and no private
  `CARGO_TARGET_DIR` (#1184 law).
- Caches that live outside the disposable workspace and are bounded by the
  toolchain/dependency universe, not by job count:
  `~/.rustup` (pinned 1.97.0 toolchain — shared with the developer seat),
  `~/.cargo` (registry + `cargo-audit`/nextest binaries — reinstall is
  idempotent), `~/runner-tachi/_work/_tool` (node 20 toolcache),
  `~/.npm` (setup-node cache). If `~/.cargo/registry` ever grows past a few
  GB, prune with `rm -rf ~/.cargo/registry/cache/*` between jobs.

## Queue hygiene (before first activation)

1. Enumerate: `gh api 'repos/kckylechen1/tachi/actions/runs?status=queued'`.
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

After cancelling a run from the UI or `gh run cancel`:

```bash
.github/scripts/runner_kill_strays.sh list   # inspect; dev builds elsewhere must not appear
.github/scripts/runner_kill_strays.sh kill   # SIGTERM, then SIGKILL survivors
```

The next job's preflight removes any partial `target/` residue, so a
cancelled build cannot poison its successor.

## Rollback / decommission

```bash
cd ~/runner-tachi && ./svc.sh stop && ./svc.sh uninstall
./config.sh remove --token "$(gh api repos/kckylechen1/tachi/actions/runners/remove-token --jq .token)"
gh api -X PUT repos/kckylechen1/tachi/actions/permissions -f enabled=false
```

The workflow edits revert with the merge commit; nothing else on the host is
runner-owned outside `~/runner-tachi` and the tool caches listed above.
