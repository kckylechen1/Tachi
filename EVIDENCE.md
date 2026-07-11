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
