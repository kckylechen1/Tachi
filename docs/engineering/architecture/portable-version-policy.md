# Portable Version Policy — per-profile schema-version projection

> **Status:** spec, pre-implementation, revision 5 (after four rounds of
> cross-vendor attack review). Owner decision **D7 = B** (2026-09-26).
> **Issue:** #1991 (parent #1987 W2-5).
> **Anchors:** #984 (downgrade gate), #1119 (migration authority), #1180
> (migration backup), #1585 (profiles and identity), #1983 (preflight before
> backup), #1984 (`ProfileRequirement::{AtLeast,Exact}`).
> **Companions:** [`portable-kernel-split.md`](./portable-kernel-split.md),
> [`../schema-migration-runbook.md`](../schema-migration-runbook.md).
> **Evidence base:** origin/main `2898c651281fd17c47b1830c17145c8995d23e07`
> (paths under `crates/memcore/src/` unless stated); #1983 head `3d6ce14d0`,
> #1984 head `983aeba07`. Both PRs have moved on since; the implementation
> re-anchors on whatever lands.

## 1. Problem

`PRAGMA user_version` is one integer, and four decisions read it without ever
looking at the store profile:

| Decision | Code | Rule today |
|---|---|---|
| Downgrade refusal (#984) | `db/migrations.rs:338-346` | `s > E` → refused |
| Migration authority (#1119) | `db/migrations.rs:419-465` | `OpenExisting` with `1 ≤ s < E`: `Deny` refuses, `Allow` migrates |
| Migration backup (#1180) | `db/schema.rs:3289-3296` | `s ∈ 1..E` forces a backup; every other open falls back to comparing the marker fingerprint |
| Stamp | `db/schema.rs:1236` | always writes `E` |

(`s` = stored `user_version`, `E` = `EXPECTED_SCHEMA_VERSION`, which is 39 on
the evidence SHA.)

A **product-only** schema bump is a no-op on a `PortableKernel` store: every
one of its migrations begins with
`if !profile.includes_product() { return Ok(0) }`. It still moves `E`, though,
and after that every `PortableKernel` store:

- needs `Allow` to open;
- takes a forced backup;
- gets re-stamped to a value that older binaries refuse.

That hits Hyperion's Hypermem stores and Tachi's own sealed private images
(`store/open.rs:559-577` always opens an existing image with
`open_existing_deny()`). Option C, gating only the migration *bodies* by
profile, changes none of the four rows above.

## 2. Definitions

- **Scope.** Every versioned migration `i` has an immutable
  `scope(i) ∈ {Portable, Product}`.
- **Relevant set.** `relevant(PortableKernel)` is the set of `i` with
  `scope(i) = Portable`. `relevant(TachiFull)` is every `i`.
- **Projection.** `π_p(E) = max { i ∈ relevant(p) : i ≤ E }`. For
  `TachiFull`, `π_TachiFull(E) = E`.
- **`PORTABLE_EXPECTED_SCHEMA_VERSION`** `= π_PortableKernel(E)`. It is derived
  from the migration table, never written by hand, and is **36** on the
  evidence SHA (§4).
- **`PORTABLE_COMPAT_FLOOR`**: the `EXPECTED_SCHEMA_VERSION` of the last
  release built without this policy. It is a frozen constant, and 39 if B
  lands before any v40.
- **Profile probe.** A read-only decode of the profile stamp only. It reads no
  role, runs no admission, and never returns an error. It reports `Portable`
  only when a profile stamp exists and decodes to `PortableKernel`. Every
  other case reports `NotPortable`: no stamp, a `TachiFull` stamp, or a stamp
  that fails to decode.
- **Resolved profile.** What #1984's resolver returns once admission passes.
  If a stamp exists, it returns the stamp's profile, even at `s == 0`. If
  there is no stamp and `s == 0`, it returns the requirement's profile
  (`None if fresh`, #1984 `store_identity.rs`).
- **Policy class.** Each open follows exactly one policy:
  - **Portable policy.** Applies when the probe is `Portable`, or when
    `s == 0` and the resolved profile is `PortableKernel`. A fresh,
    unstamped file under a `P` requirement is Portable even though its probe
    is `NotPortable`.
  - **Today's policy.** Applies to everything else. This is exactly what
    #1983/#1984 implement.
- **Regions** under the Portable policy:
  - `fresh`: `s == 0`
  - `pending`: `1 ≤ s < π_P(E)`
  - `band`: `π_P(E) ≤ s ≤ E`
  - `newer`: `s > E`

  Under today's policy the band is exactly `s == E`.

## 3. Rules

**R0 — Invariance.** Every open that runs under today's policy in both the
preflight and the authoritative phase keeps every outcome, error, error
precedence and side effect it has in #1983/#1984. An open whose policy class
differs between the two phases is a deliberate exception, listed in §6. The
Portable policy changes only four things:

- the version decision in `band` (R2);
- the output of fresh builds and pending migrations: stamp and sentinels
  (R4, R5.1);
- the added validation (R5.2);
- the coverage rule for Portable band (§5).

The refusal *order* stays that of #1983/#1984 under both policies. The
Portable policy can change which error wins, because an admission that used
to stop at the #1119 gate now reaches later checks. §6 lists every such case.
If any rule text contradicts R0 for an open under today's policy, R0 wins and
the text is a spec bug.

**R1 — Downgrade.** `newer` is refused under every policy at the header gate,
as today, before any identity decode.

**R2 — Portable band.** An `OpenExisting` open in `band` is admitted under
either authority when the rest of §5 admits it, including R5.2. It:

- runs no versioned migration;
- does not write `user_version`;
- takes no version-forced backup. The marker-fingerprint fallback still
  applies, as #1983 specifies, and so does its coverage rule (§5).

The only schema maintenance is `init_schema_inner` with the frozen
un-versioned set (R7, §8 A6):

- On a converged store, `sqlite_schema` and the DDL cookie stay unchanged
  (T8).
- Some older shapes are normalized to the complete baseline by the frozen
  set. One example is the previous `memory_search_generation_after_update`
  trigger (`search_generation.rs:180-199`). Such shapes are admitted.
- A shape the frozen set cannot bring to the complete baseline is refused by
  R5.2b. The transaction rolls back, just as today's in-transaction
  validators refuse it. Example: the pre-`migrate_enum_constraints`
  `memories` shape. Its rebuild drops the v23 reserved-reference guards, and
  only v23 reinstalls them. There is no allowlist of historical variants.

Other writes happen as they do today: identity adoption (stamping a missing
role on a labelled open), per-open data repairs (#1987 W2-7), the marker, the
WAL-mode PRAGMA and `memories_vec` provisioning (R7). Authority is never
consulted and no migration log line is emitted.

**R3 — Portable pending (the #1119 decision).** `Deny` refuses with
`SchemaMigrationOptInRequired`. `Allow` does the following, in order:

1. takes the forced backup;
2. runs every `relevant(P)` migration whose sentinel is missing, **whatever
   its index**. Selection is by sentinel, as today
   (`db/migrations.rs:889-899`); the index only sets execution order;
3. records the vacuous sentinels of R5.1;
4. runs R5.2;
5. stamps per R4.

R5.2 is new on this path; §6 lists the outcome change.

**R4 — Portable stamp.** After a fresh build or an authorized pending
migration under the Portable policy, the stamp is
`max(s, π_P(E), min(PORTABLE_COMPAT_FLOOR, E))`. A stamp is never lowered, and
in `band` it is never written.

- **While `π_P(E) ≤ PORTABLE_COMPAT_FLOOR`**, the floor means any store a B
  binary creates or migrates can be reopened by the last pre-B binary under
  `Deny`. That includes sealed images, which have no `Allow` path. Once a
  Portable migration above the floor ships, the stamp exceeds the floor, and
  the last pre-B binary refuses the store as `newer` (§7).
- Above the floor, the stamp moves only when a Portable migration ships.
  Every index between `π_P(E)` and `E` is Product by definition, so the floor
  never claims a Portable migration that was not applied.

**R5 — Sentinels and validation (Portable policy).**

1. **Creation.** Every fresh build and every authorized pending migration
   records the sentinel of each `Product` migration with index
   `≤ min(PORTABLE_COMPAT_FLOOR, E)`. These are recorded vacuously, without
   running the body. Pre-B validators require them
   (`db/migrations.rs:352-363`). Sentinels of Product migrations above the
   floor are never written to a Portable store. Extra sentinels are tolerated
   and never removed.
2. **Validation.** It runs on every admission under the Portable policy that
   is not a fresh build.
   - (a) **Band input integrity.** This runs for band only, in both the
     preflight and the authoritative integrity step (§5). It is today's
     `validate_current_schema_integrity` projected to the Portable policy:
     the same object validators it runs at `s == E` today, applied to the
     sentinel set `relevant(P) ≤ π_P(E)` plus every `Product` sentinel with
     index `≤ min(s, PORTABLE_COMPAT_FLOOR)`.
     - For a `P@39` input this is exactly today's check, so today's error and
       its precedence are unchanged.
     - A missing sentinel refuses, because band runs no migration (R2). That
       matches how today refuses an incomplete `F@E`.
   - (a′) **Pending output inventory.** This runs for pending only, inside the
     transaction, after R3's migrations and before the stamp. It checks the
     complete `relevant(P) ≤ π_P(E)` inventory plus the R5.1 vacuous set.
     Pending preflight imposes no sentinel requirement, so missing-sentinel
     recovery at any index (R3) still works.
   - (b) **Complete shape.** Every Portable table, column, index and
     trigger, including index uniqueness and partial-index predicates. It
     runs **once**, inside `BEGIN IMMEDIATE`, after `init_schema_inner` has
     applied the frozen set and before commit. A mismatch refuses and rolls
     back.

   Two consequences:
   - A marker-fallback backup taken before the transaction can remain after
     such a refusal. That is the same class as today's in-transaction
     validator failures, recorded in #1990's side-effect table.
   - Today's validators miss this case: a non-unique
     `idx_memories_idless_identity_active` survives
     `CREATE UNIQUE INDEX IF NOT EXISTS` (`db/schema.rs:1474-1479`) and breaks
     `ON CONFLICT(idless_identity)` (`db/memory_crud.rs:2970-2972`). R5.2b
     refuses it.
3. Validation under today's policy is unchanged (R0). Bringing full-shape
   validation to `TachiFull` is a separate hardening leaf.

**R6 — Scope assignment.**
1. **Existing migrations v1–v39.** `scope = Portable` if and only if the
   migration body, as implemented on the evidence SHA, takes effect on
   `PortableKernel` stores (no `includes_product()` early return). Otherwise
   the scope is `Product`.
   - The rule is mechanical and reproduces what every existing Portable
     store contains.
   - Older Product migrations that only gained their guard in #1585 are
     judged by the evidence-SHA implementation.
   - v33–v36 stay `Portable`. That is the owner sub-decision default, and it
     matches the author's intent at
     `db/migrations/harness_session_attachments.rs:5`.
2. **New migrations v40+.** The scope is declared in the migration table and
   follows the objects touched, not the PR's intent. If any profile branch
   creates, alters, rewrites or drops a Portable object, or rewrites data in
   a Portable table, the migration is `Portable`. "Profile-neutral for
   convenience" is not allowed.
3. **Immutability.** A shipped scope is never edited. A wrong scope is
   corrected by a new migration (§8 A5).

**R7 — DDL discipline.**
- A new Portable table, column, index or trigger ships only as a
  Portable-scoped versioned migration. The additive-base-chunk amendment rule
  (`db/schema/ddl.rs:2128-2146`) stays available for `SchemaScope::Product`
  chunks only.
- The existing un-versioned Portable maintenance is frozen (§8 A6). No
  additions. On a converged store, every item must leave `sqlite_schema`
  unchanged. The attack reviews found no unconditional DDL on a converged
  Portable schema at the evidence SHA.
- **`memories_vec` exception.** `try_load_sqlite_vec` runs after schema init
  (`store/open.rs:709-712`; private images `:585-588`) and executes
  `CREATE VIRTUAL TABLE IF NOT EXISTS memories_vec` (`db/sqlite_vec.rs:29-41`).
  It provisions an optional capability, not a schema-version fact, so it may
  create the table in `band`. T8 excludes `memories_vec*` only when vector
  availability changed between the two opens.
- Per-open data repairs are out of scope here (#1987 W2-7).

**R8 — #1119 amendment.** "From `user_version` only, never from DB content"
(`db/migrations.rs:386-418`) becomes **"from `user_version` and the policy
class"**. For `s > 0` the policy class comes from the probe of the write-once
profile stamp. The profile stamp is identity, not content: it is write-once
(#1585), the state API protects it from set and delete (`db/state.rs`), and
it already decides admission. For `s == 0` the #1119 decision is a build,
whatever the profile, exactly as today. The policy class then changes only
what the build writes (R4, R5.1).

**R9 — #1180 amendment.** In `maybe_backup_before_migration`, the
"version migration, backup forced" branch (`db/schema.rs:3289-3291`) covers
exactly the `pending` region of the applicable policy. Today's policy keeps
`1..E`, so nothing changes. Under the Portable policy it becomes
`1..π_P(E)`. The marker-fingerprint fallback is unchanged under both
policies, including #1983's one-time backup for old-format markers.

## 4. Scope table on the evidence SHA

| Scope | Versions | Basis |
|---|---|---|
| Portable | 1–11, 13, 19, 22–30, 33–36 | body runs on every profile (`db/migrations.rs:586-816`) |
| Product | 12, 14–18, 20, 21, 31, 32, 37–39 | `if !profile.includes_product() { return Ok(0) }` guard |

`π_P(39) = 36`, confirmed by all three attack rounds. Notes:

- v10 and v11 drop retired tables that neither shape contains. They are
  ungated, so R6.1 makes them `Portable`, and they do not affect `π_P`.
- v5, v6, v8, v9, v13, v19 and v24 are no-ops on a converged Portable store.
  **v7 is not**: it adds `memories.location`, which v9 then drops
  (`db/migrations/legacy_columns.rs:102-107`).
- No SQL foreign key crosses the Portable/Product boundary in either
  direction. v33 holds soft TEXT references (`work_claim_id`,
  `agent_identity_id`) to product identity concepts, which is allowed.

The implementation replaces the hand-written call chain
(`db/migrations.rs:586-816`) and `MIGRATION_SENTINEL_KEYS` (`:182-222`) with a
single table `[(index, sentinel_key, scope, fn)]`. `E`, `π_P`, the sentinel
list and the vacuous set are all derived from it.

## 5. Open funnel

B keeps #1983's funnel and its order. It adds the probe, the policy class and
one coverage rule.

**Preflight.** Advisory. It runs in one read snapshot: a read transaction that
ends before the connection PRAGMAs and `BEGIN IMMEDIATE`.

0. Read the header `s` and probe the profile stamp (no error).
1. `check_schema_version_gate`: R1 (`newer`).
2. The #1119 intent/authority gate, including `CreateFresh` →
   `DbCreateTargetExists`. The decision comes from the policy class: R2/R3
   under the Portable policy, today's decision otherwise. At `s == 0` it is
   always a build.
3. Integrity. Under today's policy: today's
   `validate_current_schema_integrity`. Under the Portable policy in band:
   R5.2a. Under the Portable policy in pending: today's behaviour, which
   skips the check for non-current versions. Nothing for fresh builds.
4. Identity, exactly as in #1983/#1984:
   - `read_identity` decodes the role, then the profile;
   - profile admission (`StoreProfileMismatch` / `StoreProfileNotExact` /
     `StoreProfileUnstamped`);
   - role resolution (`StoreRoleConflict`).

   The resolved profile fixes the policy class of a fresh build (§2).

**Side effects.** Take the backup that R9 or the marker fallback requires,
then run the connection PRAGMAs.

**Authoritative phase** (`BEGIN IMMEDIATE`). Only this phase decides.

0–3. Repeat preflight steps 0–3 on the in-transaction state.

3½. **Coverage**, placed between integrity and identity, as in #1983
(`schema.rs:1319-1333`). Both rules below apply only when
`filesystem_artifacts` is true, which is the condition #1983's check already
uses. Private images are exempt: by design they take no backup and write no
marker (`schema.rs:1182-1192`), and they must not be made to create plaintext
artifacts. Refuse with `SchemaChangedDuringOpen` in either case:
   - (i) The authoritative decision is an authorized migration, and
     `(s, probe)` differs from the preflight's. This is #1983's rule with
     the probe added.
   - (ii) *Portable policy only.* The authoritative state is Portable `band`,
     its marker fallback would require a backup (non-zero schema cookie and
     a marker that doesn't match), and this open took no backup.

   Every other authoritative state is admitted, whatever the preflight saw.
   That keeps #1983's accepted `F@38 → F@39` race working
   (`open_race_tests.rs:184`); the Full current case is untouched by (ii).
   It also closes two gaps:
   - `P@36 → F@36` under `Allow`: rule (i) applies.
   - An empty file replaced by a marker-less `P@36`: rule (ii) applies.

4. Repeat preflight step 4 (identity).

Then, as today: `init_schema_inner(resolved profile)`, the identity stamp, the
migrations (R3 or today's), R5.2b under the Portable policy, the stamp (R4 or
today's), the remaining validators, commit, and the marker.

## 6. Outcome changes

Refusal order is #1983/#1984's for every row. Example values: `E = 39`,
`π_P = 36`, floor 39.

| # | Policy | `s` | Region | `Deny` | `Allow` | Change vs. today |
|---|---|---|---|---|---|---|
| 1 | any | `> E` | newer | `newer` | same | none |
| 2 | today's | any | today's | today's | today's | **none (R0)** |
| 3 | Portable (fresh, resolved P) | 0 | fresh | build P, stamp `max(π_P(E), min(39, E))`, vacuous sentinels ≤ floor | same | none at `E = 39`. At `E > 39` with `π_P(E) ≤ 39` (product-only bumps), the stamp and sentinels stop at the floor instead of `E`. Once `π_P(E) > 39`, the stamp is `π_P(E)` |
| 4 | Portable | 1–35 | pending | OptInRequired | backup, missing `relevant(P)` sentinels (any index), vacuous ≤ floor, **R5.2**, stamp 39 | Allow: **a store whose shape fails R5.2b is now refused** (today it migrates and is stamped 39) |
| 5 | Portable | 36–38 | band | **admitted if §5 and R5.2 admit**: no migration, no stamp, no forced backup | same | **was OptInRequired (Deny) / backup + stamp 39 (Allow)** |
| 6 | Portable | 39 | band | admitted if §5 and R5.2 admit | same | adds R5.2 |

**Errors unmasked in band (rows 5–6).** Today, `Deny` on `P@36..38` stops at
`SchemaMigrationOptInRequired`. Under B the open passes the gate, so a later
refusal becomes the returned error:

- a malformed role (e.g. `{"value":7}`) → the role-decode error;
- a missing sentinel → the R5.2a error;
- an unsatisfied requirement (`AtLeast(F)`, `Exact(F)`) → `StoreProfileMismatch`
  or `StoreProfileNotExact`;
- a role conflict → `StoreRoleConflict`;
- a shape defect that the frozen set does not repair (e.g. a non-unique
  `idx_memories_idless_identity_active`) → the R5.2b refusal, inside the
  transaction.

**Other outcome changes.** Each is pinned in T7 with both the old and the B
outcome (success or refusal, phase, and side effects), not just the error:

- **Band + `Allow` with a missing sentinel.** Example: `P@36`, otherwise
  canonical, but missing the v3 sentinel. Today this is a pending migration:
  the runner executes v3 and the open succeeds. Under B it is band, R5.2a
  refuses it, and nothing is written. This matches how today treats an
  incomplete `F@E`.
- **Band shape defect under `Allow`.** Today the store migrates, the defect
  survives, and the open succeeds. Under B, R5.2b refuses it.
- **Precedence at `P@39` for defects that today's integrity check catches.**
  Unchanged. R5.2a *is* today's check at `s == E`, so a missing delivery index
  combined with a malformed role still returns today's integrity error before
  identity. Only defects that today's validators miss, and that R5.2b alone
  detects, come after identity.
- **Cross-policy race (intended).** A full-shaped `P@36` flipped to `F@36` in
  the window under `Allow`. Today this passes #1983's unchanged-version
  coverage check and migrates. Under B, rule (i) refuses it because the probe
  changed. R0's invariance covers opens that stay under today's policy in
  both phases. This sequence crosses policies, so it is excluded from R0 on
  purpose.

Row 4 and every row under today's policy keep today's winning error. So
`P@30, Deny, AtLeast(F)` still returns `SchemaMigrationOptInRequired`, and
`unstamped s=28, Deny, AtLeast(P)` still returns
`SchemaMigrationOptInRequired`, not `StoreProfileUnstamped`.

**Worked product-only bump** (`E = 40`, v40 is `Product`, `π_P` stays 36, floor
39):

| Store | Today | Under B |
|---|---|---|
| `P@39` | Deny: OptInRequired. Allow: forced backup, stamp 40 | band: admitted, stays 39 |
| fresh P (empty file under `P` requirement, or new private image) | stamp 40, v40 sentinel | stamp 39, no v40 sentinel |
| `F@39` | Deny refuses. Allow migrates | unchanged |

**Worked Portable bump** (`E = 41`, v41 is `Portable`, `π_P = 41`): every
Portable store is `pending`. `Deny` refuses. `Allow` backs up, migrates and
stamps 41. The production private-image door always passes `Deny`
(`store/open.rs:559-577`), so a sealed image refuses. Migrating sealed images
across a Portable bump needs an authorized entry point that does not exist
today (§11).

## 7. Rollout and rollback matrix

| Binaries | Store | Result |
|---|---|---|
| pre-B 39 → B (`E = 39`) | existing `P@39`, opened | band. Writes are the same as a pre-B open (R2). Stays at 39 |
| B (`E = 39`) → pre-B 39 | `P` created or migrated by B | stamped 39 with every sentinel ≤ 39 (R5.1), so pre-B opens it under `Deny` |
| B (`E = 40`, product-only) → pre-B 39 | `P` created or migrated by B | stamped 39 (floor), so pre-B opens it. The v40 key is absent, and pre-B doesn't know it |
| B (any) → pre-B 39 | existing `P@36`, merely opened by B | stays 36. Pre-B refuses it under `Deny`, **exactly as before B** |
| B (`E = 40`) → pre-B 39 | `F` migrated by B (38/39 → 40) | refused as `newer`, as today. Recovery: restore that migration's forced backup |
| B (`E = 40`) → pre-B 39 | `F` built fresh by B | refused as `newer`, as today. A backup exists only if the file had a non-zero schema cookie and no matching marker (marker fallback). An empty file has none |
| B (`E = 41`, Portable) → any binary with `E ≤ 40` | file `P` migrated to 41 under `Allow` | refused as `newer`. Recovery: restore the forced backup |
| B (`E = 41`, Portable) | sealed private image | the production door refuses the forward migration (`Deny`). No migrated image exists to roll back |

## 8. Appendix: blast radius (evidence SHA)

**A1. memcore sites that compare `s` with `E` or write the stamp**

`db/migrations.rs`:
- `:338-346`: R1, unchanged.
- `:352-384`: R5.2a branch.
- `:419-465`: policy class (R2/R3).
- `:552-564`: the public standalone `run_data_migrations_with_profile`
  validates and writes the stamp. It must apply R3/R4/R5 or be restricted to
  `TachiFull`.
- `:578-818`: the runner becomes table-driven.
- `:182-222`: the sentinel list becomes derived.

`db/schema.rs`:
- `:1203-1257`: the funnel (§5), R5.2b.
- `:1236`: R4.
- `:3254-3312`: R9.
- #1983's `reevaluate_admission_in_tx`: the §5 step 3½ coverage.

`store/open.rs`:
- `:692-700`: older-stamp trigger relaxation.
- `:790-796`: `stored != E` in the fresh identity-bound reopen.
- `:950-970`: the read-only open routes a band store through the `Deny` gate
  as "older".
- `:1025-1031`: exact-dedupe `stored != E`.

Unchanged: `db/filename.rs:257-258`.

**A2. Readers outside the gate.** The implementation PR must state, for each
reader, whether it admits Portable stores or stays `TachiFull`-only (keeping
its equality check behind an explicit profile assertion):
- `tachi-server/src/doctor/schema_skew.rs:52-117` gives a false "behind" for
  band stores.
- `bootstrap/migrate_cli.rs:224-227, 295-325, 451-457`: the sweep would try to
  migrate band stores.
- `bootstrap/manifest_cli.rs:118`.
- The wiki corpus is `TachiFull`-only, so assert there rather than project:
  `bootstrap/wiki_corpus/fs.rs:679-685`, `plan.rs:768-772`,
  `legacy.rs:411-415`, `classify.rs:738, 797-811`.
- `memory-server-runtime/src/lib.rs:1304-1308` is dead at `E = 39`.

Readers that do project move to `store_version_status` (§9).

**A3. Hyperion.** A separate leaf, after intake:
- `hypermem/src/lib.rs:26`
- `main.rs:15-19`
- `migration.rs:305-324`: the legacy import window `[20, E)` would accept a
  band source as legacy.
- `docker/deploy.sh:320-331, 420-424`
- `deploy_contract_selftest.sh`

**A4. Tests that flip under B.** The implementation PR lists each flip, with
before and after.
- `db/migrations/current_truth.rs:181-264`: `Deny` on `P@36/37` is now
  admitted, and fresh P stamps stay at the floor.
- `db/migrations/verified_admissions.rs:46`, `current_truth.rs:37` and
  `mirror_eval_identity.rs:113`: Product bodies ≤ floor are recorded
  vacuously, not called.
- `store/profile_identity_tests.rs:928-957`: the sentinel set is
  profile-invariant only up to the floor.
- `portable-server/src/main.rs:178-237, 264-307, 358-376`: `E − 1` falls in
  the P band, so fixtures must use `π_P − 1`.
- `db/migrations.rs:2180-2294`: the gate tests gain the policy-class axis.
- "Sentinel set is profile-invariant" doc comments (v12, v14–v18, v20, v21,
  v32, v39 modules) must be rewritten.
- `store/profile_identity_tests.rs:110-126` `KERNEL_TABLES`: add
  `memory_outbox_*`, `harness_session_*`, `delivery_*` and
  `memory_search_generation`. R5.2b's baseline is derived from the same
  source.
- These **must pass unchanged**: `db/migrations.rs:1984-2000` and #1983
  `open_race_tests.rs:184`.

**A5. Why shipped scopes are immutable.** Reclassifying v33–v36 as `Product`
would drop `π_P(39)` to 30. P stores stamped 36–39 contain the v33–v36
tables, but later fresh ones would not, and both would count as current. The
unconditional validators (`db/migrations.rs:369-371`,
`db/schema.rs:1238-1244`) would then reject one population or the other.

**A6. Frozen un-versioned Portable maintenance** (`db/schema.rs:1435-1611`):
- `recall_cache.generation_fingerprint`.
- `memories` columns: `archived`, `created_at`, `updated_at`,
  `scored_count`, `revision`, `valid_from`, `valid_until`,
  `retention_policy`, `domain`, `superseded_by`, `idless_identity`
  (+ index), `recall_count`, `query_diversity`, `tier`, `last_use_at`.
- `access_history.query_hash`, `event_kind`.
- `memory_edges.valid_from`, `valid_to`.
- `derived_items.summary`, `importance`, `scope`, `created_at`.
- `bridge_hypertachi_memory_columns`.
- The v8/v9 bodies re-run at `:1589-1591`.
- `ensure_search_generation_schema`.
- `ensure_fts_backfilled`.
- `migrate_enum_constraints`: a conditional rebuild, which drops the v23
  guards (R2).
- `ensure_optimization_indexes`.
- The Portable `MIGRATED_INDEXES_CHUNKS`.

Outside the schema transaction: `try_load_sqlite_vec` (the R7 exception).

## 9. Consumer contract

- memcore exports `PORTABLE_EXPECTED_SCHEMA_VERSION`,
  `PORTABLE_COMPAT_FLOOR`, and a read-only
  `store_version_status(path, requirement)`. It returns one of `Fresh`,
  `Current { stamp }`, `Pending { from, to }`, `Newer { stamp }` or
  `Refused(error)`, follows §5's preflight, and writes nothing.
- Hypermem `--schema-version` prints both `E` and `π_P`. A new
  `--check-store <path>` prints the status and exits with a distinct code per
  variant. `docker/deploy.sh` (today an exact integer comparison in both
  directions, `:420-424`) calls `--check-store` instead. This is a Hyperion
  leaf.
- No consumer may re-implement the band in shell, or compare `user_version`
  against a constant.

## 10. Tests (acceptance for the implementation PR)

Every test must assert that it reached the branch it names (a receipt, a
hook, or an error variant), not only the end state.

- **T1 — Classification.**
  - (a) For each Product migration, run its full-profile branch on a
    **populated** full-shaped store at the preceding version. The store
    needs rows in every Portable table, wiki-sourced memories included.
    Assert that every Portable object's schema and every Portable table's
    content hash are unchanged.
  - (b) For each Portable migration, run it on a historically shaped P store
    at the preceding version. Build that store with the existing historical
    fixture helpers (e.g. `db/migrations/harness_session_events.rs:129-159`),
    not by replaying today's installers. Then validate it with that prefix's
    validators.
  - (c) Two deliberately misclassified test-only Product migrations must each
    fail (a): one ALTERs `memories`, the other rewrites `memories.text`.
- **T2 — Product-only bump.** Inject a test-only Product migration at
  `E + 1`, together with a consistent catalogue, `E` and projection, through
  the real funnel.
  - A P store at 39, and one at 36, each with a matching marker, open under
    `Deny`. Assert: zero backups; unchanged `user_version` and
    `PRAGMA schema_version`; **zero versioned-migration invocations** (a
    counter hook in the runner); and an **unchanged migration-sentinel
    inventory**, so the `E + 1` Product sentinel is absent. Vector
    availability is controlled.
  - An F store refuses under `Deny`.
- **T3 — Portable bump.** Inject a test-only Portable migration at `E + 1`
  that has an observable effect. This test also covers the pending path's
  absent preflight sentinel requirement (R5.2a′): the new migration's
  sentinel is necessarily missing going in.
  - Under `Deny`, a P store refuses.
  - Under `Allow` it backs up; the backup holds the pre-migration state, the
    effect is present, and the stamp is `E + 1`.
- **T4 — Downgrade and snapshot.**
  - (a) `s = E + 1` returns `newer` for P, F, no stamp, and a malformed
    profile stamp.
  - (b) A hook records the values the **preflight itself** observed: header,
    probe, role stamp and profile stamp. A writer commits a new identity and
    version under WAL between the preflight's first and last reads. Assert
    that the recorded values all come from one commit.
- **T5 — Today's-policy invariance.**
  - Every existing test that exercises **today's policy** passes with zero
    assertion changes. The only assertions allowed to change are the Portable
    ones listed in §8 A4, and each change needs a before/after. The covered
    tests include #1984's admission tables, the marker-fallback tests (a missing or
    old-format marker on `F@39`, non-empty `s = 0`),
    `db/migrations.rs:1984-2000`, and #1983 `open_race_tests.rs:184`.
  - `F@38` missing the v3 sentinel migrates v3 under `Allow`.
  - The simultaneous version-and-role-change race: preflight a valid `F@39`
    under `Allow`, `Exact(F)`, role `global`; the hook sets `s = 38` and the
    role to `wiki`. The result must be `SchemaChangedDuringOpen`, not
    `StoreRoleConflict`.
- **T6 — Router shape.** A synthetic v28 P store is taken in under `Allow`
  and stamped 39. A binary with a product-only bump then restarts it under
  `Deny`, and it succeeds with no forced backup. This proves synthetic
  behaviour only, not router lineage.
- **T7 — Outcome table.** For every row of §6, run one valid-input baseline
  that reaches the row's branch. On that baseline, run each corruption
  variant separately: malformed role, malformed profile, missing sentinel,
  unsatisfied requirement, and an unrepaired shape defect. Also run these
  combined variants: shape defect + malformed role; today's integrity defect
  (e.g. a missing delivery index at `P@39`) + malformed role. Every case is
  pinned twice, for the old (#1983/#1984) behaviour and for B. Each pin is an
  **outcome**, not just an error: success or refusal, the phase that
  decided it, and side effects (backups, stamp, sentinels, rows). The two
  pins differ exactly where §6 ("errors unmasked in band" and "other outcome
  changes") says they do.
- **T8 — Idempotent maintenance.** Reopening a converged P store and a
  converged F store leaves `sqlite_schema` and `PRAGMA schema_version`
  unchanged. `memories_vec` is controlled.
- **T9 — Validation.**
  - A `P@36` store with all sentinels but a non-unique
    `idx_memories_idless_identity_active`, with a matching marker, refuses
    at R5.2b inside the transaction. Assert: rolled back, zero backups, not
    repaired.
  - The same for a missing Portable column and a missing trigger. Choose
    objects the frozen set does **not** recreate; a missing search-generation
    trigger, for example, is recreated, so it doesn't qualify. Assert which
    validator refused.
  - A `P@35` store with a complete v35 inventory, under `Allow`, migrates
    v36. R5.2a′ passes after the migration and the stamp is 39. This shows
    that pending preflight does not demand the v36 sentinel.
  - A `P@36` store that is otherwise canonical but missing the v3 sentinel
    refuses at R5.2a under `Allow`, and nothing is written. Today the same
    store migrates and succeeds (§6 "other outcome changes").
  - The same non-unique-index store stamped 30 under `Allow` refuses at
    R5.2b (§6 row 4).
  - A `P@39` store missing the v39 Product sentinel refuses at R5.2a in the
    preflight.
  - A genuine `P@36` store without v37–v39 sentinels is admitted.
  - A sealed private `P@39` image with the previous search-generation trigger
    is admitted through the production private door, and ends up normalized.
  - A `P@36` store with the pre-enum-constraints `memories` shape refuses at
    R5.2b, as it does under today's post-maintenance validators.
- **T10 — Coverage race (i).** A `P@36` store with a **matching marker**,
  under `AtLeast(P)`, with an admitted role, under `Allow`.
  - First assert zero backups before the hook fires.
  - The hook flips the profile stamp to F.
  - Result: `SchemaChangedDuringOpen`. No migration, `user_version`
    unchanged.
- **T11 — Coverage race (ii).** An empty file, under `OpenExisting` +
  `Allow`, `AtLeast(P)`.
  - The hook installs a converged, marker-less `P@36`.
  - Result: `SchemaChangedDuringOpen`. Zero backups, nothing written.
- **T12 — Rollback matrix.** One test per row of §7. The pre-B side is the
  pinned pre-B gate and validators, as a test fixture. Sealed-image rows go
  through the production private door with real images.
- **T13 — Fresh P at a product bump.** At `E = 40` (v40 test-only Product),
  run two creations:
  - an empty unstamped file opened under `AtLeast(P)`, and under `Exact(P)`;
  - a new sealed private image.

  Each is stamped 39 and has no v40 sentinel. The pre-B fixture reopens each
  under `Deny`.
- **T14 — Private image forward.** A real sealed `P@39` image created before
  a product-only bump opens under `Deny` after it, through the production
  private door. Today it refuses (`store/open.rs:559-577`).

## 11. Open items

- **Router profile stamp.** #1585 D2 landed at v28, so the router's v28 store
  may carry no profile stamp.
  - Under `Deny`, the intake open returns `SchemaMigrationOptInRequired`
    first (§6).
  - Under `Allow` with `AtLeast(P)` or `Exact(P)`, it refuses with
    `StoreProfileUnstamped`. Intake (#1987 item 6) then needs an operator
    stamp first.

  Check this on the router before intake.
- **Ordering with #1983/#1984.** Implementation starts after both land. The
  spec's anchors to #1983's `reevaluate_admission_in_tx` must be re-checked
  against the merged code.
- **Historical stores.** A `ca8b0540` store (schema 33) is `pending`. Its
  sentinel and object provenance under R3 needs a historical fixture before
  Hyperion intake relies on it.
- **Sealed images across a Portable bump.** No authorized migration entry
  point exists for sealed private images (the door is always `Deny`). That
  needs its own decision before the first Portable migration ships.
- **Validator asymmetry.** `db/migrations.rs:369-371` validates v36;
  `db/schema.rs:1238-1244` does not. Under the Portable policy R5.2 covers
  both. For `TachiFull`, align them in the hardening leaf.
