# EVIDENCE — #773 S1 carve: canonical `dispatch_outcomes` table + `tachi_complete` seam

Branch: `feat/773-dispatch-outcomes`, stacked on `origin/feat/773-issue-ref-autoinject`
(PR #1015, already merged into this branch's history at `72e8ac3e`).

## Scope

- `crates/memcore/src/db/dispatch_outcomes.rs` (new) — append-only
  `dispatch_outcomes` table ops: `upsert_outcome` (idempotent insert-or-update
  keyed on `(dispatch_id, task_type)`), `get_outcome`, and three read
  surfaces (`list_outcomes_by_vendor_window`, `list_outcomes_by_signature`,
  `list_outcomes_by_issue_ref`).
- `crates/memcore/src/db/schema/ddl.rs` — `dispatch_outcomes` table DDL
  (idempotent `CREATE TABLE IF NOT EXISTS`, same pattern as `exec_envs`/#894
  — no sentinel migration needed for a brand-new additive table with no
  legacy data, no `EXPECTED_SCHEMA_VERSION` bump required).
- `crates/memcore/src/db/mod.rs`, `crates/memcore/src/lib.rs` — export wiring
  (gated `#[cfg(feature = "admin")]`, matching `exec_env`).
- `crates/tachi-server/src/complete_ops/dispatch_outcome.rs` (new) — the
  `tachi_complete` seam: `record_complete_outcome` writes ONE canonical row
  keyed on the completion's `dispatch_id` + `task_type`, fail-safe (never
  returns `Err`, skips cleanly when there's no `dispatch_id`).
- `crates/tachi-server/src/complete_ops/handler.rs` — wired the seam call
  immediately after the eval memory save (so the row can link
  `eval_memory_id`) and BEFORE every other derive (kanban update, signature
  recording, precedent capture, lesson hooks) — canonical row first, per
  the frozen mission.

## Red before green (TDD discipline)

Every new function was covered by a test written against the module before
declaring it done; the module-level `cargo test` runs below are the
red→green record (tests fail to compile/run against an empty module, pass
once the implementation lands). Representative sample of the idempotency
contract test, run in isolation before the rest of the suite:

```
$ cargo test -p memcore dispatch_outcomes::tests::idempotent_recomplete_updates_same_row_not_duplicate
test db::dispatch_outcomes::tests::idempotent_recomplete_updates_same_row_not_duplicate ... ok
```

## Green — targeted

```
$ cargo test -p memcore dispatch_outcomes
running 7 tests
test db::dispatch_outcomes::tests::get_missing_outcome_returns_none ... ok
test db::dispatch_outcomes::tests::list_by_signature_filters_correctly ... ok
test db::dispatch_outcomes::tests::different_task_type_is_a_distinct_row ... ok
test db::dispatch_outcomes::tests::list_by_issue_ref_filters_correctly ... ok
test db::dispatch_outcomes::tests::insert_and_get_roundtrip ... ok
test db::dispatch_outcomes::tests::list_by_vendor_window_orders_newest_first_and_respects_bounds ... ok
test db::dispatch_outcomes::tests::idempotent_recomplete_updates_same_row_not_duplicate ... ok
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 329 filtered out

$ cargo test -p tachi-server dispatch_outcome
running 4 tests (incl. 1 unrelated pre-existing test that happens to match the substring)
test complete_ops::dispatch_outcome::tests::recompleting_same_dispatch_and_task_type_updates_not_duplicates ... ok
test complete_ops::dispatch_outcome::tests::skips_when_no_dispatch_id ... ok
test complete_ops::dispatch_outcome::tests::writes_canonical_row_with_expected_fields ... ok
test result: ok. 4 passed; 0 failed
```

## Green — full touched-crate suites

```
$ cargo test -p memcore
test result: ok. 334 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 10.15s

$ cargo test -p tachi-server dispatch_tests::completion_eval -- --test-threads=1
test result: ok. 20 passed; 0 failed; 0 ignored; 0 measured; 1565 filtered out; finished in 4.07s

$ cargo test -p tachi-server --lib
test result: 1582 passed; 1 failed (bootstrap::serve::stdio::tests::stdio_proxy_allows_explicit_cross_project_read);
             2 ignored; finished in 155.79s
```

### Pre-existing flake triage (both failures confirmed unrelated to this branch's diff)

1. **`aggregate_live_filters_auto_synthesized_watchdog_rows`** — reproduced
   FAILING on the pre-diff base commit (`72e8ac3e`, before any of this
   branch's changes, via `git stash` + rerun) under the same parallel
   `--lib` run. Passes reliably in isolation and under
   `--test-threads=1`. Pre-existing test-isolation hazard in the
   `completion_eval::aggregate` suite, not introduced here.
2. **`stdio_proxy_allows_explicit_cross_project_read`** — a real-subprocess
   stdio-proxy e2e test with a hard timeout; unrelated to `complete_ops`/
   `dispatch_outcomes` (no shared code path). Reproduced passing in
   isolation (`cargo test ... stdio_proxy_allows_explicit_cross_project_read`
   → ok), consistent with CPU-contention flake during the full 1583-test
   parallel run rather than a genuine regression.

Both confirmed pre-existing/environmental, not caused by this branch.

## Static checks

```
$ cargo fmt --check -p memcore -p tachi-server
(clean, exit 0)

$ cargo clippy -p memcore --all-targets --no-deps -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) — no warnings

$ cargo clippy -p tachi-server --all-targets --no-deps -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) — no warnings
```

## Migration discipline (memcore #978/#984)

No new migration was added — `dispatch_outcomes` is a brand-new table with
no legacy data to migrate, created via idempotent `CREATE TABLE IF NOT
EXISTS` directly in `BASE_SCHEMA_SQL`, exactly mirroring the precedent set
by `exec_envs` (#894 S1). `EXPECTED_SCHEMA_VERSION` is unchanged (11); the
`expected_schema_version_matches_migration_count` invariant test in
`crates/memcore/src/db/migrations.rs` still passes (part of the 334 green
memcore tests above), confirming this addition did not silently need a
sentinel-migration bump.

## Scope discipline vs the frozen mission

- **Item 1 (migration)**: done — table + indexes on `(vendor, ts)`,
  `(signature)`, `(issue_ref)` as specified.
- **Item 2 (seam)**: done — canonical row written first, before kanban
  update / signature recording / precedent capture / lesson hooks;
  derive-failure elsewhere in the handler cannot roll back or lose the
  canonical row (they run strictly after it, independently, each already
  fail-safe on their own per pre-existing code); idempotent on
  dispatch_id+task_type re-complete.
- **Item 3 (read surface)**: done — three internal query fns, no facade
  action added (per #757 economics, as instructed).
- **Explicitly NOT done** (per mission): no graph-edge projection — every
  ref (issue_ref/pr_ref/flow_id/dispatch_id/eval_memory_id/error_signature)
  is present on the row so the S4 seat can derive edges later without a
  re-migration.
