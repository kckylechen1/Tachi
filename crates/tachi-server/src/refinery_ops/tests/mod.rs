//! #1002 Issue Refinery v1 acceptance tests. Every test in this tree is
//! fixture-driven: zero live `gh`/`git` calls, zero GitHub mutation
//! (acceptance criterion 7) — the injected `FixtureDocResolver` (see
//! `refinery_ops::fixtures`) stands in for the real `GitRefResolver`, and
//! `build_refinery_packet` is the exact function the live `refine_issues`
//! action calls, not a re-implementation.
//!
//! | file | acceptance criteria |
//! |---|---|
//! | `grounding.rs` | 1 (exact anchor), 2 (missing anchor), 3 (full coverage, no truncation) |
//! | `disposition_rules.rs` | 4 (5 historical cases + 7 failure classes + closed vocabulary) |
//! | `replay_and_safety.rs` | 5 (anti-replay staleness), 6 (model-only verification safety) |
//!
//! Criterion 8 (judged red-before/green-after) is the state of this test
//! tree itself relative to the pre-#1002 behavior, which had no
//! `refine_issues` action at all (every test here is new, not a modified
//! existing assertion).

mod disposition_rules;
mod grounding;
mod replay_and_safety;
