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
