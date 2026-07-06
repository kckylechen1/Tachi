# lanes — Claude Code plugin

Bundles the lanes ACP server and its dispatch entrypoints:

- **MCP server `lanes`** — declared in [`.mcp.json`](.mcp.json), launched as
  `node ${CLAUDE_PLUGIN_ROOT}/dist/lanes-mcp.mjs`. `dist/lanes-mcp.mjs` is a
  **self-contained esbuild bundle** of the server + its SDK deps. It is committed
  because plugin install copies the plugin directory to a cache and a plugin cannot
  reference files outside itself (so the server cannot live one level up). Regenerate
  it with `npm run bundle` in `integrations/lanes-mcp` after changing `src/`.
- **Ignition-hand agents** `codex` / `grok` / `oc` ([`agents/`](agents)) — haiku,
  zero-discretion long-poll relays. Each starts one `lane_dispatch_start` and loops
  `lane_wait` until the turn completes, returning only `final_message` + result fields.
  Their UI rows read `lanes:codex` / `lanes:grok` / `lanes:oc`.
- **Commands** `/lanes:codex`, `/lanes:grok`, `/lanes:oc` ([`commands/`](commands)) —
  argument mapping preserved from the current habits (`--write` → `read_only:false`,
  `--background` → outer Agent `run_in_background`, oc model shortnames → full ids).

## Install (adjudicator runs these; the implementer does not touch user config)

The MCP bundle is committed, so no build step is required to install. From the repo,
with `<REPO>` the absolute repo root:

```bash
# 1. Register this repo's local marketplace (marketplace root = integrations/lanes-mcp,
#    which contains .claude-plugin/marketplace.json listing the `lanes` plugin at ./plugin).
claude plugin marketplace add <REPO>/integrations/lanes-mcp

# 2. Install the plugin from it.
claude plugin install lanes@lanes-mcp
```

Then restart Claude Code (or `/reload-plugins`) so the `lanes` MCP server and the
`/lanes:*` commands load. Approve the `lanes` MCP server when prompted (same per-server
approval as a project `.mcp.json`).

To uninstall later: `claude plugin uninstall lanes@lanes-mcp` and
`claude plugin marketplace remove lanes-mcp`.

**If you changed `src/`** first run `npm install && npm run bundle` in
`integrations/lanes-mcp`, then `claude plugin marketplace update lanes-mcp` (or reinstall).

## Migration table (old → new)

| Old entrypoint | New entrypoint | Notes |
|---|---|---|
| `/codex:dispatch <task>` | `/lanes:codex <task>` | ACP turn, blocking, cannot detach |
| `/grok:dispatch <task>` | `/lanes:grok <task>` | |
| `/oc-dispatch <glm\|ds\|kimi\|free> <task>` | `/lanes:oc <glm\|ds\|kimi\|free> <task>` | same model-shortname mapping |
| `~/bin/lane-run codex\|grok\|oc <prompt-file>` | `lane_dispatch` / `lane_dispatch_start` MCP tools (via the ignition agents) | typed call, no shell quoting |
| codex-companion (background poll) | `lane_dispatch_start` + `lane_wait` long-poll | no two-layer completion trap |

**The old plugins and `~/bin/lane-run` are left untouched during the observation
window.** They are retired only at Step 4 (spec §9), after this plugin has been
dogfooded for a week. Both sets of entrypoints can coexist meanwhile.

## Read-only and write isolation (the real safety boundary)

`read_only: true` (the default for `/lanes:*` without `--write`) is enforced at the
client layer: the server's `session/request_permission` handler declines any permissioned
operation (it never auto-selects an `allow*` option under read-only), and the client
`fs/write_text_file` handler refuses writes unconditionally. **codex** additionally runs
with its native `INITIAL_AGENT_MODE=read-only`; **grok** and **opencode** have **no native
read-only ACP mode**, so for them read-only is only the client-side gate.

Because native read-only is not uniform, **the real isolation boundary for writes is the
worktree**: a `--write` dispatch (`read_only: false`) is *required* to run in a
server-created git worktree cut from `origin/main`, and the server rejects a write whose
`cwd` resolves inside the primary checkout. Writes never touch the main working tree.

## Caveat: MCP tool names in agent frontmatter

The ignition agents restrict `tools:` to `mcp__plugin_lanes_lanes__lane_dispatch_start` and
`mcp__plugin_lanes_lanes__lane_wait` (the `mcp__<server>__<tool>` convention, server key `lanes`).
If your Claude Code version namespaces plugin MCP tools differently, the agents would
have no tools; verify the exact names after install (`/plugin` inventory or the tool
picker) and adjust `agents/*.md` if needed. The zero-discretion body contract (no Bash,
no backgrounding, relay-only) holds regardless.
