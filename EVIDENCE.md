# #773 S1 — Evidence Log

## Item 1: Relation ontology v1 write-validation

### Design decision (deviation flagged for owner review)

Mission scoped the legal set to "the EXISTING scorer weight table vocabulary
... PLUS about". Audit of every `MemoryEdge{relation: ...}` construction site
in the workspace (`rg 'relation:' crates/tachi-server/src crates/memcore/src`)
found `component_governance_ops::seed_component_records` (tachi#772,
#796/#815) writes `owns` / `consumes` / `backflow_candidate` / `blocked_by` on
**every server boot** (idempotent seed, `bootstrap/serve.rs:591`) — none of
which are in the scorer table. The #773 v3/v4 frozen design explicitly gates
#772 out of S1 scope ("component registry 行将来走同一 anchor 惯例, S1 落地前
不动"). Rejecting these relations at the choke point would break server boot
on every fresh/existing install, which is not a S1 goal and not something I
can silently paper over (frozen-assertion discipline). Resolution: added
these 4 as an explicitly-labeled grandfathered set
(`COMPONENT_GOVERNANCE_GRANDFATHERED` in `relation_ontology.rs`), separate
from `ONTOLOGY_V1`, with a comment pointing at #772's own eventual anchor
migration as the retirement path. Flagging this for owner ratification in the
PR body — this is a real scope expansion beyond the literal mission text, not
a call I should make unilaterally without visibility.

### Red (pre-fix behavior, illustrative — this is a new module, no prior
behavior to regress): N/A, new choke-point validation.

### Green
```
cargo test -p memcore --lib relation_ontology
running 6 tests
test relation_ontology::tests::component_governance_grandfathered_relations_remain_legal ... ok
test relation_ontology::tests::about_is_legal ... ok
test relation_ontology::tests::ontology_v1_matches_scorer_named_arms ... ok
test relation_ontology::tests::empty_or_whitespace_relation_rejected ... ok
test relation_ontology::tests::related_to_is_deprecated_not_legal_for_new_writes ... ok
test relation_ontology::tests::unknown_relation_rejected_with_legal_set_in_message ... ok
test result: ok. 6 passed; 0 failed
```

Full memcore suite green after wiring validation into `db::add_edge`
(required fixing one pre-existing test that wrote `related_to` directly —
swapped to `similar_to`, same 0.55 weight class, preserves test intent):
```
cargo test -p memcore --lib
test result: ok. 337 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out
```

## Item 2: auto_link stops emitting related_to

Retired the `related_to` fallback branch in `spawn_auto_linking`: when neither
`supersedes` nor `reinforces` fires, auto_link now skips (no edge write) —
entity co-occurrence without a stronger signal is query-time recoverable via
shared-entity search, per mission text. Removed the now-dead
`should_related_to` fn + its two threshold consts (only test-referenced after
the retirement).

Fixed 2 pre-existing tests that assumed the retired fallback:
- `save_memory_auto_link_does_not_bump_target_access_count` deliberately
  engineered a 2-shared-entity/no-reinforce/different-path-root scenario to
  force `related_to`. Retargeted to same path root so `should_supersede`
  fires instead (still proves the access-count-not-bumped invariant, on the
  surviving edge type).
- `status_ops::tests::vector_namespace::namespace_health_counts_...` inserts
  a `related_to` row via raw SQL (bypasses `add_edge`/the choke point
  entirely) to simulate a legacy DB row for namespace-health-stats
  assertions — left untouched, this is exactly the grandfathered-read
  scenario, not a new write.

### Green
```
cargo test -p tachi-server --lib auto_link
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 1594 filtered out

cargo test -p tachi-server --lib vector_namespace
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 1600 filtered out
```

## Item 3: Legacy fog retirement (close_related_to_fog)

`memcore::db::close_related_to_fog` / `MemoryStore::close_related_to_fog`:
idempotent UPDATE closing `valid_to` on every still-open `related_to` edge.
Rows are not deleted (audit trail preserved), only closed so they leave
`get_edges`/`graph_expand` traversal.

### Dependency surfaced: ported PR #1013 (uncommitted) into this branch

While writing the discrimination test (insert a legacy `related_to` edge with
`valid_to = NULL`, close it, assert `get_edges` excludes it immediately), hit
exactly the bug PR #1013 (`fix/773-edge-valid-to-normalization`, not yet
merged to main — confirmed via `git merge-base --is-ancestor c3b35074
origin/main` -> false) fixes: `get_edges`'s `valid_to > datetime('now)`
compares lexically, and RFC3339 (`...T...Z`) sorts greater than SQLite's
`datetime('now')` text output, so a same-day-closed edge stayed "active"
forever. Per the branch's own instructions ("use the SAME normalized
format/comparison [#1013] establishes... if it merges first, rebase onto
it") — since #1013 hasn't merged, ported its exact diff (not a second
convention): `add_edge` normalizes `valid_to` via `normalize_utc_iso_or_now`
on write; all 5 read sites (`get_edges` x3 direction branches,
`get_edges_batch`, `get_contradiction_count`) now compare
`datetime(valid_to) > datetime('now')` (format-agnostic). If #1013 merges
before this branch, this is the same patch twice — trivial rebase conflict,
not a semantic conflict.

### Red (pre-port, illustrative)
```
thread '...close_related_to_fog_closes_open_rows_and_excludes_from_get_edges' panicked:
closed related_to edge must leave get_edges traversal, got [MemoryEdge { ...
  valid_to: Some("2026-07-11T20:45:57.994Z") }]
