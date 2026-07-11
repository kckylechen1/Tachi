# #1001 presence claims — evidence log (Wizard/Sonnet seat)

Branch: `wizard/1001-presence-claims` (local name only — `feat/1001-presence-claims`
is already checked out dirty in another worktree, `wf_adfaee31-047-5`, which holds
the original salvage from the dead seat at an older base 8f8c22fd; per `他物勿动`
I did not touch it. This worktree's branch pushes to the same eventual PR target;
remote push uses a distinct branch name to avoid collision — see PR description.)

Base: origin/main @ c1622cbd (matches `git merge-base HEAD origin/main`).

## Scope checklist (issue #1001)

1. [x] Claims storage (memcore `session_claims` table + lease lifecycle) — DONE, tested green.
2. [ ] Zero-ceremony hooks (briefing/intake/dispatch auto-register/heartbeat, degrade-to-no-op) — wired into intake + dispatch; briefing wired for READ (board projection) but not yet degrade-to-no-op tested end-to-end.
3. [ ] Briefing 工位表 — wired into both briefing surfaces (tachi_memory + tachi_task feature briefing) + markdown formatter; NOT yet compiled/tested at tachi-server level.
4. [ ] Collision warnings — `collision_warnings()` implemented + unit tested (pure logic, no DB), wired into briefing read paths.
5. [ ] Manual claim/release actions — added `tachi_memory(action='claim'|'release')`, inventory/policy updated.

## Test evidence so far

### memcore (real, ran, green)

```
cargo test -p memcore --lib session_claims
running 17 tests
test db::session_claims::tests::fresh_heartbeat_survives_ttl_boundary ... ok
test db::session_claims::tests::ttl_boundary_is_exact_not_off_by_one ... ok
test db::session_claims::tests::stale_heartbeat_past_ttl_is_reaped ... ok
test db::session_claims::tests::unparsable_heartbeat_is_treated_as_stale_fail_closed ... ok
test db::session_claims::tests::state_parse_rejects_unknown ... ok
test db::session_claims::tests::list_active_claims_excludes_stale_and_released ... ok
test db::session_claims::tests::insert_and_get_roundtrip_defaults_to_active ... ok
test db::session_claims::tests::release_by_dispatch_id_targets_the_active_claim ... ok
test db::session_claims::tests::release_is_idempotent_no_second_write ... ok
test db::session_claims::tests::upsert_creates_separate_claims_for_different_issues ... ok
test db::session_claims::tests::list_filters_by_state ... ok
test db::session_claims::tests::upsert_heartbeats_existing_active_claim_for_same_identity_no_duplicate_row ... ok
test db::session_claims::tests::release_missing_claim_reports_not_found ... ok
test db::session_claims::tests::upsert_inserts_fresh_claim_when_none_exists ... ok
test db::session_claims::tests::duplicate_claim_id_is_rejected_not_silently_overwritten ... ok
test db::session_claims::tests::release_flips_active_to_released_and_stamps ... ok
test db::session_claims::tests::two_concurrent_sessions_each_see_the_others_claim ... ok

test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 329 filtered out; finished in 0.22s
```

### tachi-server (NOT YET RUN — pending, honest status)

`crates/tachi-server/src/claims_ops.rs` has 8 pure-logic unit tests
(collision_warnings + generate_claim_id) but the crate has not been compiled yet
in this session (shared cargo target is under contention from concurrent
worktrees). This is the next immediate step — see "still to do" below.

## Still to do (as of this checkpoint push)

- Compile+test `tachi-server`, `tachi-params`, `tachi-hub` (touched crates).
- `cargo fmt --check` + `cargo clippy -D warnings` on touched crates.
- Add an integration-style test exercising `auto_register_or_heartbeat_claim`'s
  degrade-to-no-op path against a real `MemoryServer` fixture (poisoned/missing
  table → briefing still succeeds) — issue's explicit acceptance criterion.
- Add a two-session "both see each other in briefing" integration test at the
  tachi-server layer (memcore-layer version already covered).
- Verify `f919_tachi_memory_actions_are_all_classified` and other action-policy
  consistency tests pass with `claim`/`release` added.
- Final commit message + PR per mission contract.
