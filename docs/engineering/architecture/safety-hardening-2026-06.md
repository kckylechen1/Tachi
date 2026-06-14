# Safety & Idempotency Hardening (2026-06)

Reference for the post–Gemini review hardening batch merged after v1.5.3. Operator-facing summary lives in [`CHANGELOG.md`](../../../CHANGELOG.md#unreleased); install notes in [`docs/INSTALL.md`](../../INSTALL.md).

## Scope

| Area | Problem | Mitigation |
|---|---|---|
| DLQ replay | Mutating tool failures could be retried and double-apply side effects | Enqueue only when `should_enqueue_dlq()`; `dlq_retry` rejects native + non-idempotent tools |
| Dispatch dedupe | Identical bare dispatches (no `flow_id`) could spawn duplicate workers | Fingerprint lock under `~/.tachi/runs/.dispatch-dedupe/` |
| Dispatch recovery | Daemon crash left orphaned in-flight runs | `recover_orphaned_dispatch_runs()` on startup |
| Foundry queue | `queue_agent_evolution` minted random job ids → duplicate synthesis | Deterministic job id + active/terminal dedupe + stale `running` reclaim (30 min) |
| Env sync | `tachi env sync` wrote plaintext secrets by default | Preview-only default; `--apply` required to write |
| MCP SSRF | Remote MCP URLs could target internal addresses | Expand-then-validate, IP blocklist, async DNS before SSE connect |
| Config TOCTOU | Sensitive files created world-readable or truncated in place | Atomic create-new + rename with `0o600` on Unix |
| Subprocess allowlist | Caller could inject `allowlist` on restricted profiles | Rejected for Codex/Grok/Kimi dispatch profiles |

## DLQ semantics

**Safe to enqueue / retry (examples):** read-only search, `get_memory`, idempotent status probes.

**Not safe (skipped):** `hub_call`, write tools (`save_memory`, `post_card`, …), mutating facade actions (`tachi_save`, `tachi_dispatch`, `tachi_task` writes, …).

Implementation: `shared_defs.rs` — `dlq_mutation_is_unsafe()`, `should_enqueue_dlq()`.

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
