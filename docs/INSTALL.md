# Tachi Installation Guide (Agent-Readable)

> **For AI Agents**: Read this document and follow the steps below to install and configure Tachi as your persistent memory backend. No human intervention required.

> **Distribution trust (#874):** the public Homebrew tap
> (`kckylechen1/homebrew-tachi`) is **install-only** — a binary-first formula
> (it downloads the release CLI archive) and checksums. It is **not** product
> design truth, capability authority, or an agent-readable issue forum. Source
> development and issue/PR workflow live on the maintainer-controlled private
> source surface. See
> [`docs/engineering/architecture/release-distribution.md`](engineering/architecture/release-distribution.md).

## What is Tachi?

Tachi is a local-first context and memory database for AI agents. It runs as an MCP (Model Context Protocol) server that gives you persistent memory, hybrid search, a capability hub, encrypted vault, inter-agent messaging, workflow dispatch, verification ledgers, and more — all from a single binary with zero external database dependencies.

Current architecture:

- `memcore` owns SQLite storage, migrations, hybrid search, graph/domain state, packs, vault metadata, and sqlite-vec integration.
- `tachi-server` owns the MCP/CLI surface, profile filtering, Hub routing, dispatch/workflow tools, wiki operations, vault encryption, daemon locking, Foundry background work, and verification/staffing/eval ledgers.
- Hosts should connect through the profile-based MCP surface instead of exposing every low-level tool by default.

---

## Step 1: Install the Tachi Binary

### macOS (Homebrew — Recommended)

Public install is a **prebuilt binary** from the public tap
(`kckylechen1/homebrew-tachi`). No Rust toolchain is required. Currently
**Apple Silicon (arm64)** only.

```bash
brew tap kckylechen1/tachi && brew install tachi
```

After upgrade, **verify the running daemon**, not a local cargo binary (#728):

```bash
tachi --version          # stamped version+git_sha_short
tachi status             # runtime.build.git_sha / runtime.binary
# Compare runtime.build.git_sha (or daemon /health git_sha) to the intended release.
# Prefer brew upgrades outside A-share hours on trading-coupled machines.
```

### macOS, Apple Silicon (Shell Installer)

The tag pipeline publishes exactly one prebuilt CLI archive per release:
`tachi-v<version>-aarch64-apple-darwin.tar.gz` (Apple Silicon macOS). There is
no prebuilt Linux, Intel macOS, or Windows asset from this tag. A separate
Windows native workflow exists, but is not this tag's prebuilt release asset.
Other platforms can build from source (`cargo build --release`), without
release verification here. The v2.0.0 tag
also publishes **no OpenClaw plugin asset**, so binary-only installs must pass
`--skip-plugin`:

```bash
bash -c "$(curl -fsSL https://raw.githubusercontent.com/kckylechen1/tachi/v2.0.0/scripts/install.sh)" -- --skip-plugin
```

On macOS, the shell installer also installs and restarts a user LaunchAgent for
the global Tachi daemon:

```text
~/Library/LaunchAgents/com.kckylechen.tachi.daemon.plist
```

That service runs `tachi --daemon --port 6919 --global-db ~/.tachi/global/tachi-memory.db
--no-project-db` with `TACHI_DAEMON_IDLE_TIMEOUT_SECS=0`, so it is a stable
global/background worker on the canonical HTTP MCP port and does not accidentally
bind itself to the directory where the installer was run. It keeps projection,
vector sweep, Foundry, and daily maintenance alive after the installing shell
exits. The LaunchAgent uses `RunAtLoad` **and `KeepAlive`**: if the daemon exits
— including the fail-loud serve contract and liveness watchdog exits (#936) that
fire when the HTTP/MCP surface is dead while background workers still run —
launchd respawns it (rate-limited by launchd's ~10s default `ThrottleInterval`).
Tachi's singleton lock keeps only one daemon per global DB, so the respawn never
races a live instance. Skip service installation with `--skip-daemon-service`
when you will manage the resident HTTP service yourself.

### Verify Installation

```bash
tachi --version
# Expected: tachi 2.0.0

tachi daemon status
# Expected on macOS after the full installer: daemon running
```

If the command is not found, ensure Homebrew's bin directory is on your `PATH`:

```bash
export PATH="/opt/homebrew/bin:$PATH"
```

### Daemon Service Operations (macOS)

The installer-managed service is a user LaunchAgent, not a root daemon. To
restart it manually:

```bash
launchctl bootout "gui/$(id -u)" ~/Library/LaunchAgents/com.kckylechen.tachi.daemon.plist 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" ~/Library/LaunchAgents/com.kckylechen.tachi.daemon.plist
tachi daemon status
```

To disable it:

```bash
launchctl bootout "gui/$(id -u)" ~/Library/LaunchAgents/com.kckylechen.tachi.daemon.plist 2>/dev/null || true
rm -f ~/Library/LaunchAgents/com.kckylechen.tachi.daemon.plist
```

Logs live under `~/.tachi/logs/launchd-daemon.out.log` and
`~/.tachi/logs/launchd-daemon.err.log`.

Homebrew remains the recommended way to install and upgrade the binary. The
release formula update script also emits a `service do` block so the tap can
support `brew services restart tachi` once the formula is published with the
service definition. The shell installer keeps the LaunchAgent fallback because
older/private taps may not expose a trusted service block yet.

The current installer configures the resident HTTP service only. OpenClaw plugin installation is retired; `--skip-plugin` remains a compatibility no-op and `--dir` refuses before installation side effects.

---

## Step 1b: Upgrading an Existing Install to 2.0.0 (schema 39)

v2.0.0 moves the on-disk schema from 28 (v1.9.x) to 39, and older binaries
refuse to open a newer-stamped store, so the upgrade order matters: stop every
writer and prevent restart, back up the whole stores and old binary, install the
new binary **without letting it serve**, convert old filenames offline, migrate
the schema, verify each finding, then restart. A `tachi migrate` exit
code 0 only means the sweep ran; success is what the findings say. This section
is the primary upgrade guidance; the README files and CHANGELOG link here.

**Offline requirement:** Old binaries do not honor this release's filename
conversion lock. The operator must stop **all** old and new readers/writers
(including MCP clients), disable their supervisors and scheduled restarts, and
keep them stopped until both filename conversion and schema migration finish.
`--offline` records that attestation; it cannot enforce it. Prior automatic
rename of only the main file could strand committed `memory.db-wal` rows.
The explicit converter uses SQLite recovery/checkpoint before moving a
self-contained main file; it refuses ambiguous/orphaned sidecars.

The procedure below applies to the arm64 macOS CLI release. Inventory all
stores first: `~/.tachi/global/`, manifest-addressed projects, repo-local
`.tachi/` stores, and any custom `MEMORY_DB_PATH` / `--global-db` paths.
Cross-check the manifest with running processes because it may omit the global
store. Ensure enough free space for a separate backup of each store directory.

### 1. Stop the actual service manager and every reader/writer

Identify how **this installation** starts Tachi. A launchd `KeepAlive` agent
respawns a bare-killed process. Use the one matching stop command:

```bash
brew services stop tachi   # only if Homebrew services manages this daemon
```

Or, if the shell installer's LaunchAgent manages it:

```bash
launchctl bootout "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.kckylechen.tachi.daemon.plist"
```

For an unmanaged daemon only, use `tachi daemon kill`. Stop editor/CLI MCP
hosts with legacy process-launch entries, scheduled jobs, and every other reader or writer of
these stores. Check `tachi daemon status` and the actual service manager: no
daemon may still be running or configured to respawn. If a stop command fails
or another writer remains, resolve that before copying or migrating. Do not
copy a live WAL database.

### 2. Back up stores and the **old** binary, before any upgrade

While the installed binary is still v1.9.x and all stores are quiescent, copy
the binary and the **whole** store directories (including SQLite sidecars,
vault, manifest). Repeat the project copy for every inventoried repo; also
copy every custom DB path and its sidecars. Choose a backup destination with
enough free space and keep it separate from directories you will restore:

```bash
backup_dir="$HOME/tachi-pre-v2.0.0-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$backup_dir"
tachi --version                    # confirm this is the OLD binary
cp -p "$(command -v tachi)" "$backup_dir/tachi-old"
cp -R "$HOME/.tachi" "$backup_dir/home.tachi"
# Repeat for each repo: set repo to its absolute path and choose a unique destination.
repo=/absolute/path/to/repo
cp -R "$repo/.tachi" "$backup_dir/project.tachi"
```

The old binary is only for use against restored **pre-migration** backups; it
cannot read schema 39.

### 3. Install the new binary without starting the daemon

Homebrew users (after the service is stopped and old binary is backed up):

```bash
brew update && brew upgrade tachi
```

Or, for the shell installer, suppress daemon start and plugin download (no
v2.0.0 plugin asset exists yet):

```bash
bash -c "$(curl -fsSL https://raw.githubusercontent.com/kckylechen1/tachi/v2.0.0/scripts/install.sh)" -- --skip-daemon-service --skip-plugin
```

The standalone `tachi-v2.0.0-aarch64-apple-darwin.tar.gz` CLI archive is an
alternative. Verify `tachi --version` says 2.0.0 **before** migrating, and
check the service remains stopped.

### 4. Convert old filenames offline, then plan and apply the schema

First plan filename conversion, review every `phase`, then perform the
**filename-only** operation under the offline attestation. A plan never opens
SQLite for write. The apply verifies committed rows and schema stamp without
upgrading the schema; `converted`, `recovered_link` and `already_converted`
are the successful outcomes. `blocked` or `failed` requires stopping and
reconciling the affected store, even if another store succeeded. A missing
source and target is not permission to create a replacement. Ordinary
`MemoryStore` opens of real `memory.db` refuse until conversion is complete.

```bash
set -o pipefail   # preserve the conversion failure exit status through tee
tachi migrate --rename-legacy --json | tee filename-plan-v2.0.0.json
tachi migrate --rename-legacy --apply --offline --json | tee filename-apply-v2.0.0.json
```

Next, run the independent **schema** plan/apply. An immutable plan refuses a
definitive `up_to_date` finding if a nonempty WAL, hot rollback journal, or
uncertain sidecar could contain committed changes. Custom DB paths (`MEMORY_DB_PATH`)
are swept as given **when selected for this CLI invocation**; named projects
under `~/.tachi/projects/` are enumerated automatically. Repeat the plan/apply
for any custom scope not included in this sweep.

```bash
set -o pipefail
tachi migrate --json | tee migrate-plan-v2.0.0.json
```

Read the findings before applying:

- `needs_migration`: this store is behind; `--apply` migrates it.
- `up_to_date` / `no_op_already_current`: nothing to do.
- `not_found`: no store file at that slot (genuinely empty).
- `unstamped`: `user_version == 0`; deliberately not touched by this sweep.
  Investigate manually; apply will not fix it.
- `unreadable`: the version could not be read, or the slot could not be
  resolved, including both real filenames, pending WAL/journal or uncertain
  sidecar state. Nothing was touched by the plan; reconcile manually first.
- `ahead_of_binary`: the store is stamped newer than this binary. You are
  running an old binary against a newer store; stop and get a newer binary
  instead of applying anything.

Apply:

```bash
set -o pipefail
tachi migrate --apply --json | tee migrate-apply-v2.0.0.json
```

### 5. Verify each finding, then restart the same service manager

Check the apply JSON, not just the exit code: every store the plan reported as
`needs_migration` must now read `migrated` (or `no_op_already_current`).
`skipped_locked` (a live daemon or lock held it), `failed`, and `not_attempted`
are **not** successes. Re-run the plan and confirm every store reads
`up_to_date` with expected version 39:

```bash
tachi migrate --json   # every store up_to_date, expected_version 39
```

Then restart **only** the service manager stopped in step 1:

```bash
brew services start tachi   # if Homebrew services was stopped in step 1
```

Or, for the shell installer's LaunchAgent:

```bash
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.kckylechen.tachi.daemon.plist"
```

For an unmanaged daemon use its original start command. Then verify:

```bash
tachi daemon status
tachi --version        # expect 2.0.0
tachi status           # runtime.build.git_sha matches the release
```

If a store was `skipped_locked`, stop the writer that held it and re-run
`tachi migrate --apply`; a skipped store is left untouched, so re-running is
safe. If anything went wrong mid-flight, restore the store directories from the
step-2 backups with the prior binary; never point an old binary at a migrated
store.

---

## Step 2: Configure Your Agent's MCP Settings

Start the resident HTTP service with `tachi --daemon --port 6919 serve`, or use the installer-managed service. First-party Tachi MCP is HTTP-only; clients must support MCP Streamable HTTP.

Generate a native HTTP entry for a supported host:

```bash
python3 scripts/setup_agent_mcp.py --agent claude-code --project Sigil-433b921b
```

Use `--agent cursor` or `--agent gemini-cli` for those hosts. The helper refuses hosts whose configured schema cannot represent native HTTP. It never falls back to a command/args entry. Review `--help` for explicit URL and workspace options; 6919 is the default service port.

A host using an `mcpServers` map can use:

```json
{
  "mcpServers": {
    "tachi": {
      "type": "http",
      "url": "http://127.0.0.1:6919/mcp",
      "headers": {
        "X-Tachi-Profile": "standard",
        "X-Tachi-Client": "claude-code",
        "X-Tachi-Project": "Sigil-433b921b"
      }
    }
  }
}
```

Use the registered project key reported by Tachi, or the supported `X-Tachi-Workspace-Root` binding for the calling workspace. Reads may explicitly select another project when the action permits it; mutations remain bound to the session's project. Public caller metadata cannot authorize Ops/admin profiles. Use supported owner CLI commands for privileged management.

Keep provider keys and database paths in the daemon's environment/configuration. Public client entries contain the endpoint and session context, never the private owner token or `MEMORY_DB_PATH`.

After a daemon restart, reconnect and initialize a new MCP session at the same endpoint. Do not automatically replay a `tools/call` whose outcome is unknown. Check service readiness with:

```bash
curl -sS http://127.0.0.1:6919/health
```

See [the HTTP cookbook](engineering/architecture/http-direct-connect.md) for host-specific schemas, identity headers, loopback trust and reconnect behavior, and [the library identity contract](engineering/architecture/library-identity-runtime.md) for read/write isolation.

---

## Step 3: API Keys

Tachi requires at minimum one API key:

| Key | Required | Provider | Purpose |
|---|---|---|---|
| `VOYAGE_API_KEY` | **Yes** | [Voyage AI](https://dash.voyageai.com/) | Vector embeddings (Voyage-4) |
| `SILICONFLOW_API_KEY` | Recommended | [SiliconFlow](https://siliconflow.cn/) | Fact extraction, summaries, foundry distillation (Qwen3.5-27B) |

Phase 2 simplified the background lanes. Extraction, summary, and daily distillation go through configured OpenAI-compatible API lanes; a distinct configured provider fallback is tried before loud `LANE_OUTAGE` recording when a chain is exhausted. `FOUNDRY_DISTILL_BACKEND=claude_cli` remains a compatibility selector but still calls the API distill lane, not a Claude subprocess. Ordinary reasoning/chat is separate: it tries Claude CLI first, then the configured API fallback; provider-only callers intentionally omit CLI. For most deployments, only `VOYAGE_API_KEY` + `SILICONFLOW_API_KEY` are needed.

### Optional Per-Lane Overrides

For advanced setups, you can still route different LLM tasks to different providers:

| Env Prefix | Purpose | Notes |
|---|---|---|
| `EXTRACT_*` | Structured fact extraction | Falls back to `SILICONFLOW_*` when omitted. |
| `DISTILL_*` | Compaction & rollups | Explicit API-lane override. API-key precedence is `DISTILL_*`, then DeepSeek, reasoning, ZAI/BigModel, extract, and SiliconFlow; a fallback requires a distinct `DISTILL_FALLBACK_*` or SiliconFlow provider. |
| `SUMMARY_*` | Fast L0 summaries | Falls back to `SILICONFLOW_*` when omitted. |
| `REASONING_*` | Skill evolution, planning | Ordinary reasoning/chat tries Claude CLI before this API lane; provider-only callers bypass it. API-key precedence is DeepSeek, `REASONING_*`, ZAI/BigModel, distill, extract, and SiliconFlow; a fallback requires a distinct `REASONING_FALLBACK_*` or SiliconFlow provider. |

Each prefix supports `_API_KEY`, `_BASE_URL`, and `_MODEL` suffixes.

#### Front-line provider selection

`EXTRACT_PROVIDER` / `SUMMARY_PROVIDER` (values: `deepseek` or `siliconflow`)
opt one front-line lane out of the alias chain above and pin it to one
official provider. When set, the lane authenticates with that provider's
canonical key only — `DEEPSEEK_API_KEY` or `SILICONFLOW_API_KEY`, from the
environment or the Tachi Vault pool. Only the stale lane API-key aliases
(`EXTRACT_API_KEY` / `SUMMARY_API_KEY`) are ignored; the lane `*_BASE_URL` /
`*_MODEL` overrides remain in force. The URL override must be `https` on the
provider's exact host and default port — any other host (known or unknown),
plaintext `http`, or a non-443 port fails startup before any
credential-bearing request, even when the key lives only in the Vault. With
no URL override the endpoint defaults to the provider's documented chat URL
(`https://api.deepseek.com/chat/completions` for `deepseek`). Models remain
free-form overrides (`EXTRACT_MODEL` → provider `_MODEL` → provider default)
and never move the endpoint or the credential. Unset or empty keeps the
legacy behavior exactly. Selecting a provider also suppresses the automatic
same-provider fallback for that lane; configure `*_FALLBACK_*` explicitly for
a genuine second provider. See `.env.example` for a complete commented
official-DeepSeek recipe (model `deepseek-flash`).

### Optional Recall Tuning

Hybrid recall defaults are behavior-preserving, but advanced deployments can
override scoring knobs in `~/.tachi/config.env` or the process environment.
Restart the resident HTTP daemon after changing these values.

| Key | Default | Purpose |
|---|---:|---|
| `TACHI_RECALL_DEFAULT_SEMANTIC` | `0.35` | Default semantic/vector weight |
| `TACHI_RECALL_DEFAULT_FTS` | `0.25` | Default FTS weight |
| `TACHI_RECALL_DEFAULT_SYMBOLIC` | `0.20` | Default symbolic-token weight |
| `TACHI_RECALL_DEFAULT_DECAY` | `0.20` | Default recency/ACT-R weight |
| `TACHI_RECALL_GUIDE_*` | `0.25/0.45/0.28/0.02/true` | Override `/guide` namespace weights |
| `TACHI_RECALL_WIKI_*` | `0.48/0.30/0.20/0.02/true` | Override `/wiki`, `/behavior`, `/rules` weights |
| `TACHI_RECALL_EVENTS_NOTES_*` | `0.35/0.25/0.15/0.25/true` | Override `/events`, `/notes` weights |
| `TACHI_RECALL_EXPANDED_FTS_SCORE_FACTOR` | `0.78` | Score multiplier for expanded FTS queries |
| `TACHI_RECALL_MAX_EXPANDED_FTS_QUERIES` | `6` | Max FTS expansion variants |
| `TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR` | `0.55` | Partial-term `OR` FTS fallback multiplier; `0` disables it |
| `TACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS` | `8` | Max query terms used by the optional `OR` FTS fallback |
| `TACHI_RECALL_RAW_HALF_LIFE_DAYS` | `30` | Decay half-life for raw memories |
| `TACHI_RECALL_CONSOLIDATED_HALF_LIFE_DAYS` | `60` | Decay half-life for consolidated memories |
| `TACHI_RECALL_PATTERN_HALF_LIFE_DAYS` | `30000` | Decay half-life for pattern memories |
| `TACHI_RECALL_ID_LIKE_EXACT_MATCH_BOOST` | `12` | Boost for exact id-like query matches |
| `TACHI_RECALL_VECTOR_ONLY_SIMILARITY_FLOOR` | `0.445` | Provisional floor for candidates without sufficient lexical evidence; exact-id and qualified lexical candidates bypass it |
| `TACHI_RECALL_OR_FALLBACK_FTS_PAIR_MIN_QUERY_TERMS` | `4` | Require two matched terms for rich-query OR fallback and weak-vector symbolic bypass; values below `2` disable |
| `TACHI_RECALL_USE_PROVENANCE_RECENCY` | `true` | Use explicit caller-recorded use events for recency, promotion, and retention; set `false` to restore legacy display/access semantics |

For namespace weight groups, replace `*` with `SEMANTIC`, `FTS`,
`SYMBOLIC`, `DECAY`, or `USE_RRF`; grouped defaults above are listed in that
same order.

To replay labeled recall probes before changing those knobs, call `tachi_tune`
(the admin/operator tuning surface — it is not visible to standard or worker
tool profiles):

```json
{
  "action": "recall_simulate",
  "enable_rerank": true,
  "metadata": {
    "cases": [
      {
        "query": "RECALL_PROBE_ALPHA_20260607",
        "expected_ids": ["recall-probe-alpha-20260607"],
        "top_k": 5
      }
    ],
    "variants": [
      {
        "name": "or-fallback-0.3",
        "recall_config": {
          "or_fallback_fts_score_factor": 0.3,
          "or_fallback_fts_max_terms": 8
        }
      }
    ]
  }
}
```

`recall_simulate` reports recall@k and MRR over the supplied cases. When
`variants` are supplied, the response includes the current runtime config plus
each candidate config so you can compare metrics before changing
`TACHI_RECALL_*` defaults. If `enable_rerank=true`, it expands candidates and
uses the same adaptive Voyage rerank gate as normal search, including the
exact-token skip policy. It bypasses the recall-cache short circuit and does not
mutate memory access counters.

To turn an improved variant into a reviewed config change, run the same labeled
set through `tachi_tune`'s proposal loop:

```json
{
  "action": "recall_proposals",
  "metadata": {
    "cases": [
      {
        "query": "RECALL_PROBE_ALPHA_20260607",
        "expected_ids": ["recall-probe-alpha-20260607"],
        "top_k": 5
      }
    ],
    "variants": [
      {
        "name": "or-fallback-0.3",
        "recall_config": {
          "or_fallback_fts_score_factor": 0.3,
          "or_fallback_fts_max_terms": 8
        }
      }
    ]
  }
}
```

`recall_proposals` persists only variants that improve recall@k, or keep
recall@k flat while improving MRR, and includes the exact `TACHI_RECALL_*`
`config_env` patch. Apply is two-step:

```json
{ "action": "recall_review", "proposal_id": "...", "review_status": "approved" }
{ "action": "recall_apply", "proposal_id": "...", "confirm": true }
```

Apply only upserts `TACHI_RECALL_*` keys in `~/.tachi/config.env` (or
`$TACHI_HOME/config.env`) and preserves provider/vault lines. Restart the daemon
or MCP server after apply; `RecallConfig` is loaded at process startup.

---

## Step 4: Verify

After configuring, restart your agent and run:

```
Search my memory for "test"
```

If Tachi is working, you will see a response from `tachi_memory(action="search")` or the profile's search tool (even if no results are found yet). If the tool is not available, check:

1. The MCP config file path is correct for your agent
2. The `tachi` binary is on your PATH
3. Your `TACHI_PROFILE` is appropriate for the host (`standard`, `coordinate`, `operate`, or `admin`)
4. Your `VOYAGE_API_KEY` is set (embedding is required for save operations)

---

## Database Locations

Tachi uses SQLite with zero external dependencies:

| Database | Path | Purpose |
|---|---|---|
| Global | `~/.tachi/global/tachi-memory.db` | Cross-project memories, user preferences |
| Project | `.tachi/tachi-memory.db` (at git root) | Project-specific context, auto-detected |

> **Safety**: Never place database files in cloud-synced folders (iCloud, Dropbox, OneDrive). SQLite WAL mode is incompatible with network filesystems.

Plan C project routing keeps the repo-local project DB as canonical and uses
`~/.tachi/projects/<sanitized-repo-name>/tachi-memory.db` only as a symlink alias.
If `tachi status`, `runtime_info`, or daemon startup reports
“Plan C split-brain”, the alias is a stale regular SQLite file. Repair it
explicitly:

```bash
tachi repair --rule R11 --dry-run --db <label-from-status>
tachi repair --rule R11 --apply --db <label-from-status>
```

The repair backs up the regular alias DB, copies alias-only memory ids into the
repo-local DB without overwriting existing canonical ids, rebuilds FTS, then
replaces the alias file with a symlink to `.tachi/tachi-memory.db`.

### Database Safety Rules

| Rule | Why |
|------|-----|
| **Single instance per DB** | The server acquires an exclusive file lock (`tachi-memory.db.lock`) at startup. Only one Tachi process should write to a given database file. |
| **No cloud-synced paths** | iCloud, Dropbox, OneDrive, and Google Drive are **incompatible** with SQLite WAL. Use a local-only directory (e.g., `~/.tachi/`). |
| **No concurrent CLI writes** | Do not run `sqlite3` INSERT/UPDATE on the database while the server is running. Read-only queries are safe with `PRAGMA busy_timeout = 5000`. |
| **Auto-recovery on startup** | The server runs `PRAGMA quick_check` on startup and auto-backfills an empty FTS index from the main `memories` table. |
| **Graceful shutdown** | The server handles SIGINT/SIGTERM to flush WAL and run `PRAGMA optimize` before exit. Avoid `kill -9`. |

Live SQLite databases should stay local. Sync encrypted bundles, append-only event logs, vault ciphertext, workflow summaries, wiki/skill artifacts, and performance aggregates instead.

---

## Legacy OpenClaw Integration

The legacy OpenClaw MCP memory adapter is retired. It required process-local database routing, the privileged `openclaw`/`operate` surface and legacy tools unavailable to public HTTP clients. Connection attempts refuse before starting a subprocess or sending HTTP. Its static configuration helpers remain usable, but the plugin cannot provide Tachi-backed memory with the HTTP-only service. See [the integration notes](../integrations/openclaw/README.md).

Use a supported native HTTP client configured in Step 2. Reworking OpenClaw around the public facade API is separate work; no replacement plugin is supplied here.

### MCP service DB scope

Start one resident daemon explicitly, for example:

```bash
tachi --daemon --port 6919 --no-project-db serve
```

`--no-project-db` makes the daemon global-only at startup and avoids inferring a project from its launch directory. Coding clients declare their calling workspace or registered project in HTTP headers; project routing and write isolation then apply per session. Database paths belong to the daemon configuration, never a generated public client entry.

---

## Available MCP Tools

Once connected, Worker (`delegate`) sessions expose the four coordination facades
`tachi_memory`, `tachi_task`, `tachi_staff`, and `tachi_a2a`, plus read-only `vault_connections`.
Default Lead (`standard`) sessions also expose `tachi_agent_eval`, for six tools.
The GitHub control facade `tachi_gh` was removed; GitHub is a read-only source.
The remaining inventory in this section documents retained routes for explicit
Ops/admin compatibility or operator CLI workflows; those routes are not daily
or default discovery, and retaining them does not mean they were deleted.

### Core Memory

(the legacy direct names `save_memory`, `search_memory`, `get_memory`, `remember`, and `find_similar_memory` are retired; `list_memories`, `archive_memory`, and `memory_stats` remain live native routes)

the legacy direct names were retired; there is no backcompat catalog entry. Daily
agent profiles should use `tachi_memory(action="get")` instead. The canonical
owners for permanent deletion and garbage collection are now the operator CLI:

```bash
tachi delete plan --db /path/to/tachi-memory.db --id <memory-id> --out delete-plan.json
tachi delete apply --plan delete-plan.json --yes
tachi gc plan --db /path/to/tachi-memory.db --out gc-plan.json
tachi gc apply --plan gc-plan.json --yes
```

Both workflows are irreversible: their durable receipts are audit and
reconciliation evidence, not backups, and there is no restore verb. They are
operator-only; the model-facing Memory facade has no delete or garbage-collection
action.

### Knowledge Graph & Domains

Graph primitives (`add_edge`, `get_edges`, `memory_graph`) were retired/internalized
off the MCP surface in #757 — no live tool call reaches them anymore; the
tachi-server facade helper layer that used to wrap them was deleted outright
once #913 found zero remaining in-crate callers, so only the
`memcore::MemoryStore` boundary remains. Agents get graph behavior through
`tachi_memory` auto-linking (`tachi_save` is a retired shorthand) and recall's graph-spreading-activation
channel — there is no standalone graph-traversal action.

The domain registry (`register_domain`, `get_domain`, `list_domains`,
`delete_domain`) was retired in #972 — domains are no longer a first-class
MCP concept; there is no replacement action.

### State & Config

State primitives (`set_state`, `get_state`) were retired/internalized off the MCP
surface in #757 for the same reason — there is no facade equivalent for raw
KV state.

`runtime_info` (explicit Ops/admin compatibility only)

### Extraction & Ingestion

Ordinary model-authored memory uses `save` and `extract_facts`. File, URL,
corpus, bulk, snapshot, and event ingestion requires an admitted typed adapter
or operator path with bounded source identity, provenance, and a receipt; it is
not a model-facing Memory action and is not hidden inside `save`.

### Neural Foundry (Context Lifecycle & Evolution)

`recall_context`, `capture_session`, `compact_context`, `section_build`, `compact_rollup`, `compact_session_memory`

### Capability Hub

`hub_register`, `hub_discover`, `hub_get`, `hub_call`, `hub_feedback`, `hub_stats`, `hub_set_enabled`, `hub_disconnect`, `hub_review`, `hub_set_active_version`, `hub_export_skills`

Skill definitions remain available for review, versioning and export. Static
workflow documents must explicitly declare `"execution": "document"` and provide
string `content` (or `prompt` / `template`). The compatibility
`tachi_skill(action="run")` action and dynamic Skill tools return that text
verbatim; the host follows the instructions with its own tools. Arguments are
ignored, templates are not rendered, and the read still updates usage statistics.
Tachi-owned Prompt Skill execution is retired. Existing custom definitions are
retained for management and export, but implicit, LLM and mock execution modes
are not callable and are not automatically converted.

### Skill Packs

`pack_register`, `pack_list`, `pack_get`, `pack_project`, `pack_remove`, `projection_list`

### Virtual Capabilities

`vc_register`, `vc_bind`, `vc_resolve`, `vc_list`

### Ghost Whispers (Pub/Sub)

`ghost_publish`, `ghost_subscribe`, `ghost_topics`, `ghost_ack`, `ghost_reflect`, `ghost_promote`

*(Aliases: `ghost_whisper`, `ghost_listen`, `ghost_channels`)*

### Handoff

The retired `tachi_handoff` tool has been removed (its last action, `promote_issue`, went with it). Sessions use `tachi_a2a(action='respond')` or `tachi_task(action='handoff')`. The retired `tachi_orchestrator` handoff route is no longer available.

### Retired Kanban (Inter-Agent)

The `post_card`, `check_inbox`, and `update_card` tools are retired.

### Vault (Encrypted Secrets and Saved Connections)

Owner CLI commands manage encrypted credentials, pools and health. Agents use
`vault_connections` with `list`, `describe`, or `prepare` to discover scoped API
and SSH connection metadata; the MCP response never contains a secret value.
See [Vault connections](engineering/architecture/vault-connections.md) for
registration examples and authorization boundaries.

```sh
tachi vault init
tachi vault set EXA_API_KEY
tachi vault connection put --file exa-search.json
tachi vault connection run exa-search --keychain -- python search.py
# With a matching daemon already running for this global DB:
tachi vault unlock
# Configure the existing pool without resubmitting credential values:
tachi vault setup-rotation MY_API_KEY --total-keys 2 --strategy round_robin
```

The raw Vault MCP management tools are retired in every profile, including
Admin. The OpenClaw raw Vault passthroughs are also retired. Owner session
commands use the same local daemon through a fixed authenticated endpoint;
restart an older daemon if the CLI reports unsupported owner management.
An error from a matched daemon is not downgraded to local or raw MCP execution.
`setup-rotation` requires a matching unlocked daemon. Without one it refuses
before opening a local database; `set-pool` retains its existing offline flow.

#### Project env CLI (`tachi env`)

Store explicit project bindings in `.tachi/vault.env`:

```dotenv
LONGPORT_APP_SECRET=vault:longbridge.1.secret
GOOGLE_API_KEY=vault:google.1.api_key
```

```sh
# Inspect bindings without decrypting values
tachi env plan
# Inject only declared bindings into a child command
tachi env --keychain run -- cargo test
# Explicit stdout export for a shell/direnv workflow
eval "$(tachi env --keychain export)"
```

`env run` and API connection run share one resolver. The declared values override
the same-named child variables; missing, denied, invalid or empty secrets stop
the launch before spawn. Other parent variables remain inherited. Child exit
codes propagate to the CLI caller. Export prints plaintext by explicit request;
keep it out of logs and tracked files.

The bare global `tachi env`, `tachi env sync`, and implicit `tachi vault exec`
entrypoints are retired. There is no generated-env-file writer. Keep only Vault
references in version control. Existing `.tachi/env.generated` files are not
erased automatically; remove obsolete plaintext files after migrating callers.
The existing doctor warning for tracked generated files remains useful.

Project dispatch retains its existing provider/runtime integration and binding
behavior. For credential profiles, new writes use `env`, `config_content_env`,
or a temporary file `config_overlay` inside a caller-granted run directory;
`file_copy` and persistent `config_patch` are retired. Historical managed-file
cleanup remains available. See [credential adapters and cleanup](engineering/architecture/credential-adapters-cleanup.md).

#### DLQ and dispatch safety

- **`dlq_list` / `dlq_retry`**: DLQ replay skips mutating tools by default. Only idempotent read/search-style failures are safe to retry automatically.
- **Bare dispatch dedupe**: repeated identical `tachi_staff(action='start')` requests without a `flow_id` coalesce via fingerprint locks under `~/.tachi/runs/.dispatch-dedupe/`.
- **Crash recovery**: restarting the daemon reconciles orphaned dispatch runs left by abrupt process exit.

See also: [`docs/engineering/architecture/safety-hardening-2026-06.md`](engineering/architecture/safety-hardening-2026-06.md).

### Sandbox & Governance

Use `tachi_sandbox` under the admin profile with `action='set_rule'|'check'|'get_policy'|'set_policy'|'list_policies'|'exec_audit'`.
The old names `sandbox_set_rule`, `sandbox_check`, `sandbox_get_policy`, `sandbox_set_policy`, `sandbox_list_policies`, and `sandbox_exec_audit` are retired and no longer routed in v2.0.0.

### Retired Recommendations

The public `tachi_dispatch(action='recommend')` surface is retired. Surviving internal dispatch-profile recommendation consumes the DecisionFactLedger and abstains when there is no evidence.

*These APIs are retired and deleted by #1690 C3: `recommend_capability`, `recommend_skill`, `recommend_toolchain`, `prepare_capability_bundle`, and `skill_evolve`. The router rejects them as unknown tools.*

### Facade & Delegation

Worker discovery contains `tachi_memory`, `tachi_task`, `tachi_staff`, `tachi_a2a`, and read-only `vault_connections`.
Default Lead discovery adds `tachi_agent_eval`, for six tools.

*Compatibility/read-only helpers such as `tachi_web_search`, `tachi_browse`,
and `tachi_verify` remain only on explicit Ops/admin surfaces.*

`tachi_unstick`, `tachi_wiki_organize`, and `chain_skills` are retired.
Use the host for debugging decisions, repository file organization and Skill
execution. `tachi_agents` retains `list` and `profiles`; heuristic `select` is
retired. `tachi_skill(action="run")` only reads explicitly declared document
Skills; legacy prompt/model execution definitions are not executed.

### Wiki System

Explicit Ops/admin compatibility only: `wiki_lint`, `tachi_wiki`,
`tachi_wiki_write`, `tachi_wiki_search`, `tachi_browse`.

### Utilities

`sync_memories`, `tachi_init_project_db`, `tachi_audit_log`, `dlq_list`, `dlq_retry`, `get_pipeline_status`

Default `tachi doctor` is the canonical read-only diagnostic owner. It does not
write the manifest, database, sidecars, cache, or daily markers; `--fix` and
`--run-daily` are explicit mutation intents. Doctor scans are not exposed as a
model-facing Memory action.

---

## Tool Surface Selection

Tachi does not need to expose the full tool catalog to every host. Use `--profile` or `TACHI_PROFILE` to select a role surface:

| Profile | Exposed surface | Best for |
|---|---|---|
| `standard` | Product facades `tachi_memory`, `tachi_task`, `tachi_agent_eval`, `tachi_staff`, `tachi_a2a`, and read-only `vault_connections`; facade action policy still applies. | Ordinary Lead sessions. |
| `coordinate` | The four coordination facades (no `tachi_agent_eval`) and read-only `vault_connections`, with legacy coordinate action permissions. | Explicit coordination compatibility without diagnostic discovery. |
| `operate` | Explicit non-default Ops surface retaining runtime, status, Foundry, and Hub diagnostics. | Runtime adapters and authorized Ops sessions. |
| `delegate` | `tachi_memory`, `tachi_task`, `tachi_staff`, `tachi_a2a`, and read-only `vault_connections`; status/read actions remain available while recursive staffing stays denied. | Bounded Worker sessions. |
| `admin` / `emergency` | Full retained catalog. Narrow-profile hiding does not physically delete compatibility routes. | Explicit maintenance, development, and emergency sessions. |

Host aliases:

- `lead`, `codex`, `claude`, `claude-code`, `cursor`, `trae`, `windsurf`, `ide`, `antigravity`, `companion`, `copilot`, `coach`, `workflow` → `standard`
- `worker`, `subagent`, `delegate` → `delegate`
- `openclaw`, `hermes`, `runtime`, `adapter`, `ops` → `operate`
- `admin`, `full`, `emergency` → `admin` (must be selected alone)

Examples:

```bash
tachi --daemon --profile standard serve
TACHI_PROFILE=claude-code tachi --daemon serve
```

If no profile is specified, Tachi defaults to `standard`. Use supported owner CLI commands for privileged maintenance. HTTP caller metadata cannot authorize Ops/admin.

First-party Tachi MCP is HTTP-only. Bare `tachi` and `tachi serve` refuse before startup; `TACHI_DISABLE_STDIO_PROXY=1` does not restore the retired transport. Start the resident service explicitly with `tachi --daemon serve` and replace command/args client entries with native HTTP configuration.

---

## Troubleshooting

| Problem | Solution |
|---|---|
| `tachi: command not found` | Run `brew tap kckylechen1/tachi && brew install tachi`, then verify PATH |
| MCP tools not appearing | Check your agent's MCP config file path and JSON syntax |
| `tachi_memory(action="save")` fails | Ensure `VOYAGE_API_KEY` is set (required for embedding) |
| Search returns no results | Memory store is empty — save some memories first |
| Database locked errors | Only one Tachi instance should access each database file |
| Profile blocks a needed tool | Use an explicitly authorized local Ops/admin maintenance session; HTTP profile metadata cannot self-authorize privileged access. |
| Tests prompt for a Vault password under tmux or an interactive terminal | Run tests with `TACHI_TEST_FORCE_NO_TTY=1`, for example `TACHI_TEST_FORCE_NO_TTY=1 cargo nextest run --workspace --locked --profile ci`. Test builds use this override to force the no-TTY path; redirecting stdin alone does not prevent `rpassword` from opening `/dev/tty`. |

---

## Uninstall

```bash
brew uninstall tachi && brew untap kckylechen1/tachi
```

To also remove data:

```bash
rm -rf ~/.tachi
```
