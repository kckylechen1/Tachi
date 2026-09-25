# Linux platform-conformance runner (C2) — operator runbook

Covers the C2 runner class from the CI census (#1852), the S2 readiness
inventory (#1877), and its activation leaf (#1975): one native Linux aarch64
runner serving the `conformance-linux.yml` lanes — the canonical Rust gate and
the Linux-only cfg paths — under the custom label `tachi-conformance`. C1's
acceptance runner (`tachi-acceptance`, macOS arm64, `acceptance-runner.md`) is
a different lane and stays untouched.

## Why this lane exists

The C1 lane executes the Linux-class acceptance surface on darwin-arm64, so
every `cfg(linux)` leg compiles out there. #1877 names the uncovered
conformance inventory: the seccomp containment escape probe (Linux
x86_64/aarch64 in `crates/tachi-server/src/dispatch_ops/subprocess.rs`), the
renameat2 no-clobber / atomic-exchange / receipt legs (`memcore::db::filename`,
`memcore::anchored_fs`, `tachi-server` `repair::receipt`,
`sticky_cutover_cli`), and — still open — a Linux kill-test certification
receipt. Only a real Linux host closes this; a macOS runner cannot.

## Host (owner-provisioned 2026-09-25, #1975)

| Field | Value |
|---|---|
| Host | DGX Spark `atom-dgx-2` (192.168.100.68) — native Linux, not a VM |
| OS / arch | Ubuntu 24.04, **aarch64** |
| Capacity | 20 cores, ~121 GB RAM, 3.5 TB free at provisioning |
| Runner user | dedicated **unprivileged** user `gha`, **no sudo** |
| Labels | `self-hosted, Linux, ARM64, tachi-conformance` (`ARM64` is GitHub's auto label for aarch64 Linux) |
| Distinct from | C1 `tachi-acceptance` (macOS) |

**Arch ruling (#1975):** aarch64. #1877 §3 admits "Linux x86_64 or aarch64",
and the seccomp filter ships a certified aarch64 audit-arch (the
`NATIVE_AUDIT_ARCH` constant has x86_64 and aarch64 arms in
`install_linux_postflight_escape_filter`, `subprocess.rs`). The workflow's
`Verify runner platform` step accepts `aarch64` and `x86_64` — the two arches
with a certified audit-arch — and fails the job loudly (exit 4) on anything
else, so a mislabeled host can never fake green. It also prints
`RUNNER_NAME`, so every job log names the runner that executed it.

## What this lane is (and is not)

- **Two jobs, one runner class** (`runs-on: [self-hosted, Linux, ARM64,
  tachi-conformance]`):
  - `rust-gate` — the canonical gate surface of `ci.yml`'s `rust` job on real
    Linux: release-version sync, nextest census contracts, setup-rust source
    contracts, workspace clippy/fmt, nextest workspace run (`--profile ci`) +
    JUnit archive (`nextest-junit-report-linux-c2`), portable-kernel feature
    boundary, doc tests. **Cargo audit is intentionally not duplicated**: it
    audits `Cargo.lock` (platform-independent) and the audited supply-chain
    policy (`validate_action_pins.rb`) pins the `cargo install cargo-audit`
    count at exactly one across all workflows; the RustSec audit keeps running
    in the `ci.yml` canonical lane.
  - `linux-platform` — the #1877 inventory as named nextest filters, **one
    step per filter, each with `--no-tests=fail`**, and every filter step runs
    even if an earlier one failed. A filter that matches zero tests (renamed
    test, or a test cfg'd out on Linux) fails its own step loudly:

    | Step | Package | Filter |
    |---|---|---|
    | Seccomp containment escape probe | `tachi-server` | `-- --exact dispatch_ops::subprocess::required_postflight_kernel_containment_discriminates_setsid_escape` |
    | renameat2 no-clobber legs | `memcore` | `db::filename::tests::atomic_rename` |
    | Anchored directory legs | `memcore` | `anchored_fs::tests` |
    | renameat2 receipt legs | `tachi-server` | `repair::receipt::tests` |
    | renameat2 atomic-exchange legs | `tachi-server` | `sticky_cutover` |

- **Not** a release/publish lane: no publication or provider secrets;
  `GITHUB_TOKEN` only, workflow-level `permissions: contents: read`.
- **Not** the C1 runner: distinct label; jobs cannot land on
  `tachi-acceptance` and vice versa. `ci.yml` is unchanged by this lane.
- **Not** Linux certification: the Linux kill-test that mints the first Linux
  certification receipt (`certifications/codex-cli.toml` is macOS-scoped) is
  **NOT** wired into CI. Receipt minting is an owner-supervised S2 act (#1877
  recommendation 2), not a routine CI lane.
- **Not** a change to Required-postflight semantics: this lane only executes
  the tests that exist on `main`; it does not alter what a Required runner is
  allowed to claim on any platform.

## Trust boundary

Identical to C1 (see `acceptance-runner.md` for the full rationale): the
fail-closed job guard — copied verbatim from `ci.yml`'s `build-seat-setup`
job — admits only owner `push`/`workflow_dispatch` or same-repo owner-authored
PRs, requires both `github.actor` and `github.triggering_actor` to be the
repository owner, and skips before any runner claim otherwise. The visibility
contract carries over verbatim: the guard is trustworthy only while repository
write access is owner-only — **stop the runner before making the repo public
or adding collaborators**.

Host-side, the runner process runs as `gha` without sudo, so a job can write
only to `gha`'s home, the runner work root, and world-writable temp space. It
cannot install packages, change system services, or read other users' files.

## Prerequisite receipt (verified before installation, #1975)

Each artifact was checked against a published checksum from an independent
source before it was extracted or executed. A checksum generated only from the
downloaded file is not verification.

| Artifact | Version / target | SHA-256 (prefix) | Verified against |
|---|---|---|---|
| PowerShell (`pwsh`) | 7.6.6 linux-arm64 | `924829e5…` | GitHub release asset digest **and** the release's `hashes.sha256` |
| actions/runner | 2.337.0 linux-arm64 | `9b1dc706…` | GitHub release asset digest **and** the release-notes SHA block |
| `rustup-init` | aarch64-unknown-linux-gnu | `15f6e4ce…` | static.rust-lang.org `.sha256` |
| Rust toolchain | 1.97.0 + clippy + rustfmt | (installed by rustup) | pinned by `rust-toolchain.toml` |

The setup-rust composite runs under `pwsh`, requires `rustup` on `PATH`, and
installs the `rust-toolchain.toml` pin itself if it is missing. Re-running it
with the pin already present is a no-op check. Any future prerequisite upgrade
repeats this procedure and appends a row to the activation receipt; never pipe
a remote installer into a shell.

The jobs also need `git`, `python3`, `lsof`, a C toolchain, and `pkg-config`
(native build scripts in the workspace). `gha` has no sudo, so any missing
system package is an owner act from an administrative account, never
something a job or the runner user installs. Verify as `gha` before the
first smoke:

```bash
for tool in pwsh rustup git python3 lsof cc pkg-config; do
  printf '%s: ' "$tool"; command -v "$tool" || echo MISSING
done
pwsh -NoProfile -Command '$PSVersionTable.PSVersion.ToString()'
rustup --version
```

## Runner registration and service environment

Registration is done as `gha` from the runner install directory
(`<runner-root>` below; record the actual path in the activation receipt).
The owner supplies the short-lived registration token interactively from
**Settings → Actions → Runners → New self-hosted runner** on the already
authenticated administration workstation; do not put it in shell history,
command arguments, logs, files, or the service environment, and do not copy a
`gh` login, PAT, or GitHub credential file onto the host.

```bash
cd <runner-root>
./config.sh --url https://github.com/kckylechen1/tachi \
  --name <runner-name> --labels tachi-conformance
```

`self-hosted`, `Linux`, and `ARM64` are applied automatically by the runner;
only the custom `tachi-conformance` label is passed. Persistent registration
is the default; do not pass `--ephemeral`. Registration tokens expire after
one hour.

Runner service environment (the service does not inherit the interactive
environment):

- `<runner-root>/.env` (one `KEY=VALUE` per line, applied to every job):

  ```
  RUNNER_ROOT=<runner-root>
  ```

  `runner_hygiene.sh` refuses to operate unless the workspace resolves inside
  `${RUNNER_ROOT:-$HOME/runner-tachi}/_work`. Unless the runner is installed
  at `~gha/runner-tachi`, this line is **required**: without it both C2 jobs
  fail closed at preflight (exit 8) instead of wiping an unverified tree.

- `<runner-root>/.path` (the service PATH): must contain the directories
  holding `pwsh`, `rustup`/`cargo` (`/home/gha/.cargo/bin`), and the system
  tools (`/usr/local/bin`, `/usr/bin`, `/bin`).

Installing a system service (`./svc.sh install`) needs root, which `gha`
does not have: the service unit is an owner act from an administrative
account, configured to run the runner as `gha`. Record the method (system
unit with `User=gha`, or a user unit with linger) in the activation receipt.

## Disk watermarks (frozen owner lines)

- The #1865 heavy-start watermark (60 GiB free) is enforced inside each job by
  `runner_hygiene.sh preflight 60`, which fails the job loudly (`::error::`,
  exit 3) below it. The activation waterline (>= 80 GiB free before the first
  registration + smoke) applies to the volume backing `<runner-root>/_work`.
  On this native host there is no VM layer: the job's `df` sees the real
  backing store (3.5 TB free at provisioning).
- Workspace/target storage is per-job ephemeral: `runner_hygiene.sh cleanup`
  wipes the workspace on every exit path. The same hygiene script C1 uses runs
  unchanged on Linux (pure bash + `df`/`find`/`lsof`/`pgrep`).
- Bounded outside-workspace caches under `gha`: `~/.rustup`, `~/.cargo`
  (prune `~/.cargo/registry/cache/*` if it grows past a few GB),
  `<runner-root>/_work/_tool`. Same accounting as C1's runbook.

## Queue hygiene before first activation

From the authenticated administration workstation, enumerate C2's queued runs
(`gh api --paginate
'repos/kckylechen1/tachi/actions/workflows/conformance-linux.yml/runs?status=queued&per_page=100'`).
Identify exact stale run IDs and cancel them with owner authorization; do not
cancel unrelated C1 or other workflows. Recheck both queued and in-progress C2
runs before dispatching the smoke.

## First-smoke requirements (#1975 acceptance)

Record, verbatim from the run: runner name and labels (the `Verify runner
platform` step prints `runner_name`, `uname_s`, `uname_m`), head SHA, start/end
free disk, and the terminal state of both jobs. The leaf is accepted only by:

- one green `conformance-linux` run on the PR head, executed on the
  `atom-dgx-2` runner;
- a **non-zero** executed-test count for every `linux-platform` filter step
  (zero matches fail that step by construction);
- any Linux-only test failure reported with its verbatim output and root
  cause — never skipped or weakened; a real Linux defect is a finding for a
  separate issue.

A green smoke proves the canonical gate surface (clippy/fmt/nextest
workspace/doc tests/portable-kernel boundary) on real Linux aarch64, and the
#1877 renameat2 / anchored-rename / receipt / sticky-cutover legs and the
seccomp setsid-EPERM probe running on their home platform.

Still NOT delivered by this lane (named gaps, never faked green): the Linux
codex kill-test certification receipt (owner-supervised, #1877), and the
Windows `cfg(not(unix))` identity leg (stays on `windows-latest`).

## Historical alternative: lima VM on the macOS host (not used)

The 2026-08-29 capacity census (#1879 candidate) found no Linux host and
proposed a lima VM on the C1 macOS host, blocked by the frozen disk
watermarks at the time (47 GiB free < 60 GiB). That path is superseded by the
native `atom-dgx-2` host above and is kept only as a record: an x86_64 guest
would run under QEMU emulation, and any VM path must apply the watermarks to
both the guest disk and the host disk backing the image. Do not provision it
without a new owner decision.

## Rollback / decommission

Requires a separate owner decision. Confirm the C2 runner has no active job,
stop and remove its service from the administrative account, then, as `gha`,
remove the registration with a short-lived removal token entered at the
interactive prompt (do not authenticate `gh` on the host):

```bash
cd <runner-root>
./config.sh remove
```

Preserve any required receipts before deleting `<runner-root>`. The workflow
file reverts with its merge commit; nothing else is runner-owned outside
`<runner-root>` and `gha`'s toolchain caches.
