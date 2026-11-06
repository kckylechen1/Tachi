# Library Identity Runtime Contract

> **Status:** runtime contract, updated for workspace-first recall and native HTTP clients (2026-11-03).
> **Parent TRACK:** [#746](https://github.com/kckylechen1/tachi/issues/746)  
> **Companion:** [`multi-project-daemon.md`](./multi-project-daemon.md) (daemon architecture)

## 1. Product invariant

One resident daemon serves **global + many project libraries**. Every request must
resolve a single **library identity** (named project or global) for writes, and
must not silently corrupt another library's DB, vector coverage, or health labels.

```text
session binding (HTTP headers / initialize meta / owner CLI)
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
| **HTTP direct-connect** | Host supplies `x-tachi-project` or `x-tachi-workspace-root`; legacy connections bind at initialization, modern requests carry the same context on each request | header `x-tachi-profile` / meta `tachiProfile` |
| **CLI tool dispatch** | Forwards bound project so daemon C1 sees a bound session | profile as today |

Binding rules:

1. **Immutable for the session lifetime** once set at initialize.
2. Project name must resolve via `resolve_named_project_db_path` (sanitized, no path traversal).
3. Existence of the project DB is the current authorization model for binding
   (single-user / loopback). Multi-tenant ACL of header claims is **#495**, out of scope.

Constants live in `crates/tachi-server/src/session_identity.rs`, which is the
authoritative list of `x-tachi-*` headers and their `_meta` twins (for example
`x-tachi-project` / `tachiProject`, `x-tachi-profile` / `tachiProfile`,
`x-tachi-client` / `tachiClient`). The dotted `_meta` aliases (`tachi.project`
and friends) are read in `server_handler.rs::request_identity`. Header-only values such
as `x-tachi-rate-limit-session` are described in
[`http-direct-connect.md`](./http-direct-connect.md).

## 3. Read / write asymmetry (#737)

| Call shape | Bound session | Unbound session |
|---|---|---|
| Tool without `project` (project-defaulting) | Inject bound project | Public memory saves require binding or an explicit global destination; read defaults remain available |
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

### Workspace-first recall and writes

The host determines the calling workspace. Agents normally omit `project`;
canonical names and hash aliases are addressing details, not fields an agent
must reconstruct from a folder name.

HTTP tool calls also accept the existing `tachiWorkspaceRoot` / `tachiProject`
metadata per request, alongside ZCode host context. Optional advertised MCP
Roots is a compatibility source when no explicit binding exists. All candidate
paths are normalized and deduplicated before any automatic registration; only
one library can be selected. The selection belongs to that call, never to the
shared daemon or a later context-free call. See the
[HTTP cookbook](http-direct-connect.md#automatic-workspace-selection).

Default ordinary recall searches one layer at a time:

1. Search the bound workspace and apply relevance, lifecycle, temporal and access filters.
2. If no eligible result remains, search global memory. Wiki uses its existing shared-wiki store for this layer; legacy global Wiki is not reintroduced.
3. If still empty, search the explicitly configured `routing.json` `fallback_projects` list, followed by `fallback_workspaces` directory paths, in order. Stop at the first eligible result.

For example, `{"fallback_projects":["team-notes"],"fallback_workspaces":["/work/reference"]}`
adds two fallback sources. Directory entries must be absolute, existing workspace
directories with registered memory stores. Read fallback never creates a database
or silently registers a new identity; bind a new directory once before listing it.

An explicit `project` selects only that library. Transport-injected project defaults
retain an internal provenance marker so they do not accidentally become explicit
single-library requests. Duplicate aliases for one physical DB are searched once.
Binding, identity, permission, store-open and query failures propagate as errors;
they are never converted to an empty result to trigger fallback. Results retain
their actual library source. Lookup does not write into fallback libraries.

`content_scope` selects recalled content (`all`, `memory`, `wiki`, `patterns`,
`sft`); `storage_scope` selects the save destination. The old action-dependent
`scope` spelling remains accepted for compatibility. Conflicting selectors or a
selector used for the wrong action are rejected with an actionable error.

Public saves default to the current workspace and refuse an unavailable project
binding instead of downgrading to global. An explicit global save remains
available. Historical internal global bookkeeping retains its own routing; a
read fallback does not change the caller's write destination.

A workspace can be an explicitly declared ordinary directory. Its default store
is `<workspace>/.tachi/tachi-memory.db`. Descendants reuse an established workspace
boundary; Git checkouts retain their repository/worktree normalization. Registered
identities, containment and physical-store checks apply equally to ordinary
folders. The daemon's launch directory never substitutes for caller context.

## 4. Workspace / worktree routing (#701)

- Bare `PWD` env must **not** beat real cwd for project scope.
- Explicit overrides (`TACHI_PROJECT_ROOT` / `TACHI_WORKSPACE_ROOT`) remain intentful.
- HTTP `X-Tachi-Workspace-Root` accepts an absolute directory on the daemon host, including a non-Git folder. Ordinary folders reuse the nearest registered or database-backed ancestor; a configuration-only `.tachi` directory does not establish a workspace.
- First-contact registration is serialized within the daemon through reservation, initialization and manifest commit. It does not replace the shared daemon default project slot.
- Read-only directory fallback resolves an already registered identity without creating databases or registry entries; missing or damaged identity evidence is an error.
- Public memory saves and checkpoints never downgrade an unavailable project destination to global. Notes validate the destination before creating their files. Internal system callers retain their explicitly separate compatibility behavior.
- Canonical `storage_scope=global` rejects an explicit `project` argument. The legacy `scope=global` plus explicit project combination retains its existing project precedence.
- Linked git worktrees resolve to the **primary checkout** project scope (via
  `gitdir:` / `commondir`), so dispatch worktrees do not invent orphan project DBs.

## 5. Plan C alias identity (#736 / #743)

- Derived alias name is **casefold-stable** on case-insensitive FS (`plan_c_dir_name_from_root`).
- User-facing status/coverage labels use the **current** derived name for a
  canonical DB file (one identity per file).
- Optional physical retirement of drifted **old-hash** alias dirs is
  **`--fix`-gated** only; legacy un-hashed basename aliases are never retired by that path.
- Repo-local DB files and `*.migration-bak.*` are never deleted by identity repair.

### Address aliases and persistent role stamps

The session's public project alias is an address, not a new store identity.

Before probing a named store's persistent role, a resident runtime verifies
any existing bound or attached handle against the current physical file.
Replacement is refused before inspecting SQLite metadata; cache misses remain
unattached. Identity-checked operations still verify their checked-out handle
before and after the operation.
Changing the root-hash naming generation must not rewrite a database's
write-once `store_identity/role` stamp. Named reads, writes, request-scoped
search and project health probes share `resolve_named_project_store_binding`
(`server_methods/db.rs`), which resolves the address and persistent role
separately.

An exact role claim, including the established `project:` legacy stamp
spelling, follows memcore's normal admission rule. A different alias is
compatible only when both names are known naming-generation derivations of
the same repository root and both resolve unambiguously to the same database.
The resulting store claim uses the persistent spelling; session binding and
cross-project write enforcement keep their existing public alias semantics.
The identity probe is read-only. The subsequent store open still enforces
the role, profile, schema and migration gates.

An arbitrary symlink, matching basename, copied foreign stamp, missing alias
evidence or divergent manifest is insufficient. Wiki/global roles do not
gain project compatibility. Unknown identities remain an error, never a
reason to clear a stamp, create an empty replacement DB or fall back to global.
This compatibility rule does not authorize an arbitrary repository relocation
or a store-role migration.

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
- Session enforcement and C1 guards apply at the HTTP request boundary.
- Public Ops/admin profile claims are refused; supported owner CLI commands use a private authenticated capability.
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
| #737 read/write asymmetry | `bootstrap/serve/http_tests.rs` (`allows_explicit_cross_project_read`, `rejects_cross_project_override`); `session_identity` unit tests |
| #701 PWD / worktree | `utils/tests.rs` |
| #736 / #743 alias | `path_utils/tests.rs`, `repair/tests` |
| #744 predicate | vector_backfill / status shared predicate tests |
| #702 shape | closed with repro notes; contention path hardened separately |
| C1 unbound write | `session_identity` unit tests + handler call_tool path |

## 9. Acceptance for #746 TRACK

- [x] Documented runtime contract (this file) across HTTP initialize, alias, vector, search reads.
- [x] Closed children (#737, #736, #743, #744, #701, #702) have discrimination coverage (see §8).
- Native HTTP capability from #732 is delivered; client configuration migration and workstation deployment are separately verified.
- Cross-library reads remain available through explicit selection and ordered fallback; writes stay isolated.

## 10. Agent rules of thumb

1. Use host-bound HTTP requests for MCP writes; first-party Tachi stdio is retired. A workspace path or registered project identity establishes the binding.
2. Cross-library **search/get** with explicit `project=` is a feature, not a bug.
3. Do not rsync operator crates into Hypermem to “fix routing” — identity lives in
   Tachi daemon + this contract.
4. When adding a new tool, classify it in `project_defaults_to_bound_project` and
   `explicit_project_can_cross_binding` **before** shipping, or writes default wrong.
