# Portable Version Policy — per-profile schema-version projection

> **Status:** spec, pre-implementation, revision 2 (after cross-vendor attack
> review round 1). Owner decision **D7 = B** (2026-09-26).
> **Issue:** #1991 (parent #1987 W2-5).
> **Anchors:** #984 (downgrade gate), #1119 (migration authority), #1180
> (migration backup), #1585 (profiles and identity), #1983 (preflight before
> backup), #1984 (`ProfileRequirement::{AtLeast,Exact}`).
> **Companions:** [`portable-kernel-split.md`](./portable-kernel-split.md),
> [`../schema-migration-runbook.md`](../schema-migration-runbook.md).
> **Evidence base:** origin/main `2898c651281fd17c47b1830c17145c8995d23e07`
> (paths under `crates/memcore/src/` unless stated); #1983 head `3d6ce14d0`,
> #1984 head `983aeba07`.

## 1. Problem

`PRAGMA user_version` is a single integer that drives four decisions, none of
which looks at the store profile:

| Decision | Code | Rule today |
|---|---|---|
| Downgrade refusal (#984) | `db/migrations.rs:338-346` | `s > E` → refused |
| Migration authority (#1119) | `db/migrations.rs:419-465` | `OpenExisting`, `1 ≤ s < E` → `Deny` refuses, `Allow` migrates |
| Migration backup (#1180) | `db/schema.rs:3289-3296` | `s ∈ 1..E` → backup forced (plus a marker-fingerprint fallback for other opens) |
| Stamp | `db/schema.rs:1236` | always writes `E` |

(`s` = stored `user_version`, `E` = `EXPECTED_SCHEMA_VERSION`, 39 on the
evidence SHA.)

The store profile is resolved only after these gates (`db/schema.rs:1203-1224`;
on #1983's head the read-only identity preflight also runs after them). So a
**product-only** schema bump, whose migrations do nothing on a
`PortableKernel` store (`if !profile.includes_product() { return Ok(0) }`),
still moves `E`. Every `PortableKernel` store then needs `Allow`, takes a
forced backup, and gets re-stamped to a value that older binaries refuse. The
affected stores are Hyperion's Hypermem stores and Tachi's own sealed private
images: `store/open.rs:559-577` always opens an existing image with
`open_existing_deny()` + `PortableKernel`. Gating migration *bodies* by profile
(option C) changes none of the four rows above.

## 2. Definitions

- **Scope.** Every versioned migration `i` carries an immutable
  `scope(i) ∈ {Portable, Product}`.
- **Relevant set.** `relevant(PortableKernel) = { i : scope(i) = Portable }`;
  `relevant(TachiFull) = all i`.
- **Projection.** `π_p(E) = max { i ∈ relevant(p) : i ≤ E }`, so
  `π_TachiFull(E) = E`.
- **`PORTABLE_EXPECTED_SCHEMA_VERSION`** `= π_PortableKernel(E)` is a
  compile-time constant derived from the migration table, never hand-written.
  It is **36** on the evidence SHA (§4).
- **`PORTABLE_COMPAT_FLOOR`**: the `EXPECTED_SCHEMA_VERSION` of the last
  release built without this policy. It is a frozen constant, and 39 if B
  lands before any v40.
- **Effective profile `p`.** This is what #1984's resolver returns: the
  stamped profile whenever a profile stamp exists, **even when `s == 0`**. With
  no stamp and `s > 0` it is `TachiFull` (adoption) or a refusal
  (`StoreProfileUnstamped`). With no stamp and `s == 0` it is the
  requirement's profile. `p` is never taken from the requirement when a stamp
  is present.
- **Regions** of `s` for effective profile `p`:
  - `fresh`: `s == 0`
  - `pending`: `1 ≤ s < π_p(E)`
  - `band`: `π_p(E) ≤ s ≤ E`. For `TachiFull` this is exactly `s == E`.
  - `newer`: `s > E`

## 3. Rules

**R0 — TachiFull and unstamped stores are unchanged.** For every input whose
effective profile is `TachiFull`, including unstamped `s > 0` stores that
adopt `TachiFull`, this policy leaves unchanged every outcome, error,
precedence and side effect of #1983/#1984. That covers migration selection,
the marker-fingerprint backup fallback, identity adoption and
maintenance. Every rule below that changes behaviour is scoped to
`PortableKernel`. Where a rule's text and R0 conflict for a `TachiFull` input,
R0 wins, and the conflict is a spec bug.

**R1 — Downgrade.** `newer` is refused for every profile. The header is read
first, inside the same read snapshot as the identity stamps (§5), so a `newer`
store is refused before any identity stamp is decoded.

**R2 — Portable band.** An `OpenExisting` open of a `PortableKernel` store in
`band` succeeds under either authority, provided the band validation of R5
passes. Such an open:
- runs no versioned migration;
- does not write `user_version`;
- takes no version-forced backup. The marker-fingerprint fallback of
  `maybe_backup_before_migration` still applies unchanged (#1983);
- applies no Product DDL and no un-versioned Portable DDL beyond the frozen
  idempotent set of R7. The DDL cookie is unchanged except for the explicit
  `memories_vec` exception in R7.

It may still perform these writes, exactly as today:
- identity adoption: stamping a missing role for a labelled open;
- per-open data repairs, which #1987 W2-7 governs;
- the marker file write;
- WAL-mode PRAGMA;
- vector-table provisioning (R7).

The authority is not consulted and no migration log line is emitted.

**R3 — Pending (the #1119 decision, Portable).** For a `PortableKernel` store
in `pending`, `Deny` refuses with `SchemaMigrationOptInRequired`. `Allow` takes
the forced backup and then runs every migration in `relevant(P)` whose
sentinel is missing, **whatever its index**. Selection is by sentinel, as today
(`db/migrations.rs:889-899`); the index only orders execution. It then runs
the vacuous-sentinel rule of R5, validates the full relevant inventory, and
stamps per R4. `TachiFull` pending is unchanged (R0).

**R4 — Stamp (Portable).** After a fresh build or an authorized pending
migration, a `PortableKernel` store is stamped
`max(s, π_P(E), min(PORTABLE_COMPAT_FLOOR, E))`. A stamp is never lowered. In
`band` the stamp is not written.
- The compat floor makes a store that a B binary created or migrated
  reopenable by the last pre-B binary (§7). Without it, a fresh B store at 36
  is refused by a pre-B 39 binary under `Deny`. A sealed private image has no
  `Allow` path, so it would become permanently unopenable after a rollback.
- Beyond the floor, the stamp moves only when a Portable migration ships.
  A `PortableKernel` store stamped 39 by a pre-B kernel stays at 39 through
  every product-only bump.
- `TachiFull` always gets `E`, as today (R0).

**R5 — Sentinels and band validation (Portable).**
1. *Vacuous sentinels.* A `PortableKernel` store carries the sentinel of every
   `Product` migration with index `≤ PORTABLE_COMPAT_FLOOR`, recorded without
   running the body, as today. Pre-B validators require the whole
   profile-invariant set at `s == 39` (`db/migrations.rs:352-363`). Sentinels
   of `Product` migrations above the floor are never written to
   `PortableKernel` stores. Any extra sentinel is tolerated and never removed.
2. *Band validation.* Before a `PortableKernel` band open is admitted, run a
   read-only validation of the **complete required Portable shape** at the
   store's stamp. It checks:
   - every `relevant(P)` sentinel `≤ π_P(E)`;
   - every table, column, index and trigger of the Portable shape, including
     index uniqueness and partial-index predicates. Today's validators miss
     at least one case: a non-unique `idx_memories_idless_identity_active`
     survives `CREATE UNIQUE INDEX IF NOT EXISTS` (`db/schema.rs:1474-1479`)
     and breaks `ON CONFLICT(idless_identity)` (`db/memory_crud.rs:2970-2972`).

   A mismatch refuses the open. It is not repaired during band admission.
   Today `validate_current_schema_integrity` runs only when `s == E`
   (`db/migrations.rs:353-355`), so without this rule a band store below `E`
   would go unchecked.
3. `TachiFull` validation is unchanged (R0). Bringing the full-shape
   validation to `TachiFull` is a separate hardening leaf, because it can
   refuse stores that are admitted today.

**R6 — Scope assignment.**
1. **Existing migrations (v1–v39).** `scope = Portable` iff the migration body,
   as implemented on the evidence SHA, runs its effect for `PortableKernel`
   (no `includes_product()` early return). Otherwise the scope is `Product`.
   The rule is mechanical, and it reproduces what every existing
   `PortableKernel` store already contains. Several older `Product`
   migrations only gained their guard in #1585; the evidence-SHA
   implementation is what counts, not the original shipping state.
   v33–v36 stay `Portable` (owner sub-decision default; the author's intent is
   recorded at `db/migrations/harness_session_attachments.rs:5`).
2. **New migrations (v40+).** Each migration declares its scope in the
   migration table. The scope follows the objects the migration touches, not
   the PR's intent. Any migration that creates, alters, rewrites or drops a
   Portable object, or rewrites data in a Portable table, in **any** profile
   branch is `Portable`. A migration may not be declared "profile-neutral for
   convenience".
3. **Immutability.** A shipped scope is never edited. A wrong scope is
   corrected by a new migration. Rescoping a shipped migration moves `π_P`
   retroactively, and stores at different stamps then disagree about what
   "current" contains (§8 A5).

**R7 — DDL discipline.**
- A new Portable table, column, index or trigger ships only as a
  Portable-scoped versioned migration. The additive-base-chunk amendment rule
  (`db/schema/ddl.rs:2128-2146`) stays available for `SchemaScope::Product`
  chunks only.
- The existing un-versioned Portable maintenance is frozen (§8 A6). No
  additions are allowed, and every item must leave a converged store's
  `sqlite_schema` unchanged. The attack review found no unconditional DDL on
  a fully converged Portable schema at the evidence SHA.
- **Exception: `memories_vec`.** `try_load_sqlite_vec` runs after schema init
  (`store/open.rs:709-712`, private image `:585-588`) and executes
  `CREATE VIRTUAL TABLE IF NOT EXISTS memories_vec` (`db/sqlite_vec.rs:29-41`).
  It provisions an optional capability and is not a schema-version fact. It
  may create the table in `band`. T8 excludes `memories_vec*` from its
  comparison only when vector availability changed between the two opens.
- Per-open **data** repairs are out of scope here (#1987 W2-7). They change
  neither `user_version` nor the DDL cookie.

**R8 — #1119 amendment.** The #1119 decision is currently "from `user_version`
only, never from DB content" (`db/migrations.rs:386-418`). It becomes: **from
`user_version` and the write-once profile stamp**. The profile stamp is
identity, not content. It is write-once (#1585), protected from set and delete
by the state API (`db/state.rs`), and it already decides admission. No other
DB content participates.

**R9 — #1180 amendment.** In `maybe_backup_before_migration`, "version
migration, backup forced" (`db/schema.rs:3289-3291`) becomes exactly `pending`
for the effective profile. For `TachiFull` that is `1..E`, unchanged. For
`PortableKernel` it is `1..π_P(E)`. The marker-fingerprint fallback that
applies to every other open is unchanged for both profiles, including the
one-time backup #1983 specifies for old-format markers.

## 4. Scope table on the evidence SHA

| Scope | Versions | Basis |
|---|---|---|
| Portable | 1–11, 13, 19, 22–30, 33–36 | body runs on every profile (`db/migrations.rs:586-816`) |
| Product | 12, 14–18, 20, 21, 31, 32, 37–39 | `if !profile.includes_product() { return Ok(0) }` guard |

This gives `π_P(39) = 36`, which the attack review confirmed. Notes:
- v10 and v11 drop retired tables that neither shape contains. Under R6.1 they
  are `Portable` (ungated), and they do not affect `π_P`.
- v5, v6, v8, v9, v13, v19 and v24 are no-ops on a converged Portable store.
  **v7 is not**: it adds `memories.location`, which v9 then drops
  (`db/migrations/legacy_columns.rs:102-107`). Only the net effect converges.
- No SQL foreign key crosses the Portable/Product boundary in either
  direction. v33 holds soft TEXT references (`work_claim_id`,
  `agent_identity_id`) to product identity concepts, which is allowed.

The implementation replaces the hand-written call chain
(`db/migrations.rs:586-816`) and `MIGRATION_SENTINEL_KEYS` (`:182-222`) with a
single table `[(index, sentinel_key, scope, fn)]`. `E`, `π_P`, the sentinel
list and the vacuous-sentinel set are all derived from it.

## 5. Open funnel

B needs the stamped profile before the #1119 decision, while R0 needs every
existing `TachiFull` precedence preserved. On top of #1983's preflight and
authoritative split, the funnel becomes:

**Preflight: advisory, one read snapshot.** Run all reads in one read
transaction (`BEGIN DEFERRED` with a first read, or an equivalent snapshot),
so the header and the stamps cannot come from different commits.
1. `check_schema_version_gate`: R1 (`newer`). The header only.
2. `CreateFresh` with `s ≠ 0`: `DbCreateTargetExists` (unchanged, profile-free).
3. Read and decode the **profile stamp only**, then run profile admission
   (`resolve_profile`), which can return `StoreProfileMismatch`,
   `StoreProfileNotExact` or `StoreProfileUnstamped`. The role stamp is not
   decoded yet.
4. The #1119 gate with the effective profile `p`: R2 or R3.
5. Integrity: R5.2 for Portable band, and today's
   `validate_current_schema_integrity` for everything else.
6. Decode the role stamp and run role resolution (`StoreRoleConflict`).
7. Record the **coverage key**: `(s, p, region, backup_required)`.

**Side effects.** When required (R9 or the marker fallback), take the backup,
then set the connection PRAGMAs.

**Authoritative: `BEGIN IMMEDIATE`.** Re-evaluate steps 1–6 on the
in-transaction state. #1983's `reevaluate_admission_in_tx` compares only the
version with `preflight_version` (#1983 `schema.rs:1310-1333`). Under B it
must compare the whole coverage key. If the key differs, the open refuses with
`SchemaChangedDuringOpen`, and no version-changing work runs without the
backup its key requires. Example: a raw writer flips `P@36` to `F@36` between
preflight and transaction. Preflight saw band and took no backup, but the
transaction sees `F` pending, so the open must refuse rather than migrate. Then
`init_schema_inner(p)`, the identity stamp, the R3 migrations, the R4 stamp,
the validators, commit and the marker run as today.

**Precedence changes.** They apply to `PortableKernel` inputs only; for
`TachiFull`, R0 holds.
- Suppose a store's profile stamp does not satisfy the requirement and its
  `s` is below `E`. Today the gate orders it as older-than-`E` and returns
  `SchemaMigrationOptInRequired` under `Deny`. Under B the profile refusal
  comes first. Example: `P@30` under `AtLeast(F)` returns
  `StoreProfileMismatch`. This is the one intentional precedence change.
- A malformed **profile** stamp on a store with `1 ≤ s < E` under `Deny` now
  returns its decode error instead of `OptInRequired`. A malformed **role**
  stamp keeps today's precedence, because the role is decoded at step 6.
- #1984's "Preconditions (funnel order)" section must be updated to this
  order.

## 6. Truth table

The table is ordered: the first applicable row wins. Profiles: `P` =
PortableKernel, `F` = TachiFull, `—` = no stamp, `bad` = malformed stamp.
Example values: `E = 39`, `π_P = 36`, `PORTABLE_COMPAT_FLOOR = 39`.

**Ordered refusals (before the version decision)**

| # | input | outcome | change |
|---|---|---|---|
| 1 | `s > E`, any profile/intent/requirement | `newer` | none |
| 2 | `CreateFresh`, `1 ≤ s ≤ E` | `DbCreateTargetExists` | none |
| 3 | profile stamp `bad` | decode error | earlier than `OptInRequired` for `1 ≤ s < E` |
| 4 | profile `—`, `s > 0`, requirement `AtLeast(P)`/`Exact(P)` | `StoreProfileUnstamped` | none |
| 5 | profile `P`, requirement `AtLeast(F)` | `StoreProfileMismatch` | earlier than `OptInRequired` for `1 ≤ s < E` |
| 6 | profile `P`, requirement `Exact(F)`; or profile `F`, requirement `Exact(P)` | `StoreProfileNotExact` | none for `F` (#1984 already orders it this way); earlier than `OptInRequired` for `P` |

**Version decision (after rows 1–6 admit; `p` = effective profile)**

| # | `p` | `s` | region | `Deny` | `Allow` | change vs. today |
|---|---|---|---|---|---|---|
| 7 | `F` (stamped, or `—` adopting `F`) | 0 | fresh | build `F`, stamp 39 | same | none |
| 8 | `P` (stamped at `s = 0`) | 0 | fresh | build **`P`** (never the requirement's profile), stamp `max(36, 39) = 39` | same | none |
| 9 | `—`, requirement `P`-based | 0 | fresh | build `P`, stamp `P`, stamp 39 | same | none |
| 10 | `F` / `—`→`F` | 1–38 | pending | OptInRequired | forced backup, run missing sentinels (all), stamp 39 | none |
| 11 | `F` / `—`→`F` | 39 | band | open, today's writes | same | none |
| 12 | `P` | 1–35 | pending | OptInRequired | forced backup, run missing `relevant(P)` sentinels (any index) + vacuous ≤ floor, validate, stamp 39 | none at `E = 39` |
| 13 | `P` | 36–38 | band | **open if R5.2 passes; no migration, no stamp, no forced backup** | same | **was OptInRequired / backup + stamp 39** |
| 14 | `P` | 39 | band | open if R5.2 passes | same | adds R5.2 validation |

After the version decision, the remaining refusals come in this order:
integrity (R5), then `StoreRoleConflict` (unchanged relative to each other),
then `SchemaChangedDuringOpen` from the authoritative re-evaluation.

**Worked product-only bump** (`E = 40`, v40 `Product`; `π_P` stays 36, floor
39):

| store | today | under B |
|---|---|---|
| `P`@39 | Deny: OptInRequired; Allow: forced backup + stamp 40 | band: open, stays 39, a 39-binary still opens it |
| `P`@36 (never produced by B once the floor is in place) | Deny refuses; Allow backs up, stamps 40 | band: open |
| fresh `P` | stamp 40 | stamp `max(36, min(39, 40)) = 39` |
| `F`@39 | Deny refuses; Allow migrates | unchanged |

**Worked Portable bump** (`E = 41`, v41 `Portable`, `π_P = 41`): every `P`
store is now `pending`. Deny refuses; Allow backs up, migrates and stamps 41.
The ceremony remains, but only where the portable shape actually changes.

## 7. Rollout and rollback matrix

| From → to | Store | Result |
|---|---|---|
| pre-B 39 → B (`E = 39`) | existing `P`@39 | band; no writes |
| B (`E = 39`) → pre-B 39 | `P` created or migrated by B | stamped 39, all profile-invariant sentinels present ≤ 39 (R5.1) → pre-B opens it under `Deny` |
| B (`E = 40`, product-only) → pre-B 39 | `P` created or migrated by the 40-binary | stamped 39 (floor) → pre-B opens it; the product v40 sentinel is absent, and pre-B does not know that key |
| B (`E = 40`) → pre-B 39 | `F` touched by the 40-binary | stamped 40 → refused as `newer` (unchanged from today) |
| B (`E = 41`, Portable) → any `E ≤ 40` binary | `P` migrated to 41 | refused as `newer` (correct: the shape moved) |

Supported recovery for the last two rows is the one that exists today: restore
the `.migration-bak` taken by the forward migration.

## 8. Appendix: blast radius (evidence SHA)

**A1. memcore sites that compare `s` with `E` or write the stamp:**
- `db/migrations.rs`:
  - `:338-346`: R1, unchanged.
  - `:352-384`: R5.
  - `:419-465`: R2/R3; the signature gains the profile.
  - `:552-564`: the public standalone `run_data_migrations_with_profile`
    validates and writes the stamp. It must apply R3/R4/R5, or be restricted
    to `TachiFull`.
  - `:578-818`: the runner, table-driven.
  - `:182-222`: the sentinel list, now derived.
- `db/schema.rs`: `:1203-1257` (funnel, §5), `:1236` (R4), `:3254-3312` (R9).
- `store/open.rs`:
  - `:692-700`: older-stamp trigger relaxation.
  - `:790-796`: `stored != E` in the fresh identity-bound reopen.
  - `:950-970`: the read-only open sends a band store through the `Deny` gate
    as "older".
  - `:1025-1031`: exact-dedupe `stored != E`.
- `db/filename.rs:257-258` (`== 0 || > E` refuse): unchanged.

**A2. Readers outside the gate.** For each, the implementation PR must record
whether it admits `PortableKernel`, or is `TachiFull`-only and keeps its
equality check behind an explicit profile assertion:
- `tachi-server/src/doctor/schema_skew.rs:52-117` reports a false "behind" for
  band stores.
- `bootstrap/migrate_cli.rs:224-227, 295-325, 451-457`: the sweep would try to
  migrate band stores.
- `bootstrap/manifest_cli.rs:118`.
- The wiki corpus, all expected to be `TachiFull`-only, so assert rather than
  project:
  - `bootstrap/wiki_corpus/fs.rs:679-685`
  - `plan.rs:768-772`
  - `legacy.rs:411-415`
  - `classify.rs:738, 797-811`
- `memory-server-runtime/src/lib.rs:1304-1308` is dead code at `E = 39`.

All projecting readers move to `store_version_status` (§9).

**A3. Hyperion.** A separate leaf after intake:
- `hypermem/src/lib.rs:26`
- `main.rs:15-19`
- `migration.rs:305-324`: the legacy import window `[20, E)` would accept a
  band source as legacy.
- `docker/deploy.sh:320-331, 420-424`
- `deploy_contract_selftest.sh`

**A4. Tests that flip under B.** The implementation PR must list each flip
with before and after:
- `db/migrations/current_truth.rs:181-264`: Deny on `P`@36/37 must now open
  without migration, and fresh `P` stamps stay 39 (floor). #1984 also touches
  this file.
- `db/migrations/verified_admissions.rs:46`, `current_truth.rs:37`,
  `mirror_eval_identity.rs:113`: `Product` bodies ≤ floor are recorded
  vacuously, not called.
- `store/profile_identity_tests.rs:928-957`: the sentinel set stays
  profile-invariant up to the floor only.
- `portable-server/src/main.rs:178-237, 264-307, 358-376`: `E − 1 = 38` is in
  the `P` band, so the fixtures must use `π_P − 1`.
- `db/migrations.rs:2180-2294`: the gate decision tests gain the profile axis.
- `db/migrations.rs:1984-2000` (a `TachiFull` fixture stamped 38 that relies
  on sentinel-driven recovery below `s`): **must pass unchanged** (R0/R3),
  as a T5 regression guard.
- Rewrite the "sentinel set is profile-invariant" doc comments (v12, v14–v18,
  v20, v21, v32, v39 modules).
- `store/profile_identity_tests.rs:110-126` `KERNEL_TABLES`: add
  `memory_outbox_*`, `harness_session_*`, `delivery_*` and
  `memory_search_generation`. R5.2 derives the Portable shape and must agree
  with this list.

**A5. Why shipped scopes are immutable.** Reclassifying v33–v36 as `Product`
would drop `π_P(39)` to 30. `P` stores stamped 36–39 contain the v33–v36
tables, while fresh ones afterwards would not, yet both would count as
current. The unconditional validators (`db/migrations.rs:369-371`,
`db/schema.rs:1238-1244`) would then reject one population or the other.

**A6. Frozen un-versioned Portable maintenance** (`db/schema.rs:1435-1611`):
- `recall_cache.generation_fingerprint`.
- memories columns: `archived`, `created_at`, `updated_at`, `scored_count`,
  `revision`, `valid_from`, `valid_until`, `retention_policy`, `domain`,
  `superseded_by`, `idless_identity` (+ index), `recall_count`,
  `query_diversity`, `tier`, `last_use_at`.
- `access_history.query_hash`, `access_history.event_kind`.
- `memory_edges.valid_from`, `memory_edges.valid_to`.
- `derived_items.summary`, `importance`, `scope`, `created_at`.
- Helpers: `bridge_hypertachi_memory_columns`; the v8/v9 bodies re-run at
  `:1589-1591`; `ensure_search_generation_schema`; `ensure_fts_backfilled`;
  `migrate_enum_constraints` (conditional rebuild);
  `ensure_optimization_indexes`.
- The Portable `MIGRATED_INDEXES_CHUNKS`.

Outside the schema transaction: `try_load_sqlite_vec` (the R7 exception).
Product-side un-versioned DDL is not affected by B: `hub_capabilities`,
`vault_entries`, `exec_envs`, `session_claims`, and the inline
`exec_env_worktree_identities` table at `:1669-1676`.

## 9. Consumer contract

- memcore exports `PORTABLE_EXPECTED_SCHEMA_VERSION`,
  `PORTABLE_COMPAT_FLOOR` and a read-only
  `store_version_status(path, requirement)`. It returns one of
  `Fresh`, `Current { stamp }`, `Pending { from, to }`, `Newer { stamp }` or
  `Refused(error)`, following the ordered table of §6. It opens the file
  read-only and writes nothing.
- Hypermem's `--schema-version` prints both `E` and `π_P`. A new
  `--check-store <path>` prints the status and exits with a distinct code per
  variant. `docker/deploy.sh` compares integers exactly in both directions
  today (`:420-424`); it switches to calling `--check-store`. That change is
  a Hyperion leaf.
- No consumer may re-implement the band in shell or compare `user_version`
  with a constant.

## 10. Tests (acceptance for the implementation PR)

- **T1 Classification**, with three halves:
  - (a) For each `Product` migration, run its **full-profile branch** on a
    full-shaped store at the preceding version. Assert that every Portable
    object's schema **and** every Portable table's content hash are unchanged.
  - (b) For each `Portable` migration, run it on a `P`-shaped store at the
    preceding version and validate with the validators applicable to that
    prefix, not with the final-version validators.
  - (c) A deliberately misclassified test-only `Product` migration that alters
    `memories` in its full branch must fail (a).
- **T2 Product-only bump.** Add a test-only `Product` migration at `E + 1`.
  - A `P` store at 39 opens under `Deny` with no forced backup, and its
    `user_version` and `PRAGMA schema_version` are unchanged. The
    `memories_vec` exception is controlled.
  - A `P` store at 36 behaves the same.
  - An `F` store refuses under `Deny`.
- **T3 Portable bump.** Add a test-only `Portable` migration at `E + 1`. A `P`
  store refuses under `Deny`. Under `Allow` it backs up, runs the migration,
  and is stamped `E + 1`.
- **T4 Downgrade and snapshot.** A store at `s = E + 1` refuses with `newer`
  for `P`, `F` and `—`, including when the profile stamp is malformed. The
  header and stamps are read in one snapshot: a writer committing between the
  two reads cannot produce a decode error ahead of `newer`.
- **T5 TachiFull invariance.** Two parts:
  - The full existing `F` suite passes with no assertion changes. That
    includes #1984's admission tables, the marker-fallback backup tests
    (missing or old-format marker on `F@39`, non-empty `s = 0`), and
    `db/migrations.rs:1984-2000`.
  - A missing-sentinel recovery case: an `F@38` store missing the v3 sentinel
    migrates v3 under `Allow`.
- **T6 Router shape.** Take a v28 `P` store. Intake under `Allow` stamps 39.
  Then a product-only-bump binary restarts it under `Deny`: it succeeds and
  the directory has no new forced backup. Uses a synthetic store unless the
  owner provides the router copy.
- **T7 Truth table.** One test per row of §6, including precedence. Each row
  is run twice: malformed role (unchanged precedence) and malformed profile.
- **T8 Idempotent maintenance.** Reopening a converged `P` store and a
  converged `F` store leaves `sqlite_schema` and `PRAGMA schema_version`
  unchanged. The `memories_vec` exception is controlled.
- **T9 Band validation.** A `P@36` store with every sentinel present but a
  non-unique `idx_memories_idless_identity_active` refuses with an integrity
  error. It is not repaired, and nothing is written. The same fixture runs
  for a missing Portable column and for a missing trigger.
- **T10 Coverage race.** Use a test hook between preflight and `BEGIN
  IMMEDIATE` to flip the profile stamp from `P` to `F` on an `s = 36` store
  under `Allow`. The open refuses with `SchemaChangedDuringOpen`, no
  migration runs, and `user_version` is unchanged.
- **T11 Rollback matrix.** One test per row of §7:
  - B creates a file store and a sealed private image; the pre-B code path
    (the pinned pre-B gate and validators, as a test fixture) reopens both
    under `Deny`.
  - A product-only-bump binary creates a store; the pre-B code path reopens
    it.
- **T12 Private image forward.** A sealed private partition image at 39 opens
  under `Deny` after a product-only bump. Today it would refuse
  (`store/open.rs:559-577`).

## 11. Open items

- **Router profile stamp.** #1585 D2 landed at v28, so the router's v28 store
  may carry no profile stamp. If so, every `AtLeast(P)`/`Exact(P)` open
  refuses with `StoreProfileUnstamped`, and intake (#1987 item 6) needs an
  operator stamp first. Check on the router before intake.
- **Ordering with #1983/#1984.** Implementation starts only after both land.
  They restructure the same funnel and `current_truth.rs`, and §5's coverage
  key extends #1983's `reevaluate_admission_in_tx`.
- **Historical stores.** A store from `ca8b0540` (schema 33) is `pending`. Its
  sentinel and object provenance under R3 needs a historical fixture before
  Hyperion intake relies on it.
- **Validator asymmetry.** `db/migrations.rs:369-371` validates v36;
  `db/schema.rs:1238-1244` does not. R5.2 subsumes both for Portable. For
  `TachiFull`, align them in the separate hardening leaf.
