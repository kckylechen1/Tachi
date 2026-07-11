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

## Round 2 (codex verdict on PR #1007, all 5 items) — commit ac435923

Codex's review found 1 BUG (claim/release wiring incomplete), 1 CONCERN (no
DB-level identity constraint), 1 Gap (scope-collision warning structurally
unreachable), and 2 more BUGs (task-markdown drops presence, unescaped
interpolation). All five adopted and fixed in this commit.

1. **complete/cancel → release_claim wiring.** New
   `claims_ops::release_claim_for_dispatch(server, dispatch_id, reason)`
   (fail-safe: warns, never fails the host action) is called from
   `complete_ops/handler.rs`'s `if let Some(ref did) = params.dispatch_id`
   block (reason="complete") and from `task_facade.rs`'s
   `handle_tachi_task_cancel` (reason="cancel"), the latter BEFORE the
   already-terminal early-return so a stuck-active claim from a dispatch
   that reached terminal state through some other path (e.g. the watchdog)
   still gets cleaned up on a late cancel call.
2. **DB-unique identity.** New `v12_session_claims_unique_identity`
   migration (`memcore/src/db/migrations/session_claims_identity.rs`)
   creates `idx_session_claims_identity_active`, a partial UNIQUE index on
   `COALESCE(session_client,''), COALESCE(issue_ref,''), COALESCE(flow_id,'')
   WHERE state='active'`. Verified directly against sqlite3 3.51 before
   wiring into Rust: (a) duplicate active triple → UNIQUE violation, (b) two
   active rows both NULL in the same column → still collide (the COALESCE
   closes the NULL≠NULL trap), (c) a released duplicate does NOT block a
   fresh active claim for the same identity. `EXPECTED_SCHEMA_VERSION`
   bumped 11→12; the same DDL is also added directly to
   `BASE_SCHEMA_SQL` so brand-new DBs get the index without waiting on the
   migration pass.
3. **Live scope collision.** `presence_briefing_section` now resolves the
   calling session's own identity, looks up its own live claim's
   `declared_file_scope` (preferring one matching the given `issue_ref`),
   and forwards that as `collision_warnings`' `new_scope` — previously
   always `&[]`, which made the file-scope-overlap branch of
   `collision_warnings` dead code from every briefing call site. Also fixed
   `exclude_session_client` (was hardcoded `None`), closing a
   self-collision bug the new discrimination test caught incidentally: a
   session re-reading its own briefing was reporting its own claim as a
   "double-claim" against itself.
4. **feature_briefing markdown presence.** New
   `markdown_presence_section` in `copilot_ops/feature_briefing/markdown.rs`
   mirrors `agent_markdown::briefing::format_briefing`'s existing presence
   rendering (same row shape); wired into
   `format_feature_briefing_markdown`'s output. Empty when both board and
   warnings are empty (no stray heading).
5. **Sanitized rendering.** `claims_ops::sanitize_presence_field(raw, cap)`
   strips Unicode control chars (incl. `\n`/`\r`), collapses to one line,
   and caps+ellipsizes; routed through both `briefing_claims_board` (board
   rows) and `collision_warnings` (warning strings — session_client,
   issue_ref, and the fully-composed warning line are all sanitized) so
   both markdown renderers inherit it from one source. Caps: 64 chars for
   identifier-shaped fields, 160 for warning/overlap text.

### Red → green proof (items 1, 3, 5 — mission-required)

Each production fix was temporarily reverted in place (sed/edit, restored
immediately after) and the new discrimination tests re-run:

- **Item 1**: reverted both `release_claim_for_dispatch(...)` call sites to
  a no-op comment → `tachi_complete_releases_the_presence_claim_...`,
  `tachi_task_cancel_releases_the_presence_claim_...`, and
  `tachi_task_cancel_on_already_terminal_dispatch_still_releases_the_claim`
  all FAILED (claim still `Active` after complete/cancel). Restored →
  all green.
- **Item 3**: reverted `presence_briefing_section` to
  `collision_warnings(&live, None, issue_ref, &[])` (the pre-fix shape) →
  `briefing_surfaces_file_scope_collision_using_the_calling_sessions_own_declared_scope`
  and `briefing_does_not_self_collide_on_its_own_declared_scope` both
  FAILED. Restored → both green.
