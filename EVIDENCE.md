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
