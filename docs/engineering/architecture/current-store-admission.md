# Current-Store Admission — refuse damaged state, rebuild only what is derived

> **Status:** spec, pre-implementation, revision 7 (after six cross-vendor attack rounds).
> **Issue:** #1995 (found in #1983's round-4 cold review). **Parent:** #1987.
> **Anchors:** `db/open.rs:630` (damaged current input is refused, not
> repaired), #1119 (migration authority), #1983 (read-only preflight plus
> in-transaction re-evaluation), D7 [`portable-version-policy.md`](./portable-version-policy.md)
> (R5.2).
> **Evidence base:** origin/main `fbab02c7d96aba06dc78fb3126c643fdb9eb739b`.
> Paths are under `crates/memcore/src/` unless stated otherwise. The inventory
> and red reproductions are on branch `test/current-store-silent-repair-1995`
> @ `06908695` (see the #1995 comment).

## 1. Problem

A store already at the current version (`s == E`) can be missing a table,
index or column. When that happens it is **silently rebuilt on open**, even
under `MigrationAuthority::Deny`. Today's admission checks run before any
repair: the persistent trigger inventory (`db/open.rs:558`, called from
`store/open.rs` before init) and `validate_current_schema_integrity`
(`db/migrations.rs:352`). They catch damage only when the missing object
happens to take a trigger or a checked column with it. Everything else is
recreated by `CREATE … IF NOT EXISTS` or `ensure_column`, or by a repair step
that runs before a rebuild. Measured on the old main with a 263-case sweep:

- **A dropped state table comes back empty** (about 40 product tables,
  `exec_env_worktree_identities`, `derived_items`, `processed_events`,
  `tachi_events`, `edge_observations`). Its rows are lost and the open
  returns `Ok`.
- **A missing unique index is rebuilt after its dedupe step rewrites rows.**
  With `idx_session_claims_identity_active` missing, the older duplicate claim
  is released (`superseded-by-unique-identity-migration`).
- **A missing column is re-added with a `DEFAULT`**, which invents values for
  every existing row. One case matters most:
  `hub_capabilities.review_status` comes back as `DEFAULT 'approved'`
  (`db/schema.rs`, `ensure_column` for hub capabilities), so every capability
  would silently become approved.

This contradicts the current-store rule at `db/open.rs:630`: damaged current
input is refused, not repaired.


## 2. Decision

**The new check is additive and default-deny. It sits on top of today's
validators.** A store at the current version must already contain every
object in its **required set** (§3) before any maintenance runs. If any
required object is missing, the open is refused with a typed error. Three
rules bound this:

- **Existing checks are unchanged.** The trigger inventory,
  `validate_current_schema_integrity` and its per-migration shape
  validators keep their errors and their precedence. The new check runs
  after them and can only add refusals. Some objects that look derived are
  already required by an existing validator, for example the delivery and
  outbox indexes (`db/migrations.rs:371`, `db/schema.rs:979-1003`,
  `:2040-2044`). Those stay refused with the error they produce today.
- **Allow grants no repair.** `Allow` is *migration* authority for
  `1 ≤ s < E`. It does not authorize repairing a damaged current store, so
  the new refusal applies under `Allow` as well. Repair is an explicit
  operator action (§6).
- **Row values are out of scope.** Row-level normalization does not
  change: the `created_at`/`updated_at`/`revision` backfills,
  `normalize_memory_validity_columns` and the legacy bridges. They repair
  row values, not missing schema, and #1987 W2-7 showed they are
  load-bearing.

Default-deny, rather than listing the state-bearing objects, is chosen for
one reason: new tables are state-bearing by default, and a new derived
object has to justify its place on the allowlist.

## 3. Required set

**Required set = profile baseline − derived allowlist.**

**Profile baseline.** The baseline is chosen by the *effective* profile,
not by sentinels:

- `TachiFull`: every Portable and Product object the current code creates.
- `PortableKernel`: every object that Portable initialization creates. That
  covers the `SchemaScope::Portable` chunks, the Portable-scoped migrations'
  objects, **and the inline maintenance objects** (e.g. the
  `memory_search_generation` table and its triggers, created by
  `search_generation::ensure_search_generation_schema`,
  `db/search_generation.rs:25-54`).

In both cases the baseline is **derived from what initialization actually
creates**, not from the chunk tags alone. The implementation builds it by
initializing a fresh store of that profile and enumerating `sqlite_schema`
plus the columns (§7 T9).

Product sentinels on a Portable store attest no Product objects: v37-v39
record their sentinel vacuously on Portable (`db/migrations.rs:944-948`,
`mirror_eval_identity.rs:36-41`). The existing *conditional* Product
validators, keyed on `identity_admissions` existing
(`db/migrations.rs:372-381`), are kept exactly as they are.

**Derived allowlist (frozen).** An object is allowed on this list only if
it is not already required by an existing validator, and it can be rebuilt
from other rows in the same store with no information loss. The
implementation enumerates it from the baseline definitions, and a test
pins it:

| Class | Members | Rebuild outcome |
|---|---|---|
| Non-unique indexes not already required by a validator | base, migrated and installer indexes, plus `idx_memories_path_active_ts` | success. `ensure_optimization_indexes` ignores its own errors (`db/schema.rs:2677-2685`), so the index may stay absent; that is today's behaviour |
| Unique indexes with no rewriting step before them, not already required | `idx_memories_idless_identity_active` | success, or a loud failure that rolls back the schema transaction when duplicate active identities exist. **Excluded:** `idx_session_claims_identity_active`, because dedupe runs first (`db/schema.rs:1861-1872`). The delivery unique indexes are already required by the delivery validator, so they are not on this list |
| FTS projections | `memories_fts`, `memories_symbolic_fts` | rebuilt from `memories`, with the search-generation bump allowed (it invalidates caches). **Narrowed claim:** "derived" here means search-derived. It does *not* mean byte-identical to the CRUD projection. The backfill projection of `keywords`/`entities` differs from CRUD (SQL bracket/quote stripping at `db/schema.rs:3370-3380` versus `.join(" ")` at `db/memory_crud.rs:2598`), most visibly on escaped JSON. That fidelity is deferred to #2001, and this spec does not depend on it: dropping FTS is still better rebuilt than refused, because the source rows are intact |
| Cache | `recall_cache` and `generation_fingerprint` | recomputed on miss. This is disposable for correctness, not lossless: rerank results and telemetry go |
| Optional capability | `memories_vec` | provisioned empty when sqlite-vec is available, absent otherwise. Embeddings are re-derived from an external provider. **Known gap:** a dropped table loses embeddings until they are backfilled |

Everything else in the baseline is required: every other table, every
`ensure_column` target, and every unique index with a rewriting step
before it. Triggers are already enforced by the trigger inventory.

**Schema-growth rule (new).** A new **required** object can only be
introduced with a versioned migration, which bumps `E`. That way, a store
stamped `E` was always initialized by code that creates every object
required at `E`. The additive-base-chunk amendment rule
(`db/schema/ddl.rs:2128-2146`, and D7 R7) remains open only for objects on
the derived allowlist.

For objects that already shipped, this holds today. v39 landed on
2026-09-23 (`de7921d10`). Every table added through the additive rule
landed earlier:

- `provider_accounts…`: 08-10
- `model_*`: 08-12 / 08-13
- `route_*`: 08-10
- inline `exec_env_worktree_identities`: 08-27 (`38ed99d47`)

The **schema initializer** stamps 39, and it creates all of these objects in
the same transaction (`db/schema.rs:20-41, 1273-1318`). A store stamped 39
through that path therefore contains them. The argument covers this repo's
history only; it proves nothing about stores written by other forks.

**The migration-only stamping path is closed.** The public
`run_data_migrations` / `run_data_migrations_with_profile`
(`db/migrations.rs:595-615`) run the sentinel migrations and write the
stamp *without* calling `init_schema_inner`. Because of that, a store they
stamp to 39 can lack additive objects (e.g. `exec_env_worktree_identities`,
which only `init_product_schema_columns` creates).

**Callers.** At fbab02c7d the lead found no in-repository production
caller. The `a2a.rs:1342/1375` calls sit inside `#[cfg(test)]`, and
Hyperion's origin/main has zero references. The API is still public
(`memcore/src/lib.rs:66`, `db/mod.rs:49`, re-exported by
`crates/portable-kernel/src/lib.rs:40`), and its docs describe standalone
use (`db/migrations.rs:562-575`). So downstream use cannot be ruled out.

**Disposition (decided): leave the API unchanged.** Routing it through the
full initializer would break frozen assertions. One is
`unstamped_db_with_existing_sentinels_skips_and_restamps`
(`db/migrations.rs:2089-2119`), which pins a sentinel-only fixture. The
other is v12's dedupe report (`db/migrations.rs:1195-1244`), which the
initializer would pre-empt. The API's behaviour and tests therefore stay
exactly as they are.

The API can still stamp an incomplete inventory, so it is not a hatch this
spec closes. Admission closes it instead. Any store the API leaves missing
a required object is **refused loudly** when it next enters one of the
**covered entry points** (§4). The refusal names the object in
`CurrentSchemaIncomplete` and comes before any repair, so nothing gets
silently recreated. Consumers that bypass those entry points are listed
as named exceptions in §4. The API's docs gain one sentence: its output
is subject to current-store admission.

**Historical outcome (fixed oracle).** Take a pre-`38ed99d47` Full store
stamped 39 through the migration-only path, so it lacks
`exec_env_worktree_identities`. On admission it **is refused with
`CurrentSchemaIncomplete`**, and the error names the missing table. The
operator recovers through §6. That is the price of default-deny. It is
chosen over silently re-creating a state table.

## 4. Where the check runs

It extends #1983's integrity step, under both D7 policies:

- **Preflight.** A read-only check, placed after the existing integrity
  validators. If the store is current, the required set must be present.
  "Current" means `s == E` under today's policy, and the Portable band
  under D7. A missing object is refused before any side effect.
  - The merged preflight is *not* yet one read snapshot
    (`db/schema.rs:1235-1243`). The D7 implementation adds the snapshot;
    this check joins it.
- **Authoritative phase.** Inside `BEGIN IMMEDIATE`, the same check runs
  again, in `reevaluate_admission_in_tx` and before `init_schema_inner`.
- **Private images.** `init_private_schema_with_label_mut` gets the same
  check against the Portable baseline. That baseline includes the inline
  search-generation triggers, so they must be present before maintenance.
  Today the private initializer skips input trigger validation
  (`input_inventory=false`, `db/schema.rs:1204-1210`), so a missing trigger
  is silently recreated. Under this spec it is refused. D7's allowance
  stays: a *present previous* `memory_search_generation_after_update`
  definition is accepted and normalized. Only an *absent* trigger refuses.
- **Read-only opens** (`store/open.rs` ~923-946) call
  `validate_current_schema_integrity` (~925) and then the persistent-trigger
  inventory (~931/935), but they never enter the initializer's preflight.
  These opens run the same presence check **after the trigger inventory and
  before row-guard installation (~937)**, so today's integrity and trigger
  errors keep their precedence. A missing required object is refused with
  `CurrentSchemaIncomplete`. Without this, `tachi status` reads a
  read-only-opened store and reports `derived_items = 0` for a table that
  does not exist (`status_ops/db_probe.rs:46-59,102-110`). That is a silent
  wrong answer.
- **Maintenance opens** (`open_existing_read_write`, ~1009-1070) run it at
  the same relative point: after the existing integrity validation and
  trigger inventory (~1005-1006), before row-guard installation (~1007) and
  before any maintenance write.
- **The public `db::init_schema`** (`db/schema.rs:15-41`) has its own
  integrity check, PRAGMAs, transaction and `init_schema_inner`, outside
  the shared funnel. `open_in_memory` always gives it fresh storage, but a
  caller can hand it an existing current connection. When the connection
  is stamped current (`s == E`), it runs the same presence check before
  its PRAGMAs and transaction. On fresh input it does nothing.
- **The identity-bound reopen after fresh creation**
  (`reopen_initialized_file_store`, `store/open.rs:739-791`) opens a new
  connection after initialization commits. It runs the presence check next
  to its existing version and integrity validation. Otherwise another
  process could drop a required table between commit and reopen, and the
  handle would be returned anyway.
- **Pending stores are not checked.** Migrations legitimately create
  objects.

**Covered entry points (exhaustive).** Checked against `store/open.rs` and
`db/open.rs` at fbab02c7d:

| Entry point | Covered via |
|---|---|
| `open`, `open_with_label`, `open_with_context`, `open_with_context_and_busy_timeout`, `open_with_label_and_context`, and the admin `open_and_vault_upsert_key_health_*` | shared writer funnel (`open_with_label_inner_while_startup_owned`) |
| the bare `init_schema_with_label_mut` | shared preflight and in-tx re-evaluation |
| `reopen_initialized_file_store` | its own presence check (above) |
| `open_private_image` | private-image door |
| `open_read_only`, `open_read_only_immutable`, `open_read_only_with_label`, `open_read_only_existing_schema_compat` | read-only check, placed after the trigger inventory. Pending stores are excluded |
| `open_existing_read_write` | maintenance check, placed after the trigger inventory |
| `db::init_schema` / `open_in_memory` | its own presence check when the store is current; fresh storage is a no-op |

The `pub(crate)` connection helpers `db::open::open_read_write*` and
`db::open::open_read_only` are plumbing. They perform no admission, and
their production callers are the entry points above.

**Named exceptions** are not covered, and the gap is documented rather than
silent:
- **Doctor/diagnostic raw connections.** Examples: `db/doctor_probe.rs`,
  and `status_ops` raw probes that open their own `Connection`. They
  deliberately bypass admission. Where they count rows in a table, they
  must report a missing table as *missing*, not as `0`. For instance, the
  missing-`foundry_jobs` probe returns all zeros today
  (`db/doctor_probe.rs:232-240`). This is a follow-up leaf, filed with the
  implementation.
- **Connections a caller already holds** are not re-admitted. Admission is
  per open.

**Side effects on refusal.** When the defect is visible to the preflight,
the refusal happens before backup, PRAGMA, DDL, stamp and marker, so
nothing is written. When the defect appears *between* preflight and
`BEGIN IMMEDIATE` (a race), the authoritative check still refuses. Any
marker-fallback backup, backup retention and the WAL PRAGMA that #1983
already permits in that window may remain, as documented at
`db/schema.rs:1253-1259`. The DB's logical state is unchanged.

**Coupling with D7.** D7's R5.2b checks shape *after* the frozen
normalizations, so it cannot see a dropped state table:
`CREATE TABLE IF NOT EXISTS` recreates it empty. So D7 R5.2a must include
this presence check. This PR amends D7 in three places:

1. **R5.2a includes the presence check.** For `P@39` it is today's check
   plus the new presence refusals.
2. **R0 (today's-policy invariance) is superseded for exactly the §5 rows
   of this spec.** A `TachiFull` current store missing a required object
   is now refused.
3. **D7 T9's missing-column and missing-trigger cases** now refuse at
   R5.2a, in the preflight, not at R5.2b. R5.2b still catches a *present
   but malformed* object, e.g. a non-unique idless index.

## 5. Outcomes

| Input (`OpenExisting`, current store) | Today | After |
|---|---|---|
| allowlisted object missing (e.g. `idx_memories_tier`) | open Ok, rebuilt | unchanged |
| idless unique index missing, duplicate active identities | schema transaction fails (index creation) | unchanged |
| delivery or outbox index missing | refused by the existing validator | unchanged (same error, same precedence) |
| state table missing (e.g. `exec_env_worktree_identities`) | open Ok, recreated empty, rows lost | **refused** (`CurrentSchemaIncomplete`), nothing written |
| `idx_session_claims_identity_active` missing, with duplicates | open Ok, older claim released | **refused**, claims untouched |
| required column missing (e.g. `hub_capabilities.review_status`) | open Ok, re-added with its DEFAULT | **refused** |
| trigger missing | refused by the trigger inventory | unchanged |
| the same inputs under `Allow` | same as `Deny` today | same as `Deny` after |
| store below current version | migration behaviour | unchanged |

Error: a new `MemoryError::CurrentSchemaIncomplete { missing:
Vec<String>, db_path }`, listing every absent required object in stable
order. The message points to §6.

## 6. Operator repair path

The refusal message and a `schema-migration-runbook.md` entry name the
options:

- restore the most recent `.migration-bak` or snapshot;
- run an explicit, reviewed repair command. This is a follow-up leaf, filed
  when the fix is implemented.

Until that command exists, the runbook documents manual recovery.

## 7. Tests (acceptance)

- **T1 Reproductions.** Un-ignore the two tests on
  `test/current-store-silent-repair-1995`:
  - (a) the missing `idx_session_claims_identity_active` with duplicates;
  - (b) the dropped `exec_env_worktree_identities`.

  Each must be refused, and claims, rows and `sqlite_schema` must be
  unchanged, with no backup and no marker.
- **T2 Allowlist, in three separate oracles:**
  - (i) every allowlisted object whose rebuild succeeds: open Ok, and the
    object is present afterwards;
  - (ii) the idless index with duplicates: the transaction rolls back with
    today's error;
  - (iii) optional capabilities: with vec unavailable, `memories_vec`
    stays absent and the open is Ok; with the optimization-index creation
    failing, the index stays absent and the open is Ok. This holds for
    **both** policies: D7 R5.2b exempts the *absence* of
    `idx_memories_path_active_ts` and `memories_vec`. A **present but
    malformed** `idx_memories_path_active_ts` (e.g. created `UNIQUE`) is
    refused **under the Portable policy only** (D7 R5.2b). Today's policy
    does no full-shape validation, and this spec does not add one: its check
    is presence-only. Pin the Portable refusal with its own test. For `F@39`
    the outcome is unchanged; the TachiFull shape hardening stays out of
    scope.

  Assert that the allowlist in code equals the enumerated derived set. The
  FTS case asserts only presence and search-generation bump. Projection
  fidelity is #2001's acceptance, not this test's.
- **T3 Default-deny, enumerated.** Enumerate the *current* baseline objects
  from the definitions, not from the historical 263-case count, and
  account for dependent and shadow objects (FTS shadow tables, autoindexes).
  - Every required object missing: refused, and nothing written.
  - Every allowlisted object: T2.
  - Objects already required by an existing validator: that validator's
    error.
  - Row-normalization cases are excluded.
  - Run it for `TachiFull`, and for `PortableKernel` with the Portable
    baseline.
- **T4 `review_status`.** Rebuild `hub_capabilities` without the column.
  The open is refused, and no capability changes state.
- **T5 Precedence.** The check sits *after* the existing integrity
  validators and *before* identity. That placement gives these outcomes:
  - A store with both a `validate_current_schema_integrity` defect and a
    missing required table returns the integrity error, as today.
  - A store with both a missing required table (e.g.
    `exec_env_worktree_identities`) and a malformed role payload
    (`{"value":7}`) now returns `CurrentSchemaIncomplete`, **not** today's
    role-decode error. This deliberately displaces a later identity error.
  - Pin the second case at `F@39` (with `exec_env_worktree_identities`
    missing) and at `P@39`. For `P@39`, use a Portable table such as
    `derived_items`, because `exec_env_worktree_identities` is Product-only.
- **T6 Race.** Using #1983's hook on a store with a matching marker, drop a
  required table between preflight and `BEGIN IMMEDIATE`. The
  authoritative check refuses. Compare the logical state against the
  **post-hook** state: no DDL, stamp or row change. Separately, assert
  only the backup and WAL effects #1983 permits.
- **T7 Private image.** These cases go through the production private door:
  - A sealed private image missing a Portable required table that today's
    validators miss (e.g. `derived_items`) is refused.
  - A sealed image missing only `memory_search_generation_after_update` is
    refused before maintenance. Today it is silently recreated.
  - A sealed image carrying the *previous* definition of that trigger opens
    and is normalized (the D7 allowance).
  - A healthy image opens (control). A healthy PortableKernel file store with every Product
  sentinel and no Product tables opens (control for §3).
- **T7b Other entry points.** An `F@39` store with `derived_items` missing
  must be refused through a **read-only open** and through
  `open_existing_read_write`. Each refusal returns `CurrentSchemaIncomplete`
  before a handle is returned or any maintenance write happens. A
  `tachi status` probe of that store must not report `derived_items = 0`.
  **Precedence regression:** an `F@39` store missing both `derived_items`
  and `memories_reserved_refs_insert_guard` returns today's missing-trigger
  error on the read-only door and on the maintenance door, not
  `CurrentSchemaIncomplete`. Additional cases:
  - `db::init_schema` on an existing current connection that is missing
    `derived_items` is refused.
  - The fresh-create reopen is refused when a hook drops `derived_items`
    between the initialization commit and the reopen.
- **T8 Invariance and supersession.** Every existing test passes unchanged,
  except tests that assert the old silent repair and the D7 T9 phase
  expectation. Each of those is listed with its before and after.
- **T9 Growth rule, version-keyed inventory.** Pin a golden, keyed by `E`,
  of the **required-object inventory** per profile: tables, columns,
  indexes (with uniqueness and partial predicate) and triggers, minus the
  allowlist. Enumerate it twice:
  - (a) from a freshly initialized store (every initializer, including
    inline maintenance, `ensure_column` and `init_product_schema_columns`);
  - (b) from an **already-current store reopened** with the same code.

  Rebuilds such as `migrate_enum_constraints` run on fresh stores but skip
  converged ones (`db/schema.rs:2880-2888`). Fresh and reopened stores can
  therefore diverge at the same `E`.
  - The test fails if (a) ≠ (b), or if either differs from the golden for the
    current `E`. A change is only accepted together with a bump of `E` and a
    new golden entry.
  - Discrimination cases that must fail it:
    - a test-only `ensure_column` on `memories` placed *before* the
      `migrate_enum_constraints` rebuild. The fresh rebuild drops the column
      but the reopened store keeps it, so (a) ≠ (b);
    - a test-only `ensure_column` elsewhere without a versioned migration;
    - a test-only inline `CREATE TABLE` without one.
  - The migration-only stamping path (§3): the historical pre-`38ed99d47`
    fixture stamped 39 through the migration-only path is **refused with
    `CurrentSchemaIncomplete` naming `exec_env_worktree_identities`**. The
    API itself is unchanged, and its existing tests
    (`db/migrations.rs:1195-1244, 2089-2119`) pass unmodified.

## 8. Not verified

- **Other schema families.** The sweep ran on the full profile only. The
  16 columns the fixture could not drop are classified by reading, not by
  running.
- **Legacy-gated steps.** These stay out of scope.
- **Live stores.** Before merging, the implementation PR must run the
  preflight check read-only against a copy of every live store on this
  host and report the result. That does not establish compatibility for
  stores on other hosts or forks.
- **FTS projection.** The CRUD-versus-backfill projection mismatch is
  proven from source only. Its effect on search results is unmeasured.
