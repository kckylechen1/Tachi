# Portable Kernel Split — memcore vs admin

> **Status:** landed 2026-07-09 (first cut); historical purpose retired and
> future direction recorded 2026-07-17 (#1195).
> **Anchors:** #770, #790–#794, #833 Phase 1.1, #1195.  
> **Companions:** [`kernel-surface-v1.md`](./kernel-surface-v1.md),  
> [`downstream-sync-surface.md`](./downstream-sync-surface.md) (when present).
>
> **Current implementation:** `portable-kernel` re-exports `memcore` with
> admin features disabled. `portable-server` exposes that kernel over MCP.
> The current Tachi Cargo inventory contains no direct ZeroClaw integration.
>
> **Owner ruling 2026-08-03 (#1585), superseding the 2026-07-17 consumer
> framing:** Hypermem is the supported external consumer of this boundary.
> Tachi owns the reusable memory kernel; Hypermem is an independent component
> (standalone or embedded in Hyperion) that depends on `portable-kernel`
> **only**, opening explicitly supplied Hyperion-owned databases. Hyperion
> owns A-share policy, HAPI aliases, model-provider configuration, runtime
> projection, packaging, and cutover; Tachi product adapters remain
> downstream. Hypermem does not consume Tachi Hub, Vault, Foundry, dispatch,
> kanban, agent profile, the generic Wiki server, `tachi-llm`, or the full
> Tachi server. The 2026-07-17 (#1195) ZeroClaw candidacy note stands as a
> separate, non-exclusive direction.
>
> **Historical provenance:** this split was cut for HyperTachi, HyperMemory,
> and adapter convergence. Hyperion-HyperTachi separated on 2026-07-14
> (`9da2015`); the pre-#1585 consumer framing below is retained only as the
> historical record of why the split was cut.

## Why

`memcore` mixed two audiences:

1. **Portable kernel** - store, schema, hybrid search/scorer, graph, events,
   sandbox. This is the current compile boundary. Per #1195 it is also a
   candidate boundary for a future ZeroClaw-native module after design and
   integration. Historically, HyperTachi, HyperMemory, and adapters motivated
   the split (see § Catch-up experiment results for the 2026-07-09 record).
2. **Tachi admin** - vault secrets, Hub capability catalog, Foundry job queue
   types, Pack / agent_profile product surfaces. Operator product only.

Without a compile boundary, a future embedder or the historical downstream
fork either:

- rsyncs the whole monorepo and inherits operator APIs, or
- hand-picks files and drifts.

## What landed

### Cargo feature on `memcore`

| Feature | Default | Contents |
|---|---|---|
| `admin` | **on** | vault, hub, foundry job types, pack, agent_profile modules + store methods |
| _(none)_ | use `default-features = false` | portable kernel only |

Since #1585, schema DDL is profile-scoped: a fresh `PortableKernel` store
creates only kernel tables (no Hub/Vault/Foundry/ExecEnv/dispatch/…), while a
portable build can still open a `TachiFull` database — the stored profile,
never the caller's requirement, drives DDL and migrations, so opening never
downgrades an existing store's schema.

### Facade crate `portable-kernel`

```
crates/portable-kernel
  └── depends on memcore with default-features = false
```

- Re-exports the portable `memcore` API.
- `ADMIN_SURFACE_ENABLED == false` by construction.
- Smoke tests: open/upsert/stats without admin types.

### Tachi product unchanged

`tachi-server`, `tachi-hub`, `tachi-foundry`, `tachi-llm`, etc. still depend on
`memcore` with **default features** (admin on). No call-site rewrites.

## How downstream should fork / sync

| Consumer | Sync unit | Feature flag |
|---|---|---|
| Hyperion-HyperTachi | `memcore` **or** `portable-kernel` | `default-features = false` on core |
| HyperMemory runtime binary | same + thin MCP/CLI profile | no dispatch/gh/merge crates |
| RomanBath `zeroclaw-memory-sigil` | algorithm/tests only (not crate path dep) | n/a |
| Projects zeroclaw | MCP adapter contract, not crates | n/a |

**Do not** rsync `tachi-server` + operator leaves into HyperTachi as a
prerequisite for memory quality. Prefer:

```text
1. Merge / vendor crates/memcore (portable build)
2. Rebuild Hypermem binary
3. Run hypermem compatibility gate + trading smoke
```

## Product policy injection (MemCore hooks)

Downstream product policy stays **outside** the kernel. MemCore exposes hooks:

| Hook | On | Default |
|---|---|---|
| `SearchOptions::precision_matchers` | `Vec<Arc<dyn PrecisionMatcher>>` | empty |
| `SearchOptions::decay_policy` | `Option<Arc<dyn DecayPolicy>>` | `None` → `DEFAULT_DECAY_POLICY` |

Example (HyperMemory trading half-lives):

```rust
let opts = SearchOptions {
    decay_policy: Some(Arc::new(TradingDecayPolicy)),
    ..Default::default()
};
store.search(query, Some(opts))?;
```

Do not re-hardcode A-share / character-card decay constants into `memcore`.

### Exact-dedupe downstream sync

`crates/memcore/src/store/exact_dedupe.rs` is the portable source of truth;
`crates/tachi-server/src/repair/exact_dedupe.rs` is the Tachi-owned CLI/daemon
ownership adapter. HyperTachi tracks the portable core in downstream
[HyperTachi#61](https://github.com/kckylechen1/Hyperion-HyperTachi/issues/61),
with reference commit
[`74c66dec`](https://github.com/kckylechen1/Hyperion-HyperTachi/commit/74c66dec).

As of tachi#1348-A, the plan/apply/restore goldens (happy path, revision-drift
refusal with batch rollback, edge transfer/dedup/self-loop drop, receipt
round-trip via `restore_exact_dedupe`, `memories_vec` conservation across
apply) live in-repo at
`crates/portable-kernel/tests/exact_dedupe_contract.rs`, run with
`cargo test -p portable-kernel --features portable-contract-test` the same
way `portable_contract.rs` does. That goldens file is the mechanism a
portable consumer (including a CI job outside this monorepo) can point at
directly; the HyperTachi manual sync above remains the cross-repo check for
as long as HyperTachi has not switched to consuming the shared crate, but it
is no longer the only mechanism proving this contract.

## Targeted issue lanes (after this split)

| Lane | Scope | Examples |
|---|---|---|
| **Portable kernel** | schema, CRUD, hybrid/RRF, vector readiness, events | #708 recall floor, #790 surface, #793 hypermem gate |
| **Tachi admin** | vault, hub governance, foundry queue product | vault residual, hub CLI, foundry scheduler |
| **Tachi operator product** | dispatch, gh, ship, merge, rescue | #878 predicate, merge-ops, component governance |
| **Adapter / product policy** | zeroclaw-memory-sigil, HyperMemory MCP profile | #792 adapter, RomanBath chat partition |

Issues should name their lane. A portable PR must not require
`tachi-dispatch` / `tachi_gh` / merge-ops to compile or test.

## Verify

```bash
export CARGO_TARGET_DIR=$HOME/.cache/sigil-shared-target

# Full Tachi product surface (admin on)
cargo test -p memcore

# Portable kernel only
cargo test -p memcore --no-default-features
cargo test -p portable-kernel
```

## Not in this cut

- Physical extract of vault/hub/foundry into separate crates (schema still shared;
  feature gate is enough for fork/compile isolation).
- `memory-server-lite` binary profile (next step once portable packaging is
  dogfooded on a native embedder per #1195; per the 2026-08-03 #1585 ruling,
  Hypermem is the supported external consumer of the library boundary, and it
  owns its own standalone/embedded shell — no server profile is owed here).
- Moving schema admin tables behind feature flags (would break open of full DBs).

## Acceptance

- [x] `memcore` has documented `admin` feature; default keeps Tachi green.
- [x] `portable-kernel` builds and tests with admin off.
- [x] Portable build still opens a schema that includes admin tables.
- [x] HyperTachi catch-up experiment: portable `memcore` only (2026-07-09).
      See § Catch-up experiment results below.
- [ ] Optional later: extract admin into `memory-admin` crate if feature gate
      proves insufficient for packaging.

## Catch-up experiment results (2026-07-09)

Isolated worktree on Hyperion-HyperTachi pin `21a43095` → branch
`experiment/portable-memcore-catchup` → draft PR
[Hyperion-HyperTachi#20](https://github.com/kckylechen1/Hyperion-HyperTachi/pull/20).

| Metric | Result |
|---|---|
| HT-only files under old `memcore` | **0** (pure older subset) |
| Compile errors in HT `tachi-server` after core replace | **14 → 0** (mechanical API) |
| `memcore` portable tests in HT tree | **254** passed |
| `memcore` full (admin) tests in HT tree | **278** passed |
| `portable-kernel` in HT tree | **2** passed |
| `cargo check -p tachi-server` (HT) | **green** |

Product policy that correctly stayed out of the kernel:

- A-share trading half-lives + session age → HT `hypermem_policy::TradingDecayPolicy`
- Ticker metadata heuristics → HT `hypermem_policy::heuristic_metadata_from_text`

**Conclusion:** portable-only catch-up is viable. Do not rsync operator crates.
Full procedure: HyperTachi
`docs/engineering/portable-memcore-catchup.md` on that experiment branch.

## Supported dependency surface (owner-ratified 2026-08-03, #1585)

The one supported dependency surface for Hypermem:

- **`portable-kernel` library only.** No dependency on `tachi-server`,
  `portable-server`, `tachi-llm`, Vault, Hub, Foundry, dispatch, GitHub/PR
  lifecycle, or agent-harness crates. The dependency graph is verified clean
  by the portable contract test and the build seat's no-default-features
  check.
- **Immutable pin.** Consumers pin a version tag (or commit SHA) of this
  repository; the compatibility policy is that stable exports (typed
  open/create/migrate via `DbOpenContext`, CRUD, hybrid search, injected
  `KernelPolicy` scorer/decay/embed configuration, precomputed query vectors,
  pure rerank blending, docs/wiki classification, and write/search receipts)
  do not break within a pinned minor line; breaking changes land behind a new
  tag with a migration note in this document.
- **Schema profile.** Hypermem creates and opens `PortableKernel`-profile
  stores only (#1585 §1). A full-Tachi database is refused typed at open, not
  silently reinterpreted; profile and role identity are write-once stamps in
  the store itself, never inferred from paths.
- **Exact profile admission (W1-2).** `DbOpenContext::required_profile` is a
  `ProfileRequirement`. `AtLeast(p)` is the #1585 lattice, under which a
  `TachiFull` store admits a `PortableKernel` caller. `Exact(p)` admits only a
  store stamped `p` (`DbOpenContext::with_exact_profile`). Under
  `Exact(PortableKernel)` the kernel refuses a `TachiFull` store with
  `MemoryError::StoreProfileNotExact`. The refusal comes from the open
  funnel's read-only identity preflight, which runs ahead of the migration
  backup and the connection PRAGMAs. On refusal memcore writes no
  `.migration-bak` and issues no DDL, stamp or identity/role write:
  `PRAGMA user_version`, the schema and `hard_state` are unchanged. This is
  not a byte-identity guarantee. The funnel's connection is read-write, so if
  the store was left with committed, uncheckpointed WAL frames (an unclean
  shutdown), SQLite's last-close checkpoint may fold them into the main file
  and remove `-wal`/`-shm`; the logical content does not change. And because
  the transaction re-resolves identity authoritatively, a stamp another
  process writes between the preflight and `BEGIN IMMEDIATE` can still
  produce a refusal after a backup was written. One labelled open with
  `with_exact_profile(PortableKernel)` therefore replaces the "unlabelled
  preflight open, check `store_profile()`, labelled open" sequence, which
  runs schema init twice per store.

  Preconditions, in funnel order, before this table applies:
  1. The schema-version gate (a stamp newer than this kernel → refused), the
     #1119 creation-intent/migration-authority gate
     (`check_db_open_context_gate`) and current-schema integrity validation
     run first and can return their own error.
  2. "Fresh" means `PRAGMA user_version == 0`, not "empty file": an unstamped
     file with content is fresh.
  3. `read_identity` reads and decodes BOTH stamps (role first, then profile)
     before profile admission, so a malformed role or profile stamp returns
     its decode error ahead of any profile verdict.
  4. Profile admission is decided before role resolution; a role conflict is
     only reported for an admitted profile.

  `Exact(PortableKernel)`, claim = role `X`:

  | stored profile | stored role | outcome |
  |---|---|---|
  | `PortableKernel` | absent | admitted; `X` stamped once |
  | `PortableKernel` | `X` (or legacy `project:X`) | admitted |
  | `PortableKernel` | `Y` ≠ `X` | `StoreRoleConflict` |
  | `TachiFull` | any | `StoreProfileNotExact` (no backup, no memcore write) |
  | absent, `user_version == 0` | absent | built portable; profile and `X` stamped |
  | absent, `user_version == 0` | `X` | built portable; profile stamped |
  | absent, `user_version == 0` | `Y` ≠ `X` | `StoreRoleConflict` |
  | absent, `user_version > 0` | any | `StoreProfileUnstamped` |

  An unlabelled open (claim `unknown`) accepts any stored role and never
  stamps one.

  **Migration note (breaking type change).** The field changed from
  `StoreProfile` to `ProfileRequirement`. Struct-literal constructors replace
  `required_profile: StoreProfile::X` with
  `required_profile: ProfileRequirement::AtLeast(StoreProfile::X)` to keep the
  old behaviour. Callers that use `with_profile(StoreProfile::X)` are unchanged
  (it sets `AtLeast`). `StoreProfile` converts `Into<ProfileRequirement>` as
  `AtLeast`.
- **Licensing** — `DECISION_REQUIRED (owner): AGPL-3.0-only embedding/distribution
  terms in Hyperion artifacts.` Until that decision is recorded here, this
  section documents the technical surface only, not a distribution grant.

### Operational notes (#1585 D6)

- **Conferral audit.** Every role/profile conferral is INFO-logged with role,
  path, and resolver. Before the first post-upgrade daemon start, record the
  inferred vs manifest-resolved label for each of the nine production
  databases (pre-cutover audit artifact).
- **Operator unstamp.** A wrong stamp is corrected on a stopped daemon by
  clearing the `hard_state` rows in namespace `store_identity` for that
  database; the next resolving open re-confers. The stamp is write-once at
  the API level precisely so this is an operator action, never an ambient
  one.

### Bounded exact-entity reads (#1882)

`MemoryStore::read_exact_entities` accepts structural entity aliases, canonical
store identity expectations, optional physical file identity, an explicit
public/private partition expectation, path/domain/surface selectors, governed
role rules, archive selection, `as_of`, and caller work ceilings. The borrowed
`db::StoreIdentity` must match this handle's resolved role and effective profile.
A supplied physical identity must match the opened file identity; no path is
reopened. `expected_private_partition: None` requires a public handle. These
checks assert existing admission; they do not grant access or route databases.
An unknown store label is still unknown, not proof of project authority.

The trusted host resolves role rules from its canonical authority for each
request. For Tachi this is the server's Global store, not the candidate store's
`sandbox_rules`. Rules use the existing deny/path evaluator before result
selection. Surface and scope are not ACLs. The kernel does not deserialize tool
arguments into authority or add a policy ledger. A Global-rule snapshot and a
separate candidate database are not an atomic cross-database snapshot; adapters
retain responsibility for admission and that existing limitation.

The reader scans bounded id/path projections, then bounded entity, temporal,
and namespace fields. Canonical namespace classifiers share a borrowed
projection with existing full-entry callers. Only admitted, deterministically
ID-ordered and capped rows use the full renderer, within the same SQLite read
snapshot. Exact metadata matching does not normalize product symbols or treat
text/keywords as identity. Canonical validity is half-open: A superseded by B in
August remains visible in June, while August's boundary and current reads select
B. Including archived rows does not revive current superseded rows.

Every final internally owned connection installs a permanent progress callback.
It shares the existing connection-lifetime authorization state but creates only
runtime instrumentation, not authority. Inactive callbacks only check the armed
flag; there is still callback overhead on unrelated SQL. Each exact read charges
a fixed database-independent probe and requires its private witness to advance.
A removed/replaced callback or replaced connection yields `CallbackUnavailable`
without installing, clearing, or restoring a callback. A foreign interrupt is
an error, not inferred budget exhaustion. RAII disarms instrumentation before
owned snapshot rollback and restores the inherited SQLite length ceiling.

VM accounting uses approximate 256-instruction callback quanta, including probe
work; it is not a wall-clock, filesystem, or lock-wait deadline. Request bytes,
scanned rows, admission bytes, per-row bytes, and hydrated bytes have independent
ceilings. SQL byte-length/CASE guards precede JSON parsing, and only admitted
content is fully hydrated. Reaching the row ceiling is conservatively exhausted
without an extra EOF step. Resource exhaustion discards all partial rows;
`ResultLimit` carries the deterministic capped result and explicitly does not
claim completeness. No index, schema migration, ranking change, or access/use
receipt write is introduced.

The isolated `exact_entity_contract` target requires `portable-contract-test`
and proves the public API without admin on disposable `PortableKernel` stores.
Workspace tests also cover callback ownership and canonical namespace behavior.
Current Hypermem adapter conformance and downstream benchmark/cutover acceptance
remain separate gates; an archived prototype is not production evidence.
