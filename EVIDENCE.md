# #1002 curator rework — evidence (codex review fix)

Base: `b87b1dc86a17fa978bb823b2304f047157cfd858` (feat/1002-issue-curator, PR #1005)

## Finding 1 — fabricated verdicts by construction (BUG, severity-max)

Codex verbatim: "live runs persist verdicts before any lane result exists.
The runner dispatches, returns only `{dispatch_id, issue_ref, profile}` with
zero cost, and explicitly does not poll/collect evidence. The batch then
classifies and stores that placeholder; it defaults to still_valid."
(`curator_ops/runner.rs:107`, `batch.rs:87`)

### RED (base b87b1dc8, before fix)

`DispatchLaneRunner::reverify` (runner.rs, pre-fix) built a placeholder JSON
(`{dispatch_id, issue_ref, profile}`) as `evidence_text` and returned
immediately (no poll). `run_curator_batch` fed that placeholder straight into
`classify_evidence`, which defaults to `StillValid` when no explicit signal
string is found — so a dispatch that hadn't even started running yet would
be persisted as a `still_valid` (仍成立) verdict.

Ran the **original** (pre-fix) test suite against this base to confirm the
bug was untested (this is the "red" baseline — the existing 14 tests all
passed even though the fabrication bug was live, because nothing exercised
the placeholder-evidence path):

```
$ git stash   # reverts working tree to b87b1dc8 exactly
$ CARGO_TARGET_DIR=$HOME/.cache/sigil-shared-target cargo test -p tachi-server --lib curator_ops::
running 14 tests
test curator_ops::verdict::tests::label_mapping_matches_1002_spec ... ok
test curator_ops::batch::tests::draft_comment_never_claims_auto_close ... ok
test curator_ops::batch::tests::classify_evidence_defaults_to_still_valid_never_silently_claims_fixed ... ok
test curator_ops::verdict::tests::briefing_queue_empty_when_no_verdicts_saved ... ok
test curator_ops::verdict::tests::rerun_same_issue_overwrites_not_duplicates ... ok
test curator_ops::verdict::tests::get_missing_verdict_returns_none ... ok
test curator_ops::verdict::tests::save_and_list_roundtrips_via_state_kv ... ok
test curator_ops::batch::tests::verifies_all_candidates_and_persists_verdicts ... ok
test curator_ops::batch::tests::lane_error_is_skipped_with_reason_not_fatal_to_batch ... ok
test curator_ops::verdict::tests::briefing_queue_reports_overflow_never_silently_caps ... ok
test curator_ops::verdict::tests::get_single_verdict_roundtrips ... ok
test curator_ops::verdict::tests::briefing_queue_splits_pending_closure_and_respec_only ... ok
test curator_ops::batch::tests::skip_if_fresh_makes_rerun_idempotent ... ok
test curator_ops::batch::tests::budget_cap_stops_dispatch_and_reports_skipped_not_silent ... ok
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 1615 filtered out; finished in 0.98s
$ git stash pop   # restores the fix
```

No test in that 14 ever constructed a `still_valid` verdict from an empty
lane result — the bug was real but invisible to the suite. This is the gap
Finding 1 names.

### Fix (option a from the review, chosen over option b alone — implemented both: (a) is the mechanism, (b) is the structural backstop)

- `curator_ops/runner.rs`: `DispatchLaneRunner::reverify` now POLLS the
  dispatched lane to terminal state using the same primitive
  `tachi_task(action='wait')` uses — `dispatch_ops::collect_run_task_for_server`
  + a terminal-state check (`TASK_STATE_COMPLETED|FAILED|CANCELED`) —
  bounded by the existing per-candidate `timeout_secs`. On terminal
  COMPLETED with non-empty `result.md`, returns `ReverifyOutcome::Evidence`.
  On timeout, returns `ReverifyOutcome::Timeout`. On terminal
  failed/canceled or empty `result.md`, returns `ReverifyOutcome::LaneFailed`.
  No second polling loop invented (per finding's guidance) — this reuses
  `dispatch_ops::handle_tachi_dispatch` + `collect_run_task_for_server`
  verbatim.
- `curator_ops/verdict.rs`: added `CuratorVerdictKind::PendingEvidence` /
  `LaneFailed` (not real verdicts) and a structural gate function
  `persist_curator_verdict(..., evidence_text: &str, ...)` that REFUSES to
  persist a real verdict (`StillValid`/`FixedPendingClosure`/`StaleSpec`)
  when `evidence_text.trim().is_empty()`. This is the single choke point —
  `batch.rs` is now the only caller and always routes through it.
- `curator_ops/batch.rs`: `run_curator_batch` matches on `ReverifyOutcome`
  and routes `Timeout` → `pending_evidence`, `LaneFailed` → `lane_failed`,
  `Evidence` → classified real verdict — always through
  `persist_curator_verdict`.
- `curator_ops/entry.rs`: writeback loop now filters
  `report.verified.iter().filter(|r| r.verdict.is_real_verdict())` so
  `pending_evidence`/`lane_failed` rows are never posted to GitHub as if
  they were a real re-verification result.

### GREEN (after fix) — new red/green invariant pair

Unit-level (persist gate itself), `curator_ops/verdict.rs`:
- `persist_refuses_real_verdict_with_empty_evidence` — RED if the gate is
  removed (would then succeed silently).
- `persist_refuses_fixed_pending_closure_with_whitespace_only_evidence`
- `persist_allows_pending_evidence_with_empty_evidence` — GREEN control
  (proves the gate isn't overbroad).
- `persist_allows_lane_failed_with_empty_evidence`
- `persist_accepts_real_verdict_with_non_empty_evidence`
- `pending_evidence_and_lane_failed_never_enter_actionable_briefing_queues`

Integration-level (whole `run_curator_batch` path), `curator_ops/batch.rs`:
- `timeout_persists_pending_evidence_never_still_valid` — asserts
  `stored.verdict != CuratorVerdictKind::StillValid` and
  `!stored.verdict.is_real_verdict()` when the fake lane returns
  `ReverifyOutcome::Timeout`.
- `lane_failed_persists_lane_failed_never_a_real_verdict`
- `real_evidence_still_persists_real_verdicts_after_the_fix` — non-regression
  control proving the happy path still works.

## Finding 2 — packet lacks the issue body (BUG)

Codex verbatim: "step 1's required issue body is absent. Freshness
candidates contain only a synthetic summary and saved references, explicit
issue refs get an empty evidence list. The packet is built from only those
fields, not the issue text or its complete anchors."
(`curator_ops/entry.rs:33`, `batch.rs:59`)

### Fix

- `CuratorCandidate` (batch.rs) gains `issue_body: String` +
  `file_line_anchors: Vec<(String, u64)>`.
- `entry.rs`: `fetch_issue_body_and_anchors` (thin async wrapper, not
  test-exercised — matches the existing #1002 "no live gh call off this
  crate's tests" contract for `DispatchLaneRunner`) calls
  `gh_ops::handle_gh_issue_read` (same idiom `gh issue view N --json
  number,title,state,body,author,labels,assignees,createdAt,updatedAt,comments`
  the freshness layer's parent branch already uses — reused verbatim, now
  exported `pub(crate)` from `gh_ops.rs` instead of `pub(in crate::gh_ops)`),
  concatenates body + comments, and extracts anchors via the existing
  `gh_ops::extract_file_line_anchors` (also newly `pub(crate)`-exported;
  logic unchanged). This runs for BOTH explicit `curator_issue_refs`
  candidates and freshness-sourced ones (closing exactly the gap the finding
  named: explicit refs got an empty evidence list before).
- `batch.rs`'s `build_packet` now renders the full issue body + extracted
  anchors into the lane prompt, with explicit "(issue body unavailable...)"
  / "(none extracted)" fallback text when fetch fails or nothing was found
  (best-effort, never silently truncates).
- Fixture tests (no live `gh` call): `enrich_candidate_extracts_body_and_
  comments_and_anchors`, `enrich_candidate_handles_missing_comments_field`
  (entry.rs), `build_packet_carries_issue_body_and_anchors`,
  `build_packet_handles_missing_body_and_anchors_explicitly` (batch.rs).

## Full curator_ops suite after both fixes

```
$ CARGO_TARGET_DIR=$HOME/.cache/sigil-shared-target cargo test -p tachi-server --lib curator_ops::
running 30 tests
test curator_ops::batch::tests::draft_comment_never_claims_auto_close ... ok
test curator_ops::batch::tests::classify_evidence_defaults_to_still_valid_never_silently_claims_fixed ... ok
test curator_ops::entry::tests::parse_issue_number_handles_owner_repo_hash_number ... ok
test curator_ops::entry::tests::parse_issue_number_returns_none_for_malformed_ref ... ok
test curator_ops::entry::tests::verdict_label_covers_all_five_kinds ... ok
test curator_ops::entry::tests::enrich_candidate_handles_missing_comments_field ... ok
test curator_ops::batch::tests::build_packet_handles_missing_body_and_anchors_explicitly ... ok
test curator_ops::batch::tests::build_packet_carries_issue_body_and_anchors ... ok
test curator_ops::entry::tests::enrich_candidate_extracts_body_and_comments_and_anchors ... ok
test curator_ops::verdict::tests::briefing_queue_splits_pending_closure_and_respec_only ... ok
test curator_ops::verdict::tests::label_mapping_matches_1002_spec ... ok
test curator_ops::batch::tests::timeout_persists_pending_evidence_never_still_valid ... ok
test curator_ops::batch::tests::budget_cap_stops_dispatch_and_reports_skipped_not_silent ... ok
test curator_ops::batch::tests::lane_failed_persists_lane_failed_never_a_real_verdict ... ok
test curator_ops::batch::tests::skip_if_fresh_makes_rerun_idempotent ... ok
test curator_ops::batch::tests::verifies_all_candidates_and_persists_verdicts ... ok
test curator_ops::batch::tests::lane_error_is_skipped_with_reason_not_fatal_to_batch ... ok
test curator_ops::verdict::tests::get_single_verdict_roundtrips ... ok
test curator_ops::verdict::tests::get_missing_verdict_returns_none ... ok
test curator_ops::verdict::tests::briefing_queue_reports_overflow_never_silently_caps ... ok
test curator_ops::verdict::tests::briefing_queue_empty_when_no_verdicts_saved ... ok
test curator_ops::batch::tests::real_evidence_still_persists_real_verdicts_after_the_fix ... ok
test curator_ops::verdict::tests::pending_evidence_and_lane_failed_never_enter_actionable_briefing_queues ... ok
test curator_ops::verdict::tests::persist_accepts_real_verdict_with_non_empty_evidence ... ok
test curator_ops::verdict::tests::persist_allows_lane_failed_with_empty_evidence ... ok
test curator_ops::verdict::tests::persist_allows_pending_evidence_with_empty_evidence ... ok
test curator_ops::verdict::tests::persist_refuses_fixed_pending_closure_with_whitespace_only_evidence ... ok
test curator_ops::verdict::tests::persist_refuses_real_verdict_with_empty_evidence ... ok
test curator_ops::verdict::tests::rerun_same_issue_overwrites_not_duplicates ... ok
test curator_ops::verdict::tests::save_and_list_roundtrips_via_state_kv ... ok
test result: ok. 30 passed; 0 failed; 0 ignored; 0 measured; 1615 filtered out; finished in 1.41s
```

`cargo check -p tachi-server`: clean.
`cargo clippy -p tachi-server --lib -- -D warnings`: clean, no warnings.
`cargo fmt -p tachi-server`: applied (formatting-only diff after logic changes).

## Continuation-seat independent re-verification (this pass)

The seat that authored the above (Findings 1+2, `8a96a211`) died mid-run
right after pushing. This continuation seat independently re-ran everything
above rather than trusting the self-report, per constitution `自报勿信`:

- `cargo test -p tachi-server --lib curator_ops::` against
  `$CARGO_TARGET_DIR=$HOME/.cache/sigil-shared-target`: 30/30 green,
  reproduces verbatim.
- `cargo fmt -p tachi-server -- --check`: clean (no diff).
- `cargo clippy -p tachi-server --all-targets --no-deps -- -D warnings`:
  clean (the only warning printed is a pre-existing multi-bin-target Cargo
  metadata note on `main.rs`, unrelated to clippy lints or this change).
- Read `persist_curator_verdict`, `DispatchLaneRunner::reverify`,
  `build_packet`, `fetch_issue_body_and_anchors`/`enrich_candidate_from_issue_json`
  in full: confirmed the empty-evidence gate is a genuine single choke point
  (`save_curator_verdict` has no other production caller) and the Finding #2
  fetch/anchor/packet-render logic matches what's claimed.
- Ran `cargo test -p tachi-server --lib` (full crate, no filter): **2
  failures outside curator_ops** —
  `arena_ops::tests::harness::arena_spawn_launches_opencode_dispatch_and_collects_result`
  and `bootstrap::serve::stdio::tests::stdio_proxy_allows_explicit_cross_project_read`.
  Confirmed via `git diff b87b1dc8..HEAD --stat` that neither `arena_ops/`
  nor `bootstrap/` is touched by this branch. Re-ran both individually with
  `--test-threads=1` in a fully isolated `$CARGO_TARGET_DIR` (not the shared
  cache) — both pass in isolation, confirming full-suite parallel-run
  flakiness (one is a literal `timed out` assertion), not a regression from
  this rework. Flagging for Oz to weigh whether these should be
  de-flaked/marked `#[ignore]` separately — out of this PR's scope.

## Still open / not done in this pass

- PR body update (opus CONCERN, accepted): DONE — `gh pr view 1005` body now
  carries a "Codex review fixes (commit `8a96a211`)" section describing both
  findings and the real synchronous-poll/timeout-bound behavior, replacing
  the stale "fire-and-report" framing. (Corrected from this file's earlier
  note, which was written before that edit landed.) Still outstanding: a PR
  *comment* summarizing both findings + evidence for reviewer visibility
  (distinct from the body edit) — see continuation seat's report.
- `DispatchLaneRunner::reverify`'s poll loop is not exercised by an
  integration test against a real `handle_tachi_dispatch` run (that would
  require a live dispatch/subprocess spawn, out of scope for this crate's
  unit-test contract — same posture the original code already had for the
  live dispatch call itself). The poll logic's terminal-state/evidence
  branching IS covered indirectly via `ReverifyOutcome`'s three variants
  being exercised in `batch.rs`'s tests through the `FakeLaneRunner`, but
  the actual `std::fs::read_to_string(result.md)` line inside
  `DispatchLaneRunner::reverify` itself has no direct unit test (would need
  a real run_dir on disk). This is a coverage gap Oz or a follow-up should
  weigh: a targeted test constructing a fake run dir + status.json +
  result.md and calling `collect_run_task_for_server` directly would close
  it without needing a live dispatch.
