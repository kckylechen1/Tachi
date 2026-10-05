# #1995 admission preparation on main v39

Continuation base: `748af28529b97fa30a6eb029a9004d905b206888`
(PR #2027 head `38810246c36d23ed7cd44234c47f749ef2de7327` plus main
`c39274edce5f053dc6f30781ac20af4c8215a7be`).
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
| First Portable `ensure_column` group, including `ensure_memories_scored_count` | P | W if missing: `memories.{archived,created_at,updated_at,scored_count,revision,valid_from,valid_until,retention_policy,domain,superseded_by,idless_identity}`. Required columns remain required even when the default is NULL. |
| Inline `idx_memories_idless_identity_active` | P | D per frozen exception; no pre-index dedupe. Duplicate active identities make the transaction fail rather than being rewritten. |
| Remaining Portable `ensure_column` group | P | W if missing: `memories.{recall_count,query_diversity,tier,last_use_at}`, `access_history.{query_hash,event_kind}`, `memory_edges.{valid_from,valid_to}`, `derived_items.{summary,importance,scope,created_at}`. |
| `init_product_schema_columns` | F | W if missing: `hub_capabilities.{review_status,health_status,last_error,last_success_at,last_failure_at,fail_streak,active_version,exposure_mode}`, `vault_entries.allowed_agents`, `exec_envs.{agent_identity_id,claim_id}`, `session_claims.mode`. In particular `review_status` defaults to `approved`. |
| Inline `exec_env_worktree_identities` | F | S; newly recreated empty. Lost identities cannot be recovered from surviving `exec_envs` leases. |
| `dedupe_session_claims_identity_conflicts` | F | W; releases older duplicate active modeless claims. Guarded by table/column presence, not migration authority. |
| `execute_schema_chunks(MIGRATED_INDEXES_CHUNKS)` | chunk-tagged P/F | Non-unique indexes D; `idx_session_claims_identity_active` is W-associated because the dedupe already ran. Definitions include keys, uniqueness and partial predicates. |
| Three inline `UPDATE memories` statements | P | W; `created_at ← timestamp`, `updated_at ← created_at`, invalid `revision ← 1`. Missing-schema admission must not replace the row-normalization contract. |
| `normalize_memory_validity_columns` | P | W; derives missing `valid_from` from timestamp, normalizes validity strings inside a savepoint. |
| `bridge_hypertachi_memory_columns` | P | W; ensures `domain`, then bridges `indexed_tags` into keywords, `domain_key` into domain, and the old mistaken domain-like `location` into domain when those columns exist. |
| `fold_and_drop_legacy_persons_column`, `migrate_v9_relocate_and_drop_location` | P | W, legacy-gated; folds persons into entities and relocates location into path/metadata before dropping old columns. Not exercised by the new fixtures. |
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

- `db/schema/inventory.rs` supplies one test-only private capture and
  classifier shared by the admission controls and future D7 tests. Raw
  snapshots retain every `sqlite_schema` tuple, autoindex and shadow object,
  `table_xinfo`, `index_list`, and `index_xinfo`; SQL preserves constraints,
  expressions and partial predicates. The populated/refusal snapshots use
  this same unfiltered capture.
- `enumerate_fresh_and_reopened_schema_for_both_profiles` initializes each
  profile through the real funnel, captures it fresh and after a current-store
  reopen, and compares both with `db/schema/goldens/required-v39.json`.
  The golden keys the required object shapes and classified membership by
  expected version and effective profile. v39 is its initial baseline;
  future required growth must add a migration/version entry rather than
  regenerate this entry to accept drift. Existing v28 DDL goldens are unchanged.
- The classification is default-deny for tables, views and triggers. Named
  nonunique indexes are classified by removing each one from an independent
  scratch backup image and invoking the existing integrity validator; its
  requirements are not copied into another name list. Canonical v39 has
  16 such required indexes in Portable and 23 in Full. Diagnostics are emitted
  as evidence, never frozen in the golden. The capture connection is untouched.
  Unique indexes remain required except the frozen idless exception; the
  claim-identity index remains required.
- Canonical capture classified all 132 Portable and 327 Full objects:
  respectively 52/120 required, 36/101 derived and 44/106 dependent objects.
  Dependent autoindexes retain table ownership and constraint shape, and
  virtual shadows retain family ownership in raw capture. SQLite's native
  `pragma_table_list` shadow type identifies FTS family members. The pinned
  one-vector-column vec0 module does not report every shadow through that API;
  its exact four generated table names are recognized only when the declared
  `memories_vec` virtual root exists. An arbitrary FTS/vec prefix is not an
  exemption. Optional vec membership and the allowed optimization-index
  absence are normalized only in the frozen classification projection, so
  optional provisioning cannot change the required golden.
- `inventory_growth_discriminators_run_in_real_initializer` uses a cfg(test)
  seam immediately before the real enum rebuild. Its RAII guard is scoped to
  the current thread and owned fixture path. For both profiles it exercises:
  a memories column dropped by the fresh rebuild but retained on converged
  reopen; an unversioned column elsewhere retained in both but rejected by
  the golden; and an inline table retained in both but rejected by the golden.
  The inline table deliberately has an FTS-like prefix and remains required.
  Each case asserts two actual seam invocations and releases its guard.
- `optional_family_absence_preserves_required_inventory` removes optional
  vec and the allowed optimization index from each owned profile fixture;
  required shapes and normalized classification must remain identical.
- `healthy_current_reopen_preserves_populated_full_store` checks a populated
  claim/worktree fixture, all logical table rows, schema, both version PRAGMAs,
  journal mode and migration marker/backup contents. It first converges the
  fresh fixture through one ordinary reopen: fresh init writes its marker
  before optional vec provisioning advances the schema cookie, so the first
  reopen can legitimately take a fallback backup and align the marker. The
  unchanged-artifact assertion starts only after that convergence.
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

Focused commands (acquire exclusive ownership of the host-approved Rust target
before running; these are scratch fixtures only):

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

The continuation's focused admission preparation run executed four tests:
healthy populated preservation, P/F fresh-reopened golden comparison,
real-initializer growth discrimination, and optional-family absence. All four
passed; the two existing refusal probes remain ignored. The six growth cases
reported both initializer invocations. Initial seed collection deliberately
failed against an empty golden; that seed receipt is not a passing test.
Focused receipts are local handoff evidence, not CI acceptance for a later head.

Production admission, migrations, dependencies, live stores and services are
unchanged. The only initializer addition is the cfg(test) fixture-scoped seam;
there is no new runtime inventory API. No trigger-DDL injection or census
exemption is introduced. The known-red probes retain their refusal and
preservation assertions; a Deny failure still does not prove Allow ran.

This slice establishes the shared version-keyed inventory and T9 preparation
checks. It does not complete T3's production damage/refusal behavior or the
rest of admission. The eventual runtime delivery still owes the typed error,
every covered door, existing-error precedence, per-profile damage tests,
in-transaction rechecks, and the explicitly authorized live-copy probe before
merge. Do not close #1995 or D7 from this preparation delivery.
