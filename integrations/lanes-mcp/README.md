# lanes-mcp

A stdio MCP server that drives the **codex / opencode / grok** external lanes over
**ACP** (Agent Client Protocol, JSON-RPC 2.0 over stdio), plus a Claude Code plugin
that exposes them as `/lanes:*` dispatch commands. It replaces the codex-companion
script (two-layer completion trap) and the `lane-run` text black box: a lane dispatch
becomes one typed, blocking MCP call whose turn cannot detach, with plan progress
projected as a checkbox-style digest.

Implements tachi#698 Step 0 (progress probe) + Step 1 (server core) + G1b (long-poll
`lane_wait`) + G1c (persistent sessions + `lane_prompt`) + G2 (Claude Code plugin).

## Layout

```
integrations/lanes-mcp/
├── src/                 # the MCP server (TypeScript)
│   ├── index.ts         #   stdio entry, McpServer + LaneManager wiring
│   ├── lanes.ts         #   lane registry: spawn recipe + model/effort/read-only mapping
│   ├── acp-client.ts    #   ACP client wrapper over @agentclientprotocol/sdk
│   ├── run.ts           #   per-session state: plan projection, digest, chunk/event logs
│   ├── manager.ts       #   orchestration: turns, long-poll, cancel, worktree, reaper
│   ├── worktree.ts      #   git worktree lifecycle + change detection
│   ├── tools.ts         #   the 7 MCP tools
│   └── inbox.ts         #   channel-two (inbox) seam — interface only this iteration
├── test/                # node:test suite + scripted fake ACP agent
├── scripts/smoke.ts     # real 3-lane handshake + read-only micro prompt
├── prototype/progress-probe/  # Step 0 probe (see its README)
└── plugin/              # the Claude Code plugin (see plugin/README.md)
```

## The double structure (spec §1)

```
main loop (you) ── free to do other work, never blocked
  │ Agent tool run_in_background  ← bottom task line + notify-on-done, fully preserved
  ▼
ignition-hand subagent (named codex / grok / oc, haiku)
  │ only action: long-poll relay — lane_dispatch_start once, then lane_wait until done
  ▼
lanes MCP server (resident, one process)
  ├─ spawn: grok agent … stdio            (native ACP)
  ├─ spawn: opencode acp                   (native ACP)
  └─ spawn: npx -y @agentclientprotocol/codex-acp   (official adapter)
```

## Tools

| Tool | Purpose |
|---|---|
| `lane_dispatch_start(lane, prompt, cwd?, worktree?, model?, effort?, read_only?)` | Start a turn, return `{id}` immediately. A write dispatch (`read_only:false`) **requires** `worktree` and refuses a `cwd` inside the primary checkout. |
| `lane_wait(id, timeout_ms?)` | Long-poll (default 30s, cap 55s). Returns `{status, digest, plan_summary, last_event_age_ms, suspected_stall}`; when terminal also `{final_message, touched_files, plan_final}`. One waiter per id (concurrent waits are rejected). |
| `lane_dispatch(...)` | Convenience: start + loop wait until the turn completes (simple blocking path). |
| `lane_prompt(id, prompt)` | New turn on an existing session (persistent-session reuse). |
| `lane_status(id)` | Cheap snapshot: plan counts + current step, `tool_calls`, `last_event_age_ms`, `suspected_stall`. |
| `lane_cancel(id)` | Send ACP `session/cancel`. |
| `lane_list()` | Active sessions: `{id, lane, state (working\|idle\|stalled), idle_ms, turns_count, ...}`. |

**Information-pollution defense (spec §6):** only `digest` (in `lane_wait`) and
`final_message` cross the tool boundary. `plan` is the primary signal → checkbox
projection; `tool_call`/`tool_call_update` are only counted; `agent_thought` /
`agent_message` chunks are logged to `~/.cache/lanes-mcp/runs/<id>/chunks.log` and never
enter a tool response (except the accumulated `final_message`). Every raw event is
appended to `events.jsonl` in the same dir.

## Lane model / effort / read-only mechanisms

Derived from each CLI's `--help` and the codex-acp README, then **verified by the real
smoke** (see below). Where a lane cannot honor an override, the dispatch does **not** fail
silently — it returns a `warnings` entry echoed in the tool response.

| Lane | ACP entry | Model | Effort | Read-only |
|---|---|---|---|---|
| **codex** | `npx -y @agentclientprotocol/codex-acp` | `CODEX_CONFIG` env `{"model": …}` | `CODEX_CONFIG` `{"model_reasoning_effort": …}` | `INITIAL_AGENT_MODE=read-only` (else `agent-full-access`) |
| **opencode** | `opencode acp` | `OPENCODE_CONFIG` → per-run JSON `{"model":"provider/model"}` | **not supported in ACP mode** → warns (the `--variant` flag has no `acp` equivalent) | client-side permission gate (no native ACP read-only flag) |
| **grok** | `grok agent --model M --reasoning-effort E stdio` | `--model` flag | `--reasoning-effort` flag | client-side permission gate |

