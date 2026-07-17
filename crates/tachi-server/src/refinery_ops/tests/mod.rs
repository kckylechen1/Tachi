//! #1002 Issue Refinery v1 acceptance tests. Every test in this tree is
//! fixture-driven: zero live `gh`/`git` calls, zero GitHub mutation
//! (acceptance criterion 7). Correction (R5-2, sol arbitration codex-r8f4e,
//! archived on issue #1002): NOT every test goes through the same layer —
//! this doc comment previously overclaimed that all of them do.
//!
//! | file | acceptance criteria | pipeline depth |
//! |---|---|---|
//! | `grounding.rs` | 1 (exact anchor), 2 (missing anchor), 3 (full coverage, no truncation) | full pipeline: every test calls `build_refinery_packet` with an injected `doc_resolver::FixtureDocResolver` standing in for the real `GitRefResolver` — the exact function the live `refine_issues` action calls, not a re-implementation |
//! | `replay_and_safety.rs` | 5 (anti-replay staleness), 6 (model-only verification safety) | full pipeline, same as `grounding.rs` (plus a few tests that construct `doc_resolver::GitRefResolver`/`NullDocResolver` directly to test resolver-level logic without any live git) |
//! | `disposition_rules.rs` | 4 (5 historical cases + 7 failure classes + closed vocabulary) | classifier-level only: these fixtures call `disposition::propose_disposition` DIRECTLY with a hand-built `RefinerySignalsV1`/evidence — they never touch `parse`/`compiler`/`doc_resolver` at all. A handful of tests in that file (tagged "through the real pipeline" in their own names/doc comments) DO call `build_refinery_packet` instead, to prove specific signals are wired end-to-end — those are the exception, not the rule, in that file. |
//! | `live_relation_signals.rs` | #1105 (live per-relation evidence + commit-reachability shipped checks) | full pipeline via `build_refinery_packet_with_live_signals`, with a hand-built `LiveRelationSignals` standing in for `collect_live_relation_signals`'s async orchestration (that async collection step itself is not unit-tested here — same "live shelling untested, pure assembly tested" split as `doc_resolver`/`GitRefResolver`; `refinery_ops::live_signals`'s own `#[cfg(test)]` module covers the pure `derive_live_relation_signals`/`classify_related_state`/`pick_shipped_evidence` assembly logic that orchestrator feeds) |
//!
//! Criterion 8 (judged red-before/green-after) is the state of this test
//! tree itself relative to the pre-#1002 behavior, which had no
//! `refine_issues` action at all (every test here is new, not a modified
//! existing assertion).

mod disposition_rules;
mod grounding;
mod live_relation_signals;
mod replay_and_safety;
