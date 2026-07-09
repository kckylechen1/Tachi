# Release & distribution pipeline

> **Authority:** owner-ratified operating model for how Tachi leaves the private
> source tree and reaches machines. Implements the remaining product surface of
> #728 (CI+brew pipeline), #758 (`ship` → release handoff), and #874 (private
> source + public distribution only).
>
> Related: `ship-verb.md` (PR ship mechanics), `docs/INSTALL.md` (user install),
> `.github/workflows/{build-bottles,update-homebrew-tap,build-native}.yml`.

## 1. Trust split (#874)

| Surface | Visibility | Authority |
|---|---|---|
| `kckylechen1/tachi` (source + issues + PRs) | **Private** maintainer workflow | Product design, capability truth, agent-readable issues |
| `kckylechen1/homebrew-tachi` (Formula, bottles, checksums) | **Public** distribution only | Install/upgrade binary identity — **not** design truth |
| GitHub Release artifacts on either side | As published | Checksums + binary bytes; agents may verify hashes |

**Invariants**

1. The public tap is **inert distribution**: formula, bottle metadata, checksums,
   install docs. No open Issues/Discussions as agent-readable product input.
2. Agents must **not** treat public tap README comments, third-party forks, or
   arbitrary web links as capability or design authority (see untrusted-input law).
3. Capability bodies stay on external packages/worktrees; Tachi owns
   registry/provenance/projection (#868 / #787), not the tap.
4. Promotion is one-way: **private tag/review → signed/published artifact →
   public formula URL+sha256 update**. The tap never becomes the source of
   product requirements.

**Owner actions outside this repo** (cannot be automated from a PR alone):

- Set `kckylechen1/tachi` visibility to private; keep Discussions disabled.
- Keep `homebrew-tachi` public; disable Issues/Discussions on the tap.
- Ensure `HOMEBREW_TAP_GITHUB_TOKEN` is configured for the update workflows.

## 2. Pipeline (#728)

```
private main (reviewed)
  → tag vX.Y.Z  (Cargo.toml version must match; scripts/check_release_versions.py)
  → GitHub Actions: release-cli-binaries.yml
      · cargo build --release -p memory-server (stamped GIT_SHA)
      · scripts/package_release_binary.sh
          → tachi-vX.Y.Z-aarch64-apple-darwin.tar.gz  (layout: <dir>/tachi)
      · attach assets to private GitHub Release vX.Y.Z
      · scripts/promote_homebrew_binaries.sh
          → public homebrew-tachi release tag tachi-X.Y.Z
          → rewrite Formula/tachi.rb as **binary install** (no Rust for end users)
  → brew upgrade tachi
  → launchd / brew services relaunches the Cellar binary
  → VERIFY THE RUNNING DAEMON (below), never a local cargo target/

Operator fallbacks:
  · scripts/package_release_binary.sh X.Y.Z          — local package
  · scripts/promote_homebrew_binaries.sh X.Y.Z       — promote only
  · update-homebrew-tap.yml (workflow_dispatch)      — re-promote without rebuild
  · build-native.yml                                 — node native module (separate)
```

**Public formula rule:** never point `url` at
`github.com/kckylechen1/tachi/archive/refs/tags/...` (private → anonymous 404).
Default `scripts/update_homebrew_formula.py --mode binary` uses:

`https://github.com/kckylechen1/homebrew-tachi/releases/download/tachi-X.Y.Z/tachi-vX.Y.Z-<triple>.tar.gz`

### Deploy failure class this kills

Hand-`cargo build` + `cp` to `~/bin` while launchd still serves
`/opt/homebrew/opt/tachi/bin/tachi` produced **build source ≠ run source**.
The gate must inspect the **serving** binary's stamped identity.

### Running-binary verification gate

Every build stamps `GIT_SHA` + `BUILD_TIME` (`build.rs` → `build_info`).

1. After upgrade, call `tachi status` (or MCP `tachi_status`).
2. Read `runtime.build.git_sha` / `runtime.build.build_id` — this is **this
   process**.
3. If a daemon is authoritative (`runtime.daemon.running` and not
   `matches_current_process`), query the daemon's `/health` (or
   `tachi daemon status`) for **its** `git_sha` and compare to the intended
   release tag's commit.
4. Never claim deploy success from a freshly built `target/release/tachi` that
   is not the path launchd/`brew --prefix tachi` executes.

`runtime.binary` shows the path of the current process so PATH vs Cellar skew
is visible (also covered by setup item `cli_binary`).

### Market-hours / resident protection

Daemon kill/replace during A-share hours is blocked by the trading-hours guard
(`trading_hours_kill_guard_enabled`). Prefer scheduling `brew upgrade tachi` /
service restarts **outside** Mon–Fri 09:00–15:30 Asia/Shanghai on machines that
host trading-coupled daemons. The distribution pipeline does not auto-restart
through that window; operators choose timing.

## 3. Ship → release handoff (#758)

| Layer | Tool | Owns |
|---|---|---|
| PR ship | `tachi_gh` ship / contract mode | Commit selected files, open PR, never merge |
| Release orchestration | human/owner + CI on tag | Version bump, changelog, tag, artifact publish |
| Distribution | Homebrew tap workflows | Formula + bottles for end machines |

`tachi ship` (gh facade) is **not** a package manager and does **not** push
formula updates. After a release PR merges and a `v*` tag is cut, CI owns the
brew promotion path above. Agents that need "is the daemon the new build?" use
the running-binary gate, not a second hand-roll of `cp`.

## 4. Residual security notes (cross-links)

- Vault sync offline-guessing residual: `vault_sync.rs` module banner +
  `--entries-only` / `--allow-cloud` (#576). Explicit residual acceptance until
  recipient-key encryption lands.
- Audit track #586: CRITICAL/HIGH closed; #547 (typed errors) remains
  opportunistic/postponed, not a release blocker.

## 5. Acceptance mapping

| Issue | Delivered in-repo |
|---|---|
| #728 | Workflows + GIT_SHA stamp + status `runtime.build` gate + **public binary formula path** + this doc |
| #758 | Explicit ship vs release vs brew layering (this doc + ship-verb.md) |
| #874 | Trust split + **public binary promotion** (`promote_homebrew_binaries.sh` / update-homebrew-tap) |
| #576 | Residual risk documented and help-tested; entries-only mode shipped |
| #586 | CRITICAL/HIGH closed; residual #547 noted as postponed |

### Residual (honest)

- Public binaries are **macOS arm64** first; Intel macOS needs a matching asset.
- `build-bottles.yml` still assumes a source-building formula for bottle rebuilds;
  binary formula is the default public path. Bottle workflow is secondary.