**Read-only enforcement.** ACP `session/new` has **no** `permissions` field (contrary to
the spec draft's `permissions:{defaultMode:"bypassPermissions"}` — that shape is not in the
protocol). Bypass/read-only is realized at the client layer per spec §5: the
`session/request_permission` handler auto-selects the first `allow*` option normally, or the
first `reject*` option when `read_only=true`; the client `fs/write_text_file` handler also
refuses writes when `read_only=true`. codex additionally gets `INITIAL_AGENT_MODE` as
defense-in-depth.

## ACP client: official SDK, not hand-rolled

Uses `@agentclientprotocol/sdk` (pinned `1.1.0`). The owner directive was "official SDK
first, fall back to a hand-rolled ~250-line core only if the API is unstable/too heavy."
The SDK was kept: its `ActiveSession` (`prompt()` + `nextUpdate()` yielding
`session_update` / `stop`) maps 1:1 onto spec §6's event-consumption model, and it already
implements + tests the ndjson JSON-RPC framing a hand-roll would duplicate. The only
awkwardness — `ClientContext` is scoped to the `connectWith` closure — is handled by parking
the closure on a shutdown promise so the session stays usable across turns (see
`acp-client.ts`). No fallback was needed.

## Build / test / smoke

```bash
cd integrations/lanes-mcp
npm install
npm run build          # tsc -p tsconfig.build.json → dist/index.js (dev/typecheck build)
npm run bundle         # esbuild → plugin/dist/lanes-mcp.mjs (self-contained plugin server)
npm run typecheck      # tsc --noEmit over src + test + scripts
npm test               # node:test suite (fake ACP agent, hermetic .test-tmp)
npm run smoke          # real 3-lane handshake + read-only micro prompt (needs the CLIs + auth)
```

The unit tests spawn a scripted fake ACP agent (`test/fake-acp-agent.mjs`) so the real SDK
client is exercised without any external CLI. The smoke uses the real registry against the
installed `codex-acp` / `opencode` / `grok` CLIs.

## Configuration (env)

| Env | Default | Meaning |
|---|---|---|
| `LANES_STALL_THRESHOLD_MS` | 300000 | Silence before a running turn is `suspected_stall` (a warning). |
| `LANES_TURN_TIMEOUT_MS` | 2700000 | Hard per-turn ceiling — the guaranteed terminal path: on hit the turn is forced to `error` and the subprocess killed. |
| `LANES_HANDSHAKE_TIMEOUT_MS` | 30000 | Handshake timeout before a lane spawn is failed. |
| `LANES_SESSION_TTL_MS` | 600000 | Idle-session TTL before the reaper closes it. |
| `LANES_WAIT_DEFAULT_MS` / `LANES_WAIT_MAX_MS` | 30000 / 55000 | `lane_wait` window / cap. |
| `LANES_PROGRESS_EXPERIMENTAL` | unset | `=1` enables channel one (`notifications/progress`) — experimental until Step 0's leader test. |
| `LANES_MCP_BASE_REPO` | server cwd | Base repo worktrees are cut from (always `origin/main`). |
| `LANES_RUNS_ROOT` / `LANES_WORKTREES_ROOT` | `~/.cache/lanes-mcp/{runs,worktrees}` | Artifact / worktree roots. |

## The plugin

See [`plugin/README.md`](plugin/README.md) for the Claude Code plugin, the old→new
migration table, and the install commands (run by the adjudicator).

## Scope notes & open items

- **Channel one** (`notifications/progress` → bottom task line) is implemented **only**
  behind `LANES_PROGRESS_EXPERIMENTAL=1`, pending the Step 0 probe's leader test
  (`prototype/progress-probe`). **Channel two** (inbox/cockpit) is an interface seam +
  `TODO(step2)` in `inbox.ts`, not wired — per the owner adjudication.
- **Worktree cleanup timing.** Because sessions are persistent (G1c), a `worktree` is
  cleaned up (removed if clean, retained + reported if dirty) at **session close/reap**, not
  after each turn — otherwise turn 2 would have no cwd. Per-turn `touched_files` still
  reflect that turn's git changes. This reconciles spec §8 with G1c; flag for review.
- **opencode effort** has no ACP-mode knob → warned, not applied.
- **`lane_wait` cursor** assumes a single consumer per `id` (the ignition hand). Concurrent
  waiters on the same id share one report cursor.
- **grok** emits a benign `skills-reload` request the client does not handle (logged, does
  not affect the turn) — noted during smoke.
