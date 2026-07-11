# #964 round-2 rework — verification evidence

Base commit: `837d1c301dba5f42989dcea20e0337eeaeb9b05f` (origin/feat/964-sticky)

Local verification note: the shared `CARGO_TARGET_DIR` (`$HOME/.cache/sigil-shared-target`)
produced a phantom compile error (`cannot find function standardize_sticky_path in module
memcore::path_router`) even though the function genuinely exists at
`crates/memcore/src/path_router.rs:185` — this is the documented
shared-target-contamination failure mode (stale/foreign `memcore` object in the shared
cache from a concurrent worktree). Switched to
`CARGO_TARGET_DIR=<worktree>/isolated-target` for all verification below, which compiles
clean. Oz should re-verify on its own dedicated target.

## fmt --check

```
$ cargo fmt -p tachi-server -- --check
(no output, exit 0)
```

## clippy -p tachi-server --all-targets --no-deps -- -D warnings

```
$ cargo clippy -p tachi-server --all-targets --no-deps -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 9.46s
(exit 0, zero warnings)
```

## cargo check -p tachi-server --all-targets

```
Finished `dev` profile [unoptimized + debuginfo] target(s) in 56.62s
(exit 0)
```

## Item 1 — TACHI_AGENT_SEAT (BUG CP2) — RED then GREEN

RED (identity.rs's delivery-path fallback temporarily reverted to read
`TACHI_PROFILE` instead of `TACHI_AGENT_SEAT`, new tests kept as-is):

```
$ cargo test -p tachi-server --lib sticky_ops::tests::cp2_
running 4 tests
test sticky_ops::tests::cp2_identity_less_caller_resolves_to_leader ... ok
test sticky_ops::tests::cp2_caller_with_explicit_agent_id_consumes_only_its_own_addressed_stickies ... ok
test sticky_ops::tests::cp2_tachi_profile_env_alone_no_longer_resolves_a_seat ... FAILED
test sticky_ops::tests::cp2_param_less_caller_with_tachi_agent_seat_env_does_not_consume_broadcast ... FAILED

---- sticky_ops::tests::cp2_tachi_profile_env_alone_no_longer_resolves_a_seat stdout ----
thread '...' panicked at crates/tachi-server/src/sticky_ops/tests.rs:824:5:
assertion `left == right` failed: TACHI_PROFILE alone (no TACHI_AGENT_SEAT, no param) must resolve to leader, not a tool-profile-named seat
  left: Some("standard")
 right: None

---- sticky_ops::tests::cp2_param_less_caller_with_tachi_agent_seat_env_does_not_consume_broadcast stdout ----
thread '...' panicked at crates/tachi-server/src/sticky_ops/tests.rs:737:9:
assertion `left == right` failed: param-less caller must resolve via the TACHI_AGENT_SEAT env fallback
  left: Some("standard")
 right: Some("wizard")

test result: FAILED. 2 passed; 2 failed; 0 ignored; 0 measured; 1607 filtered out; finished in 0.31s
```

GREEN (fix restored):

```
$ cargo test -p tachi-server --lib sticky_ops::
running 18 tests
... (all 18 ok, see full run below)
test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 1593 filtered out; finished in 0.67s
```

## Item 2 — CP3 doc honesty — GREEN (doc-verification test, no behavior change)

`cp3_row_stuck_unread_after_cas_win_is_still_recoverable_via_include_read` hand-simulates
the exact crash window (CAS commits via `try_claim_sticky`, `mark_claimed` never runs) and
asserts: (a) the normal unread-claim path never re-delivers it (the "silently drop" half the
doc is now honest about), and (b) `include_read=true` still recovers the original text (the
recovery-escape claim). This test is new code-verification, not a regression test against a
prior bug — there was no code change for Item 2, only the module doc. It passed on first run
(see aggregate run below); no red/green pair applicable since no behavior changed.

## Item 3 — render-time scrub (CP4 belt-and-suspenders) — RED then GREEN

