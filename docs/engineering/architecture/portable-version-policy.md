# Portable Version Policy — per-profile schema-version projection

> **Status:** spec, pre-implementation. This is revision 3, written after two
> rounds of cross-vendor attack review. Owner decision **D7 = B** (2026-09-26).
> **Issue:** #1991 (parent #1987 W2-5).
> **Anchors:** #984 (downgrade gate), #1119 (migration authority), #1180
> (migration backup), #1585 (profiles and identity), #1983 (preflight before
> backup), #1984 (`ProfileRequirement::{AtLeast,Exact}`).
> **Companions:** [`portable-kernel-split.md`](./portable-kernel-split.md),
> [`../schema-migration-runbook.md`](../schema-migration-runbook.md).
> **Evidence base:** origin/main `2898c651281fd17c47b1830c17145c8995d23e07`.
> Paths are under `crates/memcore/src/` unless stated otherwise. PR heads:
> #1983 at `3d6ce14d0`, #1984 at `983aeba07`.

## 1. Problem

`PRAGMA user_version` is a single integer. Four decisions depend on it, and
none of them looks at the store profile:

| Decision | Code | Rule today |
|---|---|---|
| Downgrade refusal (#984) | `db/migrations.rs:338-346` | `s > E` → refused |
| Migration authority (#1119) | `db/migrations.rs:419-465` | `OpenExisting` with `1 ≤ s < E`: `Deny` refuses, `Allow` migrates |
| Migration backup (#1180) | `db/schema.rs:3289-3296` | `s ∈ 1..E` → backup forced; other opens fall back to a marker-fingerprint check |
| Stamp | `db/schema.rs:1236` | always writes `E` |

Here `s` is the stored `user_version` and `E` is `EXPECTED_SCHEMA_VERSION`
(39 on the evidence SHA).

A **product-only** schema bump changes nothing on a `PortableKernel` store:
each of its migrations starts with
`if !profile.includes_product() { return Ok(0) }`. It still moves `E`, and
then every `PortableKernel` store:

- needs `Allow` to open;
- takes a forced backup;
- gets re-stamped to a value that older binaries refuse.

The affected stores are Hyperion's Hypermem stores and Tachi's own sealed
private images (`store/open.rs:559-577` always opens an existing image with
`open_existing_deny()` + `PortableKernel`). Gating migration *bodies* by
profile (option C) leaves all four rows above as they are.

## 2. Definitions

- **Scope.** Every versioned migration `i` carries an immutable
  `scope(i) ∈ {Portable, Product}`.
- **Relevant set.** `relevant(PortableKernel) = { i : scope(i) = Portable }`
  and `relevant(TachiFull) = all i`.
- **Projection.** `π_p(E) = max { i ∈ relevant(p) : i ≤ E }`. It follows that
  `π_TachiFull(E) = E`.
- **`PORTABLE_EXPECTED_SCHEMA_VERSION`** `= π_PortableKernel(E)`. It is a
  compile-time constant derived from the migration table and never
  hand-written. On the evidence SHA it is **36** (§4).
- **`PORTABLE_COMPAT_FLOOR`** is the `EXPECTED_SCHEMA_VERSION` of the last
  release built without this policy. It is a frozen constant, equal to 39 if
  B lands before any v40.
- **Profile probe.** A read-only decode of the profile stamp *alone*: no role
  read, no admission, and no error is ever returned from it. It yields
  `Portable` only when a profile stamp exists and decodes to
  `PortableKernel`. Every other state yields `NotPortable`: absent,
  `TachiFull`, or undecodable.
- **Versioning rules.** `Portable` probe → the Portable rules (`π_P`).
  `NotPortable` probe → today's rules (`π = E`), exactly as #1983/#1984
  implement them.
- **Regions** of `s` under the Portable rules:
  - `fresh`: `s == 0`
  - `pending`: `1 ≤ s < π_P(E)`
  - `band`: `π_P(E) ≤ s ≤ E`
  - `newer`: `s > E`

  Under today's rules the band is exactly `s == E`.
- **Resolved profile.** This is what #1984's resolver returns after
  admission. When a profile stamp exists, the stamped profile wins, even at
  `s == 0`. It never differs from the probe in a way that matters: a
  `Portable` probe means the stamp is `PortableKernel`, so the resolver
  returns `PortableKernel` or refuses.

## 3. Rules

**R0 — Nothing changes for non-Portable stores.** Take any input whose probe
is `NotPortable`: `TachiFull` stamps, unstamped stores, malformed profile
stamps, and fresh files without a stamp that are opened under a `TachiFull`
requirement. For all of these, this policy changes no outcome, no error, no
error precedence and no side effect relative to #1983/#1984. For an input
whose probe is `Portable`, the refusal order also stays #1983/#1984's order.
The changes are confined to three things:

- the version decision for Portable stores in `band`;
- Portable stamps and sentinels (R4, R5.1);
- the added band validation (R5.2).

If any rule text below conflicts with R0 for a `NotPortable` input, R0 wins
and the conflict is a spec bug.

**R1 — Downgrade.** `newer` is refused for every profile at the header gate,
exactly as today. That check runs before the probe's result is used and
before any identity decode.

**R2 — Portable band.** Take an `OpenExisting` open of a store whose probe is
`Portable` and whose stamp is in `band`. It is admitted under either
authority if the rest of the funnel admits it (§5), including R5.2. Such an
open:

- runs no versioned migration;
- does not write `user_version`;
- takes no version-forced backup.

The marker-fingerprint backup fallback still applies, unchanged from #1983.

Schema maintenance on a band open is limited to what `init_schema_inner`
already does today with the frozen un-versioned set (R7, §8 A6). On a
converged store that leaves `sqlite_schema` and the DDL cookie unchanged
(T8). On a store carrying a recognized historical variant, the frozen set
may normalize it, as it does today. One example: the previous
`memory_search_generation_after_update` trigger is recognized and replaced by
`search_generation.rs:180-199`.

The other writes are the same as today:

- identity adoption, meaning a missing role is stamped on a labelled open;
- per-open data repairs (governed by #1987 W2-7);
- the marker file;
- the WAL-mode PRAGMA;
- `memories_vec` provisioning (R7).

The authority is not consulted and no migration log line is emitted.

**R3 — Portable pending (the #1119 decision).** In `pending`, `Deny` refuses
with `SchemaMigrationOptInRequired`. Under `Allow` the open, in order:

1. takes the forced backup;
2. runs every `relevant(P)` migration whose sentinel is missing, **whatever
   its index**. Selection is by sentinel, as today
   (`db/migrations.rs:889-899`); the index only orders execution;
3. records vacuous sentinels per R5.1;
4. runs the validation of R5.2;
5. stamps per R4.

**R4 — Portable stamp.** After a fresh build or an authorized pending
migration, a store under the Portable rules is stamped
`max(s, π_P(E), min(PORTABLE_COMPAT_FLOOR, E))`. A stamp is never lowered,
and in `band` it is not written at all.

- The floor keeps any store that a B binary *created or migrated* reopenable
  by the last pre-B binary under `Deny` (§7). This includes sealed images,
  which have no `Allow` path.
- Above the floor, the stamp moves only when a Portable migration ships. A
  Portable store stamped 39 stays at 39 through every product-only bump.
- Nothing in the table between `π_P(E)` and `E` is Portable, by definition.
  So a stamp raised by the floor never claims an unapplied Portable
  migration.

**R5 — Sentinels and band validation (Portable rules).**

1. *Creation.* Every fresh build or authorized pending migration under the
   Portable rules records the sentinel of every `Product` migration with
   index `≤ min(PORTABLE_COMPAT_FLOOR, E)`. These are recorded vacuously,
   without running the body, as happens today. That keeps pre-B validators
   satisfied: they require the whole profile-invariant set at
   `s == E_preB` (`db/migrations.rs:352-363`). Sentinels of `Product`
   migrations above the floor are never written to Portable stores. Any
   extra sentinel is tolerated and never removed.
2. *Band validation.* A band open must pass both:

   - (a) **Sentinel evidence:** every `relevant(P)` sentinel `≤ π_P(E)` is
     present. So is every `Product` sentinel with index
     `≤ min(s, PORTABLE_COMPAT_FLOOR)`: the vacuous marks any pre-B or B
     writer of that stamp left behind.
   - (b) **Shape:** the complete Portable baseline. That covers every table,
     column, index and trigger, including index uniqueness and partial-index
     predicates. The recognized historical variants listed in §8 A7 are also
     accepted, but only as inputs to the frozen normalizations.

   The check runs twice:

   - **In the preflight**, (a) runs as stated and (b) accepts the baseline
     *or* an A7 variant.
   - **Inside `BEGIN IMMEDIATE`**, after `init_schema_inner` has run the
     frozen set, (a) and (b) run again against the exact baseline.

   A failure at either point refuses the open. Inside the transaction the
   refusal rolls back, and nothing beyond the frozen normalizations is ever
   repaired.

   Today's validators miss at least one bad shape. A non-unique
   `idx_memories_idless_identity_active` survives
   `CREATE UNIQUE INDEX IF NOT EXISTS` (`db/schema.rs:1474-1479`) and breaks
   `ON CONFLICT(idless_identity)` (`db/memory_crud.rs:2970-2972`). It is
   neither the baseline nor an A7 variant, so it refuses. Today
   `validate_current_schema_integrity` runs only when `s == E`
   (`db/migrations.rs:353-355`). Without R5.2, a band store below `E` would
   not be checked at all.
3. Validation under today's rules does not change (R0). Bringing full-shape
   validation to `TachiFull` is a separate hardening leaf.

**R6 — Scope assignment.**

1. **Existing migrations, v1–v39.** `scope = Portable` iff the migration
   body, as implemented on the evidence SHA, has its effect on
   `PortableKernel` stores, with no `includes_product()` early return.
   Otherwise the scope is `Product`.
   - This is mechanical, and it reproduces what every existing
     `PortableKernel` store already contains.
   - Several older `Product` migrations only got their guard in #1585. The
     evidence-SHA implementation is what counts, not the state they shipped
     in.
   - v33–v36 stay `Portable`. This is the default of the owner's
     sub-decision, and it matches the author's intent recorded at
     `db/migrations/harness_session_attachments.rs:5`.
2. **New migrations, v40 and later.** Each migration declares its scope in
   the migration table. The scope follows the objects the migration touches,
   not the PR's intent. A migration that does any of the following, in
   **any** profile branch, is `Portable`:
   - creates, alters, rewrites or drops a Portable object;
   - rewrites data in a Portable table.

   "Profile-neutral for convenience" is not allowed.
3. **Immutability.** A shipped scope is never edited. A wrong scope is
   corrected by a new migration (§8 A5).

**R7 — DDL discipline.**

- A new Portable table, column, index or trigger ships only as a
  Portable-scoped versioned migration. The additive-base-chunk amendment
  rule (`db/schema/ddl.rs:2128-2146`) remains available for
  `SchemaScope::Product` chunks only.
- The existing un-versioned Portable maintenance is frozen (§8 A6). Nothing
  may be added to it. On a converged store every item must leave
  `sqlite_schema` unchanged; it may only normalize the recognized variants
  in §8 A7. The attack review found no unconditional DDL on a fully
  converged Portable schema at the evidence SHA.
- **The `memories_vec` exception.** `try_load_sqlite_vec` runs after schema
  init (`store/open.rs:709-712`; private image at `:585-588`) and executes
  `CREATE VIRTUAL TABLE IF NOT EXISTS memories_vec`
  (`db/sqlite_vec.rs:29-41`). It provisions an optional capability, which is
  not a schema-version fact, so it may create the table in `band`. T8
  excludes `memories_vec*` from its comparison only when vector availability
  changed between the two opens.
- Per-open **data** repairs are out of scope here (#1987 W2-7).

**R8 — #1119 amendment.** The #1119 decision is currently made "from
`user_version` only, never from DB content" (`db/migrations.rs:386-418`). It
becomes: **from `user_version` and the profile probe of the write-once
profile stamp.** The profile stamp is identity, not content:

- it is write-once (#1585);
- the state API protects it from set and delete (`db/state.rs`);
- it already decides admission.

No other DB content takes part in the decision.

**R9 — #1180 amendment.** "Version migration, backup forced" in
`maybe_backup_before_migration` (`db/schema.rs:3289-3291`) becomes exactly
the `pending` region of the applicable versioning rules. Under today's rules
that region is `1..E`, so nothing changes. Under the Portable rules it is
`1..π_P(E)`. The marker-fingerprint fallback for every other open stays as it
is, including the one-time backup that #1983 specifies for old-format
markers.

## 4. Scope table on the evidence SHA

| Scope | Versions | Basis |
|---|---|---|
| Portable | 1–11, 13, 19, 22–30, 33–36 | body runs on every profile (`db/migrations.rs:586-816`) |
| Product | 12, 14–18, 20, 21, 31, 32, 37–39 | `if !profile.includes_product() { return Ok(0) }` guard |

`π_P(39) = 36`, as confirmed by both attack rounds.

- v10 and v11 drop retired tables that neither shape contains. They are
  ungated, so R6.1 makes them `Portable`. Neither affects `π_P`.
- v5, v6, v8, v9, v13, v19 and v24 are no-ops on a converged Portable store.
  **v7 is not**: it adds `memories.location`, which v9 then drops
  (`db/migrations/legacy_columns.rs:102-107`).
- No SQL foreign key crosses the Portable/Product boundary in either
  direction. v33 has soft TEXT references (`work_claim_id`,
  `agent_identity_id`) to product identity concepts. That is allowed.

The implementation replaces the hand-written call chain
(`db/migrations.rs:586-816`) and `MIGRATION_SENTINEL_KEYS` (`:182-222`) with a
single table `[(index, sentinel_key, scope, fn)]`. `E`, `π_P`, the sentinel
list and the vacuous-sentinel set are all derived from that table.

## 5. Open funnel

B keeps #1983's funnel and its order. The only addition is the probe, and the
probe's result feeds only the version rules.

**Preflight** (advisory). It runs in one read snapshot: a read transaction
that ends before the connection PRAGMAs and before `BEGIN IMMEDIATE`.

0. Read the header `s`, then probe the profile stamp (§2). This step returns
   no error.
1. `check_schema_version_gate` (R1, `newer`).
2. The #1119 intent/authority gate, which includes `CreateFresh` →
   `DbCreateTargetExists`. It is evaluated with the versioning rules the
   probe selected (R2/R3).
3. Integrity. A `NotPortable` probe runs today's
   `validate_current_schema_integrity`. A `Portable` probe in band runs the
   preflight form of R5.2; outside band it runs today's check.
4. Identity, exactly as in #1983/#1984: `read_identity` decodes the role and
   then the profile, followed by profile admission (`StoreProfileMismatch`,
   `StoreProfileNotExact`, `StoreProfileUnstamped`) and role resolution
   (`StoreRoleConflict`).

**Side effects.**

- A backup is taken when R9 or the marker fallback requires one.
- The connection PRAGMAs are set.

**Authoritative phase** (inside `BEGIN IMMEDIATE`). Steps 0–4 run again on the
in-transaction state, and only this evaluation decides. **Coverage** follows
#1983's rule, extended by the probe:

- If the authoritative decision is an authorized migration and
  `(s, probe)` differs from the preflight's, the open refuses with
  `SchemaChangedDuringOpen`. The backup on disk, if any, is of a different
  state.
- An authoritative decision of `band` or `current` is admitted, whatever the
  preflight saw.

This keeps #1983's accepted race intact: another process completes
`F@38 → F@39` between preflight and transaction, and the open then succeeds
(#1983 `open_race_tests.rs:184`). It also closes the profile race: a raw
writer flips `P@36` to `F@36` between preflight and transaction. The
authoritative decision is now `F` pending under `Allow`, `(s, probe)` has
changed, and no backup covers it, so the open refuses.

After admission: `init_schema_inner(resolved profile)`, the identity stamp,
the migrations (R3 or today's), the stamp (R4 or today's), the validators
(R5.2 in-tx form for Portable band opens), commit, and the marker.

**Precedence.** It does not change. Every refusal keeps #1983/#1984's order
for every input. The observable changes are all outcome changes, listed in
§6.

## 6. Outcome changes

The refusal order is #1983/#1984's for every row. A malformed profile stamp
probes `NotPortable`, so it follows today's rules, and `read_identity` then
reports its decode error in the usual place. Example values: `E = 39`,
`π_P = 36`, floor 39.

| # | Probe | `s` | Region | `Deny` | `Allow` | Change vs. today |
|---|---|---|---|---|---|---|
| 1 | any | `> E` | newer | `newer` | same | none |
| 2 | `NotPortable` | any | today's | today's | today's | **none (R0)** |
| 3 | `Portable` | 0 | fresh | build P, stamp `max(36, min(39, E))` | same | none at `E = 39`; above 39, the stamp stays at the floor instead of `E` |
| 4 | `Portable` | 1–35 | pending | OptInRequired | backup, missing `relevant(P)` sentinels (any index) + vacuous ≤ floor, R5.2, stamp 39 | none at `E = 39` |
| 5 | `Portable` | 36–38 | band | **admitted if §5 admits (R5.2)**: no migration, no stamp, no forced backup | same | **was OptInRequired (Deny) / backup + stamp 39 (Allow)** |
| 6 | `Portable` | 39 | band | admitted if §5 admits | same | adds R5.2 |

A `Portable` store opened under a requirement it doesn't satisfy (e.g.
`AtLeast(F)` or `Exact(F)`) is refused at step 4, as today. For rows 4–6 that
refusal comes *after* the version decision, as it does today. So for
`P@30, Deny, AtLeast(F)` the result stays `SchemaMigrationOptInRequired`.

**Worked product-only bump** (`E = 40`, v40 is `Product`, `π_P` stays 36,
floor 39):

| Store | Today | Under B |
|---|---|---|
| `P@39` | Deny: OptInRequired. Allow: forced backup, stamp 40 | band: admitted, stays 39 |
| fresh `P` | stamp 40 | stamp 39 |
| `F@39` | Deny refuses. Allow migrates | unchanged |

**Worked Portable bump** (`E = 41`, v41 is `Portable`, `π_P = 41`): every
Portable store is now `pending`. `Deny` refuses. `Allow` backs up, migrates
and stamps 41.

## 7. Rollout and rollback matrix

Stores fall into three groups: created by a B binary, migrated by it, and
merely opened by it (band).

| Binaries | Store | Result |
|---|---|---|
| pre-B 39 → B (`E = 39`) | existing `P@39`, opened | band. Same writes as a pre-B open (R2). Stays 39 |
| B (`E = 39`) → pre-B 39 | `P` created or migrated by B | stamped 39. Sentinels ≤ 39 all present (R5.1). pre-B opens it under `Deny` |
| B (`E = 40`, product-only) → pre-B 39 | `P` created or migrated by B | stamped 39 (floor). pre-B opens it; it doesn't know the v40 key |
| B (any) → pre-B 39 | pre-existing `P@36`, merely opened by B | stays 36. pre-B refuses it under `Deny`, **exactly as it did before B** |
| B (`E = 40`) → pre-B 39 | `F` migrated by B (38/39 → 40) | refused as `newer`, as today. Recovery: restore the forced backup that migration took |
| B (`E = 40`) → pre-B 39 | `F` freshly created by B | refused as `newer`, as today. **No backup exists** (a fresh build takes none) |
| B (`E = 41`, Portable) → any binary with `E ≤ 40` | `P` migrated to 41 | refused as `newer`. Recovery: restore the forced backup for files |
| same | **sealed private image** migrated to 41 | refused as `newer`. **No backup exists**: private init suppresses filesystem artifacts (`db/schema.rs:1182-1192`). Recovery is the owning partition's own image history. That is outside memcore, and this spec does not provide it |

## 8. Appendix: blast radius (evidence SHA)

**A1. memcore sites that compare `s` with `E` or write the stamp.**

`db/migrations.rs`:

- `:338-346`: R1, unchanged.
- `:352-384`: gains the R5.2 Portable branch.
- `:419-465`: gains the probe (R2/R3).
- `:552-564`: the public standalone `run_data_migrations_with_profile`
  validates and writes the stamp. It must either apply R3/R4/R5 or be
  restricted to `TachiFull`.
- `:578-818`: the runner becomes table-driven.
- `:182-222`: the sentinel list becomes derived.

`db/schema.rs`:

- `:1203-1257`: the probe and the R5.2 in-tx validation (§5).
- `:1236`: R4.
- `:3254-3312`: R9.
- #1983's `reevaluate_admission_in_tx`: coverage keyed on `(s, probe)`.

`store/open.rs`:

- `:692-700`: older-stamp trigger relaxation.
- `:790-796`: `stored != E` in the fresh identity-bound reopen.
- `:950-970`: the read-only open sends a band store through the `Deny` gate
  as if it were older.
- `:1025-1031`: exact-dedupe `stored != E`.

Unchanged: `db/filename.rs:257-258`.

**A2. Readers outside the gate.** For each one, the implementation PR must
record either that it admits Portable stores, or that it is
`TachiFull`-only and keeps its equality check behind an explicit profile
assertion.

- `tachi-server/src/doctor/schema_skew.rs:52-117` reports a false "behind"
  for band stores.
- `bootstrap/migrate_cli.rs:224-227, 295-325, 451-457`: the sweep would try to
  migrate band stores.
- `bootstrap/manifest_cli.rs:118`.
- The wiki corpus is `TachiFull`-only, so assert the profile there rather
  than project: `bootstrap/wiki_corpus/fs.rs:679-685`, `plan.rs:768-772`,
  `legacy.rs:411-415`, `classify.rs:738, 797-811`.
- `memory-server-runtime/src/lib.rs:1304-1308` is dead code at `E = 39`.

All readers that project move to `store_version_status` (§9).

**A3. Hyperion.** This is a separate leaf, after intake:

- `hypermem/src/lib.rs:26`
- `main.rs:15-19`
- `migration.rs:305-324`: the legacy import window `[20, E)` would accept a
  band source as legacy.
- `docker/deploy.sh:320-331, 420-424`
- `deploy_contract_selftest.sh`

**A4. Tests that flip under B.** The implementation PR lists each one, before
and after.

- `db/migrations/current_truth.rs:181-264`: `Deny` on `P@36` and `P@37` is now
  admitted, and fresh `P` stamps stay 39. #1984 also touches this file.
- `db/migrations/verified_admissions.rs:46`, `current_truth.rs:37`,
  `mirror_eval_identity.rs:113`: `Product` bodies at or below the floor are
  recorded vacuously and not called.
- `store/profile_identity_tests.rs:928-957`: the sentinel set stays
  profile-invariant only up to the floor.
- `portable-server/src/main.rs:178-237, 264-307, 358-376`: `E − 1 = 38` falls
  in the P band. Fixtures must use `π_P − 1`.
- `db/migrations.rs:2180-2294`: the gate decision tests gain the probe axis.
- Doc comments claiming "sentinel set is profile-invariant" (v12, v14–v18,
  v20, v21, v32 and v39 modules) must be rewritten.
- `store/profile_identity_tests.rs:110-126` `KERNEL_TABLES`: add
  `memory_outbox_*`, `harness_session_*`, `delivery_*` and
  `memory_search_generation`. The R5.2 baseline is derived from the same
  source.
- These **must pass unchanged** (R0 regression guards):
  `db/migrations.rs:1984-2000` and #1983 `open_race_tests.rs:184`.

**A5. Why shipped scopes are immutable.** Reclassifying v33–v36 as `Product`
would drop `π_P(39)` to 30. Then `P` stores stamped 36–39 would contain the
v33–v36 tables and later fresh ones would not, yet both would count as
current. The unconditional validators (`db/migrations.rs:369-371`,
`db/schema.rs:1238-1244`) would reject one of the two populations.

**A6. Frozen un-versioned Portable maintenance** (`db/schema.rs:1435-1611`):

- `recall_cache.generation_fingerprint`
- memories columns: `archived`, `created_at`, `updated_at`, `scored_count`,
  `revision`, `valid_from`, `valid_until`, `retention_policy`, `domain`,
  `superseded_by`, `idless_identity` (+ index), `recall_count`,
  `query_diversity`, `tier`, `last_use_at`
- `access_history.query_hash`, `event_kind`
- `memory_edges.valid_from`, `valid_to`
- `derived_items.summary`, `importance`, `scope`, `created_at`
- `bridge_hypertachi_memory_columns`
- the v8/v9 bodies re-run at `:1589-1591`
- `ensure_search_generation_schema`
- `ensure_fts_backfilled`
- `migrate_enum_constraints` (conditional rebuild)
- `ensure_optimization_indexes`
- the Portable `MIGRATED_INDEXES_CHUNKS`

Outside the schema transaction: `try_load_sqlite_vec` (the R7 exception).

**A7. Recognized historical variants** that R5.2's preflight accepts and the
frozen set normalizes. The implementation must enumerate this list in full,
from the frozen helpers' own recognition code, and freeze it. It currently
includes:

- the previous `memory_search_generation_after_update` trigger with
  `UPDATE OF …` (`search_generation.rs:147-150, 180-199`);
- the pre-`migrate_enum_constraints` `memories` shape (`db/schema.rs:2681`);
- memories columns absent before their `ensure_column` (A6).

Nothing outside A7 is normalized on a band open.

## 9. Consumer contract

- memcore exports `PORTABLE_EXPECTED_SCHEMA_VERSION`, `PORTABLE_COMPAT_FLOOR`
  and a read-only `store_version_status(path, requirement)`, which returns
  one of `Fresh`, `Current { stamp }`, `Pending { from, to }`,
  `Newer { stamp }` or `Refused(error)`. It follows §5's preflight and writes
  nothing.
- Hypermem's `--schema-version` prints both `E` and `π_P`. A new
  `--check-store <path>` prints the status and exits with a distinct code per
  variant. `docker/deploy.sh` today compares integers for exact equality in
  both directions (`:420-424`); it switches to calling `--check-store`. This
  is a Hyperion leaf.
- No consumer may re-implement the band in shell or compare `user_version`
  against a constant.

## 10. Tests (acceptance for the implementation PR)

Every test asserts that it reached the branch it names, through a receipt, a
hook or an error variant, not only its final state.

- **T1 Classification.**
  - (a) For each `Product` migration, run its full-profile branch on a
    **populated** full-shaped store at the preceding version. The store
    must include rows in every Portable table, including wiki-sourced
    memories. Then assert that every Portable object's schema and every
    Portable table's content hash are unchanged.
  - (b) For each `Portable` migration, run it on a historically shaped `P`
    store at the preceding version, built with the existing historical
    fixture helpers (e.g. `db/migrations/harness_session_events.rs:129-159`
    for shipped v34), not by replaying today's installers. Validate with the
    validators for that prefix.
  - (c) Two deliberately misclassified test-only `Product` migrations must
    each fail (a): one alters `memories`, and one runs
    `UPDATE memories SET text = … WHERE …`.
- **T2 Product-only bump.** Add a test-only `Product` migration at `E + 1`,
  injected with a consistent catalogue, `E` and projection through the real
  open funnel.
  - A `P` store at 39 opens under `Deny`, with no forced backup, and
    `user_version` and `PRAGMA schema_version` unchanged. The marker and
    vector availability are controlled.
  - The same holds for `P` at 36.
  - An `F` store refuses under `Deny`.
- **T3 Portable bump.** Add a test-only `Portable` migration at `E + 1` with
  an observable effect. A `P` store refuses under `Deny`. Under `Allow` it
  backs up, the backup's contents are the pre-migration state, the effect is
  present, and the stamp is `E + 1`.
- **T4 Downgrade and snapshot.**
  - (a) `s = E + 1` returns `newer` for `P`, `F` and absent profile stamps,
    and for malformed profile stamps.
  - (b) A separate, admitted-start race covers the snapshot. A `P@36` store
    passes preflight step 0 and a writer commits under WAL before the
    identity read. The preflight either observes a consistent snapshot or
    the authoritative phase refuses. The open never mixes the header of one
    commit with the stamps of another.
- **T5 Non-Portable invariance.**
  - The full existing suite passes with no assertion changes: #1984's
    admission tables, the marker-fallback backup tests (missing or
    old-format marker on `F@39`, non-empty `s = 0`),
    `db/migrations.rs:1984-2000`, and #1983 `open_race_tests.rs:184`.
  - Plus, under `Allow`, an `F@38` store missing the v3 sentinel migrates v3.
- **T6 Router shape.** A synthetic v28 `P` store goes through intake under
  `Allow` and is stamped 39. A binary with a product-only bump then restarts
  it under `Deny`: the restart succeeds and no forced backup appears. This
  establishes synthetic behaviour, not router lineage.
- **T7 Outcome table.** For every row of §6, one valid-input baseline that
  reaches the row's branch, plus separate corruption variants on that same
  baseline: a malformed role, a malformed profile, and a missing sentinel.
  Each variant asserts #1983/#1984's error for that input.
- **T8 Idempotent maintenance.** Reopening a converged `P` store and a
  converged `F` store leaves `sqlite_schema` and `PRAGMA schema_version`
  unchanged, with `memories_vec` controlled.
- **T9 Band validation.**
  - A `P@36` store whose sentinels are all present but whose
    `idx_memories_idless_identity_active` is non-unique refuses at the
    in-tx R5.2. It is rolled back and left unrepaired.
  - The same holds for a missing Portable column and for a missing trigger.
    Each case asserts which validator refused it.
  - A `P@39` store missing the v39 `Product` sentinel refuses (R5.2a).
  - A genuine `P@36` store without v37–v39 sentinels is admitted.
  - A sealed private `P@39` image carrying the previous search-generation
    trigger (A7) is admitted through the production private door, and the
    trigger ends normalized.
- **T10 Coverage race.** This uses #1983's hook between preflight and
  `BEGIN IMMEDIATE`. The store is `P@36` under `AtLeast(P)`, with the role
  admitted, under `Allow`. The hook flips the profile stamp to `F`. The open
  refuses with `SchemaChangedDuringOpen`, no migration runs, and
  `user_version` is unchanged.
- **T11 Rollback matrix.** One test per row of §7. Pre-B behaviour is
  represented by the pinned pre-B gate and validators as a test fixture. For
  sealed images, a real sealed image goes through the production private
  door.
- **T12 Private image forward.** A real sealed `P@39` image, created before
  the bump, opens under `Deny` after a product-only bump through the
  production private door. Today that open refuses (`store/open.rs:559-577`).

## 11. Open items

- **Router profile stamp.** #1585 D2 landed at v28, so the router's v28
  store may have no profile stamp. If so, every `AtLeast(P)` or `Exact(P)`
  open refuses with `StoreProfileUnstamped`, and intake (#1987 item 6) needs
  an operator stamp first. Check this on the router before intake.
- **Ordering with #1983/#1984.** Implementation starts after both have
  landed. §5 extends #1983's `reevaluate_admission_in_tx`.
- **Historical stores.** A `ca8b0540` (schema 33) store is `pending`. Its
  sentinel and object provenance under R3 needs a historical fixture before
  Hyperion intake relies on it.
- **Validator asymmetry.** `db/migrations.rs:369-371` validates v36 and
  `db/schema.rs:1238-1244` does not. R5.2 subsumes both for Portable stores;
  align them for `TachiFull` in the separate hardening leaf.
