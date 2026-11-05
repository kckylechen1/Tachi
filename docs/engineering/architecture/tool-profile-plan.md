---
title: "Agent MCP Surface and Internal Policy"
summary: "Defines the single ordinary Agent MCP entry, legacy worker restrictions, and separate owner CLI management authority."
category: "engineering/architecture"
organize: true
---
# Agent MCP Surface and Internal Policy

This is the current exposure contract. [Kernel Surface V1](./kernel-surface-v1.md)
defines the underlying kernel boundaries; this document defines what an Agent
can discover and how retained maintenance handlers are reached.

## One ordinary entry

An ordinary host connects to the resident Streamable HTTP endpoint without
choosing a profile. Its default `tools/list` contains six tools:

| Tool | Responsibility |
|---|---|
| `tachi_memory` | Memory retrieval, saving, and reusable context. |
| `tachi_task` | Task context, claims, progress, and completion evidence. |
| `tachi_agent_eval` | The bounded native-worker experience ledger. |
| `tachi_staff` | Explicit external-staffing exceptions and run status. |
| `tachi_a2a` | Admitted agent coordination. |
| `vault_connections` | Read-only connection metadata and command preparation. |

Facade action policy still applies. Discovering `tachi_staff` does not grant
unbounded execution, recursive staffing, a new identity, or a broader workspace.
Ordinary local delegation uses the host's native subagent facility.

## Compatibility restrictions

`standard` remains the compatibility wire spelling of the default Agent surface.
Existing ordinary host aliases still resolve to that surface. New client setup
omits the default profile header instead of asking users to choose a tier.

Existing restricted contracts keep their meanings:

| Selector | Retained constraint |
|---|---|
| `delegate` / `worker` / `subagent` | Five tools, excluding `tachi_agent_eval`; bounded worker actions and no recursive staffing. |
| `observe` / `read` / `reader` | Existing read-oriented action policy. |
| `remember` (retired) / `write` / `writer` / `agent` | Memory-writing action policy retained for compatibility; the literal `agent` alias is not renamed into the default surface. |
| `coordinate` | Existing coordination action policy. |

These selectors are compatibility and worker-contract vocabulary, not product
setup tiers. Unknown settings fail rather than silently falling back to a
broader policy. `TACHI_EXPOSED_TOOLS`, when set, continues to intersect the
visible set; it cannot add authority.

Dispatch profiles remain a separate concept: they describe a worker's role,
model context, allowed capabilities, and required evidence. Their persisted
contracts are not renamed by the public entry cleanup. ACP capability grants
continue to admit only canonical `observe` and `delegate` tool profiles.

Current clients select an admitted public profile through native HTTP client
configuration. Owner management uses supported CLI commands; public caller
metadata cannot select Ops/admin authority. See [HTTP direct connect](http-direct-connect.md).

Supported owner CLI commands authenticate to the daemon with its existing
private owner capability on each call and request one specific internal tool. That bounded
request does not produce a larger `tools/list` catalog. Neither the private
capability nor its per-tool authorization header belongs in an ordinary host
configuration, generated Worker descriptor, or ACP attachment.

Raw handlers used by owner CLI commands and internal server workflows remain
registered where required. Their retention is an implementation fact, not a
public MCP tier. A tool hidden from ordinary discovery is also refused on an
ordinary direct call; physical retirement is separately tested as route absence.

## Configuration and transport

Start the supervised resident service on a fixed port:

```sh
tachi --daemon --no-project-db --port 6919 serve
```

### Retired OpenClaw extension

The OpenClaw plugin, memory tools, lifecycle hooks and experimental passthroughs
are removed. The historical `openclaw` profile alias does not provide an
installed adapter or authorize public HTTP management. See the
[retirement guidance](../../../integrations/openclaw/README.md).

### Compaction primitive

`Tachi` now exposes `compact_context` as a runtime-only API.

- Input
  - `agent_id`
  - `conversation_id`
  - `window_id`
  - `messages`
  - token-budget hints
- Output
  - `compacted_text`
  - `estimated_tokens`
  - topic/signal summaries for later section work

This is a typed runtime primitive. The OpenClaw adapter is retired; there is no pending OpenClaw compaction-hook implementation in this plan.

### Capability recommendation primitive (RETIRED by #1690 C3)

The first-pass capability layer — `recommend_capability`, `recommend_skill`, `recommend_toolchain` — was **deleted end-to-end** in #1690 C3 (the "second model brain" tool family; the router rejects these tools as unknown).

Historical behavior (deterministic ranking over Hub capabilities, visibility/callability-aware and host-aware scoring, Pack/projection-aware toolchain suggestions, and host-tool inference) is kept for record only. The surviving internal dispatch-profile recommendation consumes the DecisionFactLedger and abstains when no usable evidence exists.

## Why This Split

This keeps the model-facing surface small while preserving a rich kernel for:

- IDE integrations
- static reviewed skill discovery and enforcement
- section / compaction artifacts
- operator and workflow tooling

It also avoids the old mutually exclusive trap where a client needed a workflow tool and suddenly had to choose between unrelated surfaces. Bundles compose upward, and aliases remain for backward compatibility.

## Next Steps

1. Harden the capability layer
   - add richer outcome signals
   - connect more pack / projection metadata
2. Revisit per-session filtering
   - Move runtime tool filters off shared server state before extending `agent_register`

## Verification

```bash
cargo test -p tachi-server
```

Generate an ordinary host configuration with `scripts/setup_agent_mcp.py`.
It writes endpoint, client label, and explicit workspace/project context,
without a default profile selector or owner credential. New Homebrew services
also use the default without setting `TACHI_PROFILE`; the shell installer
preserves an explicitly supplied legacy restriction for startup validation.

When a resolved Staff contract requests Tachi injection,
`dispatch_ops/mcp_config.rs` creates a native HTTP descriptor with the bounded
`delegate` default, caller-bound project, a separate child seat, and incremented
dispatch depth. The MCP Tasks CLI retains its existing `coordinate` policy.
See [HTTP Direct-Connect](./http-direct-connect.md) for admission and reconnect
behavior. First-party Tachi stdio and the legacy OpenClaw memory adapter are
retired; third-party Hub MCP subprocesses retain their own transports.

## Hub and Skills

Hub stores reusable MCP definitions and document Skills. Supported owner CLI
commands manage that inventory. Hosts keep their native MCP connections and
Skill loading/execution; Tachi does not require every Agent to receive the same
MCP set. Retained Skill document access does not execute arbitrary Skills or
provide automatic skill recommendation.

This surface cleanup does not implement Tachi Cloud, cross-device OAuth sharing,
or automatic synchronization of host instructions.

## Verification boundaries

The server suite checks discovery and direct-call admission, bounded Worker
actions, public rejection of retired privileged selectors, and authenticated
owner CLI dispatch. The execution-surface census distinguishes the public
catalog from retained registered routes. Generated-config tests cover native
HTTP shape, workspace binding, removal of stale transport credentials, and
idempotent updates. Authority and persisted ACP contracts retain their own
regression tests.
