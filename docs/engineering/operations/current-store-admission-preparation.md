# #1995 admission preparation on main v39

Source base: `7873b720993cc10519db6232863f4db965c657f2`.
This is a test/inventory handoff, not a production fix or a replacement spec.
The owning decisions remain [current-store-admission.md](../architecture/current-store-admission.md)
revision 7 and [portable-version-policy.md](../architecture/portable-version-policy.md)
revision 6. Their historical line numbers must not be used as current anchors.

## Current call path and mutation inventory

Paths below are under `crates/memcore/src/`. The three classes describe
effects, not permission: **D** is disposable/derived, **S** is persisted state
or an integrity rule, **W** is a step that can rewrite existing state.
Being D does not bypass an existing validator. The frozen allowlist and its
exceptions are in the owning spec §3, not inferred from this classification.

The generic writer runs `store/open.rs::open_with_label_inner_while_startup_owned`
→ input trigger inventory → `db/schema.rs::init_store_schema_with_label_mut`
→ preflight → backup/PRAGMAs → `BEGIN IMMEDIATE` → authoritative admission
→ `init_schema_inner` → identity stamps → sentinel migrations → validators
→ commit → marker → optional vec provisioning. Neither input admission
currently checks the complete required-object set.

Every direct stage of `init_schema_inner`, in execution order:

| Stage / current symbol | Scope | Class / affected objects |
|---|---|---|
| `execute_schema_chunks(BASE_SCHEMA_CHUNKS)` | chunk-tagged P/F | All base table/index definitions in `db/schema/ddl.rs`; ordinary tables S, FTS/cache D, non-unique indexes D, unique constraints S. This is not the entire final baseline: later migrations and inline installers add objects. |
| `ensure_column(recall_cache, generation_fingerprint)` | P | D; empty default invalidates old cached generations. |
| Portable `ensure_column` calls, including `ensure_memories_scored_count` | P | W if missing: `memories.{archived,created_at,updated_at,scored_count,revision,valid_from,valid_until,retention_policy,domain,superseded_by,idless_identity,recall_count,query_diversity,tier,last_use_at}`, `access_history.{query_hash,event_kind}`, `memory_edges.{valid_from,valid_to}`, `derived_items.{summary,importance,scope,created_at}`. Required columns remain required even when the default is NULL. |
| Inline `idx_memories_idless_identity_active` | P | D per frozen exception; no pre-index dedupe. Duplicate active identities make the transaction fail rather than being rewritten. |
| `init_product_schema_columns` | F | W if missing: `hub_capabilities.{review_status,health_status,last_error,last_success_at,last_failure_at,fail_streak,active_version,exposure_mode}`, `vault_entries.allowed_agents`, `exec_envs.{agent_identity_id,claim_id}`, `session_claims.mode`. In particular `review_status` defaults to `approved`. |
| Inline `exec_env_worktree_identities` | F | S; newly recreated empty. Lost identities cannot be recovered from surviving `exec_envs` leases. |
| `dedupe_session_claims_identity_conflicts` | F | W; releases older duplicate active modeless claims. Guarded by table/column presence, not migration authority. |
| `execute_schema_chunks(MIGRATED_INDEXES_CHUNKS)` | chunk-tagged P/F | Non-unique indexes D; `idx_session_claims_identity_active` is W-associated because the dedupe already ran. Definitions include keys, uniqueness and partial predicates. |
| Three inline `UPDATE memories` statements | P | W; `created_at ← timestamp`, `updated_at ← created_at`, invalid `revision ← 1`. Missing-schema admission must not replace the row-normalization contract. |
| `normalize_memory_validity_columns` | P | W; derives missing `valid_from` from timestamp, normalizes validity strings inside a savepoint. |
| `bridge_hypertachi_memory_columns` | P | W; ensures `domain`, then bridges `indexed_tags` into keywords, `domain_key` into domain, and the old mistaken domain-like `location` into domain when those columns exist. |
| `fold_and_drop_legacy_persons_column`, `migrate_v9_relocate_and_drop_location` | P | W, legacy-gated; folds data into metadata before dropping old columns. Not exercised by the new fixtures. |
| First `ensure_search_generation_schema` | P | S/W; creates `memory_search_generation`, seeds its row and installs/validates nine triggers on memories/edges/access history; can normalize the accepted previous update-trigger definition. |
| `ensure_fts_backfilled` | P | D + W on generation; deletes projection orphans, inserts missing rows in both FTS projections, bumps generation iff rows change. Projection fidelity remains #2001, not certified here. |
| `migrate_enum_constraints` | P | W, shape-gated; normalizes source/category/scope/retention, backfills retention defaults and rebuilds memories/its indexes. Can drop attached triggers; fresh vs converged paths need separate inventories. |
| Second `ensure_search_generation_schema` | P | S/W; reinstalls and validates generation authority after the possible memories rebuild. |
| `ensure_optimization_indexes` | P | D; may create `idx_memories_path_active_ts`; its own errors are discarded today. Absence and malformed-present shape have different D7 outcomes. |

