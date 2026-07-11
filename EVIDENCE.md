# #1000 round-3 — codex verdict rework (Wizard/Sonnet)

Base: `origin/feat/1000-issue-freshness` @ `ec97c465` (round-2 "rework per
codex+opus review"). This baton addresses codex's round-3 verdict: 6 BUG
findings + 1 wording finding, all leader-accepted.

## Findings, file:line, fix, test — verbatim

### Finding 1 — merge-commit message never parsed
- **Codex claim**: `mergeCommit` only carries `{oid}` from `gh pr list
  --json`; a `Refs #N` that lives ONLY in a true (2-parent) merge commit's
  own message is invisible to the scan.
- **File:line (before fix)**: `issue_freshness.rs` `fetch_merged_prs`
  (was ~450-470), `parse_merged_prs_json` (was ~473-525).
- **Fix**: added `MergedPr::merge_commit_message: String`
  (`issue_freshness.rs:59-69`), populated in `fetch_merged_prs` via
  `resolve_merge_commit_message` (`git -C repo_root log -1 --format=%B
  <sha>`, best-effort — a resolve failure degrades to empty string, never a
  hard scan failure). Folded into `scan_zombies`'s text concatenation
  alongside title/body/commit_messages. `fetch_and_scan_zombies` /
  `fetch_merged_prs` now take `repo_root: Option<&Path>`; `router.rs` computes
  `repo_root` once up front (moved before the zombie arm) and passes it in.
- **Tests (verbatim, all green)**:
  `scan_zombies_catches_merge_commit_only_reference`,
  `scan_zombies_dedupes_when_same_ref_in_body_and_merge_commit`.

### Finding 2 — gate-phrase guilt-by-association ("gate 短语连坐")
- **Codex claim**: `gated on #1 (Refs #2)` flags #2 as a gate too — the old
  `extract_gate_issue_numbers` scanned the WHOLE gate-marked sentence for any
  `#N`, not just the one attached to the phrase.
- **File:line (before fix)**: `issue_freshness.rs:286-340` (old
  `extract_hash_numbers` + `extract_gate_issue_numbers`).
- **Fix**: replaced whole-sentence `#N` extraction with
  `extract_hash_numbers_immediately_after(span, phrase_end)` — walks forward
  from the phrase match, allowing only separator chars (space/colon/comma/
  parens) between the phrase and the `#N` run; anything else stops the walk.
  `extract_gate_issue_numbers` now finds each gate-phrase match's own
  position and attributes only the `#N` immediately following it.
- **Tests (verbatim, all green)**: standalone Rust harness confirmed the
  regression pre-fix (`gated on #1 (Refs #2)` → `[1, 2]`) and post-fix
  (→ `[1]`) before touching the crate. In-crate:
  `extract_gate_issue_numbers_does_not_attribute_trailing_refs_in_same_sentence`
  (codex's exact example), `extract_gate_issue_numbers_attributes_multiple_gate_phrases_in_one_sentence`
  (positive control — two genuine gate phrases in one sentence each keep
  their own target), `stale_candidate_gate_attribution_ignores_trailing_ref_in_gate_sentence`
  (end-to-end through `scan_stale_candidates`: closing the trailing Refs
  target does NOT flag; closing the real gate DOES).

### Finding 3 — reap errors swallowed to 0
- **Codex claim**: DB list/delete failures in `reap_stale_kind_rows` were
  `.unwrap_or(0)`'d in `router.rs` — a closed zombie whose row fails to
  delete stays a ghost in the briefing with `rows_reaped` reporting 0,
  indistinguishable from "nothing needed reaping".
- **File:line (before fix)**: `router.rs` all three reap call sites (was
  ~294-300 zombie, ~355-364 stale, ~410-419 churn).
- **Fix**: all three reap calls now `match` on the `Result`; a DB error
  pushes into a new `reap_errors: Vec<String>` and marks the kind in
  `reap_incomplete_kinds`. Both fields are new top-level keys on the
  `issue_freshness_scan` JSON response.
- **Test**: no live-DB-failure-injection test was added (the existing
  `reap_stale_kind_rows` unit tests already cover the success path
  end-to-end via `test_server()`; injecting a genuine SQLite failure would
  need corrupting the on-disk DB mid-test, which the file's existing test
  harness has no fixture for — flagged as an honest gap below, not silently
  skipped).

### Finding 4 — three inline parse sites never extracted to fixture-tested pure fns
- **Codex claim**: `open issue numbers` / `issue bodies` / `closed issue
  numbers` parsing was still inline (unlike the finding-8 extractions
  `parse_merged_prs_json` / `parse_open_issues_with_activity_json` /
  `parse_merged_pr_surfaces_json`).
- **File:line (before fix)**: `fetch_open_issue_numbers` (was ~429-447),
  `fetch_open_issues_with_body` (was ~625-651), `fetch_closed_issue_numbers`
  (was ~653-670).
- **Fix**: extracted `parse_issue_numbers_json` (shared by both the
  open-issue-number and closed-issue-number fetches — same `number`-only
  shape) and `parse_issue_numbers_with_body_json`. Both `pub(crate)`,
  fixture-tested.
- **Tests (verbatim, all green)**: `parse_issue_numbers_json_extracts_numbers`,
  `parse_issue_numbers_json_skips_rows_missing_number`,
  `parse_issue_numbers_json_empty_array_yields_empty`,
  `parse_issue_numbers_with_body_json_extracts_number_and_body`,
  `parse_issue_numbers_with_body_json_defaults_missing_body_to_empty`,
  `parse_issue_numbers_with_body_json_skips_rows_missing_number`.

### Finding 5 — `tachi_task` markdown briefing dropped `issue_freshness`
- **Codex claim**: the JSON response has carried `issue_freshness` since
  #1000 shipped; `copilot_ops/feature_briefing/markdown.rs`'s renderer never
  rendered it (only `tachi_memory`'s `agent_markdown::format_briefing` did).
- **File:line (before fix)**: `copilot_ops/feature_briefing/markdown.rs:3-109`
  (`format_feature_briefing_markdown` had no freshness section at all).
- **Fix**: extracted the freshness-rendering block out of
  `agent_markdown/briefing.rs::format_briefing` into a new shared
  `pub(crate) fn render_issue_freshness_section(&Value) -> Option<String>`
  (re-exported via `agent_markdown.rs`), called from BOTH
  `format_briefing` (unchanged behavior) and
  `format_feature_briefing_markdown` (new — the actual fix). Keeps the two
  renderers' wording (including the finding-7 fix below) from drifting
  apart.
- **Tests (verbatim, all green)**:
  `feature_briefing_markdown_renders_issue_freshness_section_when_nonempty`,
  `feature_briefing_markdown_omits_issue_freshness_section_when_empty`,
  `feature_briefing_markdown_handles_missing_issue_freshness_field`
  (new, in `copilot_ops/feature_briefing/markdown.rs`); pre-existing
  `format_briefing_renders_issue_freshness_section_when_nonempty` /
  `format_briefing_omits_issue_freshness_section_when_empty` in
  `agent_markdown.rs` still pass unchanged after the extraction.

### Finding 6 — churn threshold=0 + merged-PR window
- **Codex claim (a)**: `churn_threshold=0` makes
  `touching_pr_numbers.len() >= 0` trivially true — every inactive issue
  with a non-empty file-surface flags regardless of actual churn evidence.
- **Codex claim (b)**: merged-PR "recency" for the churn heuristic was
  bounded only by `--limit` (a PR count), not the same `activity_since`
  window the issue-activity side uses.
- **File:line (before fix)**: `router.rs:370-382` (threshold plumbing),
  `issue_freshness.rs:388-399` (old `scan_same_surface_churn` threshold
  check), `issue_freshness.rs:681-714,777-791` (`fetch_and_scan_same_surface_churn`,
  `fetch_merged_pr_surfaces`).
- **Fix (a)**: clamped `churn_threshold.max(1)` in TWO places — the pure
  `scan_same_surface_churn` itself (the invariant belongs to the function
  that owns "what counts as churn") AND in `router.rs` before calling it
  (so `churn_threshold_requested` / `churn_threshold_effective` /
  `churn_threshold_clamped` can be reported on the response). Negative
  values are already rejected at param deserialization (`churn_threshold:
  Option<u32>`, `tachi-params/src/gh.rs:157`) — only 0 is reachable.
- **Fix (b)**: added `MergedPrSurface::merged_at: String` (from `gh pr list
  --json ...,mergedAt`), extracted pure `filter_merged_prs_since(merged_prs,
  activity_since)` (missing/empty `mergedAt` sorts before any real RFC3339
  timestamp, so it's treated as NOT recent, never always-included), called
  from `fetch_and_scan_same_surface_churn` before the pure scan.
- **Tests (verbatim, all green)**:
  `scan_same_surface_churn_threshold_zero_does_not_flag_untouched_surface`,
  `scan_same_surface_churn_threshold_zero_still_flags_when_actually_touched`,
  `filter_merged_prs_since_excludes_prs_merged_before_cutoff`,
  `filter_merged_prs_since_includes_pr_merged_exactly_at_cutoff`,
  `filter_merged_prs_since_excludes_pr_with_missing_merged_at`,
  `scan_same_surface_churn_windowed_prs_below_threshold_does_not_flag`,
  `parse_merged_pr_surfaces_json_missing_merged_at_defaults_empty` (updated
  existing `parse_merged_pr_surfaces_json_extracts_touched_paths` to assert
  `merged_at` too).

### Finding 7 — wording: "verdict rows" contradicts the module's own frozen posture
- **Codex claim**: `agent_markdown/briefing.rs:60-64` blurb read "...these
  are judgment-free verdict rows, not auto-closes." — the module's whole
  posture (its own doc comment, `issue_freshness.rs:7-11`) is "圈候选不判决"
  (circle the candidate, do not judge it); calling the rows "verdict rows"
  says the opposite.
- **Fix**: wording changed to "...these are judgment-free review
  candidates, not verdicts or auto-closes." in the new shared
  `render_issue_freshness_section` (so both `tachi_memory` and `tachi_task`
  markdown surfaces get the corrected wording). Grepped the whole crate for
  the old string post-fix — zero remaining occurrences.

## Verification run (this baton, HEAD `ec97c465`, uncommitted working tree)

- `cargo fmt --check -p tachi-server -p tachi-params -p memcore` → clean.
- `cargo clippy -p tachi-server -p tachi-params -p memcore --all-targets --
  -D warnings` → clean, 0 warnings.
- `cargo test -p tachi-server --lib gh_ops::issue_freshness` → **60/60
  green** (includes all new round-3 tests).
- `cargo test -p tachi-server --lib gh_ops::router` → 4/4 green.
- `cargo test -p tachi-server --lib gh_ops` → **155/155 green** (full
  gh_ops surface, safe_merge/ship/transport included — no regression from
  the `repo_root` plumbing change to `fetch_and_scan_zombies`'s signature).
- `cargo test -p tachi-server --lib copilot_ops` → 23/23 green.
- `cargo test -p tachi-server --lib agent_markdown` → 6/6 green (both
  pre-existing freshness-section tests still pass after the extraction).
- `cargo test -p tachi-server --lib` (full crate) → **1655 passed, 1
  failed** (`bootstrap::serve::stdio::tests::stdio_proxy_allows_explicit_cross_project_read`,
  timeout). Re-ran that ONE test standalone → **passes**. This is a known
  flaky test family under concurrent/CI-loaded test runs (see memory card
  `feedback_test_worktree_race` / prior `fix/987-997-flaky-hygiene` deflake
  work) — unrelated to any file touched this baton (`bootstrap/serve/stdio`
  vs `gh_ops`/`agent_markdown`/`copilot_ops`). Not a regression introduced
  here; flagged honestly rather than silently re-run-until-green.
- `cargo test -p tachi-params -p memcore --lib` → 338/338 green.

## Honest gaps (not silently skipped)

- Finding 3's reap-error surfacing has no test that injects a genuine DB
  failure (would require corrupting the on-disk SQLite mid-test; the
  existing `test_server()` harness has no such fixture). The success-path
  reap logic is covered by pre-existing tests
  (`reap_stale_kind_rows_drops_rows_missing_from_fresh_hit_set`,
  `reap_stale_kind_rows_only_touches_its_own_kind`); the NEW error-plumbing
  code path (`router.rs`'s `match` arms → `reap_errors`/
  `reap_incomplete_kinds`) is compiled and typechecked but not exercised by
  a failure-injection test.
- No live-`gh`/live-`git` integration test for `resolve_merge_commit_message`
  (finding 1) — it's a thin `git log` shell-out; correctness was verified by
  reasoning through `scan_zombies`'s pure-function fixture tests (which
  cover the text-concatenation logic it feeds) plus manual confirmation that
  `git -C <path> log -1 --format=%B <sha>` is a well-formed invocation
  (`ship.rs`'s existing `run_git`/`run_git_os` use the identical `-C`
  pattern).