- **Item 5**: reverted `sanitize_presence_field` to a passthrough
  (`raw.to_string()`, cap ignored) → 5 of the sanitize/collision
  discrimination tests FAILED
  (`sanitize_presence_field_strips_newlines_and_collapses_to_one_line`,
  `..._strips_markdown_instruction_like_injection`,
  `..._caps_length_with_ellipsis`, `collision_warning_line_is_sanitized_end_to_end`,
  `briefing_board_row_is_sanitized`). Restored → all 5 green.

### Test evidence (round 2 — actually ran)

```
cargo test -p memcore --lib   (FULL crate)
350 passed; 0 failed; 2 ignored   (includes golden_corpus_* — untouched, still green)

cargo test -p tachi-server -p memcore -- claim   (targeted)
34 passed (memcore) + 29 passed (tachi-server); 0 failed

cargo test -p tachi-server -- presence            → 8 passed; 0 failed
cargo test -p tachi-server -- briefing             → 33 passed; 0 failed
cargo test -p tachi-server -- markdown_sections    → 2 passed; 0 failed
cargo test -p tachi-server -- presence_claim_release → 2 passed; 0 failed
cargo test -p tachi-server -- task_control          → 6 passed; 0 failed

cargo test -p tachi-server --lib   (FULL crate suite)
1598 passed; 1 failed; 2 ignored
  — same pre-existing flake as baton 4's own evidence log above:
    bootstrap::serve::stdio::tests::stdio_proxy_allows_explicit_cross_project_read
    (timeout under concurrent multi-agent build load on this shared machine;
    reproduced IDENTICALLY on the pre-round-2 baseline via `git stash`, and
    passes cleanly in isolation both before and after this diff — not caused
    by this round's changes, which never touch bootstrap/stdio).

cargo fmt --check -p memcore -p tachi-server        → clean (exit 0)
cargo clippy -p memcore -p tachi-server --all-targets -- -D warnings
                                                     → clean, 0 warnings
```

Note: this round's local verification ran under
`CARGO_TARGET_DIR=/private/tmp/.../scratchpad/isolated-target-1001` rather
than the shared `$HOME/.cache/sigil-shared-target` — the shared target was
under concurrent write lock from other live agents mid-session
("Blocking waiting for file lock on artifact directory", plus a transient
`memcore` symbol-resolution error consistent with a half-written rlib),
so the build was isolated per the workspace-module contamination protocol
rather than trusted through the contention.

HEAD after this round: `ac435923`.

## Round 3 (codex verdict on PR #1007, 2 remaining BUGs) — commit `7a0b12e2`

