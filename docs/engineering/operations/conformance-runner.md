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

## Installation (owner-authorized activation, once disk allows)

Merging the workflow does not authorize provisioning, registration, or
activation. Recheck capacity and the runner inventory before starting; the
capacity record above is a dated census, not a current probe.

Use the owner's already-authenticated administration workstation for GitHub
operations. If using the CLI, install `gh` there and verify `gh auth status`
for the owner account before proceeding. Do not copy that login, a PAT, or a
GitHub credential file into the VM. The runner VM needs only the short-lived
registration token supplied interactively by the owner.

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
sudo apt-get install -y git curl ca-certificates lsof python3 python3-venv build-essential pkg-config libssl-dev
```

Before registration, provision **PowerShell (`pwsh`)** and **rustup** as
explicit prerequisites. The setup-rust composite runs under PowerShell and
requires rustup; it then installs the repository's `rust-toolchain.toml` pin.
Use owner-approved signed packages or locally staged, version-pinned release
artifacts. Record each version, official source URL, and independently
verified release checksum in the activation receipt **before** extracting
or executing it. A checksum generated only from the downloaded file is not
verification. Never pipe a remote installer into a shell or a tarball into
privileged extraction. If these prerequisites or their verification evidence
are missing, stop preparation rather than inventing a version or hash.

On the administration workstation, open **Settings → Actions → Runners → New
self-hosted runner**, select **Linux / x64**, and use the download and SHA-256
verification instructions shown for that exact runner release. Stage the
verified archive in the VM and extract it as the unprivileged runner user in
the directory below. Do not select an unpinned `latest` asset. See
[GitHub's runner registration instructions](https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/add-runners).

```bash
mkdir -p ~/runner-tachi-c2 && cd ~/runner-tachi-c2
# After extracting the verified runner release here, verify prerequisites:
command -v pwsh
command -v rustup
pwsh -NoProfile -Command '$PSVersionTable.PSVersion.ToString()'
rustup --version
# The owner enters the short-lived token at the interactive prompt. Do not
# put it in shell history, command arguments, logs, files, or the service env.
./config.sh --url https://github.com/kckylechen1/tachi \
  --name tachi-conformance-1 --labels tachi-conformance
```

Persistent registration is the default; do not pass `--ephemeral` for this
service runner. `--ephemeral=no` is not a supported way to disable the flag.
Registration tokens expire after one hour; request a fresh token from the
owner if preparation takes longer. No `gh` installation or login is needed
inside the VM.

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

From the authenticated administration workstation, enumerate C2's queued
runs (`gh api --paginate
'repos/kckylechen1/tachi/actions/workflows/conformance-linux.yml/runs?status=queued&per_page=100'`).
Identify exact stale run IDs and cancel them with owner authorization; do not
cancel unrelated C1 or other workflows. Recheck both queued and in-progress
C2 runs, then dispatch exactly one `conformance-linux` smoke on current
`main`. The 2026-08 memcore-mirror dispatch must never execute on this lane.

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

Requires a separate owner decision. Confirm that the named C2 runner has no
active job, stop its service, and obtain a short-lived removal token from the
owner's runner settings. Enter it at the interactive removal prompt; do not
authenticate `gh` in the VM. Confirm the exact VM and preserve any required
receipts before deleting its disk image.

```bash
cd ~/runner-tachi-c2 && sudo ./svc.sh stop && sudo ./svc.sh uninstall
./config.sh remove
limactl stop tachi-c2 && limactl delete tachi-c2   # frees the VM image
brew uninstall lima                                  # optional, host reclaim
```

The workflow file and the cfg-widened tests revert with the merge commit;
nothing else is runner-owned outside the VM and `~/runner-tachi-c2`.
