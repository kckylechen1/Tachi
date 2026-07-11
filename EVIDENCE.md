# SQLite/recall/coldpath perf pack — evidence log

Branch: `feat/perf-sqlite-pack`. Each item below is opus-xhigh-adjudicated
DO-NOW, independently proved, zero correctness risk. Real production data
used for evidence: a read-only copy of the live `~/.tachi/global/memory.db`
(474 memories, 15,608 `access_history` rows, 152 `hard_state` rows) copied
into `.perf-evidence/memory_readonly_copy.db` inside this worktree — never
the live daemon's file.

---

## Item 1 — Read-pool pragmas (`crates/memcore/src/db/open.rs`)

**Before**: `configure_connection` (called by both `open_read_write` and
`open_read_only`) set only `busy_timeout`. The `cache_size = -16000` value
was already computed and applied on the writer connection (`schema/ddl.rs`
`CONNECTION_PRAGMA_SQL`) but never reached read-only handles opened via
`open_read_only` — every read pool connection ran with SQLite's tiny default
page cache (~2MB) instead of the already-chosen 16MB budget.

**Change**: added `configure_read_only_connection` (called only from
`open_read_only`), setting:
- `PRAGMA cache_size = -16000` (16 MB page cache — matches the writer value)
- `PRAGMA mmap_size = 268435456` (256 MB mmap window)

Both PRAGMAs are legal on read-only handles — they configure this
connection's local cache/mmap window, not the DB file. `journal_mode` and
`foreign_keys` were deliberately NOT added to the read path per the task
scope (writer/schema concerns).

**File:line**: `crates/memcore/src/db/open.rs:34-38` (`open_read_only`),
new fn `configure_read_only_connection` at `crates/memcore/src/db/open.rs`
(added below `configure_connection`).

**Verification**: `cargo test -p memcore --lib` — 328 passed, 0 failed (no
existing test asserted on read-connection PRAGMA state, so this is a
behavior-additive, non-breaking change; covered by every existing
`open_read_only`-exercising test continuing to pass).

---

## Item 2 — `hard_state` index (migration v12)

**Before** (real DB, `orchestrator` namespace, 127 rows):

```
sqlite> EXPLAIN QUERY PLAN
   ...> SELECT key, value_json, version, updated_at FROM hard_state
   ...> WHERE namespace = 'orchestrator' ORDER BY updated_at DESC, key ASC;
QUERY PLAN
|--SEARCH hard_state USING INDEX sqlite_autoindex_hard_state_1 (namespace=?)
`--USE TEMP B-TREE FOR ORDER BY
```

Full temp-B-tree sort of every namespace-matched row.

**After** (same DB, index applied):

```sql
CREATE INDEX IF NOT EXISTS idx_hard_state_ns_updated
  ON hard_state(namespace, updated_at DESC);
```

```
QUERY PLAN
|--SEARCH hard_state USING INDEX idx_hard_state_ns_updated (namespace=?)
`--USE TEMP B-TREE FOR LAST TERM OF ORDER BY
```

The index satisfies `WHERE namespace = ?` AND the primary `ORDER BY
updated_at DESC` term directly; only the secondary `key ASC` tiebreak
(among rows sharing an identical `updated_at` — rare) needs any sort, per
opus's "collapses to LAST TERM OF ORDER BY" characterization. Confirmed
verbatim on the live dataset, not a synthetic fixture.

