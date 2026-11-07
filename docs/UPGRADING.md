# Upgrading Tachi

This guide explains what changes between versions and what you need to do. The exact, ordered commands for stopping services, backing up, and migrating stores live in [`docs/INSTALL.md` Step 1b](INSTALL.md). That procedure stays authoritative; this page tells you when you need it and what else to update.

| You are coming from | Read |
|---|---|
| 1.9.x or earlier | [Part 1: 1.9.x to 2.0.0](#part-1-19x-to-200), then Part 2 if you track `main` |
| 2.0.0, tracking `main` | [Part 2: changes on main since 2.0.0](#part-2-changes-on-main-since-200) |
| Any version, with scripts or prompts that name older tools | [Retired and removed surfaces](#retired-and-removed-surfaces) |

---

## Part 1: 1.9.x to 2.0.0

2.0.0 is a major release. The on-disk schema moves from 28 to 39, standalone store files are renamed, and several model-facing routes were removed.

### What changes

- **Schema 28 to 39.** A binary refuses to open a store stamped newer than it supports. Once a store reaches 39, roll back by restoring the backup, not by reinstalling the old binary.
- **Store filename.** Standalone stores are now `tachi-memory.db`. Ordinary opens refuse an existing `memory.db`; it must be converted offline first. Conversion leaves a compatibility symlink for this release window.
- **Task status.** `tachi_task(action='status')` now carries CurrentTruth work facts (board column, blockers, top action) for the resolved refs. It orients you; it is not a completion verdict.
- **Dispatch V2 plans** now live at `<run_dir>/status.json#/model_plan`. V1 single-stage dispatch is unchanged.

### What to do

Follow [`docs/INSTALL.md` Step 1b](INSTALL.md) in order. In short:

1. Stop the actual service manager and every old or new reader and writer, and keep them from restarting.
2. Back up whole store directories **and the old binary**.
3. Install the new binary without starting the service. For the shell installer, pass `--skip-daemon-service --skip-plugin`.
4. Plan with `tachi migrate --rename-legacy`, then convert filenames with `tachi migrate --rename-legacy --apply --offline`.
5. Upgrade the schema with `tachi migrate --apply`.
6. Read every finding. Exit code 0 only means the sweep ran: `skipped_locked`, `failed`, and `not_attempted` need attention, and a re-run plan should show every store `up_to_date` at 39.

`--offline` is your attestation that nothing else is running. It does not stop an old binary from restarting.

---

## Part 2: changes on main since 2.0.0

These changes are merged to `main` but not yet in a tagged release. They do not change the store schema.

### 1. Tachi MCP is HTTP-only

First-party Tachi MCP is served only by the resident HTTP daemon. Clients no longer launch Tachi as a subprocess, and bare `tachi` or `tachi serve` refuse to start an MCP pipe.

To update an existing setup:

1. Start the daemon once, through your service manager or by hand:
   ```sh
   tachi --daemon --no-project-db --port 6919 serve
   ```
2. Replace any client entry that uses a command and args with a native HTTP entry. The setup helper generates it and backs up the old file:
   ```bash
   python3 scripts/setup_agent_mcp.py --agent claude-code --url http://127.0.0.1:6919/mcp
   # review the preview, then re-run with --apply
   ```
3. Check with `tachi daemon status`.

Third-party MCP servers registered in the Hub, and ACP harness protocol pipes, keep their own stdio transports.

### 2. Workspace binding

One shared daemon cannot infer a caller's project from its own working directory, so each call needs a workspace. Any of these works:

- the `x-tachi-workspace-root` header in the client configuration (recommended);
- per-call workspace metadata, which ZCode sends automatically;
- MCP Roots advertised by the host, used as a fallback for unbound project-sensitive calls.

Subdirectories and linked worktrees of one project resolve to the same library. If a call resolves to no library, or to more than one, a project save fails with an error instead of writing to the wrong store. Unbound sessions can still read another project explicitly through read-only `tachi_task` and `tachi_staff` actions, and unbound `brief`/`board` calls resolve a registered `cwd` automatically.

Antigravity users: the installer now writes the native `serverUrl` format to `.agents/mcp_config.json` (project) or `~/.gemini/config/mcp_config.json` (user).

### 3. One default agent profile

Ordinary agents connect without a profile and see six tools: `tachi_memory`, `tachi_task`, `tachi_agent_eval`, `tachi_staff`, `tachi_a2a`, and read-only `vault_connections`. Worker (`delegate`) sessions see five.

Public `operate`/`ops` and `admin`/`emergency` selectors and their aliases are retired. A client configuration, environment variable, or HTTP header that still selects one now refuses to start. Remove the selector and run maintenance through supported owner CLI commands such as `tachi status`, `tachi doctor`, `tachi gc`, and `tachi hub ...`. Each owner call authenticates separately and never widens the agent catalog. `standard`, `observe`, and `coordinate` keep working as before.

### 4. Upstream MCP credentials move into the Vault

Third-party MCP tools are now exposed directly as `server__tool`, with their upstream schemas and results unchanged. API keys and OAuth tokens for those servers live in the Vault.

- For OAuth servers, sign in with the owner command `tachi mcp login`, which runs the PKCE browser flow and stores encrypted, URL-bound credentials.
- Existing `mcp-remote` wrappers are normalized to the built-in HTTP connector automatically.
- Remove API keys and tokens from any MCP directory or agent configuration once they are stored in the Vault.

### 5. Usage guidance as an MCP Skill

Hosts that support the MCP Skills extension can load `skill://tachi/SKILL.md` on demand. The always-loaded server instructions shrank from 3,556 to 1,478 bytes. Hosts without the extension still get concise fallback guidance. No action is needed.

### 6. Briefings

Compact task briefs are more focused, and briefs show execution, adjudication, and delivery state separately. `tachi_memory(action="briefing")` accepts an opt-in `metadata.knowledge_context` manifest so clients can avoid re-receiving unchanged Wiki and Guide bodies. Calls without it behave as before.

---

## Retired and removed surfaces

If a script, prompt, or host instruction still names one of these, it now gets `tool not found` or a refusal. Every name below is retired; use the replacement column instead.

### Removed after 2.0.0 (on main)

| Retired surface | Replacement |
|---|---|
| Tachi launched as a stdio subprocess, `TACHI_DISABLE_STDIO_PROXY` | Resident HTTP daemon (Part 2, section 1) |
| Public `ops`/`operate`/`admin`/`emergency` selectors | Owner CLI commands |
| `tachi_gh` and GitHub write actions | None. GitHub is a read-only source for `tachi_task` intake and status |
| `hub_call`, gateway/flatten modes, `hub_set_active_version` | Upstream tools exposed directly as `server__tool` |
| `tachi_unstick`, `tachi_wiki_organize`, `chain_skills`, heuristic worker selection | None; the host owns these decisions |
| Tachi-owned Prompt Skill execution | `tachi_skill(action="run")` returns declared document content for the host to follow |
| OpenClaw plugin | None. Disable existing installs before upgrading; data is not removed |
| Public memcore mirror | Depend on `memcore` from this repository by git revision |

### Removed in 2.0.0

| Retired surface | Replacement |
|---|---|
| Memory save/search aliases | `tachi_memory(action='save'\|'search')` |
| Direct Kanban routes | `tachi_task` board actions |
| `tachi_orchestrator`, skill recommendation and evolution routes, standalone `run_skill` | None |
| Memory `progress` action | `tachi_task(action='status')` |
| Memory `readiness` action | `tachi status` CLI |
| Memory `delete`, `gc`, `doctor_scan` actions | `tachi delete`, `tachi gc` plan/apply, `tachi doctor` |
| Memory `ingest`, `ingest_source`, `pattern_feedback` actions | None |
| Expired `sandbox_*` aliases | Internal sandbox actions; not in ordinary agent discovery |

---

## After upgrading: checklist

- [ ] `tachi --version` shows the expected version and `tachi daemon status` reports the daemon running.
- [ ] Each agent host connects over HTTP and lists the expected tools.
- [ ] A memory search from inside a project returns that project's memories.
- [ ] No client configuration still contains a subprocess command, a retired profile selector, or plaintext MCP credentials.
- [ ] `tachi doctor` reports no errors.

Release details: [CHANGELOG.md](../CHANGELOG.md).
