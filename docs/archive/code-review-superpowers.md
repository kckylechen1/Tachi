# Code Review: Virtual Capability Implementation Pass

**Reviewer:** OpenCode (Claude Opus)
**Branch:** `feat/virtual-capability-review-pass`
**Date:** 2026-03-26
**Methodology:** [obra/superpowers](https://github.com/obra/superpowers/tree/main/skills) — `requesting-code-review/code-reviewer.md` template
**Scope:** All 19 uncommitted files on top of `v0.10.0`

---

## Verification Status

| Check | Result |
|-------|--------|
| `cargo test -p memory-server` | **21/21 passed** |
| `npm run build` (tachi-desktop) | **Success** |
| Uncommitted files match claimed changeset | **Yes** (14 modified, 5 untracked) |
| No secrets in diff | **Clean** |

---

## Executive Summary

The Virtual Capability (VC) layer is well-structured, deterministic, and follows the existing hub governance patterns. The code is conservative — VCs only target MCP backends, resolution is priority-ordered with version pinning, and sandbox policies fall back from concrete to VC. The fail-closed `fs_read_roots`/`fs_write_roots` hardening for process transport is the right call. Tests cover the happy path and key edge cases (version mismatch, policy inheritance, fs roots rejection, project DB idempotency).

**Verdict: APPROVE with findings.** The 2 Important findings should be addressed before merge; the Critical finding should be triaged and at minimum documented as a known limitation.

---

## Critical Findings

### C-1: `tachi_init_project_db` does not attach the created DB to the running server

**File:** `crates/memory-server/src/project_db_ops.rs:3-55`

The handler creates a new SQLite database on disk via `MemoryStore::open()`, returns a success JSON including `activation.daemon_hint`, but **does not set `server.project_db_path`** or swap the project store on the running `MemoryServer` instance. The `_server` parameter is intentionally unused (prefixed with `_`).

This means:
- An agent calls `tachi_init_project_db` → gets back `"initialized": true`
- Agent then calls `vc_register` with `scope: "project"` → **silently falls back to global** because `server.project_db_path.is_some()` is still false (see `hub_ops.rs:1104`)
- The agent believes project-scoped data is being stored, but it is not

**Why it matters:** This is a correctness gap that violates the principle of least surprise. The tool creates a file but doesn't wire it up, and the agent has no way to know.

**Recommendation:** Either:
1. Have the handler hot-swap `server.project_db_path` (requires interior mutability, e.g. `RwLock<Option<PathBuf>>`) so the DB is immediately active, **or**
2. Document clearly in the tool response that the daemon must be restarted with `--project-db` to activate the created database, and change the response to include `"active": false` so agents know

---

## Important Findings

### I-1: `unwrap_or_default()` silently swallows corrupted VC binding metadata

**File:** `crates/memory-core/src/db/virtual_capability.rs:50`

```rust
let metadata = serde_json::from_str(&metadata_raw).unwrap_or_default();
```

If the stored `metadata` JSON is malformed, this silently replaces it with `{}` instead of surfacing the corruption. This is a data integrity concern — a corrupted row will appear normal but lose its metadata.

**Contrast:** The rest of the codebase (e.g., hub capability deserialization) propagates errors rather than swallowing them.

**Recommendation:** At minimum, log a warning when `from_str` fails. Ideally, propagate the error:
```rust
let metadata: Value = serde_json::from_str(&metadata_raw)
    .map_err(|e| rusqlite::Error::FromSqlConversionFailure(
        5, rusqlite::types::Type::Text, Box::new(e)
    ))?;
```

### I-2: VC bindings and VC registration can land in different databases

**File:** `crates/memory-server/src/hub_ops.rs:1185-1195` (bind) vs `1102-1108` (register)

`handle_vc_register` stores the VC capability based on the `scope` param (defaulting to project if available, else global). `handle_vc_bind` determines the target DB by checking whether the VC exists in the project DB. But if the VC was registered in global, and there's an active project DB, the bind logic at line 1185-1195 will check the project DB first, find nothing, and correctly fall back to global. **However**, if someone later creates a project DB and registers a *different* VC with the same ID in project scope, the bindings from global are now orphaned — `vc_list` (line 1225-1248) only returns VCs via `hub_discover` which merges both DBs, but the binding lookup at `server_methods.rs:151-172` returns project bindings first if non-empty, never merging with global bindings.

**Why it matters:** This creates a subtle split-brain scenario. A VC registered globally with bindings in global will stop resolving correctly once a project-scoped VC with the same ID appears (even if it has no bindings).

**Recommendation:** Either:
1. Forbid duplicate VC IDs across scopes (fail on register if the ID exists in the other scope), **or**
2. Document the precedence rule explicitly in tool descriptions so agents know project-scope shadows global

---

## Minor Findings

### M-1: `version_pin` cast from `i64` to `u32` can panic on negative values

**File:** `crates/memory-core/src/db/virtual_capability.rs:55`

```rust
version_pin: row.get::<_, Option<i64>>(3)?.map(|v| v as u32),
```

If a negative `i64` is stored in SQLite (e.g., via manual DB edit or a bug), `v as u32` will silently wrap. The insert path at line 29 uses `i64::from(u32)` which is always safe, but the round-trip is not fully defensive.

**Recommendation:** Use `v.try_into().unwrap_or(0)` or propagate the error.

### M-2: `handle_vc_register` hardcodes `version: 1` and `review_status: "approved"`

**File:** `crates/memory-server/src/hub_ops.rs:1125,1129`

Virtual capabilities are auto-approved on registration, bypassing the governance gates that protect concrete MCP capabilities. This is likely intentional (VCs are logical abstractions, not executable code), but it is undocumented and could surprise an agent that expects the full governance lifecycle.

**Recommendation:** Add a comment explaining why VCs skip review, or add a brief note in the tool description.

### M-3: `handle_vc_list` re-parses JSON that was just serialized

**File:** `crates/memory-server/src/hub_ops.rs:1230-1231`

```rust
let raw = handle_hub_discover(server, params).await?;
let mut items: Vec<Value> = serde_json::from_str(&raw)...
```

`handle_hub_discover` serializes to `String`, then `handle_vc_list` immediately deserializes back to `Vec<Value>` to inject binding data, then re-serializes. This is a minor perf concern (double serde round-trip) and a maintenance hazard (if `hub_discover` changes its output shape, this breaks at runtime not compile time).

**Recommendation:** Extract a shared `hub_discover_inner()` that returns `Vec<Value>` directly.

### M-4: `tachi_init_project_db` test doesn't clean up on failure

**File:** `crates/memory-server/src/tests.rs:1024`

```rust
let _ = std::fs::remove_dir_all(root);
```

This cleanup only runs if the test reaches line 1024. If any assertion panics before that, the temp directory leaks. This is minor (tests use UUID-named dirs), but `tempfile::TempDir` would handle this automatically.

### M-5: No test for `hub_call` routing through a Virtual Capability end-to-end

The VC resolve logic is well-tested, but there is no test that exercises `handle_hub_call` with a `vc:*` server_id end-to-end (i.e., verifying that `resolve_call_target` is actually invoked and the proxy call reaches the resolved backend). The current tests stop at `vc_resolve`.

**Recommendation:** Add an integration test that calls `hub_call` with a VC ID and asserts the resolved server field in the response. This can use a mock/stub MCP backend or just verify the resolution path up to the proxy call attempt.

### M-6: Desktop `api.ts` hardcodes `localhost:6919/mcp`

**File:** `apps/tachi-desktop/src/services/api.ts`

The daemon URL is hardcoded. If the daemon port is configurable (the server accepts `--port`), the desktop app should read from an environment variable or config.

---

## Strengths

1. **Deterministic VC resolution** (`server_methods.rs:174-268`): The priority-ordered, first-match resolution with full candidate reporting is excellent. Agents can inspect why a binding was skipped via the `candidates` array — this is exactly the kind of observability that makes debugging agent workflows tractable.

2. **Fail-closed sandbox** (`mcp_connection.rs:292-313`): Rejecting unenforceable `fs_read_roots`/`fs_write_roots` for process transport is the correct security posture. The audit trail logging before denial is thorough.

3. **Policy inheritance** (`server_methods.rs:387-405`): The fallback from resolved capability → requested VC ID for sandbox policy lookup is clean and handles the common case where you set policy on the VC rather than per-backend.

4. **Schema design** (`schema.rs:147-161`): The composite primary key `(vc_id, capability_id)` with the `ON CONFLICT ... DO UPDATE` upsert is correct and idempotent. The priority+id index ensures deterministic ordering.

5. **Test quality**: The version-pin-mismatch test (`tests.rs:922-990`) is particularly good — it verifies both the fallback behavior and the diagnostic output.

6. **Consistent error handling**: The codebase consistently uses `Result<String, String>` with descriptive error messages that include context (capability ID, status fields). This makes agent-side debugging much easier.

---

## Coverage Gap Analysis

| Area | Tested | Gap |
|------|--------|-----|
| VC register + resolve happy path | Yes | — |
| VC version pin mismatch fallback | Yes | — |
| VC policy inheritance | Yes | — |
| VC resolve with all bindings disabled | No | Should return clear error |
| VC resolve with target not callable (circuit open) | No | Would exercise `target_not_callable` path |
| `hub_call` with `vc:*` ID end-to-end | No | Only resolve is tested, not the full call path |
| `tachi_init_project_db` idempotency | Yes | — |
| `tachi_init_project_db` with non-git root | Implicit | `.git` check exists but not tested for rejection |
| `fs_read_roots` rejection | Yes | — |
| VC bind to non-MCP target rejection | No | Guard exists at `hub_ops.rs:1178` but untested |
| Cross-scope VC ID shadowing | No | The split-brain scenario from I-2 |

---

## Recommendations for Next Steps

1. **Fix C-1** (project DB activation) — this is the most impactful issue for the agent experience
2. **Address I-2** (cross-scope shadowing) — either forbid or document
3. **Add the 3-4 missing test cases** identified in the coverage gap table
4. **Commit the current changes** — the code is solid enough to land; the findings are improvements, not blockers (except C-1 which should at minimum have a clear `"active": false` in the response)
5. **Next phase**: The VC layer provides the abstraction needed for the installation governance closed loop — an agent can now `vc_register` a logical capability, then `hub_call` against it before any concrete backend is installed, receiving a clear error that guides it through `hub_install` → `hub_review` → `vc_bind`

---

*Review produced following [obra/superpowers requesting-code-review](https://github.com/obra/superpowers/tree/main/skills/requesting-code-review) methodology with [verification-before-completion](https://github.com/obra/superpowers/tree/main/skills/verification-before-completion) applied.*