P means shared Portable initialization; F means Full-only initialization.
This stage list also supplies D7 §8 A6's maintenance boundary; it does not
extend that frozen list. Adjacent operations outside `init_schema_inner`:

- The version runner records sentinels and can run any missing migration,
  including lower-numbered ones. A healthy current store is not proof that
  a sentinel-only API output has all initializer objects. The public
  migration-only API and its frozen tests remain unchanged.
- Identity adoption writes `hard_state` rows, in the schema transaction.
- Backup/retention and WAL-mode PRAGMA happen before that transaction;
  the marker is written after commit.
- `db/sqlite_vec.rs::try_load_sqlite_vec` creates `memories_vec` after schema
  commit. It contains provider-derived embeddings (S), but the spec explicitly
  permits optional empty reprovisioning. Its shadow objects travel with it.
- The generic file, private-image, read-only, maintenance, public initializer
  and fresh-reopen doors need distinct checks as listed in spec §4. The tests
  in this slice cover only the generic writer and healthy profile enumeration.

## Executable inventory and refusal probes

`store/current_store_admission_tests.rs` creates real temporary stores through
`MemoryStore::open_with_context`; it does not mock admission or DDL.

- `enumerate_fresh_and_reopened_schema_for_both_profiles` enumerates **all**
  `sqlite_schema` tuples, table columns through `table_xinfo`, and index flags
  through `index_list` for each profile. Autoindexes and virtual-table shadow
  objects are retained. SQL records constraints, expressions and partial
  predicates. It compares fresh and converged inventories and emits JSON.
  This prepares the shared D7 baseline; it is **not** a version-keyed golden
  or a classified allowlist, and does not satisfy T3/T9 by itself.
- `healthy_current_reopen_preserves_populated_full_store` checks a populated
  claim/worktree fixture, all logical table rows, schema, both version PRAGMAs,
  journal mode and migration marker/backup contents.
- The two `#[ignore]` refusal probes deliberately assert the desired
  `CurrentSchemaIncomplete` result and preservation, not the old silent repair.
  One removes the claim identity index and seeds duplicate active `mode IS NULL`
  claims with NULL `flow_id`; the other seeds a lease/identity then drops the
  identity table. Each defines Deny and Allow cases on separate fresh stores.
  A red assertion stops that test at its first failure, so a Deny red run does
  not establish that its subsequent Allow iteration executed.
- Snapshots are taken **after the deliberate damage**. The contract is no
  further repair, not resurrection of already dropped rows. Pre-existing
  markers/backups are compared rather than pretending fresh initialization
  never created them. Logical equality does not assert byte-identical DB/WAL
  files; SQLite can checkpoint or create transient sidecars on connection close.

Focused commands (use a separately owned target directory):

```sh
cargo test --locked -p memcore --lib current_store_admission_tests
cargo test --locked -p memcore --lib current_store_admission_tests -- --ignored --nocapture
cargo test --locked -p memcore --lib enumerate_fresh_and_reopened_schema_for_both_profiles -- --nocapture
```

Before the runtime fix, the first command should exercise controls while the
second is intentionally red. This is preparation, not a reason to register
these failures as accepted production behavior or weaken the assertions.

## Refusal side-effect contract

| Condition | Required result and side effects |
|---|---|
| Required object already missing at preflight, current store, Deny **or** Allow | Typed refusal naming absent objects, after existing integrity errors but before identity; no repair, new backup, retention deletion, WAL-mode change, stamp or marker rewrite. |
| Required object disappears after preflight | In-transaction refusal before initializer writes; logical DB changes roll back. Compare against post-damage state. Existing #1983 backup/retention/WAL effects may remain: this is **not** filesystem zero side effects. |
| Derived allowlisted object absent | Preserve the spec's rebuild behavior; existing validators retain their errors and precedence. Do not grant blanket repair authority. |
| Missing object plus an earlier trigger/integrity defect | Existing winning error remains. Missing-object refusal only supersedes the later identity failure documented in spec T5. |
| Pending or fresh store | Preserve existing migration/provisioning rules; do not apply current-store presence requirements prematurely. |

## Verification and remaining work

On this host, the initial offline locked memcore test build failed before
compilation: `libsimple 0.9.0` is not cached. The new controls, ignored red
probes and runtime object inventory have **not** executed here. The issue's
2026-09-26 DGX2 receipts are historical evidence only, not results at this base.
No schema initializer, migration, dependency, live store or host service is
changed by this slice.

The eventual runtime delivery still owes spec T1–T9, the typed error, every
covered door, existing-error precedence, enumerated per-profile damage tests,
the version-keyed required inventory and discrimination mutations, plus the
explicitly authorized live-copy probe before merge. Do not close #1995 from
these preparation tests or describe missing dependencies as PASS.
