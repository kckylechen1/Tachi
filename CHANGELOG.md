# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Quick Navigation

- [Unreleased](#unreleased)
- [1.6.1](#161---2026-06-29) — Homebrew daemon startup and tap automation
- [1.6.0](#160---2026-06-29) — continuity memory, lifecycle routing, and runtime hardening
- [1.5.x](#156---2026-06-15) — repair and tidy cleanup UX
- [1.4.x](#140---2026-06-01) — Plan C project DB, SFT factory, and facade contracts
- [1.3.x](#130---2026-05-30) — search quality, memory lifecycle, and audit hardening
- [1.2.0](#120---2026-05-25) — shell orchestration, dispatch v2, daily distill
- [1.1.0](#110---2026-05-04) — truth-maintenance security fixes
- [1.0.1](#101---2026-05-02) — default profile changed to `standard`
- [1.0.0](#100---2026-05-01) — Tool Surface v2 and facade tools
- [0.16.x](#0164---2026-04-28) — memory governance, rescue, and coherent distill
- [0.15.x](#0150---2026-04-06) — retention policy and domain-aware routing
- [0.14.0](#0140---2026-04-03) — agent install guide and security hardening
- [0.13.0](#0130---2026-04-03) — Neural Foundry V1 and capability recommendations
- [0.12.0](#0120---2026-03-27) — Vault hardening and Kanban GC
- [0.11.0](#0110---2026-03-26) — multi-agent orchestration layer
- [0.10.0](#0100---2026-03-26) — Virtual Capabilities and governance
- [0.9.0](#090---2026-03-25) — Agent Kanban communication board
- [0.8.0](#080---2026-03-24) — memory infrastructure and MCP proxy hardening
- [0.7.0](#070---2026-03-23) — MCP Client Proxy Phase 2
- [0.6.0](#060---2026-03-23) — project renamed to Tachi
- [0.5.0](#050---2026-03-23) — Dual-DB architecture (global + project)
- [0.4.0](#040---2026-03-18) — native Rust MCP server
- [0.3.0](#030---2026-03-14) — `hard_state` and derived-item isolation
- [0.2.0](#020---2026-03-08) — causal worker pipeline
- [0.1.0](#010---2026-03-05) — initial Sigil release

## [Unreleased]

> **Note to maintainers**: Add unreleased changes here during development. Before cutting a release, move the content under a new `## [X.Y.Z] - YYYY-MM-DD` header and update the Quick Navigation above.

### Changed

- `tachi_gh(action="ship")` now provides a deterministic ship primitive that stages an exact file list, commits caller-authored messages verbatim, refuses protected branches and unchanged/missing files, pushes feature branches when `origin` exists, and optionally opens/links a PR without drafting text.
- `tachi_skill(action="run")`, native `run_skill`, direct skill tools, and skill DLQ retry now return a JSON simulation envelope instead of bare skill text for LLM/mock skill runs. Simulated outputs are prefixed with an explicit marker warning that no commands, files, or tests were executed; `chain_skills` preserves raw step-to-step piping while adding chain-level simulated/warning provenance. This is a breaking response-shape change for consumers that parsed raw text.
- Built-in Superpowers and Waza workflow skills now run in document mode, returning the workflow document for the caller to follow with their own tools instead of asking the LLM to fabricate execution results.
- Review-stage dispatch profiles default `auto_capability_bundle` off unless the caller explicitly passes `auto_capability_bundle=true`.
- Fold `get_memory`, `tachi_board`, and `tachi_dispatch` out of daily agent profiles in favor of `tachi_memory(action="get")` and `tachi_task(action="board"|"dispatch")`, while keeping the native routes admin/backcompat-only.
- Retire the old `tachi_progress_check` and `wiki_browse` observe aliases; agents should use `tachi_unstick` and `tachi_browse`.

### Fixed

- `tachi_gh safe_merge` now records already-merged pull requests as `merge_state="merged"` instead of overwriting lifecycle state with a blocked merge attempt.

## [1.6.1] - 2026-06-29 — Homebrew daemon startup and tap automation

Patch release for making the Homebrew install path safer on macOS launchd and restoring tag-driven tap updates.

### Changed

- `serve --daemon --no-project-db` now defers manifest startup hygiene and scopes daemon background jobs to its own DBs so the Homebrew service can bind to the global daemon without synchronously scanning project entries under protected folders such as Desktop.
- The Homebrew tap update workflow now runs automatically on `v*` tag pushes while retaining manual dispatch for patch retries.
- Release-facing Cargo, npm, installer, README, OpenClaw, and current-state version fields are aligned to `1.6.1`.

### Fixed

- Homebrew formula generation now emits the modern single-binary install path, current `tachi hub` smoke tests, and a `brew services` daemon block for the global no-project service.

## [1.6.0] - 2026-06-29 — Continuity memory, lifecycle routing, and runtime hardening

Major release for turning Tachi from a memory + workflow backend into a project-cycle continuity layer. This line adds typed continuity events, project lifecycle read models, issue/PR/doc closure flows, recall tuning loops, skill-source governance, stronger project DB routing, and a cleaner OpenClaw MCP-only adapter. It also makes the 1.6 release boundary explicit: Cargo, npm, docs, installer URLs, and OpenClaw plugin metadata are now checked together.

### Added

- **Continuity event substrate**: typed continuity events, storage, metrics, projection, context building, outcome labeling, and optional distill/labeler pipeline hooks.
- **Continuity projections**: session captures can project into pattern/timeline memory entries with maturity metadata, active-pattern context, and read-only status exposure.
- **Pattern memory feedback loop**: `tachi_search scope="patterns"` records `seen` exposure through continuity events, and `tachi_memory action="pattern_feedback"` records reviewed `hit` / `miss` / `stale` signals without auto-promoting patterns into skills.
- **Project-cycle memory spine**: new architecture docs and examples for moving from session events to reusable patterns, bonding memory, and issue/doc/wiki closure.
- **Lifecycle task actions**: `tachi_task` now covers `doc_index`, `cycle_status`, `cycle_plan`, `pr_status`, `release_note`, `build_references`, and `close_loop` so issues, PRs, docs/specs, memory, wiki, verification, and release notes can be treated as one project loop.
- **GitHub lifecycle wiring**: issue intake, PR linking/status, review digest routing, linked issue checks, and close-loop comments are wired into the task lifecycle surfaces.
- **Agent profile projection**: Tachi can render host-facing agent/profile guidance, including continuity context and OpenClaw-oriented `AGENTS.md` / `SOUL.md` / `IDENTITY.md` / `USER.md` / `TOOLS.md` style surfaces.
- **Recall tuning loop**: recall simulations can compare config variants, replay rerank policy, generate reviewed scoring proposals, and keep default behavior stable until proposals are accepted.
- **Configurable recall scoring**: recall weights and text/graph precision helpers moved behind explicit config modules instead of hard-coded scorer behavior.
- **Skill-source governance**: built-in Superpowers and Waza skill manifests now carry source metadata; `skill-surface sources` and reviewed sync planning expose pinned upstream status and drift before sync.
- **Built-in workflow skills**: Superpowers and Waza gates are embedded as first-class skill capabilities for planning, execution, review, verification, release, audit, research, and writing workflows.
- **Poke smoke suites**: `tachi poke` adds isolated local product probes for memory, dispatch, skill, shell, and verification paths.
- **Vault onboarding**: setup can funnel provider keys into the encrypted vault, and the TypeScript onboarding UI supports masked/skippable API-key entry.
- **macOS daemon service installer**: the shell installer can install/restart a user LaunchAgent for the global Tachi daemon, with idle shutdown disabled and logs under `~/.tachi/logs`.
- **Release version gate**: `scripts/check_release_versions.py` verifies that Cargo package versions, Cargo.lock entries, npm package files, package-lock files, installer URLs, OpenClaw metadata, and `docs/current-state.agent.yaml` agree with the `memory-server` version.
- CI now runs the release version sync check before clippy.
- `TACHI_DISABLE_STDIO_PROXY=1` forces a stdio MCP process to serve locally instead of forwarding to an already-running compatible daemon.
- OpenClaw plugin metadata now declares its expected tool contract (`memory_search`, `memory_get`, `memory_save`, `memory_graph`, `memory_runtime_info`, `todo_write`, `todo_read`, `todo_spawn_summary`).

### Changed

- `memory-core`, `memory-node`, `memory-server`, `memory-server-params`, the OpenClaw plugin, and npm package metadata are aligned to `1.6.0`.
- Installer URLs across README, install docs, scripts, and OpenClaw docs now point at the `v1.6.0` release tag.
- The Homebrew formula updater now emits a daemon `service do` block so tap releases can support `brew services restart tachi`; the shell installer keeps a LaunchAgent fallback for older/private taps without a trusted service definition.
- **Project DB routing is repo-local first**: source-tree MCP launches, daemon facades, and embedded adapters now prefer the active project store instead of relying on Plan C symlink behavior.
- **No-project/global serve isolation**: `--no-project-db serve` runs from a neutral runtime directory and skips project `.env` loading so global/OpenClaw/desktop launches do not inherit unrelated project paths.
- **Stdio proxy self-healing**: stdio proxy forwarding detects dead or stale daemons, checks version/project-scope compatibility, and avoids forwarding writes across project DB boundaries.
- **Status and health are product-facing**: `tachi status` reports real health scores, clearer deductions, compact default surfaces, provider probe context, namespace drift, continuity metrics, and self-explanatory remediation hints.
- Missing daemon state remains visible in runtime/status output, but no longer lowers health by itself; health scoring now penalizes concrete consequences such as stale distill, provider probe failures, vector gaps, or exhausted Foundry jobs.
- **Foundry failure handling**: failed jobs retry with backoff, terminal failures become visible, stale failure markers can be repaired, and health scoring only penalizes exhausted failures.
- **Daily/background work covers named projects**: distill and WAL checkpoint routines now cover named/project DBs instead of only the currently bound store.
- **OpenClaw stays a thin facade**: the plugin owns hook timing, tool exposure, and MCP calls; embedding, rerank, distill, graph maintenance, Foundry work, and database writes stay in Tachi.
- **Generic routing is domain-agnostic**: finance/A-share scoring logic was lifted out of shared generic recall paths and kept behind configuration rather than hard-coded into the base product.
- **Tool/router maintainability**: the large MCP tool router, facade params, memory CRUD, migrations, search, wiki, vault, arena, verify, status, setup, tidy, and OpenClaw/bootstrap surfaces were split into focused modules.
- **Test maintainability and speed**: monolithic test files were split by behavior, avoidable sleeps were removed, vault crypto tests were bounded, and fast Tachikoma verification scripts were added.
- **Node/CLI lifecycle**: `tachi-cli` derives version from package metadata, delegates daemon lifecycle to the real `tachi` binary, and avoids leaking i18n keys in UI output.
- README and install docs now document the difference between `TACHI_DISABLE_STDIO_PROXY` and `TACHI_DISABLE_AUTO_DAEMON` for MCP host debugging.
- OpenClaw plugin configuration is kept MCP-only; model work, embedding, rerank, distill, graph maintenance, and Foundry lifecycle stay owned by the Tachi runtime.

### Fixed

- Plan C project DB split-brain detection, repair, and repo-local addressing.
- Project-scoped MCP routing for agent hosts after global/no-project serve isolation.
- Stale daemon/process reuse, cache-hit stalls, and embedded MCP adapter scope drift.
- Namespace hygiene warnings that were too noisy or hid real drift.
- Recall scoping, exact-id/path slug recall, partial-term FTS coverage, rerank replay fidelity, and dogfood recall probe anchoring.
- Point-in-time memory recovery: contradicted or auto-linked superseded facts now close `valid_until`, preserving historical `as_of` recall.
- `kind="wiki"` save facade consistency so wiki saves work through the memory facade.
- Briefing quality issues: stale arena cards are kept out, real health score is shown, and reads stay on the daemon authority.
- OpenClaw `<think>` capture leakage and MCP runtime identity/DB routing checks.
- WAL growth under daemon use through periodic checkpointing across global and named project DBs.
- Dispatch classification for research/exploration work, route recommendation penalty bounds, stale non-terminal kanban cards, and closure debt actions.
- Credential/vault drift handling, provider key materialization, API-key pool health, and setup flow edge cases.
- Strict clippy failures around hybrid-search ranking argument count and repair-report type complexity.
- Removed a misleading `init_schema_with_label` API that initialized schema but never ran data migrations.

### Removed

- Nine legacy tool aliases (`cyberbrain_*`, `section9_*`, `shell_*`, `tachi_plan`) that duplicated current facade surfaces.
- Disabled durable recall-cache jobs no longer enqueue background work.
- Legacy OpenClaw shadow-store, benchmark, backfill, and ad hoc test scripts that were no longer part of the MCP-only runtime package surface.
- Hardcoded local `memory-node` migration/import helper scripts that were not part of the release path.

## [1.5.6] - 2026-06-15 — Repair and tidy cleanup UX

Patch release for stale project DB symlink cleanup and Foundry repair follow-through.

### Changed

- `tachi tidy` now includes `memory.db` symlinks even when their targets are missing, reports the target path, and recommends `remove_broken_symlink`.
- `tachi tidy --apply` now removes broken `memory.db` symlinks and cleans up the empty parent directory when possible.
- `memory-core`, `memory-node`, `memory-server`, and npm package metadata versions are aligned to `1.5.6`.
- Installer URLs across README and install docs now point at the `v1.5.6` release tag.

## [1.5.5] - 2026-06-15 — Harness and skill-surface diagnostics

Patch release for keeping multi-agent host guidance and local skill projections auditable from the Tachi CLI.

### Added

- **Harness inventory**: `tachi harness status` scans Codex, Claude, Gemini, Antigravity, and Cursor instruction surfaces for managed Tachi guidance, legacy blocks, duplicates, and stale host-specific hardcodes.
- **Skill surface inventory**: `tachi skill-surface status` compares CC Switch, Tachi, agent, and host skill stores; reports broken symlinks, missing `SKILL.md` files, same-name content drift, and CC Switch projection status.

### Changed

- `memory-core`, `memory-node`, `memory-server`, and npm package metadata versions are aligned to `1.5.5`.
- Installer URLs across README and install docs now point at the `v1.5.5` release tag.

## [1.5.4] - 2026-06-15 — 🔒 Issue-driven automation gates and runtime hardening

Patch release for GitHub issue-driven agent handoffs, runtime boundary hardening, and bounded read concurrency.

### Added

- **Issue automation plans**: `tachi_task(action="intake")` now records `automation_plan` metadata, including dispatch readiness, leader-gate reasons, suggested branch, PR title, and PR body contract.
- **Leader-gated dispatch**: issue flows missing acceptance criteria or touching high-risk boundaries now block `tachi_task(action="dispatch")` unless a leader explicitly confirms with `confirm=true`.
- **PR handoff artifacts**: `tachi_task(action="pr_handoff")` writes branch/title/body handoff material with linked issue, completed dispatches, verification evidence, and known gaps.
- **Bounded read-only store pools**: daemon-bound global/project read paths now use a configurable read-only `MemoryStore` pool via `TACHI_MEMORY_READ_POOL_SIZE` (default 4, capped at 32).

### Changed

- `memory-core`, `memory-node`, `memory-server`, OpenClaw plugin, and npm package metadata versions are aligned to `1.5.4`.
- Installer URLs across READMEs, `docs/INSTALL.md`, and OpenClaw docs now point at the `v1.5.4` release tag.
- Agent workflow docs now treat GitHub issues/PRs as authoritative handoff records for spec/document-driven work.
- Production `tool_params` imports were narrowed to explicit schema/serde imports instead of broad `use super::*`.

### Fixed

- Cached read stores no longer serialize all daemon read closures behind one mutex.
- Named/path read helper stores now open through the read-only store API.
- Issue-based worker dispatch no longer silently proceeds from underspecified or high-risk issue text.

### Security & idempotency hardening

Post–Gemini review hardening landed after v1.5.3 (PRs #359–#376 security batch, #377 idempotency follow-up).

#### Added

- **`tachi env sync --apply`**: project env materialization is preview-only by default. Pass `--apply` to write `.tachi/env.generated`; `--dry-run` remains an explicit preview alias.
- **Foundry queue dedupe**: `queue_agent_evolution` derives a deterministic job id from input fingerprint; active or terminal jobs return `deduped` instead of spawning duplicate synthesis. Stale `running` jobs (>30 minutes) can be reclaimed.
- **Global dispatch dedupe**: bare `tachi_dispatch` calls without `flow_id` dedupe via `~/.tachi/runs/.dispatch-dedupe/` fingerprint locks.
- **Dispatch crash recovery**: daemon startup runs `recover_orphaned_dispatch_runs()` for orphaned in-flight dispatches.

#### Changed

- **DLQ safety**: failed tool calls enqueue to DLQ only when replay is considered safe. `dlq_retry` rejects native tools and non-idempotent mutating replays (`hub_call`, write tools, mutating facade actions).
- **MCP remote URL validation**: expand-then-validate hostnames, block private/link-local targets (including IPv4-mapped IPv6), and resolve DNS asynchronously before SSE MCP connections.
- **Secret file creation**: MCP config, Claude pool state, and bootstrap env exports use atomic `0o600` file creation where supported.
- **`tachi env sync` CLI default**: no longer writes unless `--apply` is passed (breaking change for scripts that relied on implicit writes).

#### Fixed

- **MCP SSRF**: remote MCP URLs are validated after hostname expansion and DNS resolution, not only on the literal input string.
- **Interpreter bypass**: MCP launcher commands must resolve to approved interpreter basenames.
- **Subprocess allowlist injection**: Codex/Grok/Kimi dispatch profiles reject caller-supplied `allowlist` overrides.
- **TOCTOU on config writes**: sensitive runtime config files are written via create-new + rename instead of truncate-in-place.

## [1.5.3] - 2026-06-08 — 🔒 Verification-ledger merge gates

Patch release for verification-ledger merge gates and background-check workflow integration.

### Added

- `tachi_verify` records background verification ledgers under `.tachi/runs/<flow_id>/verification.json`.
- Briefing output now surfaces recent verification gate state so leaders can see what background checks already proved.
- Standard, coordinate, and focused agent profiles expose `tachi_verify` for worker-driven verification reporting.

### Changed

- `memory-core`, `memory-node`, `memory-server`, and `@chaoxlabs/tachi-node` versions are aligned to `1.5.3`.
- Installer URLs across READMEs and `docs/INSTALL.md` now point at the `v1.5.3` release tag.
- `tachi_gh(action="safe_merge")` can consume required same-head Tachi verification evidence when a `flow_id` is supplied.

### Fixed

- Missing, mismatched, or skipped required verification items without matching `head_sha` are treated as stale instead of green for merge gates.
- Recent verification summaries cap metadata-sorted candidate scans before parsing ledger JSON.

## [1.5.2] - 2026-06-08 — 🔒 Safe merge gate correctness

Patch release for safe merge gate correctness and agent-facing merge workflow clarity.

### Changed

- `memory-core`, `memory-node`, `memory-server`, and `@chaoxlabs/tachi-node` versions are aligned to `1.5.2`.
- Installer URLs across READMEs and `docs/INSTALL.md` now point at the `v1.5.2` release tag.
- `tachi_gh(action="safe_merge")` now reports requested merge mode separately from actual merge execution so dry-run previews cannot be mistaken for completed merges.
- Agent-facing tool descriptions now separate GitHub PR safe merge from local dispatched worktree merges.

### Fixed

- PRs with no GitHub check runs now surface structured `checks:none` gate state instead of failing the whole safe-merge call.
- Safe merge no longer reports head consistency as proven when GitHub does not expose independent check/review head SHAs.
- Strict safe-merge policy accepts either an explicit Tachi `flow_id` or GitHub closing issue reference, rather than requiring both.

## [1.5.1] - 2026-06-08 — 🛡️ Post-1.5.0 MCP and agent hardening

Patch release for post-1.5.0 MCP and agent workflow hardening.

### Added

- `tachi_memory(action="get")` on the unified memory facade, so standard-profile agents can fetch a full memory entry without exposing raw lower-level tools.
- Vault provider key pools that materialize rotation members under logical env names, rotate concrete keys in process, cool down keys on HTTP 429, and surface provider key metadata in status output.
- `tachi vault sync-export`, `sync-import`, and `sync-status` for encrypted Vault ciphertext bundles that can live in iCloud Drive without moving live SQLite databases into cloud sync.
- Agent credential-surface documentation for future Codex, Claude Code, OpenCode, Hermes, OpenClaw, and Gemini materializers.

### Changed

- `memory-core`, `memory-node`, `memory-server`, and `@chaoxlabs/tachi-node` versions are aligned to `1.5.1`.
- Installer URLs across READMEs and `docs/INSTALL.md` now point at the `v1.5.1` release tag.
- Tachi status now reports broader provider-key coverage, including search, OpenAI-compatible, Anthropic-compatible, Google/Gemini, DeepSeek, Zhipu/BigModel, Tavily, and Exa lanes.

### Fixed

- Precise memory recall for exact ids, path slugs, hyphenated technical terms, short technical tokens, and deep scoped superseded rows.
- Run-ledger board ordering for mixed dispatch id formats such as timestamp-leading ids and `flow_YYYYMMDDTHHMMSSZ_...` ids.
- Async board handling avoids running synchronous filesystem scans on the Tokio reactor.
- Vault keychain loading no longer collapses standalone `*_2` style secret names unless a rotation prefix is explicitly configured.
- Vault env materialization uses already decrypted pool entries instead of triggering DB-writing reads while preparing child env.
- Vault sync bundle export creates temporary bundle files with private `0600` permissions before writing bytes.

## [1.5.0] - 2026-06-07 — 🚀 Agent engineering control plane

Release focused on turning Tachi into a more complete agent engineering surface: native multi-agent coordination, auditable worker runs, skill policy, cleanup utilities, runtime observability, and release-ready package alignment.

### Added

- `tachi_arena` / arena-backed worker tracking for auditable external-agent runs, status board reads, and recovery-oriented delegation.
- Native skill policy and embedded Superpowers/Waza SOP routing so shell stages, PR review, issue handling, docs, release, and audit workflows can invoke approved skills instead of relying on prompt memory.
- Agent eval harness and documentation for measuring multi-agent work, including task memory hygiene and SFT-safe capture boundaries.
- Dispatch profile and prompt-envelope support for four-agent fleet routing, backend model tiers, Kimi output handling, and mixed-case intent routing.
- `tachi clean` in the main CLI, reusing `tachi-clean` for safe target cleanup, marked worktree cleanup, stale temp sweeps, and Tachi self-maintenance cleanup. Cleanup is dry-run by default and destructive only with `--force`.
- Runtime observability for daemon/stdio authority, sidecar health, provider-key drift hints, and hidden/visible tool readiness.
- Wiki write references, orchestrator TODO/handoff state, and issue-document-memory workflow closure primitives.

### Changed

- `memory-core`, `memory-node`, `memory-server`, `@chaoxlabs/tachi-node`, OpenClaw plugin, and TypeScript CLI package versions are aligned to `1.5.0`.
- Installer URLs across READMEs and `docs/INSTALL.md` now point at the `v1.5.0` release tag.
- SFT/training data is kept out of live recall by default, and low-signal memory is less likely to crowd normal recall.
- Full CI is no longer run automatically for every small PR while GitHub Actions minutes are constrained.

### Fixed

- Vector sweep startup, schema v9 batch migration, and Foundry shadow-write behavior.
- Default wiki recall cache pollution and Gemini follow-up issues around wiki recall, runtime, and routing.
- Agent recovery and skill lookup dead ends, mixed-case task intent routing, and Kimi dispatch output formatting.
- Vault/provider-key drift handling, stale provider key surfacing without live probes, and daemon health readiness.

### Removed

- Legacy `memory-python` PyO3 crate and dead/zombie code paths that were no longer part of the active Tachi architecture.

## [1.4.3] - 2026-06-02 — ✨ MCP UX polish

Polish release plus MCP UX fixes from hands-on testing: briefing compact mode, kanban metadata, wiki multi-store reads, ask cross-store hints, status warnings with DB names, Voyage rerank empty-document guard, and install URL pinning.

### Added

- `tachi_memory` briefing `compact=true` caps memories/wiki/kanban/checkpoints and skips health/wiki-hygiene for cheaper session starts.
- `SearchMemoryParams.include_metadata` (default `false`; board enables it) so kanban `a2a_state` surfaces in briefings.
- Ask responses expose `cross_store` / `cross_store_hint` when evidence spans global and project stores without a pinned `project`.

### Fixed

- Kanban tasks no longer render `[unknown]` in briefings when `a2a_state` is stored in metadata.
- Checkpoint titles in briefings truncate to 140 chars (same helper as section rows).
- `tachi_wiki` read/list merges named, workspace project, and global wiki stores (with dedup and `limit` enforcement).
- Status warnings and readiness output name affected DBs and hidden required tools.
- Voyage rerank filters empty/whitespace documents and maps indices back to the original evidence rows.

### Changed

- `crates/memory-{core,node,python,server}/Cargo.toml` bumped to `1.4.3`.
- Installer URLs across all READMEs and `docs/INSTALL.md` now point at `https://raw.githubusercontent.com/kckylechen1/tachi/v1.4.2/...` (the latest released tag) rather than `main`.

### Style

- `rustfmt` normalization on `bootstrap/tidy.rs`, `docs_ops.rs`, `facade_save_ops.rs`, `utils.rs`, and `tests/docs_tests.rs`. No semantic changes; 497 tests still pass, `cargo clippy -- -D warnings` clean.

## [1.4.2] - 2026-06-01 — 📝 `tachi_memory` format contract

Final format-contract patch for the `tachi_memory` facade.

### Added

- `tachi_memory` now accepts `format="json"` (alias: `output_format`) for stable, minified JSON responses on `search`, `save`, `extract_facts`, `checkpoint`, `alerts`, `ask`, `consolidate`, `progress`, `readiness`, and `briefing`.

### Changed

- Default `tachi_memory` responses remain compact Markdown for human/LLM consumption; programmatic clients should request `format="json"` when they need field-level contracts.

### Fixed

- `tachi watcher status --json` now reads the passive watcher status directly instead of parsing Markdown briefing output.

## [1.4.1] - 2026-06-01 — 🔧 Plan C, daemon safety, and GC hardening

Patch release: post-1.4.0 hardening for Plan C, daemon safety, GC, and Linux release builds.

### Fixed

- Reject `db_relpath` path traversal (`..`) and paths that escape `project_root`.
- Unify Plan C symlink directory naming (`sanitize_safe_path_name`) across init, serve, and named-project resolution; honor `TACHI_HOME` / `SIGIL_HOME` / `TACHI_APP_HOME`.
- Hot-activated project DB (`tachi_init_project_db`) overrides the boot-time project store; `project_db_path_buf` reports the active path.
- CLI daemon forward requires matching `version` in `daemon.pid` (avoids stale daemon after upgrade).
- `sqlite_vec` auto-extension uses `c_char` for `aarch64-unknown-linux-gnu` release builds.
- GC reconciles `query_diversity` after `access_history` pruning; Jaccard dedup propagates SQL errors and folds `persons` into FTS entities.
- `tachi-cli` UI/MCP client version strings aligned to package version.

### Changed

- `tachi_memory` facade actions (`save`, `checkpoint`, `ask`, `consolidate`, `readiness`, `progress`, `extract_facts`) return Markdown for human/LLM reading; `tachi_save` / `save_memory` / `remember` still return JSON. Programmatic clients must not assume JSON on those facade actions.

## [1.4.0] - 2026-06-01 — 🏗️ Plan C project DB and SFT factory

Plan C project DB, memory lifecycle / SFT factory, and facade module split.

### Added

- **Plan C**: local project `memory.db` with `~/.tachi/projects/<name>/memory.db` symlinks and hot `tachi_init_project_db` activation.
- **SFT factory** and **REM wiki evolver** foundry pipelines; parallel `briefing` memory + wiki search.
- **Memory lifecycle**: `recall_count`, `query_diversity`, tier promotion (`raw` → `consolidated`); metadata `tier` on save.
- **`build.sh`**: release build, install to `bin/memory-server`, refresh `~/bin/tachi` symlink (macOS ad-hoc sign).

### Changed

- Split `facade_memory_ops` and `memory_search_ops` into submodules.
- Merge `main`: drop legacy `persons` column / `clawdoctor`; `MEMORY_SELECT_COLUMNS` uses `'[]' AS persons`.
- MCP metadata forwarding on `tachi_save` / `tachi_memory`.

### Fixed

- #137: `path_utils` dedup, entity pollution in `capture_session`, word-boundary agent matching.
- Post-merge upsert SQL and SFT/wiki SELECT without dropped `persons` column; CI clippy (`-D warnings`).
- Jaccard dedup refreshes candidate `memories_fts` row; `record_access` increments `query_diversity` incrementally.
- Plan C symlink uses sanitized project dir name; non-Unix hosts get an explicit note in the init response.
- Align `crates/memory-node/package.json` to 1.4.0 for release version checks.

## [1.3.1] - 2026-05-31 — 🔧 #137 follow-up fixes

Patch release for #137 follow-up fixes after code review.

### Fixed

- **`matches_agent_tag`**: hyphenated agent ids such as `jayne-main` match again; `user-memory` slugs avoid substring false positives like `my-user-memory-analyzer`.
- **`path_utils::tachi_home`**: restore legacy `TACHI_APP_HOME` fallback for SFT output paths and other foundry artifacts.
- **`visit_jsonl_files`**: follow directory symlinks with cycle detection so passive Claude JSONL discovery works through linked roots.

### Changed

- Ignore local `bin/` build outputs in git.

## [1.3.0] - 2026-05-30 — 🔍 Search quality and memory lifecycle hardening

Twenty commits since v1.2.0. Major themes: search quality improvements, memory lifecycle hardening, MCP numeric coercion fixes, wiki read/browse refactor, docs organizer, and comprehensive audit fixes.

### Added

- **`tachi_wiki(action="read")`** (PR #131): new action to retrieve full wiki entry text by path, returning Markdown with metadata.
- **`tachi_wiki(action="browse")`** (PR #131): now returns Markdown instead of JSON for consistency with other read operations.
- **`tachi distill run --db PATH`**: manual one-shot daily batch distill against any project DB.
- **`FOUNDRY_DISTILL_BACKEND`**: choose `raw_api` (default) or `claude_cli` for daily batch distill.
- **`FOUNDRY_DISTILL_BATCH_SIZE`**: tune groups per LLM call (default 6).
- **`tachi_memory` actions**: `briefing`, `checkpoint`, `alerts`, `ask`, `consolidate`, `progress`, `readiness`.
- **`tachi watcher`**: passive Claude JSONL transcript discovery and capture helpers.
- **Status diagnostics**: health score, API key drift detection, provider probes, distill marker staleness.
- **Vector-weighted RRF scoring** (PR #120): cosine similarity is blended into the final RRF score via a configurable weight, reducing rank inversions on highly semantic queries.
- **Confidence reinforcement** (PR #121): memories that pass the similarity threshold but fall short of supersession now have their confidence score incremented, extending the useful lifetime of soft-corroborated facts without triggering a supersession write.
- **Automatic contradiction detection** (PR #122): `apply_auto_contradiction_detection` identifies semantically conflicting memories via entity overlap, numeric mismatch, and vector similarity, verifies candidates with an LLM, and persists typed contradiction edges.
- **Bitemporal memory validity** (PR #123): each `MemoryEntry` carries `valid_from` / `valid_until` fields; `search_memory` accepts an `as_of` parameter for point-in-time retrieval; a startup migration normalises existing rows automatically.
- **Query expansion for FTS** (PR #124): `search_fts_with_expansion` generates synonym, acronym, and phrase variants for every FTS query, raising recall on sparse corpora without touching vector search.
- **Spreading activation with seed weights** (PR #125): graph search propagates activation from per-seed initial weights rather than a uniform floor; within-hop accumulation uses noisy-OR to prevent cluster over-activation.
- **Memory insight inference** (PR #126): `infer_memory_insight` scores memories against a surprise composite (importance delta, rarity, contradiction count) and emits structured `memory_insight` signals consumed by the neighbourhood job.
- **Synthesis thinking scaffold** (PR #127): `build_thinking_scaffold` composes a layered evidence brief for synthesis queries, ranking sources by relevance score with gap-analysis annotations.

### Changed

- Daily batch distill defaults to SiliconFlow raw API via the `DISTILL_*` lane; Claude CLI remains opt-in.
- API batches split and retry on JSON parse failure before per-group fallback.
- Manifest/doctor skip archival and backup DB paths to reduce noisy status/backfill hints.
- Secret scrubbing on save/progress; `scrub_secrets` regexes compiled once via `OnceLock`.
- **MemoryServer decomposition** (PRs #114–#119): `VaultState`, `RateLimiter`, `ToolDiscovery`, `AgentRuntime`, `EnrichmentRuntime`, and `FoundryRuntime` each extracted into dedicated sub-modules. `shell_ops.rs` (1,826 lines) and `status_ops.rs` (2,376 lines) split into focused files. `MemoryServer` is now a thin coordinator; no public tool-surface changes.

### Fixed

- **MCP numeric parameter coercion** (PR #132): all `Option<u32>`, `Option<u64>`, and `Option<f64>` MCP parameters now accept both JSON numbers and numeric strings, preventing deserialization failures when clients serialize numbers as strings.
- **P1/P2/P3 audit + Gemini review fixes** (PR #133): ~40 fixes across 19 files including:
  - `register_bucket()` changed from `OnceLock` to `Mutex<Vec>` for thread-safe dynamic registration
  - Dispatch plan review gate returns `Ok` instead of `Err` for successful plan-review pauses
  - `build_batch_user_payload` tracks actual group count after trimming to fit token budget
  - `quality_multiplier` importance≥0.9 floor now excludes `foundry_distill` entries
  - Setup wizard inline comment detection uses `find(' #')` instead of `find('#')` to avoid matching `#` inside values
  - `symbolic_score` Jaccard denominator now uses union of query and text tokens
  - Wiki dedup single-token topic guard requires parent directory overlap
  - Wiki path root guard rejects empty path (root `/`) with clear error message
  - `secure_join` canonicalizes parent directory instead of cursor for non-existent files
  - Removed redundant `root.is_symlink()` check that broke dotfiles manager symlinks
  - Binary split on error now has `max_depth=8` to prevent unbounded recursion
  - Search error propagation logs warnings instead of silently swallowing errors
  - Health score dimension mismatch penalty increased from 5×min(15) to 15×min(30)
  - Added `is_wiki()`, `is_kanban()`, `is_handoff()`, `is_foundry_distill()` methods to `MemoryEntry`
  - `precision_query_multiplier` fast path skips expensive bundle construction for non-precision queries
- `batches_dispatched` metric counts split API distill retries accurately.
- Progress `status.json` updates use file locking; JSONL watcher scan runs off the async executor.
- Config.env discovery uses `dirs::home_dir()`; macOS keychain probe skipped on other platforms.
- **P0/P1/P2 tech debt** (PR #113): 19 new regression and integration tests; silent error paths in enrichment, contradiction, and graph operations hardened.
- **Gemini review rollup** (PR #128): `tokio::spawn` hot-path in enrichment replaced with spawn-blocking; thousands-separator regex corrected; `WHERE` clause and transaction wrapper added to `normalize_memory_validity_columns`; `push_unique` prevents duplicate FTS expansion terms; noisy-OR accumulation corrected in graph spreading activation; `avg_importance` hoisted above topic guard in insight inference; null filter and `evidence_ref` fallback added to synthesis scaffolding.

## [1.2.0] - 2026-05-25 — 🐚 Shell orchestration, dispatch v2, and daily distill

Forty-eight commits since v1.1.1. Major themes: shell orchestration, dispatch v2, daily distill, health/observability, search/wiki hygiene, guide layer, and OpenClaw per-agent routing.

### Added

- **`tachi_shell`**: skill-gated flow orchestration facade with convoy dispatch slices and lifecycle tests.
- **`tachi_status` / `runtime_info` MCP tools**: daemon health, vector coverage, foundry queue warnings, and runtime DB routing self-check.
- **Guide layer write side**: foundry distill artifacts under `/guide/<type>/<agent>/<ts>` with causal edges (`distilled_from`, `causes`, `fixed_by`, `rejected_because`).
- **Dispatch v2**: two-stage plan-execute with full trajectory capture.
- **Setup wizard TUI** (`tachi setup --interactive`) and **`tachi tidy`** for fragmented DB cleanup.
- **Daily batch distill** (Claude pool) as primary scheduler; legacy 30-minute fallback retained.
- **Wiki in-place update + Jaccard dedup**; search hygiene filters (wiki logs, rerank cache, kanban/handoff noise).
- **OpenClaw `Route::Path`**: per-agent foundry jobs route to isolated agent DBs; MCP client verifies `runtime_info` on connect.
- **Vault-aware `backfill-vectors`**; stdio MCP auto-spawns daemon when none detected.

### Fixed

- **`tachi stats --project-db`** now reads the specified project DB instead of silently using global.
- LLM 3-layer consolidation (`extract` / `reasoning` / `embed` lanes); `evolve.rs` fallback uses extract lane.
- Touch/access-stat semantics in `record_access`, auto-link, and vault touch.

### Changed

- **`tachi_memory` facade** unifies search/save; GH merge worktree safety hardened.
- **Release alignment**: Rust crates, `@chaoxlabs/tachi-node`, OpenClaw plugin, and install docs unified on `1.2.0`.

## [1.1.0] - 2026-05-04 — 🛡️ Truth-maintenance security fixes

Eight P0/P1 findings from a four-LLM code review pass on the truth-maintenance-v2 branch are fixed. No public CLI/tool surface changes, but several defaults are now safer.

### ⚠️ BREAKING

- **`tachi_dispatch` defaults to a permissioned sandbox.** Calls without `permission_profile` previously implied `"full"` (Claude `--dangerously-skip-permissions`, Codex `--dangerously-bypass-approvals-and-sandbox`). They now imply `"default"`. Pass `permission_profile: "full"` explicitly to opt back in to unsandboxed dispatches.
- **`tachi repair --apply` no longer runs R8 (`duplicate_old`) by default.** R8's cross-path text-only PARTITION silently deleted legitimate distinct memories that happened to share identical text. Run R8 explicitly with `--rule R8` if you understand the trade-off.

### Fixed

- **R2 retention pins handoff/kanban records.** Coordination paths (`/handoff`, `/kanban`) and categories (`handoff`, `kanban`) are now backfilled to retention `pinned` instead of `ephemeral`, matching `default_retention_for` and the schema migration. Daemon coordination state no longer expires under retention sweeps.
- **Kanban scope fallback.** `get_kanban_state` and `update_kanban_state` now fall back to the global store when the project store has no matching row, mirroring `resolve_write_scope` at write time. Daemon and no-project dispatches no longer get stuck in `TASK_STATE_WORKING` because their kanban entry was written globally and read locally.
- **Auto-link `supersedes`/`related_to` edges stay open.** Edges are created with `valid_to=None` instead of `Some(now)`. The previous behavior caused `get_edges` to immediately filter the edge out as expired. Edges are still closed when the supersession is explicitly reversed.
- **`hub_search` SQL-LIKE wildcard escaping.** Search terms have `\`, `%`, and `_` escaped before being wrapped in `%...%` patterns and all three LIKE clauses now use `ESCAPE '\'`. A query of `%` no longer matches every row in the Hub catalog.
- **R5 (`integrity_check`) reports findings on heavy corruption.** When SQLite's FTS5 vtable constructor itself returns `SQLITE_CORRUPT` during PRAGMA preparation (rather than the PRAGMA returning a corruption message), R5 now records an `integrity_fail` finding so the dispatcher can act on it instead of bubbling an opaque `RepairError`.
- **Diagnostics no longer take write locks.** `probe_db`, kanban listing, eval-ledger collection (status_ops), and the daily pipeline stats collector now open stores read-only. They no longer run schema migrations on databases the caller only intended to read.

### For contributors

- `repair::tests::r5_integrity_detects_corruption` is now deterministic — it pins `page_size=4096`, VACUUMs, and stomps the entirety of page 2 with `0xFF`. Verified 20/20 consecutive passes.

## [1.0.1] - 2026-05-02 — 🔧 Default profile changed to `standard`

### ⚠️ BREAKING

- **Default profile changed from `admin` to `standard`**. When no `TACHI_PROFILE` is set, Tachi now exposes only the 12-tool standard surface instead of the full 148-tool admin surface. To restore the previous behavior, set `TACHI_PROFILE=admin` explicitly in your MCP config or environment.

  | Migration | Before (v1.0.0) | After (v1.0.1) |
  |-----------|-----------------|-----------------|
  | No profile set | `admin` (all tools) | `standard` (12 tools) |
  | `TACHI_PROFILE=admin` | admin | admin (unchanged) |
  | `TACHI_PROFILE=standard` | standard | standard (unchanged) |
  | `TACHI_PROFILE=coordinate` | coordinate | coordinate (unchanged) |

### Added

- **`call_tool` profile enforcement**: tool calls are now validated against the active profile. Hidden tools return `"tool not found"` (identical to truly missing tools) — no information leakage about tool existence.
- **Default profile startup notice**: when no profile is specified, Tachi prints to stderr: `No profile specified; defaulting to 'standard'. Set TACHI_PROFILE=admin to restore legacy full surface (148 tools).`
- **Metadata consistency guard tests**: automated tests verify that every non-admin tool is properly classified into a bundle, every wildcard pattern matches real tools, and every write tool invalidates the read cache.
- `standard_profile_direct_add_edge_call_is_rejected` integration test: confirms that raw admin tools cannot be called when a standard profile is active.

### Changed

- `tachi_complete` reclassified from **Observe** to **Remember** bundle. It writes eval data, so it belongs with write tools.
- `tachi_complete` added to `CACHE_INVALIDATING_TOOLS` to prevent stale read cache after task completion.

### Fixed

- `CACHE_INVALIDATING_TOOLS` now includes `archive_memory`, `sync_memories`, `ghost_publish`, `ghost_whisper`, `ghost_subscribe`, `ghost_listen`, `ghost_ack`, `handoff_leave`, `handoff_check`, `post_card`, `update_card`, and `tachi_complete` — previously these write tools could leave stale data in the read cache.
- Stale comments corrected (said "10 tools" → actually 12).

### Tests

- `cargo test -p memory-server -- profiles::tests` (14 tests)
- `cargo test -p memory-server -- standard_profile_direct_add_edge` (1 integration test)

## [1.0.0] - 2026-05-01 — 🎭 Tool Surface v2 and facade tools

### Added
- **Tool Surface v2**: added compact facade tools (`tachi_search`, `tachi_web_search`, `tachi_save`, `tachi_handoff`, `tachi_plan`, `tachi_unstick`, `tachi_browse`) plus delegate/eval tools (`tachi_dispatch`, `approve_merge`, `tachi_complete`).
- **Agent-facing web search gateway**: `tachi_web_search` routes through `vc:web_search` with Exa/Tavily fallback behavior and schema-aware argument mapping.
- **Capability-scoped Vault URL placeholders**: remote MCP URLs can now use `${vault:SECRET_NAME}` so query-string API keys such as Tavily's can live in Tachi Vault instead of Hub definitions.

### Changed
- **v1 cleanup split**: `memory-core` store methods are split into domain modules, and `memory-server` moved its `#[tool_router]` implementation out of `main.rs` into `tools.rs`.
- **Tool profiles are additive allowlists**: standard/delegate profiles now expose the compact facade and recommendation paths while keeping raw admin surfaces out of normal agent views.
- **OpenClaw JavaScript bridge removed from the Rust repo**: the live integration path is the native Tachi MCP binary plus install-time extension setup.

### Fixed
- **Tavily / `mcp-remote` transport**: remote HTTP MCP servers wrapped by `mcp-remote` now use Tachi's raw Streamable HTTP JSON-RPC path, including optional `Mcp-Session-Id` handling.
- **Malformed skill JSON hard fail**: invalid skill definitions are rejected before persistence, preventing corrupt Hub rows from blocking later startup.
- **Secret redaction**: Hub discovery/get output and sandbox audit output redact API-key/token/password fields and secret-bearing URL query parameters.
- **Foundry/LLM tool robustness**: string message/item params deserialize correctly, reasoning-prefixed JSON is extracted safely, and recoverable LLM failures return structured tool results instead of transport errors.

### Tests
- `cargo test -p memory-core`
- `cargo test -p memory-server`
- `cargo check -p memory-server`

## [0.16.4] - 2026-04-28 — 🔍 FTS maintenance CLI

### Added
- **FTS maintenance CLI**: `tachi backfill-fts [--db PATH] [--dry-run] [--full]` now reports, backfills, or fully rebuilds the `memories_fts` index for local stores.

### Fixed
- **Stale-branch safety patches recovered**: `sync_memories` now reads legacy project-scoped agent known-state before classifying entries as new, and the noise filter catches additional low-signal assistant boilerplate.
- **Release alignment**: Crate/package versions, OpenClaw client version, install docs, GitHub release, and Homebrew tap are aligned on `0.16.4`.

## [0.16.3] - 2026-04-28 — 🛡️ Memory governance suite

### Added
- **Memory governance suite**: doctor v2, manifest v1, manifest-aware write guards/resolvers, save-time capture gate validation, Foundry job lifecycle hardening, and antigravity multi-project rescue tooling.
- **OpenClaw manifest-aware bridge**: OpenClaw now routes agent DBs through `~/.tachi/manifest.json` when available, raises the default auto-capture floor to 200 characters, and tolerates noisy MCP JSON payloads.

### Changed
- **Antigravity rescue flow**: mixed antigravity memory rows can now be split into project DBs with provenance and quarantine-style source backup instead of deletion.

## [0.16.1] - 2026-04-23 — 🔧 Distill scheduler and release chain fixes

### Fixed
- **Distill scheduler/executor drift**: scheduled distill jobs now key on `/<root>#<coherence_key>` instead of only the top-level path segment, and the worker only processes the exact `memory_ids` selected by the scheduler. This closes the gap where a queued `/hapi` job could later re-scan the whole path window and distill a different bucket than the one originally chosen.
- **Homebrew release chain**: bottle builds now install the formula via an explicit local file path (`./tap/Formula/tachi.rb`) instead of a path that Homebrew misparsed as `tap/formula`, and both tap-update workflows now only diff `Formula/tachi.rb` before committing.
- **`tachi-hub` formula packaging**: `scripts/update_homebrew_formula.py` now upgrades the tap formula structure as part of every release, so Homebrew installs and tests both `tachi` and `tachi-hub` instead of leaving the new binary out of the bottle.
- **License metadata alignment**: the repo is AGPLv3, so crate manifests and the `@chaoxlabs/tachi-node` / OpenClaw package metadata now declare `AGPL-3.0-only` instead of stale `MIT` values.

## [0.16.0] - 2026-04-23 — 🧠 Coherent distill and tool surface bundles

### Fixed
- **Coherent foundry distill**: `process_memory_distill_job` now buckets candidate memories by `topic:` / `entity:` before invoking the LLM. Previously the worker passed every record under a `path` prefix to the model, producing "缝合怪" (frankenstein) summaries that mixed unrelated topics. Memories without a topic or entity are skipped; the largest coherent bucket wins, and distilled output is tagged with a `coherence_key` for traceability. (`crates/memory-server/src/foundry_runtime_ops/maintenance.rs`)
- **Hallucinated foundry rows purged**: 47 phantom `topic='foundry_distill'` records under `/foundry/%` were hard-deleted from the antigravity DB (and verified absent from global). FTS, edges, and vector caches were swept in the same migration.
- **Project DB schema drift**: Older project DBs (`tachi`, `sigil`, `openclaw`) were missing the `retention_policy` and `domain` columns added in 0.15.x. `tachi-hub doctor --fix` now patches drift in-place; the standard `memory-server` boot path already auto-migrates.

### Added
- **`tachi-hub` CLI**: New standalone read-only inspector binary shipped from the `memory-server` crate. Subcommands: `list`, `show`, `packs`, `bindings`, `stats`, `doctor [--fix]`. Reads `~/.tachi/global/memory.db` (or `$TACHI_HOME`) without spawning the MCP server. Brew bottle now ships both `memory-server` and `tachi-hub`.
- **Tachi usage addendum (`prompts/tachi_addendum.md`)**: Curated guide that operators can include into agent root prompts (`AGENTS.md` / `CLAUDE.md` / `GEMINI.md`). Covers the three iron rules (search-before-write, structured `save_memory`, skill-first), tool quick-reference table, path conventions, anti-patterns, and the new `tachi-hub` CLI surface. Not auto-injected — operators copy the fenced block manually.
- **`VOYAGE_RERANK_API_KEY` setup hint**: Added as an optional fifth entry in `bootstrap::SETUP_API_KEYS` so `tachi setup` and `install.sh` surface it. The key is currently informational; the rerank wiring is intentionally not yet enabled in the search pipeline.
- **Antigravity DB de-noising**: `scripts/migrate_antigravity_split.py` reclassified 808 cross-cutting records out of the antigravity DB into their owning project DBs (hapi 501, quant 148, openclaw 55, tachi 36, sigil 35, global 22, hyperion 11), and stood up the new `quant` and `hyperion` project DBs. The script doubles as a worked example for the foundry classifier model.
- **`build-bottles.yml` workflow**: GitHub Actions workflow for cross-platform Homebrew bottle builds, triggered on tag push or manual dispatch.

### Changed
- **`integrations/openclaw`**, **all four core crates**, and the brew formula bumped to `0.16.0`.
- **Tool surface bundles**: Replaced mutually exclusive tool profiles with additive surface bundles: `observe`, `remember`, `coordinate`, `operate`, and `admin`.
- **Compatibility default**: Tachi still keeps `admin` as the implicit no-profile default, but host aliases and docs now steer new integrations toward explicit least-privilege bundles.
- **Host alias mapping**: `antigravity` now resolves to the coordination surface, while `openclaw` resolves to the runtime/operator surface. OpenClaw’s embedded client now sets `TACHI_PROFILE=openclaw`.
- **Capability-first agent surface**: `run_skill` is now part of the normal agent-facing write surface, while raw hub/pack/vault/vc governance tools remain admin-only.

## [0.15.1] - 2026-04-08 — 🔍 Search and Hub hardening

### Fixed
- **Hub feedback truthfulness**: `hub_record_feedback` now returns whether a capability record was actually updated. Missing hub items correctly surface `"recorded": false` instead of silently reporting success.
- **Search path-prefix filtering**: `search_vec` and `search_fts` now push `path_prefix` filtering down into SQL via `m.path LIKE ?`, reducing noisy candidates before scoring.
- **Importance and rating bounds**: `save_memory`, pipeline ingestion, and hub feedback now clamp invalid numeric inputs into safe ranges (`importance` to `0.0..=1.0`, `rating` to `0.0..=5.0`).
- **Nested fenced JSON extraction**: `strip_code_fence` now trims against the last closing fence, avoiding truncation when model output contains nested fenced snippets.
- **Projection test/runtime roots**: Foundry projection root detection now includes the workspace root in addition to the current directory and Git root, fixing rooted write validation in workspace-driven runs.

### Changed
- **Extraction schema enrichment**: prompt and parser paths now preserve `persons` and `entities` fields when distilling structured facts into memory entries.
- **Noise filtering hardened**: AI boilerplate denial phrases such as `I apologize`, `As an AI`, and `I cannot` are now treated as ignorable noise on ingest.
- **All crates and packages bumped to 0.15.1**: `memory-core`, `memory-node`, `memory-python`, `memory-server`, `integrations/openclaw`, and npm optionalDependencies.

### Tests
- **Regression coverage expanded**: added tests for hub-feedback misses, importance clamping, nested code-fence stripping, `persons` / `entities` extraction, FTS `path_prefix` filtering, and new noise-denial patterns.

## [0.15.0] - 2026-04-06 — 🏷️ Retention policy and domain-aware routing

### Added
- **Memory Retention Policy** (#38): New `RetentionPolicy` enum with four variants — `Ephemeral`, `Durable` (default), `Permanent`, and `Pinned`. Stored as TEXT in the `memories` table (`NULL` = durable). `Permanent` and `Pinned` entries are exempt from garbage collection. The `retention_policy` field is accepted on `save_memory` and returned on all read paths.
- **Domain-Aware Routing** (#32): New `DomainConfig` entity with `domains` table and full CRUD lifecycle. Four new MCP tools: `register_domain`, `get_domain`, `list_domains`, `delete_domain`. Each domain carries `name`, `description`, optional `gc_threshold_days`, `default_retention`, `default_path_prefix`, and arbitrary `metadata`. The `domain` field is available on `save_memory` and `search_memory` for write tagging and read filtering.
- **Externalized GC Configuration**: New `GcConfig` struct replaces all hardcoded GC thresholds (`access_history_keep_per_memory`, `processed_events_max_days`, `audit_log_max_days`, `audit_log_max_rows`, `agent_known_state_max_days`). Passed into `gc_tables()` for full configurability.
- **`MEMORY_GC_STALE_DAYS` Environment Variable**: Controls the stale-memory archival window (default: 90 days). Retention-aware archival logic now applies differentiated importance thresholds and respects GC-exempt retention policies.

### Changed
- **`MemoryEntry` struct**: Added `retention_policy: Option<String>` and `domain: Option<String>` fields. All 26 construction sites across `memory-core` and `memory-server` updated.
- **`SearchOptions` / `SearchMemoryParams`**: Added `domain: Option<String>` field for domain-scoped search filtering.
- **`gc_tables()` signature**: Now accepts `&GcConfig` instead of using hardcoded constants.
- **`archive_stale_memories()`**: Retention-aware — skips `Permanent`/`Pinned` entries, applies tiered importance thresholds, respects per-domain GC overrides.
- **Schema migrations**: Forward-compatible `ensure_column()` additions for `retention_policy` and `domain` on the `memories` table. New `domains` table with indexes.
- **All crates and packages bumped to 0.15.0**: `memory-core`, `memory-node`, `memory-python`, `memory-server`.

### Tests
- **147 tests passing** (up from ~134): New coverage for retention policy variants, domain CRUD operations, GC config externalization, and retention-aware archival logic.

## [0.14.0] - 2026-04-03 — 📖 Agent install guide and security hardening

### Added
- **Agent-first installation guide** (`docs/INSTALL.md`): A standalone document that any AI agent can read to autonomously install and configure Tachi — no human intervention needed. All three READMEs now link to this guide as the primary install path.

### Fixed
- **CRITICAL: Vault rotation underflow** (`vault_ops.rs`): Unsigned integer underflow in round-robin key rotation when `current_index` was 0 and subtraction wrapped. Now uses checked arithmetic with modular fallback.
- **HIGH: UTF-8 boundary panic** (`hub_ops/evolve.rs`): Multi-byte character slicing in skill evolution output could panic at byte boundaries. Switched to `char_indices`-based truncation.
- **Unbounded channel backpressure**: `enrich_tx` and `foundry_tx` were `mpsc::unbounded_channel` with no backpressure. Replaced with bounded channels (512 and 256 respectively) and `try_send` to prevent memory growth under sustained load.
- **Unbounded rate limiter maps**: `rate_limit_bursts` and `rate_limit_windows` HashMaps grew without bound per unique session. Added stale-entry eviction at 1024 and 4096 caps.
- **Unbounded tool cache**: `tool_cache` HashMap in `ServerHandler` grew without limit. Added LRU eviction at 256 entries.
- **DB lock contention on hub register**: Background `tokio::spawn` in `hub_ops/register.rs` held the async runtime while waiting for the DB write lock. Switched to `spawn_blocking` to avoid starving the Tokio thread pool.

### Changed
- **Python MCP server removed**: The legacy `mcp/` directory (Python 3.10+ MCP server) has been deleted. The native Rust binary is now the only MCP server. All READMEs updated to remove Python references and badges.
- **READMEs overhauled** (English, 简体中文, 文言文): Added agent-driven install option, updated architecture diagrams (removed Python paths), added Ghost Whispers / Neural Foundry / Skill Packs / Capability Recommendations to feature lists.
- **OpenClaw compatibility hardened**: Version aligned to `0.14.0`, `compact_context` guard added (checks required parameters before calling), phantom `record_access` field removed from TypeScript types, deprecated `shadowStorePath` removed from `plugin.json`.
- **Agent MCP configs updated**: Gemini CLI and Antigravity configs had stale `TACHI_EXPOSED_TOOLS` restrictions — removed to expose full tool surface.
- **All crates and packages bumped to 0.14.0**: `memory-core`, `memory-node`, `memory-python`, `memory-server`, `integrations/openclaw`, and npm optionalDependencies.

### Known Issues
- **Homebrew tap CI**: The "Update Homebrew Tap" GitHub Actions workflow requires a `HOMEBREW_TAP_GITHUB_TOKEN` secret to be configured in the repository settings. This is a manual step.
- **Low-severity items deferred**: Various `.unwrap()` calls in non-critical paths, unbounded `proxy_tools` vec, `hub_ops/export.rs` clean mode may delete non-tachi files, and extensive `#[allow(dead_code)]` annotations remain for a future cleanup pass.

## [0.13.1] - 2026-04-03 — 🔧 Lane wiring and lockfile consistency

### Changed
- **MiniMax lane wiring clarified**: documented and configured `MiniMax M2.7` as the default `DISTILL` and `SUMMARY` target using its OpenAI-compatible `chat/completions` endpoint, instead of treating it as a future gateway-only option.
- **Release examples tightened**: `.env.example` and `README.en.md` now show the tested lane stack explicitly: `Qwen3.5-27B` for extract, `MiniMax M2.7` for distill/summary, and `GLM-5.1` for reasoning/skill-audit.
- **Cargo lock aligned with release version**: `Cargo.lock` now records the `memory-server` package at `0.13.1`, keeping tagged builds internally consistent.

### Fixed
- **Post-tag release cleanup**: followed up the initial `0.13.0` lane-config release with lockfile/version consistency fixes and direct MiniMax endpoint guidance.

## [0.13.0] - 2026-04-03 — 🏭 Neural Foundry V1 and capability recommendations

### Added
- **Neural Foundry V1 runtime**: introduced server-owned `recall_context`, `capture_session`, `compact_context`, `section_build`, `compact_rollup`, and `compact_session_memory` so memory capture, context compaction, and durable session artifacts live in Tachi instead of host adapters.
- **Capability recommendation layer**: added `recommend_capability`, `recommend_skill`, `recommend_toolchain`, and `prepare_capability_bundle` to let Tachi recommend and package skills, packs, and host toolchains from one kernel surface.
- **Agent evolution pipeline**: added proposal synthesis, queue/review/project tools for agent profile evolution, plus richer evidence ingestion from inline docs, file paths, and memory-query bundles.
- **Read-only memory graph tool**: added `memory_graph` so agents can inspect graph neighborhoods without direct edge mutation access.
- **Kernel surface docs**: documented `kernel / capability / runtime / workflow / admin` layers and lane benchmark round-2 guidance for model selection.

### Changed
- **OpenClaw became a thin adapter**: the OpenClaw integration now keeps only a small agent-facing tool surface (`memory_search`, `memory_save`, `memory_get`, `memory_graph`) while `before_agent_start` and `agent_end` delegate recall/capture back to Tachi.
- **Tool exposure profiles**: built-in `ide`, `runtime`, `workflow`, and `admin` profiles now gate MCP tool exposure by host/runtime needs instead of exposing the full server by default.
- **LLM lane configuration**: the Rust client now supports separate `EXTRACT_*`, `DISTILL_*`, `SUMMARY_*`, and `REASONING_*` environment slots on top of the shared `SILICONFLOW_*` fallback, preparing Tachi for per-lane model routing.
- **CLI/server split cleanup**: `memory-server` moved CLI argument parsing, enrichment batching, and MCP pool logic into dedicated modules (`cli.rs`, `enrichment.rs`, `mcp_pool.rs`) to reduce `main.rs` churn.

### Fixed
- **OpenClaw / Opencode naming drift**: local configs now consistently refer to the memory kernel as `tachi`, and stale `sigil-node` package-lock remnants were removed from the live OpenClaw plugin copy.
- **Projection and maintenance hardening**: proposal writes remain rooted, distilled-memory retention is recency-safe, and foundry maintenance claims include state fingerprints to avoid skipping post-enrichment reruns.

### Tests
- **`memory-server` suite**: `cargo test -p memory-server` now passes with 99 tests after the module split and Foundry lane work.
- **OpenClaw build**: `npm --prefix integrations/openclaw run build` passes against the thin-adapter plugin.

## [0.12.3] - 2026-04-01 — 🎯 Named project targeting

### Added
- **Named project targeting for core memory APIs**: `save_memory`, `search_memory`, and `get_memory` now accept an optional `project` parameter so callers can explicitly target `~/.tachi/projects/<name>/memory.db` instead of relying only on the daemon’s current default project DB.
- **Server-side named project helpers**: `memory-server` gained `with_named_project_store()` and `with_named_project_store_read()` to open project DBs by name for both read and write paths.

### Changed
- **OpenClaw integration naming**: the OpenClaw-side memory plugin is now documented and configured as `tachi` instead of `memory-hybrid-bridge`.
- **OpenClaw integration topology**: docs now reflect the consolidated single-plugin runtime that combines memory, session intelligence, task tracking, and run audit.

### Tests
- **Expanded coverage for named project params**: test suite updated so `get_memory` / `save_memory` round-trips include the new `project` field, plus regression coverage for the new server-side path.

## [0.12.2] - 2026-03-30 — 📦 Release reproducibility (Cargo.lock)

### Fixed
- **Homebrew/release reproducibility**: started tracking the workspace `Cargo.lock` so release tarballs build against the tested dependency graph instead of drifting to newer incompatible crates during package installs.

### Changed
- **Patch release for packaging only**: no runtime behavior changes beyond restoring deterministic builds for `cargo build` consumers such as Homebrew.

## [0.12.1] - 2026-03-30 — 📊 Vector backfill and write provenance

### Added

#### Search + Backfill Ergonomics
- **`backfill-vectors` CLI command**: new maintenance command to count and backfill missing Voyage embeddings in any SQLite DB (`--db`, `--batch-size`, `--dry-run`). Useful for agent-local stores such as OpenClaw, Antigravity, or migrated databases.
- **Vector health helpers in `memory-core`**: `entries_missing_vectors()` and `vector_stats()` expose direct DB introspection for maintenance tools and migration scripts.
- **Write provenance metadata**: primary write paths now inject `metadata.provenance` (`save_memory`, `extract_facts`, `ingest_event`, `post_card`, `handoff_leave`, `ghost_promote`). Captures tool name, source kind, requested scope, resolved DB scope/path, registered agent identity, and optional profile/domain env tags.

### Changed
- **`search_memory` now auto-embeds queries** when `query_vec` is omitted and vector search is available, restoring true hybrid retrieval for plain-text clients.
- **`tachi search` CLI now matches MCP behavior** by generating a query embedding when vectors are enabled instead of silently degrading to lexical-only search.
- **OpenClaw runtime guidance updated**: current active topology is per-agent (`data/agents/<agent>/memory.db`), while root `data/memory.db` is legacy-only.

### Fixed
- **Hub schema migration ordering**: delayed `review_status` / `health_status` index creation until after migration guards, preventing startup issues on older DBs.
- **Live agent retrieval quality**: filled missing vectors in Antigravity/OpenClaw stores and archived stale topology memories that incorrectly claimed the legacy shared DB was still active.

### Tests
- **57 tests** (up from 55): 2 new provenance tests covering `save_memory` and `post_card` metadata injection.

## [0.12.0] - 2026-03-27 — 🔐 Vault hardening and Kanban GC

### Added

#### Vault Hardening
- **Auto-lock timeout**: Vault automatically locks after 30 minutes of inactivity (configurable via `vault_auto_lock_after_secs`). `vault_get` returns "Vault auto-locked" when timeout is exceeded. `vault_status` includes `auto_lock_after_secs` field.
- **Brute-force protection**: After 5 failed `vault_unlock` attempts, lockout for 5 minutes. Counter resets on successful unlock. Clear error messages with remaining lockout time.
- **Audit logging**: New `vault_audit` table with indexes on `timestamp`, `operation`, and `secret_name`. `record_vault_audit()` helper called from `vault_init`, `vault_unlock` (success/fail), `vault_lock`, `vault_set`, `vault_get`, and `vault_remove`.
- **Access control (`allowed_agents`)**: `vault_set` now accepts `allowed_agents: Vec<String>` (optional). Stored as JSON in `vault_entries.allowed_agents` column. `vault_get` requires `agent_id` parameter when `allowed_agents` is set. Returns clear "Access denied" errors for unauthorized agents.
- **Vault list returns `allowed_agents`** metadata for each secret.
- **Minimum password length**: `vault_init` enforces 8+ character passwords.

#### Kanban Card GC
- **`gc_expired_kanban_cards`**: Deletes kanban cards with status "resolved" or "expired" older than configurable `max_age_days` (default: 30). Integrated into both CLI `gc` command and `memory_gc` tool, returning `kanban_cards_pruned` count.
- **Background GC integration**: Periodic GC timer (6h interval) now includes kanban card pruning alongside existing table maintenance.

#### MCP Connection Pool Hardening
- **13 bare `.lock().unwrap()` calls replaced** with `lock_or_recover()` / `read_or_recover()` / `write_or_recover()` helpers in MCP pool code (`connections`, `connecting_locks`, `circuits`, `semaphores`). Prevents panics on poisoned mutexes.

### Changed
- **Vault key management**: `get_vault_key()` now returns owned `[u8; 32]` instead of `RwLockReadGuard`, using `read_or_recover` helper for poison recovery. `vault_lock` uses `clear_cached_vault_state()` which clears both `vault_key` and `vault_unlock_time`.
- **Vault `vault_get` rotation**: Entry selection moved into `select_vault_entry()` called under write lock (`with_global_store`) for atomicity with rotation state.
- **Vault `vault_remove`**: Now returns error instead of `"removed": false` for non-secret failures (audit logging, DB errors).

### Fixed
- **vault_audit table missing**: Added `CREATE TABLE IF NOT EXISTS vault_audit` to `schema.rs` with proper indexes. `vault_insert_audit` handler no longer panics.
- **MCP pool poison recovery**: All `.lock().unwrap()` calls in pool management replaced with `lock_or_recover()` to handle poisoned mutexes gracefully.

### Tests
- **55 tests** (up from 50): 5 new tests:
  - `vault_auto_lock_expires_cached_key`: Auto-lock after timeout, status shows locked
  - `vault_unlock_enforces_bruteforce_lockout_and_resets_on_success`: 5 failed attempts → lockout, counter reset on success
  - `vault_get_respects_allowed_agents`: Missing agent_id → denied, wrong agent → denied, correct agent → allowed
  - `vault_operations_record_audit_entries`: Audit rows for init/set/get/lock/unlock(both success+fail)/remove
  - `memory_gc_prunes_expired_resolved_kanban_cards`: Resolved card older than 30 days gets deleted

## [0.11.1] - 2026-03-26 — 👻 Ghost aliases, persistence, and governance

### Added

#### Ghost in the Shell Tool Aliases (#25)
- **Ghost layer aliases**: `ghost_whisper` → `ghost_publish`, `ghost_listen` → `ghost_subscribe`, `ghost_channels` → `ghost_list_topics`. Fully backward-compatible; original tool names continue to work.
- **Shell layer aliases**: `shell_set_policy` → `sandbox_set_rule`, `shell_get_policy` → `sandbox_get_rule`, `shell_list_policies` → `sandbox_list_rules`, `shell_exec_audit` → `sandbox_exec_audit`. Maintain the same cache invalidation behavior as original tools.
- **Section 9 aliases**: `section9_review` → `hub_review`, `section9_audit_log` → `tachi_audit_log`. Reflects the Ghost in the Shell universe's Section 9 intelligence unit.
- **Cyberbrain aliases**: `cyberbrain_write` → `save_memory`, `cyberbrain_search` → `search_memory`. Stylized aliases for the core memory operations.
- **Cache behavior preserved**: All alias write/read paths correctly invalidate caches matching the canonical tool behavior.

#### Ghost Phase-3 Persistence (#24)
- **Persistent ghost tables**: New SQLite tables `ghost_messages`, `ghost_subscriptions`, `ghost_cursors`, `ghost_topics`, `ghost_reflections` in `memory-core`. Ghost pub/sub is now fully DB-backed and restart-safe.
- **Restart-safe cursors**: Per-subscriber message cursors survive daemon restarts. `ghost_subscribe` resumes from the last acknowledged message position.
- **`ghost_ack` Tool**: Acknowledge ghost messages by ID, advancing the subscriber cursor. Prevents re-delivery of already-processed messages.
- **`ghost_reflect` Tool**: Create a reflection entry from a ghost message — capturing insights, patterns, and optional rule derivations from observed agent communications.
- **`ghost_promote` Tool**: Promote a ghost message or reflection to long-term memory (`save_memory` with `category="ghost"`). Optionally triggers reflection-to-rule derivation via LLM.
- **DB-backed ghost_publish/subscribe/topics**: All three core operations now read/write from persistent SQLite instead of in-memory state, enabling true cross-session message delivery.

#### Sandbox Executor Phase-2 Audit & Policy Enforcement (#23)
- **`sandbox_exec_audit` table**: New `sandbox_exec_audit` persistence table in `memory-core` recording preflight, startup, and tool-call sandbox decisions with error kind classification.
- **`sandbox_exec_audit` Tool**: New MCP tool exposing sandbox audit log for observability — query by agent, tool, decision (allow/deny), and time range.
- **Runtime policy enforcement**: Policy presence is now enforced on the MCP `connect` and `call` path. Connections from agents without a matching sandbox policy are rejected with a clear error.
- **Policy denial logging**: All policy-based denials are logged to both `sandbox_exec_audit` and `audit_log`, making policy rejects distinguishable from runtime failures.
- **Audit record classification**: Records distinguish between `preflight` (before connection), `startup` (at process spawn), and `tool_call` (at invocation) decision points.

#### Hub Governance Phase-1 (#22)
- **Governance metadata fields**: `hub_capabilities` extended with `review_status` (pending/approved/rejected), `health_status` (healthy/degraded/offline), `fail_streak`, `last_error_at`, `last_success_at`, and `exposure` metadata.
- **`hub_version_routes` table**: New table mapping capability IDs to active version pins. Enables deterministic active-version resolution across capability upgrades.
- **`hub_review` Tool**: Review a capability — set `review_status` to approved or rejected with an optional note. Only approved capabilities are callable via `hub_call`.
- **`hub_set_active_version` Tool**: Pin a capability ID to a specific version string. Used by `skill_evolve` to activate evolved skill versions.
- **Governance gates in `hub_call`**: Before proxying a call, `hub_call` checks `review_status == approved` and `health_status != offline`. Rejects non-approved or offline capabilities with actionable error messages.
- **Call outcome persistence**: `hub_call` records success/failure outcomes to `hub_capabilities.fail_streak`, `last_error_at`, and `last_success_at` after every invocation.
- **Bootstrap initializers updated**: New capabilities registered at startup are initialized with `review_status=approved` and `health_status=healthy` to avoid breaking existing workflows.

## [0.11.0] - 2026-03-26 — 🤝 Multi-agent orchestration layer

### Added

#### Wave 10 — Multi-Agent Orchestration Layer
- **`agent_register` Tool**: Register an agent profile per-session with identity (`agent_id`, `display_name`), capabilities, tool allowlist (glob patterns), and per-agent rate limit overrides. Stored in-memory, scoped to the MCP session lifetime.
- **`agent_whoami` Tool**: Return the current agent profile for this session, or a clear `"unregistered"` status if no profile is set.
- **`handoff_leave` Tool**: Leave a structured handoff memo for the next agent session. Includes summary, next_steps list, optional target agent, and arbitrary context JSON. Persisted both in-memory (fast cross-session) and to the global memory store (cross-restart durability, `category="handoff"`, `importance=0.9`). Caps at 50 in-memory memos with LRU eviction.
- **`handoff_check` Tool**: Check for pending handoff memos, filtered by target agent. Supports acknowledgment (marks memos as read). Designed to be called at the start of every new agent session.

#### Wave 9 — Rate Limiter & Loop Detection
- **Per-session rate limiter**: Sliding window RPM (requests per minute) enforcement per MCP session. Configurable via `RATE_LIMIT_RPM` env var (default: 0 = unlimited).
- **Burst / loop detection**: Detects identical tool+args calls within a 60-second window. Default burst limit: 8. Configurable via `RATE_LIMIT_BURST` env var.
- **Agent profile overrides**: `agent_register` can set per-agent `rate_limit_rpm` and `rate_limit_burst` that override server-wide defaults.
- **Clear error messages**: Rate limit and loop detection errors include actionable guidance ("Retry in Ns", "Break the loop by varying your approach").

#### Wave 8 — Skill Export & Evolution
- **`hub_export_skills` Tool**: Export Hub skills to agent-specific file formats. Supports 4 agent targets:
  - `claude`: Writes `SKILL.md` files to `~/.tachi/skills/<name>/` with symlinks to `~/.claude/skills/`.
  - `openclaw`: Generates a plugin manifest JSON in `~/.openclaw/plugins/`.
  - `cursor`: Writes `.mdc` rule files to `.cursor/rules/` (project-relative).
  - `generic`: Raw markdown export to a specified directory.
  - All modes support visibility filtering (`listed`, `discoverable`, `all`), skill ID selection, agent-local scope filtering, and clean mode (removes stale exports).
- **`skill_evolve` Tool**: LLM-powered skill prompt improvement. Analyzes the current skill prompt, usage feedback, and success/failure metrics to generate an improved version. Creates a new versioned capability (`skill:name/vN`), supports optional auto-activation via `hub_version_routes`, and dry-run mode.
- **Feedback recording in `run_skill`**: Skill executions now automatically record call outcomes (success/failure + latency) via `hub_record_call_outcome`, enabling data-driven evolution.

#### Infrastructure
- **Project DB hot-activation** (`tachi_init_project_db`): Creates and immediately wires up a project-scoped SQLite database at runtime without daemon restart. Uses `ProjectDbState` struct with `Arc<StdRwLock>` interior mutability. All ~30 `project_db_path.is_some()` checks replaced with `has_project_db()` which checks both static config and hot-swapped state.

### Changed
- **32 tests**: Test suite expanded from 21 to 32 tests covering rate limiter burst detection, RPM enforcement, agent profile overrides, agent register/whoami roundtrip, handoff leave/check with target filtering and acknowledgment, handoff persistence to memory store, and skill export (empty set, unknown agent, generic file write).
- **`hub_discover` refactored**: Extracted `hub_discover_inner()` returning `Vec<Value>` directly, eliminating double serde round-trip in `handle_vc_list` (M-3 from code review).

### Fixed
- **I-1: Metadata error swallowing**: `virtual_capability.rs` now propagates JSON parse errors for VC binding metadata instead of silently replacing with `{}`.
- **I-2: Cross-scope VC shadowing**: `vc_register` now rejects registration if the same VC ID exists in the opposite scope (global vs project), preventing orphaned bindings.
- **M-1: version_pin i64→u32 cast**: Uses `try_into().unwrap_or(0)` instead of unchecked `as u32` cast that could silently wrap negative values.
- **M-2: VC auto-approval undocumented**: Added comment explaining why VCs skip `hub_review` (logical routing abstractions, not executable code).
- **MemoryEntry construction**: Handoff memo persistence now constructs `MemoryEntry` with all required fields instead of relying on missing `Default` impl.

## [0.10.0] - 2026-03-26 — 🔮 Virtual Capabilities and governance

### Added

#### Wave 7b — Virtual Capabilities & Governance Hardening
- **Virtual Capability (VC) layer**: Logical capability abstraction on top of concrete Hub backends. Register VCs (`vc:*` IDs), bind to multiple concrete MCP backends with priority ordering, and resolve at call time. Deterministic priority-ordered resolution with version pinning and full candidate reporting.
- **`vc_register` Tool**: Register a Virtual Capability with contract, routing strategy, tags, and input schema.
- **`vc_bind` Tool**: Bind a concrete MCP capability to a VC with priority, version pin, and enable/disable toggle.
- **`vc_resolve` Tool**: Resolve a VC to its best available concrete backend. Returns the resolved ID and a detailed resolution report with candidate status.
- **`vc_list` Tool**: List all VCs with their bindings, merged from both global and project databases.
- **Sandbox policy inheritance**: Sandbox policies fall back from resolved concrete capability to the requesting VC ID, enabling policy-once-at-VC-level.
- **Fail-closed fs_roots**: Process-transport MCP capabilities with `fs_read_roots`/`fs_write_roots` now fail closed (rejected at preflight) because stdio processes cannot enforce filesystem isolation. Audit trail logged before denial.
- **`virtual_capability_bindings` table**: New SQLite table with composite PK `(vc_id, capability_id)`, priority+id index for deterministic ordering, and `ON CONFLICT DO UPDATE` upsert semantics.

### Changed
- **Hub governance**: Prompt security scanning on skill registration (high-risk auto-disable, medium-risk flag). Static scan for shell injection patterns and prompt injection markers.
- **Desktop API resilience**: `api.ts` now tries multiple base URLs (`VITE_TACHI_BASE_URL`, proxy, localhost) with automatic failover.
- **Desktop proxy fix**: `vite.config.ts` proxy configuration corrected for daemon communication.

## [0.9.0] - 2026-03-25 — 📋 Agent Kanban communication board

### Added

#### Wave 7 — Agent Kanban Communication Board
- **`post_card` Tool**: Create inter-agent kanban cards as first-class memory entries (`category="kanban"`, `path="/kanban/{from}/{to}"`) with normalized metadata (`from_agent`, `to_agent`, `status`, `priority`, `card_type`, `thread_id`).
- **`check_inbox` Tool**: Query per-agent inbox with status/since filters, optional broadcast fan-in (`to_agent="*"`), and deterministic ordering by priority then recency.
- **`update_card` Tool**: Update card status via revision-checked optimistic locking and append threaded replies (`metadata.replies`) without introducing new tables.
- **Optional local classification pipeline**: Background kanban enrichment hook (`KANBAN_CLASSIFY_ENABLED`, `KANBAN_MODEL_URL`, `KANBAN_MODEL_NAME`) to tag cards with `topic`, `keywords`, and `priority_suggestion`.

#### Hub Policy & Visibility
- **Capability visibility policy**: Skill/MCP definitions now support `policy.visibility` (`listed`, `discoverable`, `hidden`) to reduce tool-list noise while preserving on-demand calls.
- **Safer default MCP exposure**: registration scripts default shared MCP entries toward `tool_exposure=gateway` and discoverable policy modes.

### Changed

- **Main server modularization**: `memory-server` tool logic is split into dedicated modules (`hub_ops`, `memory_search_ops`, `pipeline_ops`, `server_methods`, etc.), reducing `main.rs` to < 1000 lines and improving maintainability.
- **Daemon project-context behavior**: daemon mode now disables auto-detected project DB by default to avoid mixed project context; explicit `--project-db` enables single-project daemon mode.

### Fixed

- **Error recovery visibility**: replaced silent fallbacks in key paths with explicit warnings for invalid definitions/tool payloads and poisoned-mutex recovery.
- **Config validation hardening**: MCP `env`/`args` definition parsing is now stricter to prevent malformed runtime config from being silently accepted.

## [0.8.0] - 2026-03-24 — 🧩 Memory infrastructure and MCP proxy hardening

### Added

#### Wave 4 — Memory Infrastructure
- **`delete_memory` Tool**: Permanently removes a memory entry along with its FTS index, vector embeddings, graph edges, access history, and agent known-state records. Full CASCADE cleanup prevents orphaned data.
- **`archive_memory` Tool**: Soft-deletes a memory entry (sets `archived=1`). Archived entries are hidden from default searches but can be recovered via `include_archived=true`.
- **`memory_gc` Tool**: On-demand garbage collection for growing tables. Prunes old `access_history` (keeps latest 256 per memory), `processed_events` (30-day TTL), `audit_log` (30-day + 100K row cap), and `agent_known_state` (90-day TTL).
- **Background GC Timer**: Automatic periodic garbage collection every 6 hours (configurable via `MEMORY_GC_INTERVAL_SECS` env var or `--gc-interval-secs` CLI flag). Runs the same logic as the `memory_gc` tool.
- **Noise Filtering on Save**: `save_memory` now rejects junk text via `is_noise_text()` — catches content that is too short, repetitive, or lacking semantic value. Bypassable with `force=true`.
- **Query Noise Guard**: `search_memory` skips trivially meaningless queries via `should_skip_query()`, returning an early advisory instead of wasting embedding API calls.

#### Wave 5 — MCP Proxy Hardening
- **`hub_set_enabled` Tool**: Enable or disable a Hub capability by ID at runtime, without requiring re-registration.
- **Environment Variable Whitelist**: `env_clear()` for child MCP server processes now preserves 21 critical system variables: `PATH`, `HOME`, `USER`, `LANG`, `LC_ALL`, `SSL_CERT_FILE`, `SSL_CERT_DIR`, `TMPDIR`, `TMP`, `TEMP`, `XDG_RUNTIME_DIR`, `XDG_CACHE_HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, and all proxy vars (`HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY`, `ALL_PROXY` in both cases). Prevents child processes from failing due to missing SSL certificates or PATH.
- **Transport Aliases**: MCP proxy now accepts `"http"` and `"streamable-http"` as transport type aliases for `"sse"`, reducing misconfiguration errors.
- **MCP Tool Exposure Modes**: Child MCP capabilities now support `tool_exposure` in definition (`"flatten"` or `"gateway"`). `gateway` keeps child tools callable via `hub_call` but hides `server__tool` fan-out from `tools/list` to avoid tool-count explosion.
- **Global Exposure Default**: New env var `MCP_TOOL_EXPOSURE_MODE` sets default exposure for child MCPs when `tool_exposure` is not explicitly set.
- **Agent Config Bootstrap Script**: Added `scripts/setup_agent_mcp.py` to detect common local agent config files and safely inject Tachi MCP entries (dry-run by default, `--apply` to write).

#### Wave 6 — Graph Activation
- **`add_edge` Tool**: Create or update directed edges in the memory graph. Supports causal, temporal, and entity relationship types with optional metadata and weight.
- **`get_edges` Tool**: Query edges connected to a memory entry. Returns all causal, temporal, and entity relationship edges for graph traversal and visualization.
- **Auto-Link on Save**: `save_memory` now automatically creates `entity` graph edges between the new memory and existing memories that share the same entities. Enabled by default (`auto_link=true`), runs asynchronously in background to avoid blocking the save response.

### Changed
- **34 MCP Tools**: Server now exposes 34 tools total (17 memory + 6 hub + 5 proxy + 3 pubsub + 2 DLQ + 1 sandbox).
- **FTS Sanitizer**: Preserve dots in version strings (e.g., `v0.7.2`) during FTS tokenization, improving search accuracy for version-related queries.
- **Graph Expand Default**: `search_memory` now defaults to `graph_expand_hops=1` (previously 0), enabling single-hop graph expansion for richer context retrieval out of the box.

### Fixed
- **DELETE CASCADE**: `delete()` now properly cascades to `access_history` and `agent_known_state` tables, preventing orphaned rows after memory deletion.
- **Scoped Graph Persistence**: Graph edges are now correctly persisted within the appropriate database scope (project vs. global).
- **Proxy Discovery Safety**: `hub_register(type=mcp)` now applies discovery timeouts before finalizing capability state and records failed discovery as disabled metadata instead of leaving a silently-enabled broken proxy.

## [0.7.2] - 2026-03-24 — 🔒 Proxy hardening and audit safety

### Added
- **`hub_disconnect` Tool**: New MCP tool to forcefully drop cached child MCP server processes from the proxy pool, allowing immediate reconnects with refreshed environment variables.
- **LRU Cursor Eviction**: `ghost_subscribe` now properly implements LRU eviction to limit pub/sub topic cursors (`PUBSUB_MAX_CURSORS=1000`), securely preventing unbounded memory growth.

### Changed
- **Strict Error Propagation**: `sync_memories` now bubbles up agent state persistence failures instead of silently logging them, ensuring no false-positive state commits.
- **Consistent Proxy Gates**: Unified capability enabled-state checks inside the internal proxy spawner `connect_child` and `proxy_call_internal`, preventing any bypassed direct `server__tool` calls for disabled child capabilities.

### Fixed
- **TOCTOU Enrichment Race Condition**: Shifted atomic revision constraints (`WHERE id=? AND revision=?`) to the start of the transaction, effectively neutralizing asynchronous vector overwrite timing bugs.
- **Deterministic Sandbox Routing**: Path matching for Sandbox semantic validation now scales strictly via mathematical matching specificity (`ORDER BY LENGTH(path_pattern) DESC`).
- **Retry Dispatch Router**: Centralized dynamic routing via `retry_dispatch` wrapper to consistently retry Native, Proxy, and Skill tool invocations within the Dead Letter Queue.

## [0.7.0] - 2026-03-23 — 🔌 MCP Client Proxy Phase 2

### Added
- **MCP Client Proxy (Phase 2)**: Tachi now acts as both MCP server and client. Register child MCP servers via `hub_register(type=mcp)`, their tools appear transparently in `tools/list` with `server__tool` prefix. Agents call them directly — Tachi handles spawn, connection, forwarding, and cleanup.
- **Connection Pool**: Lazy-connect on first use, reuse across calls, idle cleanup after 5 minutes. No more zombie processes — one Tachi instance manages all child MCP servers.
- **Circuit Breaker**: Per-child failure tracking (Closed → Open → HalfOpen). Only transport errors trigger circuit open, not tool-level errors.
- **SSE/Streamable HTTP Transport**: Connect to HTTP-based MCP servers (Linear, Vercel, etc.) alongside stdio servers.
- **Skill-as-Tool**: Skills registered with `hub_register(type=skill)` can expose callable tools (`tachi_skill_*`) with LLM-backed execution.
- **Audit Log**: Every proxy call recorded (`tachi_audit_log` tool). Tracks server, tool, duration, success/failure, args hash.
- **Command Allowlist**: MCP server registration validates commands against trusted list (`npx`, `python3`, `node`, `cargo`, brew paths, etc.). Untrusted commands registered but disabled.
- **Tool Deny-List**: Per-server `permissions.deny` blocks dangerous tools (e.g. `delete_repo`).
- **Per-Child Concurrency Semaphore**: `max_concurrency` config per MCP server (default: 1 for stdio).
- **Timeout Config**: `tool_timeout_ms` and `startup_timeout_ms` per MCP server definition.
- **`hub_call` Fallback Tool**: Explicit proxy call via `hub_call(server_id, tool_name, args)` when direct tool names are unavailable.
- **`${VAR}` Env Resolution**: Environment variable references in MCP server definitions resolved at runtime. Missing vars fail with clear error.
- **TOCTOU Fix**: Per-server connecting lock prevents duplicate child process spawning under concurrent calls.

### Changed
- **rmcp 0.1 → 1.2**: Major SDK upgrade. `#[tool(tool_box)]` → `#[tool_router]`, `ToolBox` → `ToolRouter`, `rmcp::Error` → `rmcp::ErrorData`, `CallToolRequestParam` → `CallToolRequestParams` (builder pattern), `ServerInfo`/`ListToolsResult` now `#[non_exhaustive]`.
- **schemars 0.8 → 1.x**: Via rmcp's re-exported `rmcp::schemars`. All 20+ param structs migrated.
- **Persistent SQLite Connections**: `MemoryServer` holds long-lived `Mutex<MemoryStore>` instead of opening per-request. `init_schema()` runs once at startup.
- **Call Dispatch Order**: Native tools → Skill tools → Proxy tools (prevents future name shadowing).
- **Project MCP Security**: Project-scope MCP capabilities with untrusted commands default to `enabled=false`.

## [0.6.0] - 2026-03-23 — 🏷️ Project renamed to Tachi

### Changed
- **Renamed to Tachi (塔奇)**: Project identity renamed from Sigil to Tachi, inspired by Ghost in the Shell's Tachikoma — AI units that evolve through shared memory. Binary now prints `tachi <version>` on `--version`/`-V`.
- **Homebrew distribution**: `brew tap kckylechen1/tachi && brew install tachi` — one-command install, 7.3MB binary.
- **`--version` flag**: Added CLI version flag before async runtime initialization.
- **MCP config**: Binary installs as `tachi` instead of `memory-server`. Config: `{"mcpServers": {"tachi": {"command": "tachi"}}}`.

## [0.5.2] - 2026-03-23 — 🎯 Sigil Hub Phase 1 (capability registry)

### Added
- **Sigil Hub Phase 1 — Capability Registry + Discovery**: A unified catalog for Skills, Plugins, and MCP Servers. Any agent connecting to Sigil can discover and retrieve all registered capabilities.
- **`hub_capabilities` table** in `memory-core`: New SQLite table with CRUD operations for registering, listing, searching, enabling/disabling, and tracking usage metrics of capabilities.
- **`hub.rs` types module**: `HubCapability` struct with id, type, name, version, description, definition, enabled, usage/success/failure counters, and rolling average rating.
- **5 new MCP tools**: `hub_register` (register a capability), `hub_discover` (list/search with dual-DB merge, project shadows global), `hub_get` (fetch single capability with project-first fallback), `hub_feedback` (record success/failure/rating), `hub_stats` (aggregated metrics across both DBs).
- **4 new NAPI methods** for OpenClaw: `hub_register`, `hub_discover`, `hub_get`, `hub_feedback` — same Hub functionality available to Node.js agents.
- **Skill batch loader** (`scripts/load_skills_to_hub.py`): Scans `~/.claude/skills/` directories, parses YAML frontmatter + full markdown content, and bulk-registers into global Hub. Successfully loaded 67 skills.
- **Hub dual-DB inheritance**: Project Hub capabilities shadow global ones by ID. Register defaults to project DB, discover/get queries both with project priority.

### Changed
- **`memory-core` public API**: Added `pub mod hub` and re-exported `HubCapability`. Added 7 Hub methods to `MemoryStore` (hub_register, hub_get, hub_list, hub_search, hub_set_enabled, hub_record_feedback, hub_delete).
- **MCP tool list**: Server now exposes 15 tools (10 memory + 5 hub).

## [0.5.0] - 2026-03-23 — 🗄️ Dual-DB architecture (global + project)

### Added
- **Dual-DB Architecture (Global + Project)**: Memory server now maintains two separate SQLite databases — a global DB (`~/.sigil/global/memory.db`) for cross-project knowledge (user preferences, universal facts) and a per-project DB (`.sigil/memory.db` at git root) for project-scoped memories (architecture decisions, codebase patterns). This is the foundation for multi-agent shared memory.
- **`DbScope` enum**: All MCP tool responses now include a `"db"` field (`"global"` or `"project"`) indicating which database sourced or stored the result.
- **Automatic Git root detection**: Server detects the nearest `.git` directory to resolve the project DB path. Falls back to global-only when not inside a git repository.
- **Legacy DB migration**: On first run, automatically migrates `~/.sigil/memory.db` to `~/.sigil/global/memory.db` for seamless upgrade from v0.4.
- **Dual-DB search merge**: `search_memory` queries both databases in parallel, merges results by `final_score` descending, deduplicates by entry ID, and truncates to `top_k`.
- **Write scope routing**: `save_memory`, `extract_facts`, and `ingest_event` route writes via `resolve_write_scope()` — `"global"` scope writes to global DB, everything else defaults to project DB (with automatic fallback + warning when no project DB is available).
- **Per-DB integrity checks**: Startup runs `PRAGMA quick_check` on both databases independently.
- **Aggregated `memory_stats`**: Returns merged totals across both DBs plus a `"databases"` breakdown showing per-DB stats and vector availability.

### Changed
- **`MemoryServer` struct**: Split from single `db_path`/`vec_available` into `global_db_path`/`project_db_path` and `global_vec_available`/`project_vec_available`.
- **`set_state`/`get_state`**: Now hardcoded to use global DB (server state is cross-project by nature).
- **`default_scope()`**: Changed from `"general"` to `"project"` to match new dual-DB routing semantics.
- **`get_memory`**: Tries project DB first, falls back to global DB.
- **`list_memories`**: Queries both DBs, tags entries with source, sorts by timestamp descending.

### Removed
- **Single-DB `with_store` helper**: Replaced by `with_global_store`, `with_project_store`, and `with_store_for_scope`.
- **Pipeline module**: Removed in prior commit (server is now pure MCP handler).

## [0.4.0] - 2026-03-18 — ⚙️ Native Rust MCP server

### Added
- **NEW: Native Rust MCP Server (`memory-server` crate)**: Complete replacement for the Python `mcp/server.py`. Single 5.2MB ARM64 binary with 10 MCP tools, built with `rmcp` SDK. Eliminates Python runtime dependency for the MCP server.
- **LLM Integration in Rust (`llm.rs`)**: Voyage-4 embedding via `reqwest` (direct API) and SiliconFlow Qwen LLM via `async-openai` for L0 summary generation and fact extraction. All API calls happen asynchronously before database locks.
- **Prompt Templates (`prompts.rs`)**: Extracted and hardcoded all LLM prompt templates (EXTRACTION_PROMPT, SUMMARY_PROMPT, CAUSAL_PROMPT) from Python into Rust constants.
- **10 MCP Tools**: `save_memory` (with real-time Voyage-4 embedding + Qwen summary), `search_memory`, `get_memory`, `list_memories`, `memory_stats`, `set_state`, `get_state`, `extract_facts` (LLM-based), `ingest_event`, `get_pipeline_status`.
- **Hard State Table in Rust**: `hard_state` table with `set_state`/`get_state` functions in `memory-core` for persistent KV storage.
- **Memory Graph (`memory_edges` table)**: Added graph structure with edge management, PageRank scoring, and graph expansion in hybrid search.
- **ACT-R Cognitive Decay**: Time-based decay scoring inspired by ACT-R cognitive architecture, integrated into hybrid search scorer.
- **PageRank Integration**: Graph-aware PageRank scoring in hybrid search for importance-weighted retrieval.
- **Noise Injection**: Configurable Gaussian noise for search score diversification.

### Changed
- **Architecture**: MCP server can now run as either Python (`mcp/server.py`) or native Rust binary (`memory-server`). Rust path eliminates PyO3 bridge overhead.
- **Embedding Decision**: A/B tested Voyage-4 vs Qwen3-Embedding-8B (SiliconFlow). Voyage-4 won on discrimination (Δ 0.46 vs 0.37) and query latency (569ms vs 1017ms). Staying with Voyage-4.
- **Thread Safety**: Replaced `tokio::sync::Mutex` with `std::sync::Mutex` for `rusqlite::Connection` (!Send safety in multi-threaded Tokio runtime).
- **JSON Safety**: All JSON output uses `serde_json::to_string` instead of `format!` string concatenation.

### Fixed
- **ServerHandler Registration**: Fixed `rmcp` integration where `#[tool(tool_box)]` was missing on `ServerHandler` impl, causing tools to not register (empty `tools/list` response).
- **Search Output Format**: `search_memory` returns human-readable summaries instead of raw JSON.
- **State Management**: Proper `hard_state` table replaces hacky MemoryEntry-based state storage.

## [0.3.0] - 2026-03-14 — 🧱 `hard_state` and derived-item isolation

### Added
- **Core/MCP**: Introduced `hard_state` table and corresponding `set_state` / `get_state` endpoints to store rigid KV data distinct from semantic mappings. This resolves data hallucination for strict key values like dynamic watchlists.
- **MCP/Workers**: Created `derived_items` table explicitly isolating Causal logic and Distillation outputs from primary empirical facts, drastically improving search relevancy.
- **Server**: Implemented `ENABLE_PIPELINE` logic (defaulting to false) to toggle the background asynchronous extraction process on demand to maximize pure querying speeds.

### Changed
- **Pipeline**: Shifted event extraction process to lazy-execution triggered under the background thread pool queue. 
- **Core/MCP**: Removed all LLM-based abstract summarization fallback functions from standard `save_memory` path to bypass latency delays.

### Removed
- **MCP**: Eliminated `Voyage-Rerank-2.5` dependency from standard hybrid searches. Core Rust pipeline handles similarity filtering accurately enough, boosting response time natively via KNN & FTS5 mechanisms alone.
- **Scrap**: Cleaned up legacy unmaintained prototype directories `memory-mcp/` and `memory-core-rs/` from local `scratch` areas.

## [0.2.1] - 2026-03-13 — 🔧 Deduplication and timeout fixes

### Fixed
- **MCP/Extractor**: Fixed `httpx.ReadTimeout` empty error bug in `extract_facts` and added 3-round exponential backoff (2s/4s/8s) for Siliconflow API calls to improve retry resilience against transient network failures.
- **Config**: Fixed `config.ts` environmental variable parsing where `MEMORY_DB_PATH` was not expanding `~` to home directory. Additionally ensured `install_openclaw_ext.sh` creates the necessary `data` directory.
- **Memory Deduplication** (PR #4): Merged two-stage memory deduplication (`HARD_SKIP` vs `EVOLVE`) with upsert indentation bug fix. This resolves the issue of over-aggressive memory deduplication. Tests confirmed this mathematical threshold approach is more robust than LLM-based (GLM-4/Qwen/DeepSeek) deduplication judgments for this specific pipeline.

## [0.2.0] - 2026-03-08 — 🌊 Causal worker pipeline

### Added
- **MCP/Workers**: Activated causal worker pipeline for asynchronous extraction of cause-and-effect relationships.
- **Core**: Refactored `memory_relations` support to allow robust linking of related memory fragments.
- **Docs**: Added one-click install script for OpenClaw.
- **Docs**: Added Sigil v2 PRD (architecture specification) for causal pipelines and memory workers.
- **Async Pipeline**: Phase 2 async event pipeline with 4 memory workers (Extractor, Distiller, CausalWorker, Consolidator).

### Changed
- **MCP/Extractor**: Upgraded default fact extraction model to `Qwen3.5-27B` for significantly better causal relationship tracking and structured fact parsing.
- **Docs**: Updated the recommended extraction model in `README.zh-CN.md` and `README.md` to `Qwen3.5-27B`.

### Fixed
- **Core**: Stabilized vector KNN searches and ensured `auto-capture` remains writable.
- **Core**: Addressed SQLite Upsert limitations by migrating to `DELETE` + `INSERT` for `sqlite-vec` `vec0` virtual tables.
- **OpenClaw Plugin**: Fixed critical hooks and synced all improvements to the latest agent platform requirements.
- **OpenClaw Plugin**: Corrected plugin kind to `memory`, removed dead code paths, and added native Voyage reranker support.
- **OpenClaw Plugin**: Ensure `installer` reliably builds bindings and loads the plugin successfully.
- **Python MCP**: Resolved code smells during fact extraction and migrations.

## [0.1.0] - 2026-03-05 — 🎉 Initial Sigil release

### Added
- Initial release of the Sigil Memory System.
- Blazing Fast Rust Core (`memory-core`) featuring Native CJK FTS5 text search and `sqlite-vec` semantic indexing.
- 4-Channel Hybrid Search Engine (Semantic, Lexical, Symbolic, Decay).
- Native Node.js `NAPI-RS` bindings for OpenClaw extension.
- Native Python `PyO3` bindings targeting MCP server frameworks.
- Added Dotenv support for graceful API key extraction from project roots.
- Support for Voyage-4, Voyage Rerank-2.5, and GLM-4 base models.
