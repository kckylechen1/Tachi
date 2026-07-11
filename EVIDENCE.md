# #1001 presence claims — evidence log (Wizard/Sonnet, baton 4)

Branch pushed as `wizard/1001-presence-claims-baton4` (packet named
`feat/1001-presence-claims`, which is occupied dirty in another worktree —
`wf_adfaee31-047-5`, at an older base 8f8c22fd, unrelated content; per `他物勿动`
not touched). Baton 1's actual work (`wizard/1001-presence-claims` @ 72935579)
was checked out detached and continued here; base is `origin/main @ c1622cbd`.

## Baton lineage (for the merge/PR reviewer)

- **Baton 1** (this seat, earlier in the day): committed `wip(1/5)` — memcore
  `session_claims` storage/lease layer (17 tests, green) plus a first pass at
  `claims_ops.rs`/hooks/briefing wiring that had NOT been compiled at the
  tachi-server layer yet (see the commit's own honest EVIDENCE.md).
- **Baton 3** (a since-dead seat, `agent-a4a0b08faaed277f6`): picked up
  baton 1's uncommitted tachi-server layer, fixed the compile errors (type
  annotations `with_global_store` needed after a closure-inference change,
  `dispatch.rs`'s reference to a nonexistent `params.branch`, missing new
  `TachiMemoryParams` fields in ~14 test fixtures across the facade test
  suite), added the consolidated `presence_briefing_section` single-call-point
  (Scope item 3), and wrote `claims_tests.rs` (server-layer degrade-to-no-op +
  two-session + collision tests) — but died before committing. Its staged
  diff was exported to scratchpad as `presence-baton3-salvage.patch` +
  `presence-untracked/.../claims_tests.rs` for this baton to review and apply.
- **Baton 4** (this session): reviewed baton 3's salvage line-by-line against
  the actual `TachiDispatchParams`/`TachiMemoryParams` definitions (confirmed
  each "fix" was a genuine compile error, not a style choice — e.g.
  `TachiDispatchParams` truly has no `branch` field), applied it, ran
  `cargo fmt`, then compiled and ran the full touched-crate test surface for
  the first time in this lineage. Everything below is this baton's own
  verification, not inherited claims.

## Scope checklist (issue #1001) — all 5 done, verified this baton

1. [x] Claims storage (memcore `session_claims` table + lease lifecycle) —
   17/17 tests green. TTL boundary is an exact `>` comparison on parsed
   `chrono::DateTime`, not string comparison (checked `is_claim_stale`
   directly — no risk of the known opus string-time-compare bug here).
   Schema DDL added to `BASE_SCHEMA_SQL` (same idempotent
   `CREATE TABLE IF NOT EXISTS` pattern as the sibling `exec_envs` table from
   #894, immediately above it in `ddl.rs`) and confirmed it runs inside the
   #984 compatibility transaction (`init_schema_with_label_mut`'s single
   `BEGIN IMMEDIATE` boundary, after the schema-version gate) — not a
   standalone migration that could escape that discipline.
2. [x] Zero-ceremony hooks — wired at `task_lifecycle/issue_flow.rs:96`
   (intake) and `dispatch_ops/dispatch.rs:284` (dispatch); briefing reads via
   `presence_briefing_section`. Degrade-to-no-op verified for real: dropped
   the `session_claims` table on a live `MemoryServer` fixture and confirmed
   both the raw hook and the consolidated briefing section return empty
   without panicking or erroring (`claims_tests.rs`,
   `auto_register_hook_degrades_to_no_op_when_claims_table_is_missing` +
   `briefing_section_is_empty_not_erroring_when_claims_table_is_missing`).
3. [x] Briefing 工位表 — single call point `claims_ops::presence_briefing_section`
   (added this baton), called from both briefing surfaces
   (`facade_memory_ops/briefing_ops.rs` for `tachi_memory(action='briefing')`,
   `copilot_ops/feature_briefing/handlers.rs` for `tachi_task` feature
   briefing) instead of each re-deriving board+warnings inline. Markdown
   rendering lives in the existing `agent_markdown/briefing.rs` formatter as
   an additive section (not a new module — that file is already the one
   shared markdown assembly point every briefing section goes through).
   Two-session visibility verified against a real two-`MemoryServer`-handle
   setup sharing one on-disk DB (`two_sessions_each_see_the_others_claim_through_the_real_server`).
