# Tachikoma Remaining Work Plan

Date: 2026-06-26

This plan tracks the closure evidence for the Tachikoma Deck, Card, Poke,
skill-source, and acpx starter architecture.

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
| cycle status/plan read model | Implemented | `crates/memory-server/src/task_lifecycle/cycle_status.rs`, `crates/memory-server/src/task_lifecycle/cycle_plan.rs` |

## Closure Evidence

1. Card read surface is closed.
   - Keep `dispatch_profiles` for compatibility.
   - Stable compact JSON is available under `cards[]` and `card`.
   - Verified by `bootstrap::cli_tool::cards::tests`.

2. Skill-source sync review is actionable and read-only.
   - Keep sync planning read-only.
   - Risk-ordered `review_batches` and explicit `next_actions` are emitted.
   - Preserve corpus-level network or upstream availability errors.
   - Verified by `bootstrap::skill_surface_cli` tests.

3. acpx verification is covered by local fixtures.
   - Keep acpx as an optional dispatch backend, not a Card or Move.
   - Missing-command and unsupported-runtime diagnostics are covered.
   - Fixture-backed adapter smokes do not require global acpx.
   - Treat live acpx runs as optional local validation.
   - Verified by `dispatch_ops::acpx::tests` and the deep acpx gate.

4. #381 and #383 can close when the verification matrix below is green on
   `main`.

## Verification Matrix

| Claim | Command |
| --- | --- |
| Fast local gate for this surface | `bash scripts/verify_tachikoma_fast.sh` |
| Deep local gate with acpx dispatch integration | `TACHI_FAST_DEEP=1 bash scripts/verify_tachikoma_fast.sh` |
| Card compact JSON is stable | `tachi card list --json` and `tachi card show codex_55_review --json` |
| Poke starter suite works | `tachi poke run --suite smoke --json` |
| Card evolution remains review-required | `cargo test -p memory-server proposal_evolution --locked -- --test-threads=1` |
| Skill sources have metadata | `tachi skill-surface sources --json` |
| Sync plan is actionable and read-only | `tachi skill-surface sync-plan --json` |
| acpx adapter remains isolated | `cargo test -p memory-server acpx --locked -- --test-threads=1` |
| cycle status/plan remains covered | `cargo test -p memory-server cycle_status --locked -- --test-threads=1` |
