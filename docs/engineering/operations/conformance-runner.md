# Linux platform-conformance runner (C2) — operator runbook

Covers the C2 runner class from the CI census (#1852) and the S2 readiness
inventory (#1877): one Linux x86_64 runner serving the
`conformance-linux.yml` lanes — the canonical Rust gate and the Linux-only
cfg paths — under the custom label `tachi-conformance`. C1's acceptance
runner (`tachi-acceptance`, macOS arm64, `acceptance-runner.md`) is a
different lane and stays untouched.

## Why this lane exists

The C1 lane executes the Linux-class acceptance surface on darwin-arm64, so
every `cfg(linux)` leg compiles out there. #1877 names the uncovered
conformance inventory: the seccomp containment escape tests (now cfg-shared
with Linux x86_64/aarch64 in
`crates/tachi-server/src/dispatch_ops/subprocess.rs`), the renameat2
no-clobber / atomic-exchange / receipt legs (`memcore::db::filename`,
`memcore::anchored_fs`, `tachi-server` `repair::receipt`,
`sticky_cutover_cli`), and — still open — a Linux kill-test certification
receipt. Only a real Linux host closes this; a macOS runner cannot.

## What this lane is (and is not)

- **Two jobs, one runner class** (`runs-on: [self-hosted, Linux, X64,
  tachi-conformance]`; GitHub's auto label for x86_64 Linux is `X64`):
  - `rust-gate` — the canonical gate surface on real Linux: release-version
    sync, nextest census contracts, workspace clippy/fmt, nextest workspace
    run (`--profile ci`) + JUnit archive, portable-kernel feature boundary,
    doc tests. **Cargo audit is intentionally not duplicated**: it audits
    `Cargo.lock` (platform-independent) and the audited supply-chain policy
    (`validate_action_pins.rb`) pins the `cargo install cargo-audit` count at
    exactly one across all workflows; the RustSec audit keeps running in the
    ci.yml canonical lane.
  - `linux-platform` — the #1877 inventory as named nextest filters:
    seccomp setsid-EPERM probe, memcore renameat2/anchored legs, tachi-server
    receipt + sticky-cutover exchange legs.
- **Not** a release/publish lane: no publication or provider secrets;
  `GITHUB_TOKEN` only, workflow-level `permissions: contents: read`.
- **Not** the C1 runner: distinct label; jobs cannot land on
  `tachi-acceptance` and vice versa.
- The Linux kill-test that mints the first Linux certification receipt
  (`certifications/codex-cli.toml` is macOS-scoped) is **NOT** wired into CI.
  Receipt minting is an owner-supervised S2 activation act (#1877
  recommendation 2), not a routine CI lane.

## Trust boundary

Identical to C1 (see `acceptance-runner.md` for the full rationale): the
fail-closed job guard admits only owner `push`/`workflow_dispatch` or
same-repo owner-authored PRs, requires both `github.actor` and
`github.triggering_actor`, and skips before any runner claim otherwise. The
visibility contract carries over verbatim: the guard is trustworthy only
while repository write access is owner-only — **stop the runner before
making the repo public or adding collaborators**.

## Capacity source (decision record, 2026-08-29)

The only trusted build host is the macOS-arm64 machine that also hosts the C1
runner. Verified absent on it: lima, colima, multipass, orb, qemu, VirtualBox,
Docker, Podman, UTM, Parallels — and no remote Linux box was provided. The
smallest reliable bootstrap is therefore **lima + the official
actions-runner linux-x64 tarball inside a lima VM** (brew install on the host
follows the C1 precedent of installing the runner itself).

**Blocked at preparation time by the frozen disk watermarks** (#1865 law,
applies to the real backing store — the host disk under the VM image):

- Host free disk at census: **47 GiB** < 60 GiB heavy-start watermark < 80 GiB
  activation waterline. No VM image, no runner registration, no smoke until
  the owner frees disk past the activation waterline.
- **Arch choice is an owner decision:** an x86_64 guest on Apple Silicon runs
  under QEMU emulation (slow: expect the workspace clippy lane to take
  multiples of the 8-minute hosted measurement). A Linux **aarch64** guest
  runs natively (lima `vz` runtime) and #1877 §3 explicitly admits "Linux
  x86_64 or aarch64" — the seccomp filter ships a certified aarch64 audit-arch
  implementation. If aarch64 is chosen, change `runs-on` to
  `[self-hosted, Linux, ARM64, tachi-conformance]` and relax the
  `Verify runner platform` step to `x86_64|aarch64` (both one-line diffs).
  The workflow as landed follows the census ruling (x86_64).

## Installation (host + VM, once disk allows)

```bash
# Host (Apple Silicon), when free disk >= 80 GiB:
brew install lima
# x86_64 per the census ruling (slow, emulated) — or aarch64 if the owner
# rules for the native-vz alternative above:
limactl start --name=tachi-c2 template://debian-12 --arch=x86_64 --disk=120
limactl shell tachi-c2
```

Inside the VM (Debian/Ubuntu example):

```bash
sudo apt-get update
sudo apt-get install -y git curl ca-certificates lsof python3 python3-venv
# pwsh: the setup-rust composite runs under pwsh (same as C1's macOS host).
# Prefer the distro tarball; the C1 runbook's staging recipe ports directly:
sudo mkdir -p /opt/pwsh && curl -sL \
  https://github.com/PowerShell/PowerShell/releases/download/<ver>/powershell-<ver>-linux-x64.tar.gz \
  | sudo tar xz -C /opt/pwsh && sudo ln -s /opt/pwsh/pwsh /usr/local/bin/pwsh
# rustup as the runner user (setup-rust refuses non-rustup hosts and installs
# the exact rust-toolchain.toml pin):
curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal

mkdir -p ~/runner-tachi-c2 && cd ~/runner-tachi-c2
# Fetch the latest actions-runner release for linux-x64 and unpack it here.
# Registration token: single-use, short-lived; pipe straight from gh into
# config.sh via command substitution -- never echoed, never stored.
./config.sh --url https://github.com/kckylechen1/tachi \
  --token "$(gh api repos/kckylechen1/tachi/actions/runners/registration-token --jq .token)" \
  --name tachi-conformance-1 --labels tachi-conformance --ephemeral=no
```

Runner service environment (the Linux systemd service does not inherit the
interactive environment, same as C1's launchd):

- `~/runner-tachi-c2/.env` (one `KEY=VALUE` per line, applied to every job):

  ```
  RUNNER_ROOT=/home/<user>/runner-tachi-c2
  ```

  `runner_hygiene.sh` refuses to operate unless the workspace resolves inside
  `${RUNNER_ROOT:-$HOME/runner-tachi}/_work` — without this line the C2
  lanes fail closed at preflight (exit 8) instead of wiping an unverified
  tree. Keep the C1 default in mind: the Linux VM has no `~/runner-tachi`,
  so `RUNNER_ROOT` is **required**, not optional.

- `~/runner-tachi-c2/.path` (appended to the service PATH):

  ```
  /home/<user>/.cargo/bin
  /usr/local/bin
  /usr/bin
  /bin
  ```

Then:

```bash
sudo ./svc.sh install && sudo ./svc.sh start
```

## Disk watermarks (frozen owner lines, both levels)

- **VM disk**: provision >= 120 GiB virtual. The 60 GiB heavy-start watermark
  is enforced inside the job by `runner_hygiene.sh preflight 60` (fails the
  job loudly with `::error::`, exit 3, below the watermark). Workspace/target
  storage is per-job ephemeral; the same hygiene scripts C1 uses run unchanged
  on Linux (pure bash + `df`/`find`/`lsof`/`pgrep`).
- **Host disk (the VM image's backing store)**: the #1865 activation
  waterline (>= 80 GiB free before first registration + smoke) and the
  runtime watermark apply to the host too — the VM's `df` sees its virtual
  disk, not the host's remaining headroom. Check both before every heavy
  lane: `df -h /System/Volumes/Data` (host) and `limactl shell tachi-c2 -- df -h /` (VM).
- Bounded outside-workspace caches inside the VM: `~/.rustup`,
  `~/.cargo` (prune `~/.cargo/registry/cache/*` if it grows past a few GB),
  `~/runner-tachi-c2/_work/_tool`. Same accounting as C1's runbook.

## Queue hygiene before first activation

Same law as C1: enumerate (`gh api --paginate
'repos/kckylechen1/tachi/actions/runs?status=queued&per_page=100'`), cancel
every stale queued run, prove the queue is empty, then dispatch exactly one
`conformance-linux` smoke on current `main`. The 2026-08 memcore-mirror
dispatch must never execute on this lane.

## First-smoke requirements

Dispatch `conformance-linux.yml` on the target `main` SHA and record: runner
identity/labels, SHA, start/end disk (host AND VM), and the terminal state
of both jobs. A green smoke proves, for the first time since the hosted-queue
death:

- the canonical gate surface (clippy/fmt/nextest workspace/doc tests/
  portable-kernel boundary) on real Linux x86_64;
- the seccomp setsid-EPERM containment probe and its discrimination test
  executing the actual filter (cfg-shared with macOS since the C2 branch);
- the renameat2 / anchored-rename / receipt / sticky-cutover legs from the
  #1877 inventory running on their home platform.

Still NOT delivered by this lane (named gaps, never faked green): the Linux
codex kill-test certification receipt (owner-supervised, #1877), and the
Windows `cfg(not(unix))` identity leg (C1/C2 both leave it to
`windows-latest`).

## Rollback / decommission

```bash
cd ~/runner-tachi-c2 && sudo ./svc.sh stop && sudo ./svc.sh uninstall
./config.sh remove --token "$(gh api repos/kckylechen1/tachi/actions/runners/remove-token --jq .token)"
limactl stop tachi-c2 && limactl delete tachi-c2   # frees the VM image
brew uninstall lima                                  # optional, host reclaim
```

The workflow file and the cfg-widened tests revert with the merge commit;
nothing else is runner-owned outside the VM and `~/runner-tachi-c2`.
