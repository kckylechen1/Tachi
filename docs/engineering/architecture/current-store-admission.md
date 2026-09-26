# Current-Store Admission — refuse damaged state, rebuild only what is derived

> **Status:** spec, pre-implementation, revision 2 (after one cross-vendor attack round).
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
- `PortableKernel`: every `SchemaScope::Portable` object, plus the
  Portable-scoped migrations' objects.

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
| FTS projections | `memories_fts`, `memories_symbolic_fts` | rebuilt from `memories`, with the search-generation bump allowed (it invalidates caches). **Known pre-existing defect:** the backfill projection of `keywords`/`entities` differs from the CRUD projection (SQL bracket/quote stripping at `db/schema.rs:3370-3380` versus `.join(" ")` at `db/memory_crud.rs:2598`). Tracked separately; fixing it is a precondition for "no information loss" on escaped JSON |
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

The code that stamps 39 creates all of them in the same transaction. So a
legitimate `s == 39` store contains them. This is a history-based argument
about this repo, not a proof about stores written by other forks.

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
  check with the Portable baseline.
- **Pending stores are not checked.** Migrations legitimately create
  objects.

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
    failing, the index stays absent and the open is Ok.

  Assert that the allowlist in code equals the enumerated derived set.
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
- **T5 Precedence.** A store with both a `validate_current_schema_integrity`
  defect and a missing required table returns the former's error.
- **T6 Race.** Using #1983's hook on a store with a matching marker, drop a
  required table between preflight and `BEGIN IMMEDIATE`. The
  authoritative check refuses. Compare the logical state against the
  **post-hook** state: no DDL, stamp or row change. Separately, assert
  only the backup and WAL effects #1983 permits.
- **T7 Private image.** A sealed private image missing a Portable required
  table is refused through the production private door. A healthy image
  opens (control). A healthy PortableKernel file store with every Product
  sentinel and no Product tables opens (control for §3).
- **T8 Invariance and supersession.** Every existing test passes unchanged,
  except tests that assert the old silent repair and the D7 T9 phase
  expectation. Each of those is listed with its before and after.
- **T9 Growth rule.** A lint or test fails if a
  `CREATE TABLE IF NOT EXISTS` base chunk introduces an object that is not
  on the allowlist without an `E` bump. At minimum: the frozen list of
  additive-rule tables must match the set present at `E`.

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
