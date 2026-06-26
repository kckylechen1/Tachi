# Tachikoma Remaining Work Plan

Date: 2026-06-26

This plan tracks the remaining implementation surface for the Tachikoma Deck,
Card, Poke, skill-source, and acpx work after the current main branch landed the
starter architecture.

## Current State

| Surface | Status | Code/document anchor |
| --- | --- | --- |
| Deck/Card vocabulary | Implemented starter | `docs/engineering/architecture/tachikoma-deck-card-move-vocabulary.md` |
| Card projection from dispatch profiles | Implemented starter | `crates/memory-server/src/dispatch_profile/cards/` |
| Read-only Card CLI | Implemented starter | `crates/memory-server/src/bootstrap/cli_tool/cards.rs` |
| Poke local smoke suite | Implemented starter | `crates/memory-server/src/bootstrap/poke_cli/` |
| Skill-source manifest status | Implemented | `crates/memory-server/src/bootstrap/skill_surface_cli/sources.rs` |
| Skill-source sync planning | Implemented starter | `crates/memory-server/src/bootstrap/skill_surface_cli/sync_plan.rs` |
| acpx execution backend | Implemented starter | `crates/memory-server/src/dispatch_ops/acpx/` |
| cycle status read model | Implemented | `crates/memory-server/src/task_lifecycle/cycle_status.rs` |

## Next Work

1. Close the Card read surface gap.
   - Keep `dispatch_profiles` for compatibility.
   - Add stable compact JSON under `cards[]` and `card`.
   - Verify `tachi card list --json` and `tachi card show <id> --json`.

2. Make skill-source sync review actionable.
   - Keep sync planning read-only.
   - Add risk-ordered `review_batches`.
   - Add explicit `next_actions`.
   - Preserve corpus-level network or upstream availability errors.

3. Finish acpx verification.
   - Keep acpx as an optional dispatch backend, not a Card or Move.
   - Add/verify missing-command and unsupported-runtime diagnostics.
   - Prefer a fixture-backed adapter smoke in tests; do not require global acpx.
   - Treat live acpx runs as optional local validation.

4. Decide when #381 and #383 can close.
   - #381 can close when Card CLI JSON, Poke smoke, Card evolution proposals,
     and skill-source review gates have green local evidence.
   - #383 can close when acpx dispatch, event persistence, status/cancel
     controls, conservative permissions, and missing-prerequisite errors are
     covered by tests or local smokes.

## Verification Matrix

| Claim | Command |
| --- | --- |
| Fast local gate for this surface | `bash scripts/verify_tachikoma_fast.sh` |
| Deep local gate with acpx dispatch integration | `TACHI_FAST_DEEP=1 bash scripts/verify_tachikoma_fast.sh` |
| Card compact JSON is stable | `tachi card list --json` and `tachi card show codex_55_review --json` |
| Poke starter suite works | `tachi poke run --suite smoke --json` |
| Skill sources have metadata | `tachi skill-surface sources --json` |
| Sync plan is actionable and read-only | `tachi skill-surface sync-plan --json` |
| acpx adapter remains isolated | `cargo test -p memory-server acpx --locked -- --test-threads=1` |
| cycle status remains covered | `cargo test -p memory-server cycle_status --locked -- --test-threads=1` |