4. [x] Collision warnings — both categories tested: same-`issue_ref`
   double-claim (pure-logic unit tests in `claims_ops.rs` + a server-layer
   replay of the 2026-07-11 near-miss scenario from the issue body,
   `collision_warning_fires_when_second_session_claims_the_same_issue`) and
   `declared_file_scope` overlap (`collision_warns_on_file_scope_overlap`).
5. [x] Manual claim/release — `tachi_memory(action='claim'|'release')` wired
   in `facade_memory_ops/mod.rs`, `action_inventory.rs`/`action_policy.rs`
   updated (23/25 soft-max actions, `f0_memory_and_verify_counts` test
   already covers the exact count and stayed green).

## File-overlap note (briefing 冲突面, mission's explicit concern)

`agent_markdown/briefing.rs`, `facade_memory_ops/briefing_ops.rs`, and
`copilot_ops/feature_briefing/handlers.rs` are also touched by sibling
branches `feat/964-sticky` and `feat/1000-issue-freshness` (confirmed via
`git diff origin/main origin/feat/964-sticky -- <same files>` and the #1000
equivalent — both non-empty). This is not something a single PR can design
away — three independent presence/sticky/freshness sections each add one
paragraph to the same shared formatter and the same two briefing assemblers.
This PR's footprint in each file is a single additive block (new `if` guard,
new match arm) rather than interleaved edits to existing logic, which keeps
the eventual conflict resolution mechanical rather than semantic. Flagging
for whoever merges these three, in sequence.

## Test evidence (this baton — actually ran)

```
cargo test -p tachi-server --lib -- claims
14 passed; 0 failed  (7 pure-logic in claims_ops::tests, 4 in tests::claims_tests
                       server-layer, 3 pre-existing unrelated "claims"-substring
                       matches from dispatch/gh_ops/foundry_ops reclaim tests)

cargo test -p memcore --lib -- session_claims
17 passed; 0 failed

cargo test -p tachi-server --lib -- facade_tests dispatch_tests memory_tests
287 passed; 0 failed; 1 ignored

cargo test -p tachi-server --lib -- action_policy f919
5 passed; 0 failed  (action-policy classification consistency, incl.
                      f919_tachi_memory_actions_are_all_classified)

cargo test -p tachi-params -p tachi-hub -p memcore --lib
tachi-params: 10 passed, tachi-hub: 53 passed, memcore: 344 passed; 0 failed

cargo test -p tachi-server --lib   (FULL crate suite)
1613 passed; 1 failed; 2 ignored
  — failure: bootstrap::serve::stdio::tests::stdio_proxy_allows_explicit_cross_project_read
    (timeout under full-suite concurrent load). Re-ran in isolation: passes in
    2.94s. This is a pre-existing, already-documented flaky test family (see
    commit 4d8d5f58 "fix(#987,#997): deflake stdio_proxy / component_check /
    subprocess-reap test families", currently being hardened further on a
    separate active branch fix/987-997-flaky-hygiene). Not caused by this
    diff — none of #1001's changes touch bootstrap/stdio/cross-project-read
    code paths.

cargo fmt --check -p tachi-server -p memcore -p tachi-params -p tachi-hub
clean (exit 0)

cargo clippy -p tachi-server -p memcore -p tachi-params -p tachi-hub \
  --all-targets -- -D warnings
clean, 0 warnings
```

## Not done / explicitly out of scope

- `EnvResolution` (dispatch's #894 env binding) exposes `cwd()`/`env_id()`/
  `stamp()` but no `branch()`; the dispatch hook's presence claim is written
  with `branch: None`. A branch value could be recovered with an extra
  `memcore::get_exec_env(env_id)` lookup, but that's an enrichment of an
  advisory display field, not required by any #1001 acceptance criterion
  (the issue's own example keys the board on issue/lane, not branch) — left
  as a possible future nicety rather than scope creep here.
- No branch-name collision with `feat/1001-presence-claims` was resolved by
  renaming that worktree/branch — per `他物勿动` that worktree's dirty state
  belongs to whoever is driving it; this PR targets `main` from
  `wizard/1001-presence-claims-baton4` instead.
