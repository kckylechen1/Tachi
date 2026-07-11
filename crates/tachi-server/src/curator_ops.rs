//! Issue-curator orchestration shell (#1002).
//!
//! A thin batch-orchestration layer over the *existing* dispatch/eval/gh
//! plumbing — no new execution machinery. Per candidate issue (surfaced by
//! the #1000 freshness layer's zombie/stale-candidate queues, or an explicit
//! issue-number list) the curator:
//!
//!   1. assembles a bounded re-verification packet (issue ref + evidence
//!      already on file + HEAD sha),
//!   2. hands it to a lane via a `LaneRunner` (the live impl dispatches
//!      through `dispatch_ops::handle_tachi_dispatch` using an existing
//!      dispatch profile — `codex_55_review` by default — and records the
//!      eval via `complete_ops::handle_tachi_complete`; unit tests use a
//!      fake runner so no live GitHub/lane call happens off this crate's
//!      test suite),
//!   3. stores a three-tier verdict row (`still_valid` / `fixed_pending_closure`
//!      / `stale_spec`) with evidence and a *drafted* (never auto-posted)
//!      writeback comment,
//!   4. enforces a per-batch token budget and reports skipped candidates
//!      explicitly — never a silent cap (`scan_open_loops` / #1000 idiom).
//!
//! Non-goals (frozen by #1002): the curator never closes an issue, never
//! edits an issue body, never opens a new issue, and never posts a GitHub
//! comment/label except through the *existing* `tachi_gh` write-back path,
//! gated by an explicit `post` flag that defaults to `false`.

mod batch;
mod entry;
mod runner;
mod verdict;

pub(crate) use entry::handle_issue_curator_batch;
pub(crate) use verdict::briefing_curator_queue;