RED (briefing.rs's sticky render loop temporarily reverted to skip `scrub_secrets`):

```
$ cargo test -p tachi-server --lib agent_markdown::tests::format_briefing_scrubs_secrets_in_sticky_text
---- agent_markdown::tests::format_briefing_scrubs_secrets_in_sticky_text_even_if_row_bypassed_write_scrub stdout ----
rendered briefing must never contain the raw secret token:
## Tachi briefing
...
### 📌 Sticky notes (unread) [AUTHORITY: WORKFLOW STATE]
...
- from **wizard**: here is the key: sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ012345
...
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 1610 filtered out; finished in 0.01s
```

GREEN (fix restored):

```
$ cargo test -p tachi-server --lib agent_markdown::tests::
running 4 tests
test agent_markdown::tests::format_briefing_compact_caps_verification_gates ... ok
test agent_markdown::tests::format_briefing_truncates_long_checkpoint_titles ... ok
test agent_markdown::tests::format_briefing_compact_caps_section_rows ... ok
test agent_markdown::tests::format_briefing_scrubs_secrets_in_sticky_text_even_if_row_bypassed_write_scrub ... ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 1607 filtered out; finished in 0.01s
```

## Aggregate green run (all sticky/dispatch-config/briefing tests, fix in place)

```
$ cargo test -p tachi-server --lib -- sticky_ops:: dispatch_ops::dispatch::tests:: agent_markdown::tests::
running 33 tests
test dispatch_ops::dispatch::tests::mcp_cleanup_removes_temp_config_on_drop ... ok
test agent_markdown::tests::format_briefing_compact_caps_verification_gates ... ok
test agent_markdown::tests::format_briefing_truncates_long_checkpoint_titles ... ok
test agent_markdown::tests::format_briefing_compact_caps_section_rows ... ok
test agent_markdown::tests::format_briefing_scrubs_secrets_in_sticky_text_even_if_row_bypassed_write_scrub ... ok
test sticky_ops::tests::addressed_sticky_visible_only_to_named_seat ... ok
test dispatch_ops::dispatch::tests::flow_dispatch_slot_blocks_duplicate_active_task ... ok
test sticky_ops::tests::broadcast_sticky_visible_only_to_leader ... ok
test dispatch_ops::dispatch::tests::global_dispatch_slot_blocks_duplicate_active_task_without_flow_id ... ok
test dispatch_ops::dispatch::tests::dispatch_runs_root_uses_canonical_tachi_home_aliases ... ok
test dispatch_ops::dispatch::tests::flow_dispatch_slot_reclaims_stale_lock_when_run_status_is_missing ... ok
test sticky_ops::tests::briefing_claims_sticky_exactly_once_then_absent ... ok
test sticky_ops::tests::addressed_sticky_invisible_to_leader_and_other_seats ... ok
test dispatch_ops::dispatch::tests::generate_mcp_config_sets_owner_only_permissions ... ok
test sticky_ops::tests::cp3_row_stuck_unread_after_cas_win_is_still_recoverable_via_include_read ... ok
test sticky_ops::tests::gc_sweep_archives_expired_unread_stickies ... ok
test sticky_ops::tests::expired_sticky_hidden_from_unread_but_visible_in_archive ... ok
test sticky_ops::tests::concurrent_claim_smoke_single_winner_under_load ... ok
test sticky_ops::tests::is_claimed_reflects_successful_claim ... ok
test dispatch_ops::dispatch::tests::opencode_serve_dispatch_fails_fast_when_probe_auth_fails ... ok
test sticky_ops::tests::mark_claimed_error_branch_is_reachable_and_does_not_affect_cas_outcome ... ok
test sticky_ops::tests::ttl_expiry_matrix ... ok
test sticky_ops::tests::sticky_persists_and_reads_back ... ok
test sticky_ops::tests::sticky_leave_scrubs_secrets_in_storage_and_briefing_render ... ok
test sticky_ops::tests::try_claim_sticky_is_cas_not_naive_upsert ... ok
test dispatch_ops::dispatch::tests::opencode_serve_preflight_uses_dispatch_credential_env ... ok
test dispatch_ops::dispatch::tests::dispatch_rejects_bare_cwd_without_unmanaged_optin_or_env_id ... ok
test dispatch_ops::dispatch::tests::recover_orphaned_dispatch_runs_marks_working_runs_failed ... ok
test dispatch_ops::dispatch::tests::v2_auto_stage_rejects_unsupported_sandbox_before_plan_stage_spawn ... ok
test sticky_ops::tests::cp2_caller_with_explicit_agent_id_consumes_only_its_own_addressed_stickies ... ok
test sticky_ops::tests::cp2_identity_less_caller_resolves_to_leader ... ok
test sticky_ops::tests::cp2_param_less_caller_with_tachi_agent_seat_env_does_not_consume_broadcast ... ok
test sticky_ops::tests::cp2_tachi_profile_env_alone_no_longer_resolves_a_seat ... ok

test result: ok. 33 passed; 0 failed; 0 ignored; 0 measured; 1578 filtered out; finished in 1.62s
```

## Not run

Full workspace `cargo test` (all crates) was not run locally — scoped to `-p tachi-server`
per this packet's file ownership (`crates/tachi-server/**` only). Oz should run the full
suite (and the shared-target build) to confirm no cross-crate breakage and to get a clean
build off the shared cache once other worktrees vacate it.

---

# #964 round-3 rework — verification evidence

Base commit: `e1b1ab0ec855fc9a1736757d1646336c8020fed4` (origin/feat/964-sticky, round-2 HEAD)

Same shared-target-contamination note as round-2 applies again this round (phantom
`cannot find function standardize_sticky_path` against `$HOME/.cache/sigil-shared-target`
even though the function is present at `crates/memcore/src/path_router.rs:185`) — switched
to an isolated `CARGO_TARGET_DIR` under the session scratchpad for all verification below,
which compiles clean. Oz should re-verify on its own dedicated target.

## Finding 1 (CP2 — identity semantics, both halves)

- `crates/tachi-server/src/sticky_ops/identity.rs:25-41` — sender path
  (`fallback_agent_id`, used by `sticky_leave`'s `resolve_from_agent`) now falls back to
  `TACHI_AGENT_SEAT` instead of `TACHI_PROFILE`, matching the delivery path's chain
  (`resolve_caller_agent_id`, unchanged from round-2, already correct).
- `crates/tachi-server/src/dispatch_ops/dispatch.rs:285-301` — `agent_seat` is now
  `Some(dispatch_id.as_str())` instead of derived from `params.profile`/`agent_norm`.
  `dispatch_id` (`new_dispatch_id`) embeds a uuid suffix and is unique per dispatch call,
  so two workers dispatched on the identical `profile` (e.g. `codex_55_review`, the
  scenario codex's finding named) can no longer collide onto the same
  `TACHI_AGENT_SEAT`.
- `crates/tachi-server/src/sticky_ops/handlers.rs:71` — stale comment fix (said
  `TACHI_PROFILE`, the delivery chain's env fallback has read `TACHI_AGENT_SEAT` since
  round-2).
- `crates/tachi-server/src/facade_memory_ops/briefing_ops.rs:206` — same stale-comment fix.

### RED (sender-path fix reverted: `fallback_agent_id` back to reading `TACHI_PROFILE`)

```
$ cargo test -p tachi-server --lib cp2_round3
running 3 tests
test sticky_ops::tests::cp2_round3_sender_identity_never_resolves_to_tool_profile_string ... FAILED
test sticky_ops::tests::cp2_round3_sender_identity_uses_tachi_agent_seat_not_tachi_profile ... FAILED
test sticky_ops::tests::cp2_round3_sticky_leave_from_agent_uses_seat_not_profile ... FAILED

---- sticky_ops::tests::cp2_round3_sender_identity_never_resolves_to_tool_profile_string stdout ----
thread '...' panicked at crates/tachi-server/src/sticky_ops/tests.rs:876:5:
assertion `left == right` failed: sender identity must never resolve to a tool-profile string like 'standard' even with TACHI_PROFILE set
  left: "standard"
 right: "unknown-agent"

---- sticky_ops::tests::cp2_round3_sender_identity_uses_tachi_agent_seat_not_tachi_profile stdout ----
thread '...' panicked at crates/tachi-server/src/sticky_ops/tests.rs:904:5:
assertion `left == right` failed: sender identity must resolve via TACHI_AGENT_SEAT, ignoring TACHI_PROFILE entirely
  left: "standard"
 right: "wizard-worker-3"

---- sticky_ops::tests::cp2_round3_sticky_leave_from_agent_uses_seat_not_profile stdout ----
thread '...' panicked at crates/tachi-server/src/sticky_ops/tests.rs:950:5:
assertion `left == right` failed: from_agent must be the TACHI_AGENT_SEAT value, not the shared TACHI_PROFILE
  left: String("codex_55_review")
 right: "worker-a"

test result: FAILED. 0 passed; 3 failed; 0 ignored; 0 measured; 1612 filtered out; finished in 0.21s
```

### GREEN (fix restored)

```
$ cargo test -p tachi-server --lib cp2_round3
running 3 tests
test sticky_ops::tests::cp2_round3_sender_identity_never_resolves_to_tool_profile_string ... ok
test sticky_ops::tests::cp2_round3_sender_identity_uses_tachi_agent_seat_not_tachi_profile ... ok
test sticky_ops::tests::cp2_round3_sticky_leave_from_agent_uses_seat_not_profile ... ok
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 1612 filtered out; finished in 0.23s
```

### Dispatch-id-seat test (finding 1b), GREEN

`crates/tachi-server/src/dispatch_ops/dispatch/tests.rs::cp3_two_dispatches_on_same_profile_get_distinct_agent_seats`
calls `generate_mcp_config` twice with two freshly generated `dispatch_id`s (same shared
`profile` "codex_55_review"), reads back each generated MCP config JSON's
`mcpServers.tachi.env.TACHI_AGENT_SEAT`, and asserts they equal their respective
`dispatch_id`s, differ from each other, and never equal the shared profile string:

```
$ cargo test -p tachi-server --lib "dispatch_ops::dispatch::tests::"
running 12 tests
... (all 12 ok, including cp3_two_dispatches_on_same_profile_get_distinct_agent_seats)
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 1605 filtered out; finished in 1.51s
```

## Finding 2 (CP3 — doc-only narrowing, no behavior change)

`crates/tachi-server/src/sticky_ops.rs:57-77` — narrows the round-2 "Recovery escape" doc
comment: `include_read` recovery is scoped to the identity a sticky is actually visible to
(the leader cannot recover a sticky `to:`-addressed to a worker; only that worker's own
identity can), notes the `STICKY_DB_LIMIT` (500) row-scan cap and the `include_read`
branch's own 50-row page cap, and explicitly declines a leader-sees-all override as
out-of-scope (noted as a follow-up candidate). Doc-only; no test required per the
adjudication, and none of the existing CP3 crash-window tests changed behavior.

## Finding 3 (CP4 — scrub at the single row-load choke point)

- `crates/tachi-server/src/sticky_ops/pending.rs:9-27` — new `scrub_sticky_text_for_read`
  helper (mirrors `sticky_leave`'s `scrub_think_tags` -> `scrub_secrets` order), applied at
  both row-emit sites: `claim_unread_stickies_for_briefing`'s `delivered.push` (the
  unread/briefing/`sticky_check` path) and `list_or_claim_stickies`'s `include_read` branch
  (the archive path). Both are the sites every JSON route (briefing JSON compact+full,
  `sticky_check` JSON) and the markdown renderer consume — scrubbing here covers all of
  them from one place.
- `crates/tachi-server/src/agent_markdown/briefing.rs:55-63` — kept the existing
  belt-and-suspenders `scrub_secrets` re-scrub at the markdown render boundary (adjudication:
  "remove... only if... provably covers it, else keep both"); updated its comment to note
  the choke-point scrub is now the primary layer.

### RED (choke-point scrub reverted: both `pending.rs` emit sites back to raw `memo.text`)

```
$ cargo test -p tachi-server --lib cp4_round3
running 2 tests
test sticky_ops::tests::cp4_round3_briefing_json_route_masks_hand_inserted_raw_secret_row ... FAILED
test sticky_ops::tests::cp4_round3_sticky_check_json_route_masks_hand_inserted_raw_secret_row ... FAILED

---- sticky_ops::tests::cp4_round3_briefing_json_route_masks_hand_inserted_raw_secret_row stdout ----
thread '...' panicked at crates/tachi-server/src/sticky_ops/tests.rs:1019:5:
raw bearer token must never appear in the JSON `text` field the briefing JSON route serializes directly; got: heads up: Authorization: Bearer sk-cp4round3secretvalue000111222 is still live

---- sticky_ops::tests::cp4_round3_sticky_check_json_route_masks_hand_inserted_raw_secret_row stdout ----
thread '...' panicked at crates/tachi-server/src/sticky_ops/tests.rs:1052:5:
sticky_check (include_read=false) JSON text must never contain the raw token; got: heads up: Authorization: Bearer sk-cp4round3secretvalue000111222 is still live

test result: FAILED. 0 passed; 2 failed; 0 ignored; 0 measured; 1615 filtered out; finished in 0.25s
```

### GREEN (fix restored)

```
$ cargo test -p tachi-server --lib cp4_round3
running 2 tests
test sticky_ops::tests::cp4_round3_briefing_json_route_masks_hand_inserted_raw_secret_row ... ok
test sticky_ops::tests::cp4_round3_sticky_check_json_route_masks_hand_inserted_raw_secret_row ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 1615 filtered out; finished in 0.22s
```

These tests hand-insert a raw-secret row directly into the store via `test_entry`/`upsert`
(bypassing `sticky_leave`'s write-time scrub entirely — the exact "bypassed write-time
scrub" residual both scrub comments call out), then assert the JSON `text` field itself
(not a markdown rendering of it) is masked — discriminating the choke-point fix from the
pre-existing write-time-only + markdown-only-render scrub coverage.

## Full sticky_ops + dispatch aggregate, GREEN (fix in place)

```
$ cargo test -p tachi-server --lib sticky_ops::
running 23 tests
... (all 23 ok)
test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 1594 filtered out; finished in 0.65s/0.70s

$ cargo test -p tachi-server --lib "dispatch_ops::dispatch::tests::"
running 12 tests
... (all 12 ok)
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 1605 filtered out; finished in 1.51s/1.62s
```

## fmt --check (round-3)

```
$ cargo fmt --check -p tachi-server
(no output, exit 0 — after one `cargo fmt -p tachi-server` pass to fix line-wrap on the new tests)
```

## clippy -p tachi-server --all-targets --no-deps -- -D warnings (round-3)

```
$ cargo clippy -p tachi-server --all-targets --no-deps -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 25.80s
(exit 0, zero warnings — required adding #[allow(clippy::await_holding_lock)] to the new
cp2_round3_sticky_leave_from_agent_uses_seat_not_profile test, matching the existing
pattern used by generate_mcp_config_sets_owner_only_permissions for the same lint)
```

## Not run (round-3)

Full workspace `cargo test` (all crates) and the shared `CARGO_TARGET_DIR` build were not
run locally this round either, for the same reasons as round-2 (scoped to `-p tachi-server`
per file ownership; shared target dir showed contamination from a concurrent worktree).
Oz should run the full suite off its own dedicated target.

## Tooling note (unrelated to code correctness)

This round hit a prolonged, severe sandbox transport instability partway through (many
consecutive tool-call failures across `git`/`Read`/`cargo`, with only intermittent success
on trivial `echo`). During that window one `Edit` call was issued to `pending.rs` referencing
a nonexistent placeholder constant, under the (correct, in hindsight) assumption that the
tool transport might not have applied it — once the transport recovered, `git diff` confirmed
the placeholder edit never actually landed on disk, so no revert was needed. Recorded here for
completeness; it does not affect the verified code above.
