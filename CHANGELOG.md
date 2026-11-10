# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Upgrade steps for each version are collected in [docs/UPGRADING.md](docs/UPGRADING.md).

## Quick Navigation

- [Unreleased](#unreleased)
- [2.1.0](#210---2026-11-08): schema 39→41, HTTP-only first-party MCP, one default agent profile, workspace-first binding, Vault-managed upstream MCP credentials, CJK recall precision
- [2.0.0](#200---2026-09-25): six-facade Lead surface, CurrentTruth task status, schema 28→39, model broker SSE, dispatch V2 atomic plans
- [1.9.2](#192---2026-08-05): wiki legacy adoption, contract-tests carve-out, pooled-client proxy fix
- [1.9.1](#191---2026-08-05): portable snapshot import, receipt content binding, distill revival, CI honesty gates
- [1.9.0](#190---2026-07-11): portable runtime, execution environments, precedent memory, and reliability hardening
- [1.8.0](#180---2026-07-10): Tachi crate branding, action-level profiles, recall discrimination, CI-state ingest
- [1.7.0](#170---2026-07-08): portable memory kernel, vector auditability, and dispatch canon
- [1.6.4](#164---2026-07-06): multi-project daemon stdio repair
- [1.6.3](#163---2026-07-06): recall quality overhaul and kernel reliability
- [1.6.2](#162---2026-07-05): release drift and CI guardrails
- [1.6.1](#161---2026-06-29): Homebrew daemon startup and tap automation

Earlier releases are archived in the [full historical changelog](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md):

- [1.6.0](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#160---2026-06-29): continuity memory, lifecycle routing, and runtime hardening
- [1.5.x](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#156---2026-06-15): repair and tidy cleanup UX
- [1.4.x](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#140---2026-06-01): Plan C project DB, SFT factory, and facade contracts
- [1.3.x](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#130---2026-05-30): search quality, memory lifecycle, and audit hardening
- [1.2.0](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#120---2026-05-25): shell orchestration, dispatch v2, daily distill
- [1.1.0](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#110---2026-05-04): truth-maintenance security fixes
- [1.0.1](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#101---2026-05-02): default profile changed to `standard`
- [1.0.0](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#100---2026-05-01): Tool Surface v2 and facade tools
- [0.16.x](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#0164---2026-04-28): memory governance, rescue, and coherent distill
- [0.15.x](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#0150---2026-04-06): retention policy and domain-aware routing
- [0.14.0](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#0140---2026-04-03): agent install guide and security hardening
- [0.13.0](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#0130---2026-04-03): Neural Foundry V1 and capability recommendations
- [0.12.0](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#0120---2026-03-27): Vault hardening and Kanban GC
- [0.11.0](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#0110---2026-03-26): multi-agent orchestration layer
- [0.10.0](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#0100---2026-03-26): Virtual Capabilities and governance
- [0.9.0](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#090---2026-03-25): Agent Kanban communication board
- [0.8.0](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#080---2026-03-24): memory infrastructure and MCP proxy hardening
- [0.7.0](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#070---2026-03-23): MCP Client Proxy Phase 2
- [0.6.0](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#060---2026-03-23): project renamed to Tachi
- [0.5.0](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#050---2026-03-23): Dual-DB architecture (global + project)
- [0.4.0](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#040---2026-03-18): native Rust MCP server
- [0.3.0](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#030---2026-03-14): `hard_state` and derived-item isolation
- [0.2.0](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#020---2026-03-08): causal worker pipeline
- [0.1.0](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#010---2026-03-05): initial Sigil release

## [Unreleased]

> **Note to maintainers**: Add unreleased changes here during development. Before cutting a release, move the content under a new `## [X.Y.Z] - YYYY-MM-DD` header and update the Quick Navigation above.

### Removed

- Dead recall tooling: `scripts/check-bootstrap-test-split.py` (uninvoked #1714 guard), `tools/zvec-shadow/` (#683 sidecar experiment, closed), and `tools/recall-eval/` (July personal-corpus toolchain that depended on the sidecar; recall-quality evaluation now runs outside this repo, see #2113). The in-repo synthetic golden-corpus regression test is unchanged.

## [2.1.0] - 2026-11-08

**Why 2.1.0 and not 3.0.0** (owner ruling, 2026-11-08): despite the breaking changes below, this is a first-party, single-operator distribution — this release ships Mac Apple Silicon only — every retired surface has an explicit migration path in [docs/UPGRADING.md](docs/UPGRADING.md), and downstream kernel consumers pin `memcore` by git revision rather than by semver.

This cycle simplifies Tachi to a single resident HTTP daemon and a single default agent entry, removes duplicate execution paths, and makes workspace binding, briefings, and Chinese recall more precise. The store schema advances from 39 to 41, so upgrading from 2.0.0 requires the offline migration in [`docs/INSTALL.md` Step 1b](docs/INSTALL.md). Migration steps are in [docs/UPGRADING.md](docs/UPGRADING.md#part-2-changes-on-main-since-200).

### Breaking / migration

- **DB schema 39 -> 41** (sentinels `v40_work_claim_project_binding`, Product scope, and `v41_host_typed_records`, Portable scope). Stop and prevent restart of all old and new readers and writers, back up whole store directories and the 2.0.0 binary, install without serving, then run `tachi migrate --apply` and read every finding; a re-run plan should show every store `up_to_date` at 41. A 2.0.0 binary refuses to open a store stamped 41, so roll back by restoring the backup. Because v41 is Portable, Portable-profile stores opened by a kernel that includes it also advance to 41.
- **First-party Tachi MCP is HTTP-only.** Supported clients and Staff MCP consumers default to the resident Streamable HTTP daemon (#2206). Tachi stdio startup is retired: bare `tachi`, `tachi serve`, process-launch client configs, ACP stdio projections, and `TACHI_DISABLE_STDIO_PROXY` refuse before any store or configuration side effect. Start the daemon explicitly with `tachi --daemon serve`. Third-party Hub MCP subprocesses and ACP protocol pipes keep their stdio transports (#2202).
- **One default agent entry.** Public Ops/Admin profile selectors and every privileged alias are retired and rejected in CLI, environment, and HTTP metadata. Owner CLI commands authenticate each exact tool call with the daemon's private capability, without storing a session grant or widening discovery. The unused Operate catalog is removed. Ordinary agents keep six tools; Worker, read, and legacy `standard` behavior are unchanged. Newly generated client configs and Homebrew service definitions omit the profile (#2212).
- **Upstream MCP tools are exposed directly as `server__tool`**, with upstream schemas, annotations, rich content, and errors preserved. The generic `hub_call`, `hub_set_active_version`, gateway/flatten mode switches, the hand-written remote JSON-RPC proxy, and the duplicate in-memory tool catalog are removed. Generic virtual-capability alias invocation through `hub_call` is retired; existing virtual capability bindings and internal web search remain (#2219).
- **Agent advice and Tachi-owned execution retired**: heuristic `tachi_agents(action="select")`, `tachi_unstick`, `tachi_wiki_organize`, Tachi-owned Prompt Skill execution, and `chain_skills`. `tachi_skill(action="run")` returns explicitly declared document content verbatim. Backend/profile discovery, MCP connections, Vault, and the Skill catalog, review, and export remain; historical data stays readable (#2212).
- **`tachi_gh` and the GitHub control code removed**: `gh_ops`, the `tachi_gh` facade, and the `tachi-gh-safe-merge` / `tachi-merge-ops` crates are gone. GitHub is now a read-only source: `tachi_task` intake and status read issue/PR snapshots, and a bound flow reads an explicitly named PR live when it has no cached snapshot. Tachi no longer comments, merges, ships, writes release notes, or closes loops on GitHub; the closure-debt briefing scan went with `close_loop`. Lead discovery is five facades and Worker discovery four; registered native tools drop from 76 to 75.
- **Legacy OpenClaw plugin removed**, including the memory provider, tools/hooks, plugin-only helpers, npm package, and the dedicated Node/OpenClaw CI lane (#2209). Disable existing OpenClaw installations before upgrading a source checkout; installed artifacts and data are not removed automatically. The old plugin installer refuses without side effects. Native Node bindings remain independent.

### Added

- **Workspace-first memory**: recall aggregates the bound project library, then global memory and the shared Wiki, before lower fallbacks; storage scope for saves is explicit (#2206).
- **Per-call workspace routing**: one global HTTP endpoint isolates each ZCode tool call to its declared workspace, preserving explicit connection bindings and rejecting conflicts before storage routing (#2210).
- **Automatic workspace binding from host context**: per-call `tachiWorkspaceRoot` / `tachiProject` metadata and advertised MCP Roots resolve to the existing project library. Subdirectories and linked worktrees deduplicate; empty, invalid, or multiple libraries fail before a save. Native Antigravity `serverUrl` setup for `.agents/mcp_config.json` and `~/.gemini/config/mcp_config.json` (#2220).
- **Unbound read-only project scope**: unbound HTTP sessions may pass an explicit `project` to read-only `tachi_task` (`brief`, `board`, `status`) and `tachi_staff` (`roster`, `preflight`, `status`, `result`) actions. Unbound `brief`/`board` resolve a registered `cwd` automatically. Cross-project writes stay refused (#2222).
- **Agent presence on the work board**: agent conversations report their work through `brief` and `board`; harness observations, the project roster, and owner-bound run receipts appear in project work views (#2207, #2199, #2200).
- **Durable delivery state in work views**: board, status, and brief distinguish execution completion from result delivery (#2208).
- **Wiki and Guide reuse in briefings**: opt-in `metadata.knowledge_context` on `tachi_memory(action="briefing")` delivers full knowledge units with provenance and omits bodies the client still holds unchanged, after server-side revalidation. Bounded to 32 KiB of units and 64 receipts; no new tool, top-level parameter, or migration (#2217).
- **Usage guidance as an MCP Skill**: the beta MCP Skills extension serves `skill://tachi/SKILL.md` with integrity metadata. Always-loaded server instructions drop from 3,556 to 1,478 bytes; legacy resource reads and fallback guidance remain (#2214).
- **Vault-managed upstream MCP credentials**: owner `mcp login` for public-client OAuth (PKCE, metadata discovery, browser callback, refresh) with encrypted, URL-bound credential records in the existing Vault. Credentials and connection metadata commit in one transaction; a per-credential lock coordinates login, restore, and refresh across processes; a queued call refuses a changed configuration (#2219).
- **Host-defined immutable typed records** in the kernel, installed by schema migration v41 (#2105, #2198).
- **Engineering recall query corpus** measuring expected IDs, rank, misses, noise, and superseded evidence through the real search pipeline (#2216).

### Changed

- **CJK recall precision**: weak lexical matches require proportional Chinese pair coverage, scattered CJK noise is rejected, and short-query partial matches, contiguous subjects, and repeated-character subjects are preserved (#2211, #2213, #2216).
- **Compact task briefs**: duplicate rosters collapse, idle stale clients are counted rather than listed, unrelated legacy board rows are filtered, text previews are bounded, and indexed Wiki evidence renders in Markdown. Briefs reuse status's execution and adjudication tokens (#2211, #2213, #2216).
- The native router contracted from 64 to 59 retained handlers; ordinary input-schema bytes are unchanged (#2212, #2219).
- Codex MCP inventory runs in the admitted worker context, sharing the pinned directory between probe and worker (#2206).
- **CI**: a test that passes only after retry now fails acceptance (#2203); hosted runners reuse compatible Rust builds keyed to the runner image (#2201); DB and search fixtures clone per-run schema snapshots (#2204); the host-build pin moved to the merged canonical revision (#2215).
- The repository contract slims review ceremony for localized, low-impact changes and forbids tautological or prose-scanning tests (#2221).

### Fixed

- **Loop/stuck detection for stdio-proxied and identity-asserting modern HTTP peers**: the stdio proxy opened a fresh daemon MCP session per tool call and modern `2026-07-28` requests got a fresh rate-limit key per request, so the repeat-call warning and the burst `Loop detected` block never fired on the default Claude Code/Cursor path. The proxy now mints one key per connection and sends it as the header-only `X-Tachi-Rate-Limit-Session`; modern peers that assert an AgentIdentity share a bucket derived from it (peers asserting the same identity, client and project share one bucket; modern peers with neither the header nor an AgentIdentity keep a per-request bucket). The header only selects a rate-limit bucket and grants no identity, profile, project, or authority. Repeated identical polling through the proxy is now subject to the same burst limits as legacy direct sessions (audit C1). Follow-up hardening (PR2009 closeout): the identity-derived bucket now takes precedence over a valid `X-Tachi-Rate-Limit-Session` whenever a modern request resolves an AgentIdentity, because every modern request is a fresh clone and a rotated legal header could otherwise let one identified peer shed its own burst window; the header remains the bucket selector for identityless requests.
- **Daily distill**: superseded rows are excluded and candidate vectors hydrated, so vectorized memories no longer fail with `source_drift`; raw control characters inside JSON string literals are escaped; canonical physical DBs are deduplicated and token budgets raised (#2222).
- Read-only DB opens retry transient `SQLITE_BUSY` / `SQLITE_LOCKED`; the work-claim board shows only active, non-stale unbound claims, and background GC prunes session claims (#2222).
- The kernel preserves snapshot lifecycle on current reopen (#2205).
- Exact-entity candidate errors carry bounded context (#2167, #2194).
- MCP connection pools evict every snapshot entry when a server circuit opens, and OAuth login verifies the supplied Vault unlock before resolving a resource URL (#2219).

### Removed

- **Public memcore mirror retired** (#1902). Tachi is public now, so consumers take `memcore` / `portable-kernel` directly from `https://github.com/kckylechen1/tachi.git`, e.g. `memcore = { git = "https://github.com/kckylechen1/tachi.git", rev = "…" }`; Cargo finds the package inside the workspace. The `publish-memcore-mirror` workflow, its script and the `MEMCORE_MIRROR_TOKEN` secret are gone. `github.com/kckylechen1/memcore` is archived read-only, so revisions that are already pinned still resolve.
- Post-2.0 cleanup: dead Kanban card handlers, the `tachi_task` brief handler, unreachable retired-alias arms, unused dependencies and public API, duplicated tests compiled into `tachi-server`, and stale or broken one-off scripts (`scripts/validate_schema.py`, `scripts/backfill_gemini_cli.py`, `scripts/test_memory_server.py`). The provenance `build.rs` no longer reruns on every build in git worktrees.
- Retired OpenClaw implementation and delivery jobs, dead Hub helpers, and obsolete host guidance (#2209).

### Known issues

- A natural-language question built on a fabricated premise still retrieves nearby noise (five rows in the engineering corpus); the corpus records the gap rather than treating those matches as an answer (#2216).
- One existing `workspace_context` test can hit `SQLITE_BUSY` when run alongside other modules but passes in isolation (#2212). Because retry-only passes now fail CI (#2203), this needs a root-cause fix.

## [2.0.0] - 2026-09-25

First mainline release since 1.9.0. The 1.9.1 and 1.9.2 tags were narrow fallback releases cut from side branches, so this entry also carries the mainline work those tags never shipped. The major bump is earned: public MCP routes were removed, the on-disk schema advanced from 28 to 39, and dispatch V2 moved where model plans live.

Distribution: the tag pipeline builds the Mac arm64 CLI tarball and promotes the public Homebrew formula (the formula installs that CLI archive; bottles are a maintainer-only rebuild path). It publishes no OpenClaw plugin asset for this tag, so binary installs from the shell installer need `--skip-plugin`. Windows assets remain pending (#1963), and npm packages (`@chaoxlabs/tachi-node`, `@tachi/cli`) publish through their own manual workflow, so a 2.0.0 tag does not by itself mean 2.0.0 is on npm; treat the git tag and the Homebrew tap as the release of record.

### Breaking / migration

- **Expired sandbox aliases removed**: `sandbox_set_rule`, `sandbox_check`, `sandbox_set_policy`, `sandbox_get_policy`, `sandbox_list_policies`, and `sandbox_exec_audit` are no longer routed, honoring their existing 1.10.0 removal deadline. Use the corresponding `tachi_sandbox(action='set_rule'|'check'|'set_policy'|'get_policy'|'list_policies'|'exec_audit')` actions under the admin profile; ordinary Lead/Worker access does not expand.
- **Removed model-facing MCP routes** (router-level, `tool not found` after upgrade), per group: the memory save/search aliases, direct Kanban routes, and duplicate wiki/brief routes retired in #1799 have their capabilities on the canonical facades (`tachi_memory(action='save'|'search')`, `tachi_task` board actions). The `tachi_orchestrator` and skill recommendation/evolution routes (`recommend_*`, `skill_evolve`, `distill_trajectory`, `prepare_capability_bundle`, standalone `run_skill`) retired in #1800/#1690 are removed without a model-facing replacement: that "second model brain" family was retired end-to-end. Memory administration (#1760) retired eight `tachi_memory` actions for every model profile, with per-action replacements: `progress` moves to `tachi_task(action='status')`; `readiness` moves to `tachi_status`, a retained route reachable only under Ops/admin authorization, not ordinary Lead/Worker discovery; `delete`, `gc`, and `doctor_scan` move to the operator CLI (`tachi delete plan|apply`, `tachi gc plan|apply`, `tachi doctor`); `ingest`, `ingest_source`, and `pattern_feedback` have no model-facing replacement. The Kanban Ollama classifier was dropped as an internal detail (#1737). The ordinary Lead surface is six facades (`tachi_memory`, `tachi_task`, `tachi_agent_eval`, `tachi_staff`, `tachi_gh`, `tachi_a2a`); Worker keeps five (no `tachi_agent_eval`) (#1964).
- **DB schema 28 -> 39** (sentinels v29-v39: memory outbox and destination apply, A2A mailbox with body retention, harness session attachments/spine/receipts, delivery spine, verified agent admissions, CurrentTruth, mirror-eval identity). Follow [`docs/INSTALL.md` Step 1b](docs/INSTALL.md): stop and prevent restart of all old/new readers and writers, back up whole store directories and the **old** binary before replacement, install the new binary without serving (`--skip-daemon-service --skip-plugin` for the shell installer), then plan and explicitly convert old `memory.db` filenames with `tachi migrate --rename-legacy --apply --offline`. Ordinary opens now refuse real legacy filenames instead of renaming them: a pending WAL can contain committed rows that a main-file-only rename would strand. `--offline` is the operator's attestation, not a software fence against old binaries. Filename conversion preserves the existing schema stamp; only afterward run ordinary `tachi migrate --apply` with its independent typed schema authority and read every finding. Exit code 0 is not success: `skipped_locked`, `failed`, and `not_attempted` findings need attention, and a re-run plan should show every store `up_to_date` at 39. Binaries refuse to open a database stamped newer than they support, so once a store is on schema 39 an older binary will not open it: restore from backup instead of rolling the binary back (#1223, #1762).
- **Dispatch V2 plans moved**: the durable model plan is now `<run_dir>/status.json#/model_plan`, published with its exact serving-model invocation receipt in one locked, fenced, revision-checked atomic replacement where first winner wins. A planner failure publishes `state=FAILED` with no `model_plan` only when no valid committed plan, terminal/cancelled state, or newer status revision already won, so a concurrent completion or cancellation winner is never overwritten; `plan.md` remains only the V1 placeholder, never written with model content. V1 single-stage dispatch is unchanged (#1969).
- **Store file rename**: standalone-Tachi stores use `tachi-memory.db`; existing `memory.db` stores require explicit offline conversion with `tachi migrate --rename-legacy --apply --offline`. Ordinary opens refuse legacy files. Conversion leaves a compatibility symlink for this release window (#1132, details under Changed).

### Added

- **Version-bound memory Resources**: eligible ordinary memories in the explicitly bound project receive additive MCP ResourceLinks without changing the original search text. `resources/read` returns exact current text only when source identity, revision, body digest and active time window still match. Global/Wiki/private/ambiguous sources are excluded; role-constrained searches and configured or unreadable sandbox policy disable this feature. Catalogs remain empty and subscriptions are not advertised. Modern/legacy HTTP and stdio are covered; installed Claude Code 2.1.282 was verified against an isolated candidate daemon (#1970, Refs #1531).
- **Presence claims and peer publication**: lease-based presence board with zero-ceremony hooks, advisory file-scope collision warnings, and the read-only `peer_query` broker (`presence` board plus an observe-only `run` projection, reached through observe-class profile bundles rather than the minimal Lead/Worker lists) so same-host sessions can see each other's active work without Tachi owning their lifecycle (#1001, #1016).
- **A2A envelopes** replace the retired read-once note surface with explicit AgentIdentity addressing, issuer-scoped idempotency, and delivery receipts; structured task batons remain HandoffPackets (#1751, #1761).
- **CurrentTruth and the Task work read model**: CurrentTruth assertions record supersession lineage so newer observations retire older ones as cited evidence instead of accumulating as rivals (#1635, #1864). `tachi_task(action='status')` now projects CurrentTruth facts (board column, blocker count, top action) for exactly the resolved refs with conservative private visibility; sources CurrentTruth does not own render typed `unavailable`, never empty or healthy. This is a read-model view for orientation, not a completion verdict, and project-scoped board/brief views are deliberately not wired yet (#1693 gap) (#1968, PR #1868).
- **Private memory partitions**: physically isolated encrypted partitions, one sealed file per `(trust_domain, subject)` with derived paths, vault-injected keys, a capability lattice (Read/Write/Export), and non-disclosing refusals (#1668-#1671).
- **Model broker and provider health**: the broker owns the provider wire directly with SSE stream grammars for OpenAI-compatible and Anthropic streams (chunk-invariance corpus included) (#1682, #1773, #1772, #1786); provider key health, lane binding across Vault overlays, and typed health collection off the async executor (#1966, #1965); chat usage persistence (#1784); a model-call census (#1788); and `tachi broker` alias governance (show, plan, apply) (#1681).
- **Dual-era MCP** peer negotiation alongside the RMCP 3.3 upgrade, with legacy-only session admission (#1947, #1892).
- **Delivery spine**: requester-side claim/ack/block/resume durable receipts (#1679), plus a memory outbox with conformance-tested destination apply and health repair (#1666, #1718, #1665).
- **Harness session spine**: attached session events, spine receipts, and SQLite extension loading for local builds (#1678, #1745).
- **Verified agent admissions** (#1938) and a mirror-eval ledger feeding per-agent/per-profile routing scores from host-native spawn receipts, so Tachi never owns worker lifecycles (#1066, #1249, #1675), with a native candidate-experience evaluation loop (#1964).
- **Graph ontology and observation ledger**: edge relation ontology v1, `ensure_anchor`, retirement of the fuzzy `auto_link` fog, an append-only `edge_observations` ledger, exact-dedupe durable receipt/restore with graph-edge transfer (#773, #774), and as-of edge validity for temporal reads (#1845).
- **Issue Refinery v1**: immutable evidence capture, disposition proposals, and a freshness layer (zombie scan, stale-candidate heuristics) with briefing queues and GitHub write-back (#1000, #1002), plus GitHub corpus refresh (#1696).
- **Dispatch evidence spine**: `dispatch_outcomes` execution facts, append-only `dispatch_adjudications` keyed by outcome, frozen dispatch identity receipts, verdict-driven terminal accounting, and principle-level precedent candidates decomposed from completion rulings (#1024, #1035, #1065, #1076).
- **Execution environments**: resource ledger (bytes, not rows), env classes with a serialized build broker, a report-only orphan reaper, and an authority contract compiler certified by an executed kill-test (#894), wired through exec-env postflight (#1322).
- **Staffing hardening**: LaunchSpec for managed custom dispatch (#1824), a nested canonical launch kernel (#1822), typed staffing parameter boundaries (#1804, #1812, #1821), and managed cancel (#1825). The host harness keeps ordinary native subagent execution; Tachi owns memory, policy, and receipts.
- **`tachi migrate`** — cross-library schema sweep with per-apply liveness re-judgment (#1223).
- **Typed lane-card declarations**: `tachi cards sync|list` mirrors dispatch-ledger cards read-only and projects countermeasures into dispatch prompts; the TOML seed layer is retired (#1202).
- **Wiki reviewed-projection lifecycle** with provenance and truthful retrieval states (#1072).
- **Surface-scoped recall**: Memory/Docs classifier plus `SearchOptions.surface`, evidence-gated decision boost, and a read-only GitHub corpus source adapter producing idempotent evidence and Pending candidates (#1059), with doc-classification receipts (#1537, PR #1943).
- **`tachi vault exec`** — run a child command with Vault-delivered credentials: Vault only fills missing env names, `--require` gives fail-loud pre-spawn gating, injection diagnostics are name-only, and the child exit code propagates. By default it is fail-closed: if the Vault cannot be unlocked or its environment cannot be loaded, it refuses to spawn a credential-less child before exec; `--allow-unauthenticated` opts back into running with the inherited environment (#1316, #1384, #1413 concern 4).
- **LLM chat-lane cross-provider fallback** with loud `LANE_OUTAGE` recording on full-chain exhaustion, plus an extract-lane discriminative quality golden (#1197, #1198).
- **Provider-key allowlist admissions**: `ZHIPUAI_API_KEY` (opencode GLM lane) and `KIMI_API_KEY`, plus env-substitution for vault-backed lane providers (#1355) and official DeepSeek v4 lanes (#1853).
- **Memory hygiene**: near-duplicate raw merge proposals (light-sleep detection port), the raw tier included in vector backfill behind a gate with a per-tier similarity floor, and vector backfill made portable across maintenance paths (#775, #788, #1242).
- **Campaign handoff verb**: `tachi_gh(action='handoff_draft'|'handoff_publish'|'handoff_repair')` plus its restricted `issue_close`, which only closes issues carrying the handoff label (#1285).
- **GitHub safe-merge**: `tachi_gh` safe-merge checks run with lifecycle and verification evidence (#1847, #1849).
- **Public memcore mirror**: `memcore` and the portable kernel publish to a public read-only mirror (#1716).
- **Build-resource governance**: packet guard, doctor patrol, and the worktree-location law for shared build caches (#1184).

### Changed

- **Store file rename**: standalone-Tachi SQLite stores change `memory.db` -> `tachi-memory.db` (`~/.tachi/global/`, `~/.tachi/projects/<name>/`, repo-local `.tachi/`), per the owner-ratified naming rule "system in the filename, scope stays in the directory" (#1132). Existing old-name stores require an explicit offline `tachi migrate --rename-legacy --apply --offline` conversion before the separate schema upgrade; ordinary opens refuse to rename them. After SQLite recovery and verified conversion the old name becomes a relative compatibility symlink for one release window. The `memory-server` `[[bin]]` PATH-compat alias (unused since the #890/#912 brand rename; never distributed) is retired in the same change.
- **Claude CLI pool retired**: background extraction/summary/distillation lanes now call configured API providers directly (with the cross-provider fallback above); the CLI spawn path and the `claude_pool` fallback chain were removed (#1261). Ordinary reasoning keeps its CLI-first lane with the configured API fallback.
- **Lead surface widened to six facades**: `tachi_agent_eval` joins `tachi_memory`, `tachi_task`, `tachi_staff`, `tachi_gh`, and `tachi_a2a` in the default `standard` profile; Worker (`delegate`) discovery intentionally excludes it (#1964).
- **Shell contracted to dispatch and status**: kanban translation, vendored-skill resolution, and flow-artifact path safety were relocated out of Shell onto their owning surfaces (#1319).
- **Recall receipts carry measured latency**: pool-checkout wait, spawn-queue, search, and write phases are separated in receipts, and symbolic candidate retrieval is bounded at large corpora (#1093, #1125, #1255).
- **Facade output economics**: `tachi_status` and the memory facade's search output default to markdown; the recommend/profiles/card read endpoints are slimmed; agent briefing defaults to the compact packet and collects typed health off the async executor (#527, #1201, #1966).
- **Capture lifecycle**: session capture is idempotent with typed retention, an opt-in capture archive sweep is available, and write-only namespaces are TTL'd with fail-closed lease checks (#1301). MCP arguments are redacted from durable auto-ingest staging (#1763).
- **Instruction surfaces**: projected host instructions now consume their owning canon instead of drifted copies (#1936), with an instruction-drift sentinel watching for divergence (#1304, #1710).
- **Intake and Vault accounts**: intake provider accounts (#1859), a Vault secret list health board (#1858), account binding fixes (#1870), lane config typing and precedence (#1862, #1863), slot rebind (#1861), and alias integrity (#1860).
- **rusqlite 0.40** for downstream matrix-sdk 0.19 compatibility (#1962); Rust toolchain pinned in CI (#1886).
- **CI gates**: a Format Gate workflow, a known-deterministic-reds nextest group with a JUnit gate, and the `nextest-census.sh` failure-signature script; vendored-corpus tests are marked provisioning-dependent (#1278, #1378). The known-reds gate matches the EXACT full test paths (anchored, no substring/prefix absorption) and refuses a valid-but-incomplete (truncated) JUnit before declaring OK (#1413 concern 5).

### Fixed

- **Save-to-retrievability contract**: recall-cache invalidation choke point and epoch-fenced write-through, so a fresh save is immediately recallable (#2059, PR #1361).
- **Memory quality wave**: summary-queue failures and historical evidence are preserved instead of dropped, L0 summaries are bounded and revision-safe with a repair path, per-item status survives in historical summaries, and project identity plus scoped graph recall stop cross-project leakage (PR #1967).
- **Project identity repair**: fail-closed gen-2 recovery with resolver/manifest invariants (#1356), plus physical-DB identity guards on Windows (#1890).
- **Delivery and merge safety wave**: unmerged closed PR worktrees and work beyond merged PR heads are preserved instead of cleaned (#1908, #1909, #1923); ambiguous automatic worktree mappings, ambiguous active claims, and worktree HEAD drift from the active claim are refused rather than guessed (#1911, #1915, #1913); executor warnings, registered-PR uncertainty, and explicit label review targets survive reconciliation (#1927, #1926, #1925); terminal worktree reconciliation cleans branches and auto-resolves (#1839); Unix status mutations are fenced across processes (#1922); docs refuse lossy frontmatter changes and publish complete new documents without clobbering (#1921, #1918); status no longer infers context absence from missing links and surfaces project store role conflicts (#1919, #1931); alias named-project attach binds to the correct project store with write-hold forensics (#1826, #1588).
- **Vault**: auto-lock clears only the vault key (unified `0=never` sentinel, #400); stored `kdf_params` are enforced at every key-derivation site with a fail-closed algorithm check; honest no-TTY errors and a Keychain path for MCP `vault_unlock`; locked-retention fixtures isolated from the host Keychain (#1894).
- **Recall**: vector KNN over-fetches past the archived post-JOIN filter (#1245); pure-Han FTS fallback candidates recovered; atomic dedup for id-less memory saves.
- **Daemon and lifecycle**: `tachi serve` exits on SIGTERM with per-parent age-ordered reap dedup (#1273); daemon locks unified and live-DB copies guarded; doctor fails closed when DB ownership is indeterminate; WAL checkpoint copy fails honestly (#1118, #1323); zero-interval scheduler busy spin prevented (#1806); bootstrap CLI stack overflow fixed (#1734).
- **Schema-init ordering escape**: evolution-column indexes, dedup-before-unique, and a 3-way schema oracle (#1289).
- **Provider secret materialization**: per-alias tolerance with loud skip observability (#1279); frontline provider selection stays bound across Vault overlays (#1965).
- The orphan reaper's destructive path was re-sheathed (report-only) pending fence repair (#1379).

### Security

- Recursive-dispatch depth gate v1 against accidental runaway dispatch, and bounded untrusted dispatch prompt inputs (#1251).
- Credential and embedding/rerank endpoints guarded against misconfigured lanes (#1774, #1780).

## [1.9.2] - 2026-08-05

Local-fallback release. Mirrored from the GitHub release notes for this tag.

### Added

- **Wiki legacy adoption**: `tachi wiki corpus --adopt-legacy` bootstraps-and-adopts the legacy wiki corpus; legacy usage counters are loaded verbatim instead of zeroed, adoption reads refuse before touching legacy state, and failure cleanup is scoped to run-created files. The path carries unit tests plus an end-to-end discriminator (#1624).

### Fixed

- Wiki search refuses loudly when it resolves zero stores; named lookup does not shadow the refusal, and stale-search assertions are non-vacuous (#1624).
- The pooled LLM client no longer honours ambient proxies, closing a chat-lanes flake and a credential-exposure risk (#1621).

### Changed

- 87 contract/census tests moved to the new `tachi-contract-tests` workspace crate; the known-reds and census gates were widened to it, and dead-code waves removed superseded seams and the zero-test bin target (#1610).

## [1.9.1] - 2026-08-05

Local-fallback release (Actions exhausted). Mirrored from the GitHub release notes for this tag.

### Added

- Portable snapshot import (#1607), released alongside the `portable-kernel-v1.9.1` tag.
- Gitleaks CI gate covering the rerouted pull-request lanes (#1578, #1553).

### Changed

- Arena retirement wave (#1319 D2/E2): the `tachi_arena` facade and mission ledger were deleted, generated runtime Staff guidance was corrected, and the final staffing projection landed with dead-code cleanup and a contraction census.

### Fixed

- Receipt content binding (#1558): wiki receipt attach is revision-guarded against concurrent writers, with `Cargo.lock` regenerated for the tachi-llm blake2 edge.
- Daily-distill revival (#1605): stale-close is refused on undecidable metadata probe errors, the registry re-check/delete race is closed under the lock, and the ordinary close path deletes the exact registry row.
- Known-reds honesty (#1278): the roster was retired once both `board_first` survivors went green; the gate script is pinned to C collation and accepts an intentionally-empty roster.

## [1.9.0] - 2026-07-11

### Added

- Portable-server and execution-environment foundations, including leases, provisioning, and dispatch environment binding (#924, #938, #976).
- Precedent-memory capture from completion rulings, plus a first-class evidence path for runtime judgment (#950, #962).
- Dispatch receipt-first status, board persistence, and completion artifacts that prevent orphaned work from being reported as complete (#971, #988).
- New memory lifecycle and recall capabilities: research feed mode, rerank provider seams, recall evaluation, and write-side enrichment (#530, #724, #921).

### Changed

- **`memcore` `SearchOptions` gains a `decay_policy` field and `DecayPolicy` now requires `Send + Sync`** (#891), so downstream policy injection can reach the live hybrid search path without forking `scorer.rs`. This is a source-breaking change for downstream code that builds `SearchOptions` via a full struct literal (add `decay_policy: None` or use `..Default::default()`) or implements `DecayPolicy` on a non-`Send`/non-`Sync` type — intentional, by design, to keep the trait usable behind `Arc<dyn DecayPolicy>` on the ranking hot path.
- The public tool surface was consolidated around the remaining memory and dispatch facades; obsolete aliases and reach-through layers were removed (#757, #974).

### Fixed

- Fail-closed authorization and remote-MCP safety checks, daemon liveness behavior, project-DB migration boundaries, and bounded daemon/project attachment behavior (#894, #947, #969, #978, #984).
- Recall-ranking, provider degradation, vault, proxy, and release-signing defects found in the post-1.8.0 hardening wave (#724, #936, #937, #958, #970, #979).

## [1.8.0] - 2026-07-10

### Added

- Native personal recall eval gate: `eval recall` CLI replaying `/eval` cases through `recall_simulate`, aggregate-only status artifact, `--min-recall`/`--min-mrr` fail-closed gate (#920, #922).
- Library binding receipts with loud `single_db_mode` warning across search, briefing, runtime_info, and `tachi_search` (#896, #898, #900).
- Cross-library project rank preference for project-bound sessions, env-tunable boost (#899, #902) with same-store precision guards (#903).
- Ops-audit recall discrimination suite with permanent red-baseline fixtures (#897, #901).
- Memory consolidation propose/review/apply lifecycle (#775, #904); `auto_link` related_to floor raised (#773, #905).
- GitHub CI-state ingest: background `ci-watch` poller writes check-state transitions; `safe_merge` consumes ingested state instead of re-polling (#605, #816).
- Managed worktree open outside TCC-protected paths + reclaim-on-merge disk-governor slice (#484, #910).
- Component cutover planner and briefing/status surfacing for stale component governance (#798, #799, #883).
- HTTP direct-connect cookbook and health reconnect for capable MCP clients (#732, #889); library identity runtime contract (#746, #888).
- Portable hybrid-floor RRF rerank blend and injectable `DecayPolicy` for downstream kernels (#891, #893).
- Action-level `ToolProfile` gating at the `call_tool` choke-point with fail-closed defaults and compile-checked action enums (#495, #918, #919).

### Changed

- MemCore / Tachi crate branding: `memory-server` → `tachi-server`, `memory-core` → `memcore`, `memory-server-params` → `tachi-params`, with a `memory-server` PATH-compat bin alias (#890, #912, #923).
- Skill-pack system retired (#908); vendored skill corpus decoupled from the git tree via central-library resolution at seed time (#894, #895, #909).
- Facade response economics F0–F2: tool inventory, compact verify responses, GH lifecycle dedup between `tachi_task` and `tachi_gh` (#527, #918).
- Deslop S0: removed `unused_imports`/`dead_code` allows across the workspace (#913, #916, #917).

### Fixed

- Memory hermeticity and exact-rank-preservation fixes from the #900–#909 batch cross-vendor review (#911, #915).
- Dispatch verification lanes: completion predicate, headless verify, leader-readable run artifacts, char-count truncation (#878, #881, #882, #885).

### Security

- Supply-chain hardening for the release pipeline: pinned third-party actions, fatal binary version probe, newline-anchored Homebrew formula regex (#907, #914).
- Worktree cleaner hardened: fail-closed dirty-guard, path boundary fencing, `--end-of-options` ref validation, TOCTOU shrink (#910).

## [1.7.0] - 2026-07-08 — portable memory kernel, vector auditability, and dispatch canon

Minor release. Tachi becomes the shared memory kernel for downstream products
(Hypermem trading adapter, zeroclaw chat-agent adapter, RomanBath frontend)
instead of being forked per product, makes vector backfill/sweep auditable and
read-only in dry-run, and ratifies the dispatch canon so the law lives in files
rather than the leader's head.

### Added

- **Portable memory kernel manifest** (`docs/engineering/architecture/kernel-surface-v1.fixture.json`) freezes the durable schema, recall primitives, vector/FTS fallback behavior, and readiness diagnostics as a product-agnostic contract. Downstream adapters consume it without inheriting Tachi's GitHub/dispatch/release surfaces. A dogfood harness proves downstreams can exercise the kernel without any product workflow dependencies. A separate manifest global-DB write-guard runs at startup (WalOrphan check); `TACHI_BYPASS_MANIFEST=1` skips that guard for development or crash-recovery.
- **Portable chat-agent memory contract** documented (`docs/engineering/architecture/host-adapter-lifecycle-v1.md`) so generic chat-agent adapters assemble stable memory, immediate recall, and reflection around Tachi surfaces without importing product workflow tools.
- **Hypermem kernel convergence gate** documented (`docs/engineering/architecture/hypermem-compatibility-gate.md`) bounding the trading adapter cutover: A-share decay and session policy stay downstream; the kernel stays product-agnostic.
- **Harness-native subagent eval evidence** is now recorded without owning the subagent lifecycle, feeding per-agent/per-profile score attribution into routing.
- **MCP surface death-list** (`docs/engineering/architecture/mcp-surface-death-list.md`) ratifies the keep/quarantine/delete decision for all routed tools, with a staged leaf queue.
- `tachi_gh safe_merge` now records CI check state during dry-run previews as an auditable artifact.

### Changed

- **Dispatch canon ratified.** `AGENTS.md` is now the thin injection kernel every backend lane reads at turn zero, subordinate to the canon `docs/engineering/architecture/dispatch-lifecycle.md`. The #733 read/write-asymmetry rule is folded into clause 3 as law: a write-guard that also blocks reads the invariant doesn't require is over-reach.
- **Vector backfill dry-run is now fully read-only** — returns before any write, proving no `vector_sweep_state` creation and no schema/metadata/migration-marker mutation on an already-compatible DB.
- **Vector sweeps are auditable per database** with durable schedule, failure state, and truth preserved across no-op updates and failures; skipped rows are no longer counted as failures.
- `tachi mcp add` reserves seat-policy fields so one-command MCP registration converges per-agent config safely.
- Proxied cross-project reads are now allowed, merging global plus project results.
- Workspace scope is routed from the real cwd before falling back to `PWD`.
- CI runs `actions/setup-node` v5.

### Fixed

- **Plan C alias identity drift** (#736): a casefold hash change left old-hash alias dirs and manifest labels never reconciling to the new derived name, so one DB could appear under two names. Resolved via label-recompute + count-basis; the unsafe FS retirement was deliberately dropped.
- **Rerank no longer evicts hybrid top hits** (#779) — promotions are preserved in flip reports so a strong lexical/symbolic match isn't displaced by a marginal vector rerank.
- **Shared cargo target collision warning** (#727/#751): concurrent same-crate worktrees building into one `CARGO_TARGET_DIR` can collide on metadata-hashed test binaries; lanes are now warned to use isolated targets for reviewer/discrimination runs.
- **Vault sync bundle offline guessing risk** (#780, refs #576, MEDIUM): the sync bundle contained the verifier, enabling offline password-guessing — surfaced and bounded.
- `alias_drift` tests no longer skip on Linux `/tmp` paths; repo-local DB fixtures are guarded from tmp-root skips.
- Stdio proxy search row shape is pinned; direct-read project selection is preserved after the proxy refactor.

## [1.6.4] - 2026-07-06 — multi-project daemon stdio repair

Patch release for restoring project-directory MCP sessions after the 1.6.3
stdio daemon proxy dogfood exposed the global-only daemon compatibility gap.

### Changed

- Stdio MCP proxying now treats project-bound sessions as pure transport to the
  resident daemon: the adapter validates the fixed project binding, refuses
  cross-project overrides, and injects the bound project only where project
  defaulting is required.
- The daemon runtime lazily attaches project databases on demand, keeping one
  resident daemon as the owner of global and project DB files instead of
  spawning direct project writers from each agent session.

### Fixed

- Project-directory stdio sessions can reuse a global-only daemon without
  fataling on incompatible scope, while still writing to the correct project DB.
- Project-bound reads now merge global plus project results, so global memories
  no longer disappear when a session carries an explicit project binding.
- Project-bound delete and archive calls try the project DB first and then fall
  back to global, preserving the pre-proxy behavior for global IDs.

## [1.6.3] - 2026-07-06 — recall quality overhaul and kernel reliability

Recall precision was measured at 8/20 on a fair corpus against its own memories while a plain BM25-OR baseline scored 20/20. This release diagnoses the causes, installs a permanent golden-corpus evaluation harness, and repairs the lexical channel — bringing fair-corpus recall to 20/20 — alongside a batch of kernel-reliability fixes.

### Added

- **Recall quality architecture** (`docs/engineering/architecture/recall-quality-architecture.md`) and a permanent golden-corpus regression harness: a synthetic in-repo fixture with ratcheting recall@k/MRR floors plus a personal-corpus evaluation toolchain (`tools/recall-eval/`) driving `recall_simulate` variant matrices, per-miss attribution, and aggregate flip reports. Every future recall config or scoring change is now gated on this harness.
- The FTS OR-fallback now handles CJK: contiguous CJK runs join as quoted phrases so partial-coverage Chinese queries recover candidates instead of being zeroed by conjunctive matching. The fallback is starvation-gated (fires only when the conjunctive expansion returns zero candidates) and enabled by default (`or_fallback_fts_score_factor` 0.55).
- `CONTRIBUTING.md` and an automated governance workflow: pull requests opened by anyone other than `kckylechen1` are now automatically closed with a link to the contribution policy.
- Missing `AGPL-3.0-only` license metadata was added to `packages/tachi-cli/package.json` and `tools/cleaner/Cargo.toml` so every shipped package consistently declares the project license.
- `tachi vault intake discover` now reports redacted, read-only credential candidates from env-family files and Codex auth metadata without unlocking or mutating Vault.

### Changed

- `tachi_gh(action="ship")` now provides a deterministic ship primitive that stages an exact file list, commits caller-authored messages verbatim, refuses protected branches and unchanged/missing files, pushes feature branches when `origin` exists, and optionally opens/links a PR without drafting text.
- `tachi_skill(action="run")`, native `run_skill`, direct skill tools, and skill DLQ retry now return a JSON simulation envelope instead of bare skill text for LLM/mock skill runs. Simulated outputs are prefixed with an explicit marker warning that no commands, files, or tests were executed; `chain_skills` preserves raw step-to-step piping while adding chain-level simulated/warning provenance. This is a breaking response-shape change for consumers that parsed raw text.
- Built-in Superpowers and Waza workflow skills now run in document mode, returning the workflow document for the caller to follow with their own tools instead of asking the LLM to fabricate execution results.
- Retired the SFT factory daily fine-tuning dataset generator because it had no automated consumer and duplicated issues, PRs, docs, and Tachi memory.
- Review-stage dispatch profiles default `auto_capability_bundle` off unless the caller explicitly passes `auto_capability_bundle=true`.
- Fold `get_memory`, `tachi_board`, and `tachi_dispatch` out of daily agent profiles in favor of `tachi_memory(action="get")` and `tachi_task(action="board"|"dispatch")`, while keeping the native routes admin/backcompat-only.
- Retire the old `tachi_progress_check` and `wiki_browse` observe aliases; agents should use `tachi_unstick` and `tachi_browse`.
- Compact briefing responses now use real `tachi_status` warnings, omit doctrine/limit metadata, skip empty sections, and apply a 0.25 relevance floor; `tachi_status` now uses a verdict-first healthy provider shape, and feature briefings deduplicate top-level wiki hits already present in `doc_index`. This is a response-shape change for consumers that relied on `layer_authority`/`limits` in compact briefing or full provider timestamps when healthy.

### Fixed

- **Recall ranking is now deterministic on score ties.** A missing stable secondary sort key let identical queries return different orderings across runs (polluting every eval metric and eroding recall trust); ranking now breaks ties by score → parsed-instant timestamp (newest first) → id, with mixed-timestamp-format correctness covered by tests.
- **Kernel reliability (#520): stdio sessions no longer open the DB directly.** They proxy writes through the resident daemon (one writer per DB file), a time-based idle reaper reclaims abandoned direct-DB-holding stdio sessions, and a version-skew guard refuses/defers a resident daemon built from a stale binary (deferring during A-share trading hours). Together these close the `SQLITE_BUSY` lock-storm and process-pileup sources.
- Schema migrations now back up the database file (via the SQLite Online Backup API) before the first migration write, replacing the previous rollback machinery.
- Memory text is scrubbed of secret patterns before any external embedding/LLM call.
- `claude-pool` slug labels are truncated at segment boundaries with a short hash suffix, preventing collisions and UTF-8 boundary panics.
- `tachi_gh safe_merge` now records already-merged pull requests as `merge_state="merged"` instead of overwriting lifecycle state with a blocked merge attempt.

## [1.6.2] - 2026-07-05 — release drift and CI guardrails

Patch release for closing release-surface drift after `tachi-server` moved to
`1.6.2`.

### Changed

- Release-facing Cargo, npm, lockfile, installer, README, OpenClaw, prompt, and current-state version fields are aligned to `1.6.2`.
- Full CI now runs automatically on pull requests and pushes to `main`, keeping the release version sync check live instead of manual-only.

### Fixed

- Native module platform package publishing now fails on `npm publish` errors instead of swallowing failed publishes.

## [1.6.1] - 2026-06-29 — Homebrew daemon startup and tap automation

Patch release for making the Homebrew install path safer on macOS launchd and restoring tag-driven tap updates.

### Changed

- `serve --daemon --no-project-db` now defers manifest startup hygiene and scopes daemon background jobs to its own DBs so the Homebrew service can bind to the global daemon without synchronously scanning project entries under protected folders such as Desktop.
- The Homebrew tap update workflow now runs automatically on `v*` tag pushes while retaining manual dispatch for patch retries.
- Release-facing Cargo, npm, installer, README, OpenClaw, and current-state version fields are aligned to `1.6.1`.

### Fixed

- Homebrew formula generation now emits the modern single-binary install path, current `tachi hub` smoke tests, and a `brew services` daemon block for the global no-project service.

## Older releases

Releases 1.6.0 and earlier are preserved verbatim in the [full historical changelog at commit 87d8a0f](https://github.com/kckylechen1/Tachi/blob/87d8a0ffada74a5073f4df5a4e8d3b7e8df31f9d/CHANGELOG.md#160---2026-06-29). Use the Quick Navigation links above to jump to a specific version.
