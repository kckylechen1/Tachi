# Current-Store Admission — refuse damaged state, rebuild only what is derived

> **Status:** spec, pre-implementation.
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

**Default-deny, with a frozen allowlist of derived objects.** When a store is
at the current version, every schema object in its profile's baseline must be
present **before** any maintenance runs. The only exceptions are objects on
the derived allowlist (§3). Those may be missing and are rebuilt, as they are
today. Anything else missing refuses the open with a typed error. Nothing is
written: no backup, no PRAGMA, no DDL, no stamp, no marker.

Why default-deny rather than listing the state-bearing objects:

- New tables are state-bearing by default. A new derived object has to
  justify joining the allowlist; a new state table needs no list entry to be
  protected.
- The 263-case sweep shows that almost everything outside the allowlist is S
  or W.

`Allow` does not change this. `Allow` is *migration* authority for
`1 ≤ s < E`. It does not authorize repairing a damaged current store. Repair
is an explicit operator action (§6).

## 3. Derived allowlist (frozen)

An object may be on this list only if rebuilding it from other rows in the
same store changes no existing row and loses no information. The
implementation must enumerate every object from the baseline definitions and
assert that the list is exact:

| Object class | Members | Why derived |
|---|---|---|
| Non-unique indexes | every non-unique index in `BASE_SCHEMA_CHUNKS`, `MIGRATED_INDEXES_CHUNKS`, the versioned-migration installers, and `idx_memories_path_active_ts` (`ensure_optimization_indexes`) | the index is a function of its table |
| Unique indexes with no rewriting step before them | `idx_memories_idless_identity_active`, `idx_delivery_events_claim_key_global`, `idx_delivery_events_ack_key_global` (each must be checked) | rebuilding either succeeds with no row change or fails loudly on duplicates. **Excluded:** `idx_session_claims_identity_active`, whose rebuild runs `dedupe_session_claims_identity_conflicts` first (W) |
| FTS projections | `memories_fts`, `memories_symbolic_fts` | `ensure_fts_backfilled` rebuilds them from `memories` |
| Caches | `recall_cache` and its `generation_fingerprint` column | a cache, recomputed on miss (`db/schema.rs`, the `recall_cache` comment) |
| Vector capability | `memories_vec` (`try_load_sqlite_vec`) | an optional capability, provisioned after init. Embeddings can be re-derived from an external provider (vector backfill). This is the D7 R7 exception, kept here for the same reason. **A known gap:** a dropped `memories_vec` loses embeddings until they are backfilled |

Everything else counts as state (S) or rewrites data when repaired (W), and
must be present. That covers every table not listed above, every
`ensure_column` target, every unique index with a rewriting step before it,
and every trigger (triggers are already enforced by the trigger inventory).

