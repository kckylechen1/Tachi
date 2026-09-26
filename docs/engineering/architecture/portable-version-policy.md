# Portable Version Policy — per-profile schema-version projection

> **Status:** spec, pre-implementation. Owner decision **D7 = B** (2026-09-26).
> **Issue:** #1991 (parent #1987 W2-5).
> **Anchors:** #984 (downgrade gate), #1119 (migration authority), #1180
> (migration backup), #1585 (profiles and identity), #1983 (preflight before
> backup), #1984 (`ProfileRequirement::{AtLeast,Exact}`).
> **Companion:** [`portable-kernel-split.md`](./portable-kernel-split.md),
> [`../schema-migration-runbook.md`](../schema-migration-runbook.md).
> **Evidence base:** origin/main `2898c651281fd17c47b1830c17145c8995d23e07`;
> all `file:line` references below are on that SHA unless marked otherwise.

## 1. Problem

`PRAGMA user_version` is a single integer that drives four decisions, none of
which looks at the store profile:

| Decision | Code | Rule today |
|---|---|---|
| Downgrade refusal (#984) | `db/migrations.rs:338-346` | `s > E` → refused |
| Migration authority (#1119) | `db/migrations.rs:419-465` | `OpenExisting`, `1 ≤ s < E` → `Deny` refuses, `Allow` migrates |
| Migration backup (#1180) | `db/schema.rs:3289-3296` | `s ∈ 1..E` → full backup |
| Stamp | `db/schema.rs:1236` | always writes `E` |

(`s` = stored `user_version`, `E` = `EXPECTED_SCHEMA_VERSION`, 39 on the
evidence SHA.)

The store profile is only resolved after these gates (`schema.rs:1203-1224`;
on #1983's head the read-only identity preflight still runs after them). So a
**product-only** schema bump — one whose migrations are no-ops on a
`PortableKernel` store (`if !profile.includes_product() { return Ok(0) }`) —
still moves `E`, and every `PortableKernel` store then needs `Allow`, takes a
full backup, and is re-stamped to a value older binaries refuse. Affected
stores: Hyperion's Hypermem stores (`hypermem/src/store.rs`), and Tachi's own
sealed private images (`store/open.rs:559-577` opens them with `Deny` +
`PortableKernel`). Gating migration *bodies* by profile (option C) changes
none of the four rows above.

## 2. Definitions

- **Scope.** Every versioned migration `i` carries an immutable
  `scope(i) ∈ {Portable, Product}`.
- **Relevant set.** `relevant(PortableKernel) = { i : scope(i) = Portable }`;
  `relevant(TachiFull) = all i`.
- **Projection.** `π_p(E) = max { i ∈ relevant(p) : i ≤ E }`.
  `π_TachiFull(E) = E` by construction.
- **`PORTABLE_EXPECTED_SCHEMA_VERSION`** `= π_PortableKernel(E)`, a
  compile-time constant derived from the migration table (never hand-written).
  On the evidence SHA it is **36** (§4).
- **Effective profile for versioning** `p`:
  - the stamped profile, if a profile stamp exists;
  - `TachiFull` if no profile stamp exists and `s > 0` (pre-#1585 store; today
    `resolve_profile` adopts `TachiFull` for full requirements and refuses
    with `StoreProfileUnstamped` for portable ones — unchanged);
  - the requirement's profile if `s == 0` (fresh build).
- **Regions** of `s` for effective profile `p`:
  - `fresh`: `s == 0`
  - `pending`: `1 ≤ s < π_p(E)`
  - `band`: `π_p(E) ≤ s ≤ E` (for `TachiFull` this is exactly `s == E`)
  - `newer`: `s > E`

## 3. Rules

**R1 — Downgrade.** `newer` is refused for every profile, before any identity
read (a newer kernel may write identity encodings this kernel cannot decode).
Unchanged from #984.

**R2 — Current band.** In `band`, an `OpenExisting` open succeeds under
**either** authority. It runs no versioned migration, writes no backup, does
not write `user_version`, and performs no DDL on the file (the DDL cookie
`PRAGMA schema_version` is unchanged; see R7). The authority is not consulted
and no migration log line is emitted.

**R3 — Pending (the #1119 decision).** In `pending`, `Deny` refuses with
`SchemaMigrationOptInRequired`; `Allow` backs up (#1180), runs
`relevant(p) ∩ (s, E]` in index order, and stamps per R4.

**R4 — Stamp.** After a fresh build or a pending migration the stamp is
`max(s, π_p(E))`. A stamp is **never lowered**. In `band` the stamp is not
written at all.

- `TachiFull`: always `E`, as today.
- `PortableKernel`: the portable high-water mark. It only moves when a
  Portable migration ships. A `PortableKernel` store stamped `39` by a pre-B
  kernel stays `39` forever under B (it lies in every later band until a
  Portable migration > 39 ships).

**R5 — Sentinels.** The migration runner iterates `relevant(p)` only.
`PortableKernel` stores stop receiving vacuous `Product` sentinels. Sentinels
already present on existing stores are tolerated and never removed.
`validate_current_schema_integrity` checks exactly the sentinels of
`relevant(p) ∩ [1, π_p(E)]`, and runs whenever `s ∈ band` (today it runs only
when `s == E`, `migrations.rs:353-355`, so a band store below `E` would go
unchecked).

**R6 — Scope assignment.**
1. **Shipped migrations (v1–v39):** `scope = Portable` iff the migration body
   executed on `PortableKernel` stores when it shipped (no profile guard);
   otherwise `Product`. This is mechanical, reproduces what every existing
   `PortableKernel` store already contains, and is immutable. It keeps
   v33–v36 `Portable` (owner sub-decision default; their author's intent is
   at `db/migrations/harness_session_attachments.rs:5`).
2. **New migrations (v40+):** `scope` is declared in the migration table and
   judged by the objects the migration touches, not the PR's intent: a
   migration that creates, alters, rewrites or drops any object a
   `PortableKernel` store contains (the `SchemaScope::Portable` chunks in
   `db/schema/ddl.rs:137-1614` and `:2050-2122`, plus every Portable-scoped
   migration's objects) is `Portable`. "Profile-neutral for convenience" is
   not allowed.
3. **Correction path:** a wrong scope is fixed by a new migration, never by
   editing a shipped scope. Changing the scope of a shipped migration changes
   `π_P` retroactively and makes stores at different stamps disagree on what
   "current" contains (§8, A5).

**R7 — DDL discipline.** A store in `band` must not be written by schema
maintenance. Therefore:
- Any new Portable table, column, index or trigger ships as a Portable-scoped
  versioned migration. The additive-base-chunk amendment rule
  (`ddl.rs:2128-2146`, which lets a `CREATE TABLE IF NOT EXISTS` chunk reach
  existing stores without a version bump) remains available for
  `SchemaScope::Product` chunks only.
- The existing un-versioned Portable maintenance in `init_schema_inner`
  (`ensure_column` calls, FTS/search-generation helpers, bridge helpers,
  `migrate_enum_constraints`; enumerated in §8, A6) is frozen: no additions.
  Each item must be idempotent on a converged store (no DDL; T8).
- Per-open **data** repairs (created_at/updated_at/revision backfills,
  `normalize_memory_validity_columns`, FTS backfill) are out of scope here;
  they are governed by #1987 W2-7 and do not change `user_version` or the
  DDL cookie.

**R8 — Frozen-rule amendment (#1119).** The #1119 decision is currently
"from `user_version` only, never from DB content" (`migrations.rs:386-418`).
It becomes: **from `user_version` and the write-once profile stamp**. The
profile stamp is identity, not content: it is write-once (#1585), protected
from set/delete (`db/state.rs`), and already decides admission. No other DB
content participates.

**R9 — #1180 amendment.** A "real migration" (which must back up) is exactly
the `pending` region: `1 ≤ s < π_p(E)`. `band` never backs up. This preserves
"every version-changing open is backed up" and removes backups of opens that
change nothing.

## 4. Scope table on the evidence SHA

| Scope | Versions | Basis |
|---|---|---|
| Portable | 1–11, 13, 19, 22–30, 33–36 | body runs on every profile today (`migrations.rs:586-816`) |
| Product | 12, 14–18, 20, 21, 31, 32, 37–39 | `if !profile.includes_product() { return Ok(0) }` guard |

`π_P(39) = 36`. Notes:
- v10/v11 drop retired tables that neither shape contains; they are
  `Portable` under R6.1 (ungated) and do not affect `π_P`.
- v5–v9, v13, v19, v24 are no-ops on a converged portable store; their scope
  is still `Portable`.
- No SQL foreign key crosses the Portable/Product boundary in either
  direction. v33 holds soft TEXT references (`work_claim_id`,
  `agent_identity_id`) to product identity concepts; that is allowed.

Implementation replaces the hand-written call chain (`migrations.rs:586-816`)
and `MIGRATION_SENTINEL_KEYS` (`:182-222`) with one table
`[(index, sentinel_key, scope, fn)]`; `E`, `π_P` and the sentinel list are
derived from it.

## 5. Open funnel order

B needs the stamped profile before the #1119 decision. On top of #1983's
preflight/authoritative split, the order becomes:

1. `check_schema_version_gate` — R1 (`newer`), header only.
2. `CreateFresh` with `s ≠ 0` → `DbCreateTargetExists` (profile-free,
   unchanged).
3. Read-only identity preflight: decode role and profile stamps
   (decode errors surface here), then **profile admission**
   (`resolve_profile`: `StoreProfileMismatch` / `StoreProfileNotExact` /
   `StoreProfileUnstamped`).
4. #1119 gate with the effective profile `p`: R2/R3.
5. `validate_current_schema_integrity(p)` — R5.
6. Role resolution (`StoreRoleConflict`) — read-only.
7. Backup, only in `pending` with `Allow` (R9).
8. Connection PRAGMAs, `BEGIN IMMEDIATE`, then re-evaluate 1–6 on the
   in-transaction state (#1983's `reevaluate_admission_in_tx`); only that
   evaluation decides.
9. `init_schema_inner(p)`, identity stamp, `relevant(p)` migrations, stamp
   per R4, validators, commit, marker.

**Precedence changes vs. today** (all are refusals trading places; no cell
changes from refuse to admit or vice versa except the band cells of §6):
- A profile refusal now precedes `SchemaMigrationOptInRequired` for a
  `pending` store with a mismatched profile (today the opt-in error wins).
- Role conflict stays after the version decision (step 6), as today.
- #1984's "Preconditions (funnel order)" must be updated to this order.

## 6. Truth table

Columns: stored profile stamp (`P` = PortableKernel, `F` = TachiFull, `—` =
none), region of `s` for the effective profile, intent, authority →
outcome. Requirement is assumed admitted by step 3 unless stated; admission
refusals are listed separately below. `E = 39`, `π_P = 36` for the example
values.

**OpenExisting**

| profile | `s` | region | `Deny` | `Allow` | change vs. today |
|---|---|---|---|---|---|
| any | 0 | fresh | build `p_req`, stamp `π_{p_req}(E)` | same | `P` fresh stamp becomes 36 (was 39) |
| any | 40 | newer | `newer` refused | same | none |
| `F` | 1–38 | pending | OptInRequired | backup, migrate all, stamp 39 | none |
| `F` | 39 | band | open, no writes | same | none |
| `—` (`s > 0`) | 1–38 | pending (as `F`) | OptInRequired | backup, migrate, stamp 39, profile `F` | none |
| `—` (`s > 0`) | 39 | band (as `F`) | open, stamp profile `F` | same | none |
| `P` | 1–35 | pending | OptInRequired | backup, migrate `relevant(P)`, stamp 36 | stamp 36 (was 39) |
| `P` | 36–38 | band | **open, no writes** | **open, no writes** | **was OptInRequired / backup+migrate+stamp 39** |
| `P` | 39 | band | open, no writes | same | none |

**CreateFresh**

| `s` | outcome | change |
|---|---|---|
| 0 | build `p_req`, stamp `π_{p_req}(E)` | `P` stamp 36 (was 39) |
| ≠ 0 | `DbCreateTargetExists` | none |

**Admission refusals** (step 3, independent of region; before the version
decision):

| requirement | stored profile | outcome |
|---|---|---|
| `AtLeast(P)` / `Exact(P)` | `—` (`s > 0`) | `StoreProfileUnstamped` |
| `AtLeast(F)` / `Exact(F)` | `P` | `StoreProfileMismatch` / `StoreProfileNotExact` |
| `Exact(P)` | `F` | `StoreProfileNotExact` |
| any | malformed stamp | decode error |

**Worked product-only bump** (`E = 40`, v40 `Product`, `π_P` stays 36):

| store | today | under B |
|---|---|---|
| `P`@39 (pre-B stamp) | Deny: OptInRequired; Allow: 390 MB backup + stamp 40 | band: open, no writes, stays 39; a 39-binary still opens it |
| `P`@36 | Deny refuses; Allow backs up, stamps 40 | band: open, no writes |
| `F`@39 | Deny refuses; Allow migrates | unchanged |

**Worked Portable bump** (`E = 41`, v41 `Portable`, `π_P = 41`): every `P`
store is `pending` — Deny refuses, Allow backs up, migrates, stamps 41. The
ceremony remains exactly where the portable shape actually changes.

## 7. Consumer contract

- memcore exports `PORTABLE_EXPECTED_SCHEMA_VERSION` and a read-only
  `store_version_status(path, requirement) -> Fresh | Current { stamp } |
  Pending { from, to } | Newer { stamp } | Refused(error)`. It opens the file
  read-only and writes nothing.
- Hypermem: `--schema-version` prints both `E` and `π_P`; a new
  `--check-store <path>` prints the status and exits with a distinct code per
  variant. `docker/deploy.sh` (today `:420-424`, exact integer equality in both
  directions) calls `--check-store` instead of comparing integers. Hyperion
  leaf; out of this repo.
- No consumer may re-implement the band in shell or by comparing
  `user_version` to a constant.

## 8. Appendix — blast radius (from the evidence SHA)

**A1. memcore gate sites that compare `s` with `E` directly and must use
`π_p`:**
- `db/migrations.rs:338-346` (R1, unchanged), `:352-384` (R5),
  `:419-465` (R2/R3; signature gains the profile), `:578-818` (runner, R5),
  `:182-222` (sentinel list → derived).
- `db/schema.rs:1203-1257` (funnel, §5), `:1236` (R4), `:3254-3312` (R9;
  backup predicate `(1..E).contains(s)` → `pending`).
- `store/open.rs:692-700` (older-stamp trigger relaxation), `:790-796`
  (`stored != E` in fresh identity-bound reopen), `:950-970` (read-only open
  sends a band store through the Deny gate as "older"), `:1025-1031`
  (exact-dedupe `stored != E`).
- `db/filename.rs:257-258` (`== 0 || > E` refuse): unchanged.

**A2. Readers outside the gate that compare with `E`:**
`tachi-server/src/doctor/schema_skew.rs:52-117` (false "behind" for band
stores), `bootstrap/migrate_cli.rs:224-227, 295-325, 451-457` (sweep would
try to migrate band stores), `bootstrap/manifest_cli.rs:118`,
`wiki_corpus/fs.rs:679-685` (global/wiki stores are `TachiFull`; verify).
`memory-server-runtime/src/lib.rs:1304-1308` is dead at `E = 39`. All move to
`store_version_status`.

**A3. Hyperion (separate leaf after intake):** `hypermem/src/lib.rs:26`,
`main.rs:15-19`, `migration.rs:305-324` (legacy import window `[20, E)`
would accept a band source as legacy), `docker/deploy.sh:320-331, 420-424`,
`deploy_contract_selftest.sh`.

**A4. Tests that encode today's coupling and flip under B** (each flip must be
listed in the implementation PR with before/after):
- `db/migrations/current_truth.rs:181-264` — Deny on `P`@36/37 must now
  **open without writes**; stamp assertions `== E` become `== 36` for fresh
  `P` stores. (This file is also touched by #1984.)
- `db/migrations/verified_admissions.rs:46`, `current_truth.rs:37`,
  `mirror_eval_identity.rs:113` — Product migrations are no longer called for
  `P`, so the `Ok(0)` assertions move to "not in `relevant(P)`".
- `store/profile_identity_tests.rs:264-268` (`P` fresh `user_version == E`),
  `:928-957` (profile-invariant sentinel set; `user_version(P) == E`).
- `portable-server/src/main.rs:178-237, 264-307, 358-376` — `E − 1 = 38` is in
  the `P` band; fixtures must use `π_P − 1`.
- `db/migrations.rs:2180-2294` gate decision tests gain the profile axis.
- Doc comments claiming "the sentinel set is profile-invariant" (v12, v14–v18,
  v20, v21, v32, v39 modules) are rewritten.
- `store/profile_identity_tests.rs:110-126` `KERNEL_TABLES` is missing the
  Portable tables `memory_outbox_*`, `harness_session_*`, `delivery_*`,
  `memory_search_generation`; add them (R6 relies on this list being true).

**A5. Why shipped scopes are immutable.** If v33–v36 were reclassified
`Product`, `π_P(39)` would drop to 30. `P` stores stamped 36–39 contain the
v33–v36 tables; `P` stores freshly built afterwards would not; both would be
"current". The unconditional validators (`migrations.rs:369-371`,
`schema.rs:1238-1244`) would then reject one population or the other.

**A6. Frozen un-versioned Portable maintenance** (`schema.rs:1435-1611`):
`recall_cache.generation_fingerprint`; memories `archived`, `created_at`,
`updated_at`, `scored_count`, `revision`, `valid_from`, `valid_until`,
`retention_policy`, `domain`, `superseded_by`, `idless_identity` (+ index),
`recall_count`, `query_diversity`, `tier`, `last_use_at`;
`access_history.query_hash`, `event_kind`; `memory_edges.valid_from`,
`valid_to`; `derived_items.summary`, `importance`, `scope`, `created_at`;
`bridge_hypertachi_memory_columns`; the v8/v9 bodies re-run at `:1589-1591`;
`ensure_search_generation_schema`; `ensure_fts_backfilled`;
`migrate_enum_constraints`; `ensure_optimization_indexes`; the Portable
`MIGRATED_INDEXES_CHUNKS`. Product-side un-versioned DDL
(`hub_capabilities`, `vault_entries`, `exec_envs`, `session_claims`, the
inline `exec_env_worktree_identities` table at `:1669-1676`) is not affected
by B.

## 9. Tests (acceptance for the implementation PR)

- **T1 Classification.** For every prefix `i` of the migration table: a
  `P`-shaped store that runs the Portable migrations `≤ i` passes the `P`
  validators; running each `Product` migration `≤ i` against it with the `P`
  profile leaves `sqlite_schema` byte-identical. A new migration that
  violates its declared scope fails T1.
- **T2 Product-only bump.** With a test-only extra `Product` migration at
  `E + 1`: a `P` store at 39 opens under `Deny`; no `.migration-bak`; marker,
  `user_version` and `PRAGMA schema_version` unchanged. An `F` store in the
  same scenario refuses under `Deny`.
- **T3 Portable bump.** With a test-only extra `Portable` migration at
  `E + 1`: a `P` store refuses under `Deny`; under `Allow` it backs up and is
  stamped `E + 1`.
- **T4 Downgrade.** `s = E + 1` refuses for `P`, `F`, `—`, before any identity
  decode (a deliberately malformed profile stamp must not change the error).
- **T5 TachiFull invariance.** The full existing `F` suite, including #1984's
  admission tables, passes with no assertion changes.
- **T6 Router shape.** A copy of a v28 `P` store: intake (Allow) → stamp
  `π_P`; then a product-only bump binary → restart under `Deny` succeeds and
  the directory has no new backup. (Uses a synthetic store unless the owner
  provides the router copy.)
- **T7 Truth table.** One test per cell of §6, including the precedence
  changes of §5.
- **T8 Idempotent maintenance.** Reopening a converged `P` store and a
  converged `F` store leaves `PRAGMA schema_version` unchanged.
- **T9 Private image.** A sealed private partition image stamped at `π_P`
  opens under `Deny` after a product-only bump (today it would refuse,
  `store/open.rs:559-577`).

## 10. Open items

- **Router profile stamp.** #1585 D2 landed at v28; the router's v28 store may
  carry no profile stamp. If so, every `AtLeast(P)`/`Exact(P)` open refuses
  with `StoreProfileUnstamped`, and intake (#1987 item 6) needs an operator
  stamp first. Check on the router before intake.
- **Ordering with #1983/#1984.** Implementation starts after both land; they
  restructure the same funnel and `current_truth.rs`.
- **Same-value `user_version` writes.** Whether `PRAGMA user_version = <same>`
  dirties the file is unverified; R2 avoids the write regardless.
- **Validator asymmetry.** `migrations.rs:369-371` validates v36;
  `schema.rs:1238-1244` does not. Align during implementation.
