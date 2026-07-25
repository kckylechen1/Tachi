# Safety & Idempotency Hardening (2026-06)

Reference for the post–Gemini review hardening batch released in v1.5.4. Operator-facing summary lives in [`CHANGELOG.md`](../../../CHANGELOG.md#154---2026-06-15); install notes in [`docs/INSTALL.md`](../../INSTALL.md).

## Scope

| Area | Problem | Mitigation |
|---|---|---|
| DLQ replay | Mutating tool failures could be retried and double-apply side effects | Admit replay only when `dlq_replay_is_explicitly_safe()` finds explicit typed `ReadOnly` + `Safe` authority; unclassified and proxy-qualified routes fail closed |
| Dispatch dedupe | Identical bare dispatches (no `flow_id`) could spawn duplicate workers | Fingerprint lock under `~/.tachi/runs/.dispatch-dedupe/` |
| Dispatch recovery | Daemon crash left orphaned in-flight runs | `recover_orphaned_dispatch_runs()` on startup |
| Foundry queue | `queue_agent_evolution` minted random job ids → duplicate synthesis | Deterministic job id + active/terminal dedupe + stale `running` reclaim (30 min) |
| Env sync | `tachi env sync` wrote plaintext secrets by default | Preview-only default; `--apply` required to write |
| MCP SSRF | Remote MCP URLs could target internal addresses | Expand-then-validate, IP blocklist, async DNS before SSE connect |
| Config TOCTOU | Sensitive files created world-readable or truncated in place | Atomic create-new + rename with `0o600` on Unix |
| Subprocess allowlist | Caller could inject `allowlist` on restricted profiles | Rejected for Codex/Grok/Kimi dispatch profiles |

## DLQ semantics

Replay authority is positive, never inferred. `action_effect::dlq_replay_metadata()` must return `ActionEffectMetadata` whose `ActionEffectMetadata::permits_dlq_replay()` accepts an explicit `ReplaySafety::Safe`; `shared_defs::dlq_replay_is_explicitly_safe()` is the production gate used by `should_enqueue_dlq()` and retry paths. Missing metadata denies replay.

Proxy-qualified `server__tool` names always receive no local metadata from `dlq_replay_metadata()`. A remote alias therefore cannot borrow the effect classification of a similarly named native facade and is denied unless a future server-owned per-remote-tool authority is added.

`search_memory` and `tachi_memory(action="search")` are non-replayable because their production paths record access telemetry. `STANDALONE_UNSAFE_ROUTES` and `facade_action_effect()` classify them accordingly; explicitly safe reads such as `tachi_event(action="metrics")` retain replay authority.

The `f1098_live_action_inventory_has_explicit_effect_metadata()` ratchet enumerates `native_route_definitions()` and each schema action enum through `action_inventory_from_live_schema()`, then requires every advertised action to have independent `facade_action_effect()` metadata. Newly advertised or invented actions without that mapping remain unclassified and fail closed.

## Dispatch dedupe

When `tachi_dispatch` is invoked without `flow_id`, the task string is hashed and a lock file is taken under:

```text
~/.tachi/runs/.dispatch-dedupe/<hash>.lock
```

Concurrent identical dispatches coalesce; the lock is released when the run finishes.

## Foundry agent evolution idempotency

1. **Job id:** `foundry-job:agent-evolution:<stable_hash(inputs)>`
2. **Queue:** active (`queued`, fresh `running`) or terminal (`completed`, `failed`) → response `status: "deduped"`
3. **Spawn:** `try_claim_foundry_job()` must succeed before synthesis; stale `running` (>30 min since `updated_at`) can be reclaimed

Proposal persistence was already idempotent per job id via `save_derived_with_id`.

## Project env sync

```bash
tachi env sync              # preview (no write)
tachi env sync --apply      # write .tachi/env.generated
tachi env sync --apply --force   # overwrite existing file
```

Generated files:

- Path: `<project>/.tachi/env.generated`
- Mode: `0600` on Unix
- Header warns against committing plaintext secrets
- `tachi doctor` warns if git-tracked

Keep `.tachi/vault.env` (aliases) in git; add `.tachi/env.generated` to `.gitignore`.

## MCP remote connections

Remote MCP registration resolves hostnames, rejects private/link-local targets (including `::ffff:127.0.0.1`), and resolves DNS before opening SSE transports. Launcher commands must use approved interpreter basenames (`node`, `python3`, `uv`, …).

## Code map

| Concern | Primary files |
|---|---|
| DLQ gating | `shared_defs.rs`, `server_handler.rs`, `server_methods.rs` |
| Dispatch dedupe / recovery | `dispatch_ops/dispatch.rs` |
| Foundry dedupe | `foundry_ops.rs` |
| Env sync CLI | `cli.rs`, `bootstrap/env_cmd.rs` |
| MCP SSRF | `mcp_connection.rs`, `utils.rs` |
| Atomic secret files | `utils.rs`, `claude_pool.rs`, `dispatch_ops/mcp_config.rs`, `bootstrap/serve.rs` |