Row-level normalization is **out of scope**. This means the
`created_at`/`updated_at`/`revision` backfills, `normalize_memory_validity_columns`
and the legacy bridges. These repair row *values*, not missing schema. W2-7
showed they are load-bearing while snapshot import and rescue apply still
write rows that violate them (#1987 W2-7). They keep running.

## 4. Where the check runs

It lives in #1983's funnel as an extension of the integrity step, for both
policies of D7:

- **Preflight** (read-only, one snapshot), at the integrity step: if the
  store is current, check that the baseline minus the allowlist is present.
  Under today's policy, "current" means `s == E`. For a D7 Portable band
  store, it means `π_P(E) ≤ s ≤ E`, with the Portable baseline plus the
  Product objects its sentinels attest. A missing object means refusal
  before any side effect.
- **Authoritative** (inside `BEGIN IMMEDIATE`), in the same step of
  `reevaluate_admission_in_tx`: the same check runs again on the
  in-transaction state, before `init_schema_inner`.
- **Precedence:** in #1983's order, the check is part of integrity. It comes
  after the version and #1119 gates and before coverage and identity, the
  same slot `validate_current_schema_integrity` already occupies. A store
  that today fails `validate_current_schema_integrity` keeps that error.
  The new check runs after it and only adds refusals.
- **Private images** (`init_private_schema_with_label_mut`, which reuses
  `init_schema_inner`) get the same check.

**Coupling with D7 R5.2.** D7's R5.2b runs a complete-shape check *after*
the frozen normalizations. That cannot catch a dropped state table:
`CREATE TABLE IF NOT EXISTS` rebuilds it empty, and the empty table then
passes the shape check. So D7's R5.2a (the band-input integrity) **must
include this presence check** (the baseline minus the allowlist, checked
before maintenance). Both specs share one object inventory. This PR amends
D7 R5.2a to say so.

Pending stores (`1 ≤ s < E`, or below `π_P` under D7) are not checked.
Migrations legitimately create objects there.

## 5. Outcomes

| Input (`OpenExisting`, current store) | Today | After |
|---|---|---|
| allowlisted object missing (e.g. `idx_memories_tier`) | open Ok, rebuilt | **unchanged**: open Ok, rebuilt |
| state table missing (e.g. `exec_env_worktree_identities`) | open Ok, recreated empty, rows lost | **refused** in preflight with `CurrentSchemaIncomplete`, nothing written |
| W unique index missing (`idx_session_claims_identity_active`) with duplicates | open Ok, older claim released | **refused** in preflight, claims untouched |
| `ensure_column` target missing (e.g. `hub_capabilities.review_status`) | open Ok, column re-added with a DEFAULT | **refused** in preflight |
| trigger missing | refused (trigger inventory) | unchanged |
| missing object that `validate_current_schema_integrity` already catches | its error | unchanged, same error and precedence |
| same inputs under `Allow` | same as `Deny` today | refused, same as `Deny` (§2) |
| store below the current version | today's migration behaviour | unchanged |

Error: a new typed `MemoryError::CurrentSchemaIncomplete { missing:
Vec<String>, db_path }`. `missing` lists every absent required object, in a
stable order. The message points to the operator repair path (§6).

## 6. Operator repair path

Refusing an open leaves an operator who needs a way forward. This spec does
not build a repair tool. It requires the refusal message and the
`schema-migration-runbook.md` entry to name the options:

- restore the most recent `.migration-bak` or snapshot;
- run an explicit, named repair command whose output is reviewed. That
  command is a follow-up leaf, created when this is implemented.

Until the follow-up exists, the runbook describes a manual recovery.

## 7. Tests (acceptance)

- **T1 Reproductions.** Un-ignore the two tests on
  `test/current-store-silent-repair-1995`. They must be green:
  - (a) `idx_session_claims_identity_active` missing, with duplicates;
  - (b) `exec_env_worktree_identities` dropped.

  Each is refused, and claims, rows and `sqlite_schema` are unchanged, with
  no backup and no marker.
- **T2 Allowlist.** For every allowlisted object, a current store missing it
  opens `Ok` and the object is rebuilt. Also assert that the allowlist in
  code equals the enumerated derived set, so the allowlist cannot grow
  silently.
- **T3 Default-deny sweep.** Promote the 263-case diagnostic sweep to an
  asserting test. Every non-allowlisted object missing means refused, with
  zero side effects. Every allowlisted object missing means Ok. Run it for
  `TachiFull`. The Portable run follows D7 (T9 there).
- **T4 `review_status`.** Rebuild `hub_capabilities` without `review_status`
  (via table rebuild; the column has an index). The open is refused and no
  capability changes state.
- **T5 Precedence.** A store with both a `validate_current_schema_integrity`
  defect and a missing state table returns the former's error, exactly as
  today.
- **T6 Race.** Using #1983's hook, a state table is dropped between
  preflight and `BEGIN IMMEDIATE`. The authoritative check refuses, and the
  database has no DDL, stamp or row change.
- **T7 Private image.** A sealed private image missing a state table is
  refused through the production private door.
- **T8 Invariance.** Every existing test passes unchanged except those that
  assert the old silent repair. Each of those is listed in the PR with a
  before and after.

## 8. Not verified

- The sweep ran on the full profile only. The 16 columns the fixture could
  not drop (index or CHECK dependencies) are classified by reading
  `ensure_column`, not by measurement.
- Legacy-gated steps (the persons/location/hypertachi bridges and the enum
  rebuild) were not exercised. They are row or legacy normalization and stay
  out of scope.
- Whether any production store today is missing a non-allowlisted object,
  in which case it would start refusing after this change. The
  implementation PR must run the preflight check read-only against a copy of
  each live store on this host, and report the result, before merging.
