# Library Identity Runtime Contract

> **Status:** current-state contract (2026-07-09), not a green-field design.  
> **Parent TRACK:** [#746](https://github.com/kckylechen1/tachi/issues/746)  
> **Companion:** [`multi-project-daemon.md`](./multi-project-daemon.md) (daemon architecture)

## 1. Product invariant

One resident daemon serves **global + many project libraries**. Every request must
resolve a single **library identity** (named project or global) for writes, and
must not silently corrupt another library's DB, vector coverage, or health labels.

```text
session binding (stdio env / HTTP headers / initialize meta)
        │
        ▼
session_identity enforce  ──writes──► only bound project (or global scope)
        │
        ├──reads with explicit project= ──► cross-library OK (read-only open)
        │
        ▼
path_utils / plan_c alias  ──► physical DB path
        │
        ▼
MemoryStore (db_label) + vector/health surfaces (same identity predicate)
```

## 2. Session binding (how identity is declared)

| Transport | How project binds | How profile binds |
|---|---|---|
| **stdio proxy** | Adapter validates name↔DB at spawn; injects bound project for project-defaulting tools; forwards `X-Tachi-Project` to daemon | Adapter env / forwarded headers |
| **HTTP direct-connect** (`http://127.0.0.1:6919/mcp`) | `initialize`: header `x-tachi-project` **or** meta `tachiProject` (aliases `tachi.project`) | header `x-tachi-profile` / meta `tachiProfile` |
| **CLI tool dispatch** | Forwards bound project so daemon C1 sees a bound session | profile as today |

Binding rules:

1. **Immutable for the session lifetime** once set at initialize / adapter spawn.
2. Project name must resolve via `resolve_named_project_db_path` (sanitized, no path traversal).
3. Existence of the project DB is the current authorization model for binding
   (single-user / loopback). Multi-tenant ACL of header claims is **#495**, out of scope.

Constants live in `crates/tachi-server/src/session_identity.rs`:

- Headers: `x-tachi-project`, `x-tachi-profile`, `x-tachi-client`
- Meta keys: `tachiProject`, `tachiProfile`, `tachiClient` (+ dotted aliases)

## 3. Read / write asymmetry (#737)

| Call shape | Bound session | Unbound session |
|---|---|---|
| Tool without `project` (project-defaulting) | Inject bound project | Global / workspace default as today |
| Explicit `project=` **same** as binding | OK | N/A |
| Explicit `project=` **different**, **read** action | **Allowed** (cross-library read) | **Allowed** for the same read allow-list |
| Explicit `project=` **different**, **write/destructive** | **Rejected** (binding mismatch) | **Rejected** (C1 unbound write) |

Read allow-list (representative; source of truth is `explicit_project_can_cross_binding`):

- Retired compatibility reader names: `search_memory`, `get_memory`, and `find_similar_memory`. Live native readers: `list_memories` and `tachi_search`. The retired `memory_graph` / `get_edges` routes were dropped from this list in #757 and internalized off the MCP surface, so there is no tool call left to cross-binding-check.
- `tachi_memory` actions: `search`, `get`, `ask`, `briefing`, `alerts`, `consolidate`
- `tachi_wiki`: `browse`, `read`, `search`
- `tachi_event`: `metrics`, `query`

Writes (`save`, `archive`, `extract_facts`, `checkpoint`, …) stay bound. Permanent
delete is an identity-targeted operator CLI operation rather than a Memory action.

**Invariant protected:** single-writer isolation + no cross-tenant write routing.
Cross-library **reads** use daemon read-only opens and do not threaten that invariant.

## 4. Workspace / worktree routing (#701)

- Bare `PWD` env must **not** beat real cwd for project scope.
- Explicit overrides (`TACHI_PROJECT_ROOT` / `TACHI_WORKSPACE_ROOT`) remain intentful.
- Linked git worktrees resolve to the **primary checkout** project scope (via
  `gitdir:` / `commondir`), so dispatch worktrees do not invent orphan project DBs.

## 5. Plan C alias identity (#736 / #743)

- Derived alias name is **casefold-stable** on case-insensitive FS (`plan_c_dir_name_from_root`).
- User-facing status/coverage labels use the **current** derived name for a
  canonical DB file (one identity per file).
- Optional physical retirement of drifted **old-hash** alias dirs is
  **`--fix`-gated** only; legacy un-hashed basename aliases are never retired by that path.
- Repo-local DB files and `*.migration-bak.*` are never deleted by identity repair.

## 6. Vector coverage vs embed selection (#744)

Coverage **display** and embed **selection** share one durable-row predicate
(recall-cache exclusion by default). Status “missing vectors” and
`backfill-vectors` / sweep selection must not disagree on basis.

## 7. HTTP direct-connect (#732)

Full cookbook: [`http-direct-connect.md`](./http-direct-connect.md).

### Landed

- Streamable HTTP MCP on loopback (`/mcp`, typically port 6919).
- Per-session identity at `initialize` (**headers and/or meta**) →
  `session_project` / profile on `MemoryServer` clone.
- Same enforce + C1 guards as stdio (`HTTP direct-connect` transport label).
- Profile `admin` refused over HTTP until #495 authorization policy exists.
- `/health` advertises `bind`, `auth_posture=loopback-trust-v1`, reconnect hints.
- CLI client forwards `X-Tachi-Project` when needed.
- CI goldens: header bind, meta bind, admin reject, unbound C1 write, bound
  cross-project write reject, global+project save landing.

### Remaining (optional / other issues)

| Gap | Owner |
|---|---|
| Multi-tenant ACL of header claims | **#495** |
| Live workstation dogfood (`pgrep` inventory with Claude Code HTTP) | ops note in cookbook — not a code gate |

## 8. Discrimination tests (where they live)

| Child | Primary tests |
|---|---|
| #737 read/write asymmetry | `bootstrap/serve/stdio/tests.rs` (`allows_explicit_cross_project_read`, `rejects_cross_project_override`); `session_identity` unit tests |
| #701 PWD / worktree | `utils/tests.rs` |
| #736 / #743 alias | `path_utils/tests.rs`, `repair/tests` |
| #744 predicate | vector_backfill / status shared predicate tests |
| #702 shape | closed with repro notes; contention path hardened separately |
| C1 unbound write | `session_identity` unit tests + handler call_tool path |

## 9. Acceptance for #746 TRACK

- [x] Documented runtime contract (this file) across stdio, HTTP initialize, alias, vector, search reads.
- [x] Closed children (#737, #736, #743, #744, #701, #702) have discrimination coverage (see §8).
- [ ] #732 full client migration + parity dogfood (remaining leaf).
- [x] No landed fix in this family weakens global+project recall (cross-library **read** kept; writes stay isolated).

## 10. Agent rules of thumb

1. Prefer **bound sessions** for writes (stdio from project cwd, or HTTP with `x-tachi-project`).
2. Cross-library **search/get** with explicit `project=` is a feature, not a bug.
3. Do not rsync operator crates into Hypermem to “fix routing” — identity lives in
   Tachi daemon + this contract.
4. When adding a new tool, classify it in `project_defaults_to_bound_project` and
   `explicit_project_can_cross_binding` **before** shipping, or writes default wrong.