**Implementation**: sentinel-gated migration `v12_hard_state_ns_updated_index`
in `crates/memcore/src/db/migrations/hard_state_index.rs`
(`migrate_v12_add_hard_state_index`), wired into
`run_data_migrations_in_tx` in `crates/memcore/src/db/migrations.rs`
following the exact v10/v11 discipline (#978/#984): `apply_versioned_migration`
gate, `MigrationReport.hard_state_index_added` field,
`EXPECTED_SCHEMA_VERSION` bumped 11 → 12, `ALL_MIGRATION_SENTINEL_KEYS` test
fixture updated, module doc comment v12 line added.

**File:line**:
- `crates/memcore/src/db/migrations/hard_state_index.rs:1-30` (new file)
- `crates/memcore/src/db/migrations.rs:64` (`EXPECTED_SCHEMA_VERSION = 12`)
- `crates/memcore/src/db/migrations.rs` (`run_data_migrations_in_tx`, new
  `hard_state_index_added` migration call after v11)

**Verification**:
- New unit test `v12_adds_hard_state_namespace_updated_index` in
  `crates/memcore/src/db/migrations.rs` — creates the index via the real
  migration runner, asserts `EXPLAIN QUERY PLAN` uses
  `idx_hard_state_ns_updated` and does NOT fall back to a full
  `"USE TEMP B-TREE FOR ORDER BY"`, and asserts idempotency (second run
  reports 0 added, index still present).
- `expected_schema_version_matches_migration_count` (pre-existing #984 F3(e)
  invariant test) passes unmodified — confirms the sentinel count the
  runner actually writes (12) matches the bumped `EXPECTED_SCHEMA_VERSION`.
- `cargo test -p memcore --lib` — 329 passed (328 + 1 new), 0 failed.
- Real-DB EXPLAIN QUERY PLAN before/after captured directly above via
  `sqlite3` CLI against the read-only production-data copy.

---

## Build/lint gates (items 1+2, memcore)

```
$ cargo fmt -p memcore -- --check      # clean
$ cargo clippy -p memcore --all-targets --no-deps -- -D warnings   # clean
$ cargo test -p memcore --lib          # 328 passed, 0 failed, 2 ignored (baseline, unrelated)
```

---

## Item 3 — ANALYZE/optimize (planner mis-pick root cause)

**Before** (real DB, no `sqlite_stat1` ever populated — this DB predates
any `ANALYZE`):

```
sqlite> SELECT COUNT(*) FROM sqlite_stat1;
Error: no such table: sqlite_stat1

sqlite> EXPLAIN QUERY PLAN
   ...> SELECT id FROM memories WHERE archived = 0 AND path = '/facts/readonly'
   ...> ORDER BY timestamp DESC LIMIT 20;
QUERY PLAN
|--SEARCH memories USING INDEX idx_memories_archived (archived=?)
`--USE TEMP B-TREE FOR ORDER BY
```

Without planner statistics, SQLite falls back to structural heuristics and
picks `idx_memories_archived` — a boolean column, 237/474 rows match
(barely better than a full scan) — over the much more selective
`idx_memories_path` (2 rows/path on average) or the purpose-built partial
index `idx_memories_path_active_ts ON memories(path, timestamp DESC) WHERE
archived = 0 AND superseded_by IS NULL`, confirming opus's "planner
mis-picking idx_memories_archived" diagnosis on live data.

**After** (`PRAGMA optimize;` run once):

```
sqlite> SELECT COUNT(*) FROM sqlite_stat1;
74
sqlite> SELECT * FROM sqlite_stat1 WHERE tbl='memories';
memories|idx_memories_path_active_ts|370 2 1
memories|idx_memories_archived|474 237
memories|idx_memories_path|474 2
... (11 more rows, one per index)

sqlite> EXPLAIN QUERY PLAN
   ...> SELECT id FROM memories WHERE archived = 0 AND path = '/facts/readonly'
   ...> ORDER BY timestamp DESC LIMIT 20;
QUERY PLAN
|--SEARCH memories USING INDEX idx_memories_path (path=?)
`--USE TEMP B-TREE FOR ORDER BY
```

With `sqlite_stat1` populated, the planner switches off the low-selectivity
`idx_memories_archived` to the far more selective `idx_memories_path`.

**Implementation**: `MemoryStore::run_optimize()` (new method,
`crates/memcore/src/store/crud.rs`, next to the existing
`checkpoint_wal_truncate`) runs `PRAGMA optimize;` — SQLite's own built-in
heuristic for "ANALYZE only the tables likely to have stale stats," safe
and cheap to call often per SQLite's own docs. Wired into the same periodic
background loop that already runs WAL-checkpoint maintenance
(`crates/tachi-server/src/bootstrap/serve/background.rs`
`spawn_wal_checkpoint`, cadence `TACHI_WAL_CHECKPOINT_SECS`, default 300s)
for the global store, project store, and every named-project store — no
new timer, no new config knob, reuses the existing "quiet moment" cadence.

**File:line**:
- `crates/memcore/src/store/crud.rs` (`run_optimize`, added after
  `checkpoint_wal_truncate`)
- `crates/tachi-server/src/bootstrap/serve/background.rs`
  (`spawn_wal_checkpoint`, `run_optimize()` calls added alongside each
  existing `checkpoint_wal_truncate()` call)

**Verification**:
- New unit test `run_optimize_refreshes_planner_statistics` in
  `crates/memcore/src/lib_tests.rs` — seeds 50 rows, calls `run_optimize`,
  asserts `sqlite_stat1` now has rows for `memories`, and asserts a second
  call doesn't error (repeat-safe).
- `cargo test -p memcore --lib` — 329 passed (328 + 1 new), 0 failed.
- Real-DB `sqlite_stat1`/`EXPLAIN QUERY PLAN` before/after captured above
  via `sqlite3` CLI against the read-only production-data copy (temp copy,
  discarded after).
- `cargo build -p memcore -p tachi-server` — clean.
- `cargo fmt -p memcore -p tachi-server -- --check` — clean.
- `cargo clippy -p memcore --all-targets --no-deps -- -D warnings` — clean.
- `cargo clippy -p tachi-server --all-targets --no-deps -- -D warnings` —
  clean.

---

## Item 4 — `get_access_times` LIMIT (`access_history` is the fastest-growing table)

**Semantics check first**: `get_access_times`' result feeds ACT-R
base-level activation (`base_level_activation` in `scorer.rs`):
`B_i = ln(Σ t_j^(-d))` — a plain sum over every returned access age, so it
is order-independent, and each term's contribution shrinks with `-d` decay
as `t_j` (age) grows. The existing query already orders
`accessed_at DESC` (most recent first). So capping to the N *most recent*
accesses per memory_id preserves the dominant terms of the sum and only
drops the vanishingly-small-contribution tail — not an approximation that
changes ranking behavior in any observable way, only a bound on unbounded
growth.

**Cap chosen**: `ACCESS_TIMES_MAX_PER_MEMORY = 256`, matching
`GcConfig::access_history_keep_per_memory` (default 256,
`crates/memcore/src/types/entry.rs:160`) — the number of rows GC already
prunes each memory_id down to in steady state
(`crates/memcore/src/db/stats_gc.rs`). Confirmed on the live DB: the
busiest memory_ids in `access_history` (15,608 rows total) sit at exactly
256 rows each (GC-steady-state), so this cap changes nothing for GC'd data
and only bounds worst-case cost for memory_ids whose history grew past 256
between GC runs.

```
$ sqlite3 memory_readonly_copy.db \
    "SELECT memory_id, COUNT(*) c FROM access_history GROUP BY memory_id ORDER BY c DESC LIMIT 5;"
21d97267-...|256
38010cb3-...|256
4ebf65d3-...|256
6112ba35-...|256
708b7fe8-...|256
```

**Before** (unbounded, `EXPLAIN QUERY PLAN` for two busy ids):

```
QUERY PLAN
|--SEARCH access_history USING COVERING INDEX idx_access_hist_mem_time (memory_id=?)
`--USE TEMP B-TREE FOR ORDER BY
```

**After** (bounded via `ROW_NUMBER() OVER (PARTITION BY memory_id ORDER BY
accessed_at DESC)`, same covering index still used for the
partition/order):

```
QUERY PLAN
|--CO-ROUTINE ranked
|  |--CO-ROUTINE (subquery-3)
|  |  `--SEARCH access_history USING COVERING INDEX idx_access_hist_mem_time (memory_id=?)
|  `--SCAN (subquery-3)
|--SCAN ranked
`--USE TEMP B-TREE FOR ORDER BY
```

Row-count check on the two busiest live ids (both already at the 256 GC
ceiling) confirmed identical output before/after — zero behavior change on
real data, as expected.

**Implementation**: `crates/memcore/src/db/memory_crud/access.rs`
`get_access_times` — added `ACCESS_TIMES_MAX_PER_MEMORY: i64 = 256` and
rewrote the per-batch query to select from a `ROW_NUMBER()`-ranked subquery
filtered to `rn <= 256`, following the exact partition/order pattern
`stats_gc.rs`'s GC query already uses (same shape, same covering index).

**File:line**: `crates/memcore/src/db/memory_crud/access.rs`
(`ACCESS_TIMES_MAX_PER_MEMORY` const + `get_access_times` body).

**Verification**:
- New test module `get_access_times_tests` in the same file:
  - `caps_at_max_per_memory_and_keeps_most_recent` — seeds 257 access rows
    for one memory_id (one over the cap), asserts the result is truncated
    to exactly 256 and the oldest (dropped) row's age never appears.
  - `under_cap_is_unaffected` — 3 rows in, 3 rows out (no accidental
    over-truncation).
- `cargo test -p memcore --lib` — 331 passed (329 + 2 new), 0 failed,
  including all `golden_corpus`/`ops_audit_corpus` recall-ranking tests
  unchanged (confirms no observable ranking-order regression).
- `cargo fmt -p memcore -- --check` — clean.
- `cargo clippy -p memcore --all-targets --no-deps -- -D warnings` — clean.
- Real-DB `EXPLAIN QUERY PLAN` + row-count before/after captured above via
  `sqlite3` CLI against the read-only production-data copy.

---

## Item 5 — coldpath: scope default `tachi status` probe, gate fleet view behind `--all-dbs`

**Before**: `collect_snapshot`/`collect_snapshot_inner` iterated every entry
in `manifest.dbs` unconditionally, opening each as a read-only `MemoryStore`
and running a full probe (job histogram, vector health, namespace counts,
continuity metrics — several queries each). On this machine's real
`~/.tachi/manifest.json`, that's 7 DBs:

```
$ python3 -c "import json; m=json.load(open('~/.tachi/manifest.json')); print(len(m['dbs']))"
7
```

Timed via the built `tachi` binary, `tachi status --json`, warm runs
(first run excluded — cold page cache):

| scope | wall time (5 warm runs) |
|---|---|
| `--all-dbs` (old default, full 7-db fleet) | 0.38s, 0.16s, 0.17s, 0.18s, 0.18s |
| default (new, global+project only) | 0.08s, 0.08s, 0.08s, 0.09s, 0.23s |

Consistent with opus's "~220ms -> ~60ms" characterization for this class
of change (exact numbers vary with OS scheduling noise, but the *shape* —
2-3x faster scoped to 2 DBs vs 7 — reproduces every run).

**After**: `collect_snapshot_scoped(app_home, global_db_path,
project_db_path, all_dbs)` — when `all_dbs = false` (the new CLI default),
`manifest.dbs` is filtered to only the entries whose path equals
`global_db_path` or `project_db_path` (via the existing `paths_equal`
canonicalization helper — the same one `is_orphan_entry` already used) BEFORE
the probe loop runs, so skipped entries never open a `MemoryStore` at all.
`--all-dbs` restores the exact prior fleet-wide behavior; the new
`collect_snapshot_with_provider_value_compare` path (used only by
`--probe-keys`, live network provider probing, already opt-in and rare)
is left as full-fleet unconditionally — not worth a second scoping axis.

This does NOT change what's IN the manifest, and does not affect any other
manifest consumer (`tachi doctor`, `tachi manifest`, etc.) — only which
entries `tachi status`'s render probes for a given invocation. The human
render prints `[i] scoped to global + current-project db; pass --all-dbs
for the full fleet` under the `Manifest (N dbs)` line when scoped, and the
JSON output carries `dbs_scoped_to_global_and_project: true/false` so
machine consumers don't silently read a partial fleet as the whole
manifest.

**File:line**:
- `crates/tachi-server/src/status_ops/snapshot.rs` (`collect_snapshot_scoped`,
  `collect_snapshot_inner`'s new `all_dbs` parameter + `scoped_entries`
  filter)
- `crates/tachi-server/src/status_ops/status_cli/status_render.rs`
  (`run_status`/`render_one` thread `all_dbs` through; human + JSON output
  additions)
- `crates/tachi-bootstrap/src/cli/commands.rs` (`Commands::Status` new
  `all_dbs: bool` field, `--all-dbs` flag)
- `crates/tachi-server/src/bootstrap/serve/cli_commands.rs` (wires the new
  field through to `run_status`)

**Verification**:
- New test module `crates/tachi-server/src/status_ops/tests/coldpath_scoping.rs`
  (registered in `status_ops/tests.rs`): builds a real 3-DB manifest fixture
  (global + project + one "extra" agent DB standing in for the rest of a
  fleet) and asserts:
  - `default_scope_probes_only_global_and_project` — scoped snapshot has
    exactly 2 `dbs` entries (global + project), not 3.
  - `all_dbs_flag_restores_full_fleet` — `all_dbs=true` returns all 3.
  - `default_scope_with_no_project_db_probes_only_global` — no project db
    path -> exactly 1 entry (global only), not a false-positive match.
- `cargo test -p tachi-server --lib status_ops` — 64 passed, 0 failed.
- `cargo test -p tachi-server --lib manifest` — 50 passed, 0 failed.
- Full-suite run: `cargo test -p tachi-server --lib` — 1577/1580 passed on
  one run, with 1-5 failures varying run-to-run entirely in
  `bootstrap::serve::stdio::tests::*` (async proxy timeouts) and
  `gh_ops::ship_tests::*` (temp-file races) — **none in `status_ops`,
  `manifest`, or any file this item touches**, each individually passes
  when re-run in isolation, and the failing set changes between runs
  (confirmed pre-existing parallel-test-load flakiness on this machine, not
  a regression from this change — see also this repo's own
  `feedback_test_worktree_race` operational note on shared-cache test
  contention).
- `cargo build -p tachi-server -p tachi-bootstrap` — clean.
- `cargo fmt -p tachi-server -p tachi-bootstrap -- --check` — clean.
- `cargo clippy -p tachi-server -p tachi-bootstrap --all-targets --no-deps -- -D warnings` — clean.
- Real-binary timing above captured directly against this machine's live
  `~/.tachi/manifest.json` (7 DBs) via the built `tachi` binary,
  `status --json` (read-only; verified no writes happen on this path
  outside `--probe-keys`).
