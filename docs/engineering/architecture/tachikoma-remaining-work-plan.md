# Tachikoma Remaining Work Plan

Date: 2026-06-26

This plan tracks the closure evidence for the Tachikoma Deck, Card, Poke,
skill-source, and acpx starter architecture.

## Current State

| Surface | Status | Code/document anchor |
| --- | --- | --- |
| Deck/Card vocabulary | Implemented starter | `docs/engineering/architecture/tachikoma-deck-card-move-vocabulary.md` |
| Card projection from dispatch profiles | Implemented starter | `crates/tachi-server/src/dispatch_profile/cards/` |
| Read-only Card CLI | Implemented starter | `crates/tachi-server/src/bootstrap/cli_tool/cards.rs` |
| Poke local smoke suite | Implemented starter | `crates/tachi-server/src/bootstrap/poke_cli/` |
| acpx execution backend | Implemented starter | `crates/tachi-server/src/dispatch_ops/acpx/` |
| cycle status read model | Implemented | `crates/tachi-server/src/task_lifecycle/cycle_status.rs` |

## Closure Evidence

1. Card read surface is closed.
   - Keep `dispatch_profiles` for compatibility.
   - Stable compact JSON is available under `cards[]` and `card`.
   - Verified by `bootstrap::cli_tool::cards::tests`.

2. acpx verification is covered by local fixtures.
   - Keep acpx as an optional dispatch backend, not a Card or Move.
   - Missing-command and unsupported-runtime diagnostics are covered.
   - Fixture-backed adapter smokes do not require global acpx.
   - Treat live acpx runs as optional local validation.
   - Verified by `dispatch_ops::acpx::tests` and the deep acpx gate.

3. #381 and #383 can close when the verification matrix below is green on
   `main`.

## Verification Matrix

| Claim | Command |
| --- | --- |
| Fast local gate for this surface | `bash scripts/verify_tachikoma_fast.sh` |
| Deep local gate with acpx dispatch integration | `TACHI_FAST_DEEP=1 bash scripts/verify_tachikoma_fast.sh` |
| Card compact JSON is stable | `tachi card list --json` and `tachi card show codex_55_review --json` |
| Poke starter suite works | `tachi poke run --suite smoke --json` |
| Card evolution remains review-required | `cargo test -p tachi-server proposal_evolution --locked -- --test-threads=1` |
| acpx adapter remains isolated | `cargo test -p tachi-server acpx --locked -- --test-threads=1` |
| cycle status/plan remains covered | `cargo test -p tachi-server cycle_status --locked -- --test-threads=1` |