Codex's round-3 review found the DB-level unique index from round 2 was
necessary but not sufficient (item #2 below), and that the round-2 sanitizer
deliberately left Markdown metacharacters + Unicode format/bidi chars
untouched (item #5, security). Both fixed this round.

1. **Identity upsert not atomic**
   (`crates/memcore/src/db/session_claims.rs:291-328`).
   `upsert_or_heartbeat_claim` was still read-then-write: a `SELECT` for an
   existing active claim inside a transaction, then a separate
   `UPDATE`/`INSERT`. This is a real TOCTOU race even inside a transaction —
   SQLite's default deferred transaction does not take a write lock until
   its first write, so two concurrent same-identity callers could both
   observe "no existing row" via their own `SELECT` before either commits,
   and one of the two subsequent `INSERT`s would then hit the round-2 unique
   index with no path to convert that failure into a heartbeat (the loser
   errors, and the host's fail-safe swallow turns that into a silent
   no-heartbeat no-op — exactly the bug the mission named). Replaced with a
   single `INSERT ... ON CONFLICT (COALESCE(session_client, ''),
   COALESCE(issue_ref, ''), COALESCE(flow_id, '')) WHERE state = 'active' DO
   UPDATE SET ... RETURNING claim_id` statement. The `ON CONFLICT` target is
   the exact expression list + partial predicate of
   `idx_session_claims_identity_active` — SQLite requires this to match the
   index verbatim to resolve against it. Verified the exact SQL (including
   the NULL-coalesced identity semantics — two active rows both NULL in
   `flow_id` still upsert onto each other) against real sqlite3 3.51
   (bundled version) in isolation before landing it in Rust.

2. **Markdown injection / bidi format chars (security)**
   (`crates/tachi-server/src/claims_ops.rs:74-131`).
   `sanitize_presence_field` bounded newlines/control chars/length but
   intentionally kept Markdown metacharacters (`* _ \` [ ] ( ) # < > | ~`),
   and `char::is_control()` only covers Unicode `Cc` — a bidi override
   character (`Cf` category) is fully "printable" by that check and survived
   untouched, letting an agent close `**bold**`, inject a
   `[link](javascript:...)`/HTML-shaped span, or visually reorder rendered
   text via a bidi override into a DIFFERENT session's briefing. Both
   briefing renderers (`agent_markdown/briefing.rs:87`,
   `feature_briefing/markdown.rs:157`) interpolate this field raw with no
   per-renderer escaping, so the fix lives at the single sanitize choke
   point both already route through, not in either renderer. Added
   `is_bidi_or_format_char` (explicit, documented code-point set — bidi
   embeds/overrides U+202A–202E, marks U+200E/U+200F, isolates
   U+2066–U+2069, ZWJ/ZWNJ, zero-width space, soft hyphen, BOM) and strip
   the Markdown metacharacter set, both applied before the existing
   control-char/collapse/cap steps.

### Test evidence (round 3 — this baton's own verification)

I did NOT run `cargo test`/`cargo clippy` myself this round (Wizard doesn't
build/test per the dispatch contract — that is Oz's job against the pushed
SHA). What I actually verified locally, and how:

- **Syntax/logic sanity, standalone**: extracted `sanitize_presence_field` +
  `is_bidi_or_format_char` into a scratch file, compiled with plain
  `rustc --edition 2021` (no workspace deps needed for this pure-logic
  function), ran it against the mission's literal adversarial example
  (`"**bold** [x](javascript:..) \u{202E}"` → `"bold x javascript:.."`, no
  `*`/`[`/`]`/`(`/`)`/bidi char survives) and a mixed bidi/format-char
  string (`"seat-a\u{202E}reversed\u{200E}\u{FEFF}\u{200B}tail"` →
  `"seat-areversedtail"`).
- **SQL correctness, standalone**: ran the exact `INSERT ... ON CONFLICT ...
  DO UPDATE ... RETURNING` statement (same expressions/predicate as
  production) against Python's bundled sqlite3 (3.51.0, same major version
  as the workspace's bundled rusqlite) with an in-memory DB carrying the
  real partial unique index — confirmed: (a) a second same-identity insert
  resolves onto the first row's `claim_id` and bumps `heartbeat_at`, (b)
  NULL-`flow_id` rows collide the same way, (c) both calls return the same
  `claim_id`.
- **Format**: `rustfmt --edition 2021 --check` on both changed files —
  clean (ran the standalone `rustfmt` binary directly on the two files, not
  a full `cargo fmt` across the workspace).
- **New tests added, NOT run by me** (Oz must run these against
  `7a0b12e2`):
  - `memcore::db::session_claims::tests::two_concurrent_same_identity_upserts_both_succeed_exactly_one_row`
    — real OS threads (`std::thread::spawn`), two separate `Connection`s to
    one shared file-backed `tempfile::NamedTempFile` DB (WAL mode, per
    `apply_connection_pragmas`), a `std::sync::Barrier` forcing both callers
    to hit the `INSERT ... ON CONFLICT` at the same instant. Asserts both
    results are `Ok`, both resolve to the identical `claim_id`, and exactly
    one active row exists for the raced identity afterward.
  - `tachi_server::claims_ops::tests::sanitize_presence_field_neutralizes_markdown_metacharacters`
  - `..._strips_bidi_and_format_chars`
  - `..._mission_example_renders_inert`
  - `..._malicious_claim_field_renders_inert_in_both_briefing_markdown_surfaces`
    — reproduces both renderers' exact `format!("- **{session}** →
    {target} (heartbeat {heartbeat})")` shape inline and asserts the
    malicious payload never survives as active markdown/bidi in either.

### Not done / explicitly out of scope this round

- Did not add a `proptest`/fuzz-style sweep over the full Unicode `Cf`
  category — `is_bidi_or_format_char` is a fixed, documented set of the
  specific bidi/format code points relevant to the described vector, not a
  general Unicode-database classifier (no such crate is a dependency of
  this workspace today; adding one for a single field sanitizer was judged
  out of scope for a round-3 bug-fix pass).
- Full `cargo test -p tachi-server -p memcore` / `cargo clippy --all-targets
  -- -D warnings` at `7a0b12e2` — pending Oz.

HEAD after this round: `7a0b12e2`. PR comment posted:
https://github.com/kckylechen1/tachi/pull/1007#issuecomment-4948548161

## Round 4 (codex verdict on PR #1007, round-3 sanitizer STILL incomplete)

Codex found round 3's `sanitize_presence_field` fix incomplete a 2nd time —
same root cause both times: a hand-maintained enumeration under-covering the
thing it claims to cover.

1. **Backslash missing from the Markdown-metachar set**
   (`crates/tachi-server/src/claims_ops.rs:129-131` — `MD_METACHARS`).
   Both renderers (`agent_markdown/briefing.rs:87`,
   `feature_briefing/markdown.rs:157`) write a fixed
   `format!("- **{session}** → ...")`. A `session_client` ending in a bare
   `\` used to sanitize through untouched, so the rendered text ended in
   `\**` — a CommonMark-compliant renderer reads that as a backslash-escaped
   literal `*` followed by one still-open `*`, so the intended closing `**`
   never actually closes, leaving emphasis open past where the fixed
   template intended it to end and letting the payload's own trailing
   content re-open Markdown structure in whatever follows in the document.
   Fixed: added `\\` to `MD_METACHARS`.

2. **Hand-maintained Cf enumeration, not a category check (root fix, not a
   2nd manual addition)** (`crates/tachi-server/src/claims_ops.rs:33-40,
   140-158`). Round 3's `is_bidi_or_format_char` was an explicit code-point
   list (bidi embeds/overrides, ZW joiners, BOM, soft hyphen) that covered
   the vector's most common instances but was, by round 3's own admission in
   this file ("no such crate is a dependency of this workspace today"), not
   a real Unicode `General_Category` classifier — codex's round-4 finding
   named the concrete gap: U+061C ARABIC LETTER MARK (`Cf`) is not in that
   list and survives. Rather than add U+061C to the list (the same fix
   shape that already failed once), replaced the function with a real
   category lookup: `icu_properties::CodePointMapData::<GeneralCategory>::new().get(ch)
   == GeneralCategory::Format`. `icu_properties` v2.2.0 was already resolved
   in this workspace's dependency graph before this change (`url` → `idna`
   → `idna_adapter` → `icu_properties`, confirmed via `cargo tree -i
   icu_properties`); adding it as a **direct** `tachi-server` dependency at
   the same already-locked version added exactly one edge to `Cargo.lock`
   (`"icu_properties"` under `tachi-server`'s existing dependency list) and
   compiled zero new crates — verified by diffing `Cargo.lock` before/after
   and by `cargo check -p tachi-server` showing no new `Compiling` lines
   beyond `icu_properties` itself. Uses the `compiled_data` feature (on by
   default), which embeds Unicode Character Database tables at compile
   time — no network fetch, no runtime data file, `no_std`+`alloc`
   compatible. `is_bidi_or_format_char` was removed (superseded by
   `is_unicode_format_char`), not kept alongside the new check, per the
   mission's "root-fix it, not another manual addition" instruction.

3. **Regression tests added** (`crates/tachi-server/src/claims_ops.rs`,
   `mod tests`, "#1001 round 4" block):
   - `sanitize_presence_field_leaves_ordinary_cjk_untouched` — pins that
     ordinary CJK (`你好世界`) survives unchanged, proving the fix strips
     `Cf` specifically, not "anything non-ASCII"/non-Latin (a category
     mistake in the other direction would have silently mangled every
     non-English presence field).
   - `sanitize_presence_field_strips_u061c_and_other_cf_chars_by_category` —
     the exact U+061C code point codex named, plus U+2062 INVISIBLE TIMES
     (a `Cf` char outside round 3's enumerated ranges), both now stripped.
   - `is_unicode_format_char_matches_cf_category_not_a_hand_list` — direct
     classifier check against a spread of `Cf` code points from several
     Unicode blocks (soft hyphen, ALM, BOM, invisible times, RLO) and known
     non-`Cf` code points (ASCII letter, CJK ideograph, digit, emoji),
     pinning the category lookup itself, not just this one call site's
     behavior.
   - `sanitize_presence_field_strips_backslash` — direct unit check.
   - `backslash_terminated_payload_cannot_escape_closing_bold_in_either_renderer`
     — end-to-end: reproduces both renderers' exact `format!("- **{session}**
     → {target} (heartbeat {heartbeat})")` literal (same pattern as round
     3's `malicious_claim_field_renders_inert_in_both_briefing_markdown_surfaces`)
     with a `\`-terminated `session_client`, asserts the rendered line
     contains no backslash at all and never produces the `\*` escape
     sequence in either surface.
   - Extended the existing
     `sanitize_presence_field_neutralizes_markdown_metacharacters` test's
     metachar loop to include `\\`.

### Test evidence (round 4 — this baton's own verification)

Unlike round 3, this round I DID run the real workspace test suite (not just
a standalone `rustc` extraction) — the fix required adding a real dependency
edge, so a standalone extraction couldn't prove it compiles/resolves against
this workspace's actual `Cargo.lock`.

- **`cargo check -p tachi-server`**: clean, no new crates compiled beyond
  `icu_properties` v2.2.0 itself (all its own transitive deps — `idna`,
  `idna_adapter`, `url`, `reqwest`, `rmcp` — were already being compiled for
  other reasons).
- **Red, standalone, before this round's fix**: extracted round-3's
  `sanitize_presence_field` + `is_bidi_or_format_char` verbatim into a
  scratch file, compiled with plain `rustc --edition 2021`, ran the two
  round-4 regression inputs against it:
  - `sanitize_presence_field("seat-a\\", 200)` → `"seat-a\\"` (backslash
    **survives** — confirms the round-3 gap codex found).
  - `sanitize_presence_field("seat-a\u{061C}mid\u{2062}tail", 200)` →
    `"seat-a\u{61c}midtail"` (U+061C **survives**; U+2062 happened to already
    be caught by round 3's `\u{2060}..=\u{2064}` range — a coincidence of
    that one code point, not evidence of category coverage, since U+061C
    sits outside every range in that list).
- **Green, real workspace, after this round's fix**:
  `cargo test -p tachi-server claims_ops::` → **22 passed, 0 failed** (all
  pre-existing sanitize/collision tests plus all 6 new round-4 tests).
  Re-ran after `cargo fmt` to confirm formatting didn't regress anything —
  still 22/22.
- **`cargo fmt -p tachi-server -- --check`**: found 4 formatting diffs in the
  newly-added test code (line-wrapping style), ran `cargo fmt -p
  tachi-server` to apply, re-checked clean.
- **`cargo clippy -p tachi-server --all-targets -- -D warnings`**: clean, 0
  warnings, 0 errors.
- **Pre-existing, unrelated failures found while running the broader
  suite (NOT caused by this round's change, NOT in this round's file
  scope)**:
  - `tachi_server::tests::claims_tests::briefing_surfaces_file_scope_collision_using_the_calling_sessions_own_declared_scope`
    fails at HEAD `40175904` **before** this round's edit too (verified via
    `git stash` + re-run) — the test asserts a collision warning contains
    the literal substring `"claims_ops.rs"`, but round 3 already put `_`
    in `MD_METACHARS`, so the file-path fixture
    `crates/tachi-server/src/claims_ops.rs` has its underscores stripped by
    the sanitizer the test itself exercises. Pre-existing round-3 test bug,
    unrelated to backslash/Cf — out of this mission's scope, flagging for
    visibility, not fixing (mission scope is the two named codex findings).
  - `memcore::db::session_claims::tests::two_concurrent_same_identity_upserts_both_succeed_exactly_one_row`
    is flaky (`SqliteFailure(DatabaseBusy, "automatic extension loading
    failed")` on some runs, passes on immediate retry) — a real-thread WAL
    concurrency test in `memcore`, outside this baton's
    `crates/tachi-server/**` scope and outside this mission's 2 named
    findings. Retried and confirmed it passes standalone.
  - Full `cargo test -p tachi-server` (whole crate, not just `claims_ops::`)
    was still running in the background at the time this file was written;
    not included in these notes — Oz should run it fresh against the pushed
    SHA regardless.
- Did **not** run `cargo test -p memcore` in full, `cargo clippy` on the
  whole workspace, or any suite outside `tachi-server`'s own — Oz's job per
  the dispatch contract, and this baton's file scope is `tachi-server`
  only.