```

### Green (post-port + new fn)
```
cargo test -p memcore --lib db::tests::graph
running 11 tests
test db::tests::graph::close_related_to_fog_closes_open_rows_and_excludes_from_get_edges ... ok
test db::tests::graph::close_related_to_fog_is_idempotent ... ok
test db::tests::graph::close_related_to_fog_leaves_other_relations_untouched ... ok
test db::tests::graph::close_related_to_fog_closed_edge_valid_to_exactly_now_is_closed_not_active ... ok
test result: ok. 11 passed; 0 failed

cargo test -p memcore --lib
test result: ok. 341 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out
```

## Item 4: ensure_anchor API + namespace/vector-sweep/recall exclusion

`memcore::db::anchor` (`ensure_anchor`, `AnchorKind`, `anchor_id`,
`anchor_path`) + `MemoryStore::ensure_anchor` wrapper. Anchors are
`memories` rows with `category="entity"` (no CHECK-constraint migration
needed, per `precedent_ops.rs:33-42`'s established reuse-existing-category
convention), deterministic id `anchor:<kind>:<key>`
(issue/pr/dispatch/seat), path `/anchors/<kind>/<key>`, `retention_policy =
'pinned'`, `tier = 'pattern'` (never GC'd, never re-embedded).

Four guards implemented:
- (a) read-verify: existing id's `metadata.anchor_kind`/`anchor_key` checked
  against the request before treating as a no-op success.
- (b) fail-closed: kind/key mismatch at an existing id -> `InvalidArg`, never
  silent reuse/overwrite. Tested via hand-forged collision rows (kind
  mismatch + key mismatch, separately).
- (c) `anchor:` namespace reserved: `memory_crud::upsert` now rejects any
  `entry.id` starting with `anchor:` outright — `ensure_anchor` is the sole
  creation path (its own `INSERT OR IGNORE`, never routes through `upsert`).
- (d) both edge endpoints in the same physical DB: falls out for free —
  `ensure_anchor` and `add_edge` share one `Connection`, no cross-DB id path
  exists. Composition tested (`ensure_anchor` -> `add_edge` with `about`
  relation -> `get_edges` roundtrip).

### namespace.rs + vector sweep + recall exclusion

- `namespace::is_anchor_entry` + wired into `is_namespace_search_noise`
  (unconditional exclusion — anchors have no scoped "browse as search
  results" use case, unlike kanban/handoff/wiki's opt-outs).
- `entries_missing_vectors` (`store/maintenance.rs`) and
  `entries_missing_vectors_filtered` (`store/enrichment.rs`, all 3 branches)
  now exclude `id LIKE 'anchor:%'` — anchors never burn embedding budget.
- **Kill-test (sol #773 v3 correction 1, the hard requirement)**: added
  `AND id/m.id NOT LIKE 'anchor:%'` to all three `collect_candidates`
  channels — `search_vec`, `search_fts_match` (both `search_fts` and raw
  match), `search_symbolic_candidates` — since anchors DO sync into
  `memories_fts` on every upsert (unconditional in `memory_crud.rs`) and
  would otherwise leak into FTS/symbolic candidates before the
  `is_namespace_search_noise` ranking-stage filter ever runs, exactly the
  failure mode the spec's kill-test describes.

### Red (proved the probe test isn't vacuous)

Temporarily stashed the 3-channel SQL exclusion and reran the new probe
test:
```
thread 'search::tests::anchor::collect_candidates_never_surfaces_an_anchor_even_on_exact_text_match'
panicked: anchor id must never appear in collect_candidates' candidate_ids,
got ["real-773-note", "anchor:issue:kckylechen1/tachi:773"]
test result: FAILED. 1 passed; 1 failed
```
Restored the fix; both tests green again.

### Green
```
cargo test -p memcore --lib db::anchor
test result: ok. 7 passed; 0 failed

cargo test -p memcore --lib search::tests::anchor
test result: ok. 2 passed; 0 failed

cargo test -p memcore --lib
test result: ok. 352 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out

cargo fmt -p memcore -- --check   -> clean
cargo clippy -p memcore --all-targets --no-deps -- -D warnings   -> clean
```

### Downstream verify (tachi-server, depends on memcore's changed upsert/API surface)
```
cargo build -p tachi-server --lib   -> clean
cargo test -p tachi-server --lib component_governance   -> 19 passed (grandfathered relations still seed clean)
cargo test -p tachi-server --lib   -> 1600 passed, 2 failed (gh_ops::ship_tests — tempfile-collision
  race under parallel threads, unrelated to this branch's files; both pass in isolation
  with --test-threads=1, confirmed not a regression)
cargo fmt -p tachi-server -- --check   -> clean (after fmt)
cargo clippy -p tachi-server --all-targets --no-deps -- -D warnings   -> clean
```
