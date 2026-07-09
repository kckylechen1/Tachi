---
title: "Multi-Project-Aware Daemon"
summary: "Resolves the stdio-proxy scope deadlock (#730) at its root: one resident daemon dynamically serves the global DB plus every project's DB on demand, so stdio sessions from any project directory proxy cleanly instead of fataling. The agent-OS terminus for the recurring stdio lifecycle/scope bugs."
category: "engineering/architecture"
organize: true
---
# Multi-Project-Aware Daemon

Design, not yet an approved implementation plan. Owner-directed 2026-07-06 after the 1.6.3 deploy dogfood surfaced #730. Resolves a recurring class of stdio bugs at the root rather than patching another special case.

## 1. Why this exists — the recurring-bug root cause

Four separate stdio fixes landed in one day (2026-07-06): idle reaper (#712), write-proxy enforcement (#716), version-skew guard (#714), and then #730 — a fatal on the very deploy that shipped them. They are not four bugs; they are one structural flaw surfacing four ways.

**The flaw**: the stdio-proxy scope model assumes *one daemon serves the entire scope a stdio session needs*. Concretely, `ensure_stdio_proxy_daemon` (`bootstrap/serve/stdio.rs:7`) resolves to one of three states:

- `Compatible` → proxy to it,
- `Missing` → spawn a new daemon with the requested scope,
- `Incompatible` → **`return None` → fatal** (`stdio.rs:24`).

`Incompatible` fires when a daemon already owns the same `global.db` but its project scope doesn't match the request. It cannot reuse the daemon (wrong project scope) and cannot spawn a second one (two daemons on one `global.db` is exactly the #520 lock storm). Deadlock → fatal.

**Why it's unavoidable in the current model**: a single `--project-db` binds one project. A workstation runs multiple projects (Sigil, Quant, RomanBath), each with its own `.tachi/memory.db`, all sharing one global daemon. A global-only (or single-project) daemon structurally cannot serve the project scope of a *different* project's stdio session. Every prior patch worked around this without dissolving it.

**Compounding**: the load-bearing functions here — `compatible_daemon`, `StdioProxyServer`, `daemon_matches_requested_dbs` — have **no end-to-end test coverage** (only scope-match unit tests exist). That is why regressions slip through repeatedly.

## 2. The load-bearing insight (what #520 actually protects)

#520's real target is **multi-writer lock contention on the global DB** — every session shares one `global/memory.db`; 18 concurrent direct openers produced the `SQLITE_BUSY` storm. Project DBs are per-project files; only concurrent sessions *within the same project* contend, an order of magnitude rarer.

So the correct terminus is not "global-only daemon + fatal on project requests," but **one daemon that owns every DB file as its single writer** — global and every project — so no other process ever opens any of them directly. That is the agent-OS shape the owner has described (one resident process managing all of the machine's memory).

## 3. Target architecture

```
single resident daemon (port 6919)
  ├─ global/memory.db                    (always open, single writer)
  ├─ Sigil/.tachi/memory.db              (attached on first request, cached)
  ├─ Quant/.tachi/memory.db              (attached on demand)
  └─ RomanBath/.tachi/memory.db          (attached on demand)

every stdio session  →  pure proxy  →  daemon routes each call to (global | that session's project)
```

- The daemon holds a **project-DB connection registry** keyed by canonical project-DB path, opened lazily on first request, cached, idle-reclaimed.
- stdio proxy becomes **unconditionally pure transport**: it never opens a DB, never fatals on scope, never spawns a competing daemon. It forwards the session's project context; the daemon routes.
- `compatible_daemon` collapses: any daemon on the matching `global.db` is compatible for *any* project, because the daemon can attach that project. The `Incompatible` state — and the #730 fatal — cease to exist.
- Single-writer coverage becomes **total**: global and every project DB have exactly one writer (the daemon). #520's protection extends to project DBs for free, which today it does not.

## 4. Key design decisions (each is a place this has broken before — decide explicitly)

### 4.1 How the daemon learns a request's project scope
The proxy must convey, per session, which project DB the session belongs to. Options:
- **target shape: per-session project binding at handshake**: the proxy sends the canonical project-DB path in `initialize` (a client-info/meta field); the daemon binds this session→project for its lifetime. Per-call routing then needs no extra data. Simplest, matches "a stdio session is one project."
- **Phase 1 stdio compatibility path**: until daemon-side session binding exists, the stdio adapter validates the project name↔DB path pair once, rejects any per-call `project` mismatch, and injects the bound project into project-defaulting calls before forwarding. This keeps stdio pure transport and removes the #730 fatal without pretending HTTP direct-connect binding is solved.
- rejected — unchecked per-call project arg: bloats every tool call, invites mismatch between calls in one session.

### 4.2 Scope routing inside the daemon (global vs project per call)
The daemon already distinguishes global vs project writes internally (the `scope` field on memory ops). The router uses the **session's bound project** as the project-DB target and the shared global DB as the global target; each tool's existing scope semantics pick which. No new per-tool classification is invented — the existing `scope=user|project|general` mapping is reused, verified against `MemoryStore`'s current global/project split.

### 4.3 Project-DB connection lifecycle
- Lazy open on first project request; cached in the registry.
- **Idle reclaim**: a project connection unused for N minutes is closed (WAL checkpointed first) to bound resource use on a machine with many projects. Provisional N = 30 min, calibrated later. Reuses the existing idle-reaper machinery conceptually, applied per-project-connection.
- Bounded registry size with LRU eviction (provisional cap, telemetry-calibrated) so a machine with hundreds of touched projects can't exhaust FDs.

### 4.4 Security isolation (NEW hard requirement — the daemon now opens paths on behalf of clients)
Today a stdio session opens its own project DB under its own FS permissions. Once the **daemon** opens arbitrary project paths on behalf of a proxy, a hostile/buggy proxy could ask the daemon to open a DB outside the caller's rights (privilege confusion / path traversal). Requirements:
- The proxy may only bind a project path that the **calling process can itself read** — the daemon verifies the requesting process's access (or the proxy passes an already-opened FD; decide in impl spec) before attaching.
- Canonicalize + validate the project path (no symlink escape, must end in `/.tachi/memory.db` under a real project root); reject anything else.
- A session bound to project P can never route to project Q's DB — binding is immutable per session.
- This isolation MUST have a discriminating test (a session bound to Sigil attempting a Quant-scoped op is refused).

Phase 1 includes the minimum safe subset of this section: project binding must be derived from a validated project DB path/name pair, same-global daemons may only accept a project-scoped stdio client when the project name resolves back to that exact DB, and the binding is fixed for the adapter lifetime. Broader local-auth/profile policy can land later, but Phase 1 cannot accept arbitrary client-supplied paths or mutable project identity.

### 4.5 CLI-arg evolution
- `--project-db <path>` / `--no-project-db` become **optional hints** (a project to pre-attach at boot), not the daemon's whole project scope. Default resident daemon needs neither — it attaches on demand.
- The launchd/brew default invocation drops `--no-project-db` (it currently *causes* #730). The daemon boots global-only and attaches projects lazily. Backward compatible: an explicit `--project-db` still pre-attaches that one.

### 4.6 Daemon-owns-DB vs. the write-proxy law
Reinforces #716 rather than relaxing it: now *no* client (stdio or CLI) ever opens *any* DB directly — global or project. `maybe_forward_server_write` already routes CLI writes to the daemon; this extends the same law to project scope. `TACHI_DISABLE_STDIO_PROXY=1` remains the debug-only escape hatch (accepts direct-open contention).

## 5. Test strategy (this is the part that has been missing)

The recurring regressions trace directly to zero end-to-end proxy coverage. This work is gated on adding it:
- **End-to-end proxy behavior tests** (the gap today): spin a real daemon on a temp global DB, run a proxied stdio session from a temp project dir, assert reads/writes land in the right DB file and never open it directly. Multi-project: two sessions, two projects, one daemon — assert isolation.
- **#730 discrimination test**: the exact failing scenario (global-only daemon + project-dir stdio) must succeed post-fix and FAIL on current `main`.
- **Security isolation test** (4.4): cross-project routing refused.
- **Idle reclaim + LRU eviction** tests for 4.3.
- Existing scope-match unit tests (`daemon_matches_requested_dbs`) are updated to the collapsed compatibility model, not deleted.

## 6. Phasing (each independently shippable and useful)

- **Phase 1 — daemon project registry + lazy attach + per-session binding + routing + minimum binding safety** (4.1–4.3 plus the minimum subset of 4.4). Ships the core; #730 fatal gone. Gate: end-to-end multi-project proxy test green, #730 discrimination test green, project binding path/name validation green, and cross-project binding refusal green.
- **Phase 2 — security isolation hardening** (the remaining 4.4 work). Ships caller-access proof or FD handoff, local-auth posture, and any multi-user safeguards. Independently mergeable; Phase 1 remains single-user/loopback-trusted only and must not expose mutable or arbitrary path binding.
- **Phase 3 — lifecycle polish**: idle reclaim tuning, LRU cap calibration from telemetry, CLI-arg deprecation cleanup (4.5).

Phase 1 alone removes the production regression; 2 and 3 harden and tune.

## 7. Interim (until Phase 1 ships)
`TACHI_DISABLE_STDIO_PROXY=1` in the MCP client env restores connectivity by forcing local serve (accepts the pre-#716 direct-open contention). Documented as the stopgap in #730. Not a fix — the lock-storm protection is off while it's set.

## 8. Premise collapse
This design assumes the daemon can hold many SQLite write connections (global + N projects) without the intra-process contention it's meant to prevent. If a single daemon serializing writes across many project DBs becomes a throughput bottleneck (unlikely — writes are small and infrequent, and the async writer queue #520/#546 already serializes), the fallback is per-project writer tasks inside the daemon, not multiple daemons. The one-daemon-owns-all-DBs invariant is non-negotiable; how it internally parallelizes is tunable.

## 9. Related
- #730 (the regression this resolves), #716 (write-proxy law, extended not relaxed), #520 (lock-storm root; protection now covers project DBs), #728 (deploy pipeline — should catch scope-config interactions), #546 (async writer queue — the internal serialization substrate). Aligns with the Tachi-as-agent-OS product thesis (one resident process owning all machine memory).

## 10. Current-state note (2026-07-09)

Phase 1 substrate and most **#746** child bugs have landed. The **executable
runtime contract** (session binding headers/meta, read/write asymmetry, Plan C
alias identity, worktree scope, vector predicate agreement, HTTP initialize) is
documented in
[`library-identity-runtime.md`](./library-identity-runtime.md).

Remaining under the library-identity track is primarily **#732** client
migration / reconnect cookbook and parity dogfood — not another redesign of
this multi-project daemon doc.
