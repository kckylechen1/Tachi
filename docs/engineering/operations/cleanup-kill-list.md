# Cleanup kill-list criteria (大扫除 Phase 0)

Status: owner-approved Phase 0 for umbrella [#2143](https://github.com/kckylechen1/Tachi/issues/2143) (parent #1299). Checked: 2026-10-27.

This page defines when code, flags, docs, or process may be deleted or consolidated
during the risk-phased cleanup. It authorizes no deletion by itself. Each later phase
needs its own owner green light on #2143, and each leaf follows
[`AGENTS.md`](../../../AGENTS.md) review-by-risk and
[`dispatch-lifecycle.md`](../architecture/dispatch-lifecycle.md).

Prefer consolidating paths over deleting product features.

## Kill-list categories

A candidate qualifies only under one named category, with the evidence that category requires.

1. **Dead.** No runtime call site outside tests, and no reference outside archived docs.
   Evidence: an `rg` census at the candidate's base commit showing zero non-test callers,
   plus a clean build and the unchanged test suite after removal.
2. **Alias.** A wrapper that only forwards to one canonical API with identical semantics.
   Evidence: the wrapper body, and the canonical path's tests covering every alias caller.
   Migrate callers first; delete the alias in a later leaf.
3. **Compensating control.** Code or process that exists only to work around a defect that
   a planned leaf removes (for example the eng-checkpoint dual-flag write recipe pending
   the Phase 2 scope fix). Remove it only in the same PR train as the fix, with a
   regression test proving the original defect stays fixed.
4. **Over-cap file split.** A file above the 2,000-line cap with mixed responsibilities.
   Split by responsibility along the AGENTS.md dependency direction. The change must be
   move-only (shown with `git diff --color-moved` or equivalent), change no logic or
   public identifier, and pass the unchanged suite.

## Not killable (this cleanup)

These stay unless the owner reopens them explicitly. Moving such code between files is
allowed only as a move-only, behavior-identical change.

- Cross-process store startup lock (#2134, PR #2136).
- Live-DB header read through SQLite's file layer, with no parallel `std::fs` handle (#2137, PR #2139).
- `private_partition::resolve_generic_open_path` and private-store stamping and refusal.
- The reserved-reference write authorizer (`authorize_reserved_reference_write`).
- Supersede, `as_of`, and multi-evidence search behavior (#2126, #2127, #2128).
- eng-checkpoint's global-DB default (#2135, PR #2138) until Phase 2 replaces the scope model.
- Public MCP tool names and schemas, CLI commands and flags, and configuration, except
  through an owner-approved compatibility policy (open question on #2143).
- On-disk schema and migration history.

## What "safe to delete or consolidate" requires

Every cleanup leaf states in its PR:

- **Category** from the list above, and the hotspot ID from #2143.
- **Call-site evidence:** the census command and its output at the PR base.
- **Tests:** the existing suite passes unchanged. Add tests only for a behavior change or
  regression; never weaken an assertion, golden, or guard to get a pass.
- **Rollback:** a single revert restores prior behavior, with no data migration or
  on-disk change needed to undo it.
- **Risk class** per AGENTS.md. Persistence, concurrency, or public-compatibility leaves are
  high-risk and need independent review of the exact candidate.

## Measurement commands

Run from the repository root. Counts include comments and tests unless filtered.

```bash
# LOC per crate
for d in crates/*/; do printf '%s %s\n' "$(find "$d" -name '*.rs' -print0 | xargs -0 cat | wc -l)" "$d"; done | sort -n

# Files over the 2,000-line cap
find crates -name '*.rs' -print0 | xargs -0 wc -l | awk '$2!="total" && $1>2000' | sort -rn

# memcore open entry points
rg -n '^\s+pub(\(crate\))? fn open' crates/memcore/src/store/open.rs

# Open-path and scope fan-out (hits / files)
for s in 'MemoryStore::open' open_with_label resolve_generic_open_path 'resolve_write_scope(' busy_timeout superseded_by; do
  printf '%s %s %s\n' "$(rg -c -F --glob '!target/**' "$s" crates | awk -F: '{n+=$2} END {print n+0}')" \
    "$(rg -l -F --glob '!target/**' "$s" crates | wc -l)" "$s"; done

# resolve_write_scope callers
rg -c --glob '!target/**' 'resolve_write_scope\(' crates --type rust

# clap argument counts in the CLI crate
rg -c '#\[arg\(' crates/tachi-bootstrap --type rust

# Open umbrella issues
gh issue list --label type:umbrella --state open --limit 100 --json number,title
```

## Baseline snapshot — tip `1793a18`, 2026-10-27

A dated baseline for comparing later leaves. Re-run the commands above rather than
trusting these numbers after the tip moves.

### LOC by crate (`.rs`)

| LOC | Crate |
| ---: | --- |
| 413,530 | `tachi-server` |
| 176,577 | `memcore` |
| 35,994 | `tachi-params` |
| 26,202 | `tachi-llm` |
| 10,327 | `tachi-dispatch` |
| 8,009 | `tachi-lesson-forge` |
| 7,684 | `tachi-contract-tests` |
| 6,872 | `tachi-exec-env-reaper` |
| 5,847 | `tachi-vault-tests` |
| 5,340 | `memory-server-runtime` |
| 4,478 | `tachi-bootstrap` |
| 3,818 | `tachi-credential-profile` |
| 3,280 | `tachi-hub` |
| 3,220 | `portable-kernel` |
| 3,174 | `portable-server` |
| 3,135 | `tachi-build-broker` |
| 2,391 | `tachi-runtime-core` |
| 1,986 | `tachi-foundry` |
| 1,675 | `tachi-bootstrap-tests` |
| ≤ 979 each | `memory-server-manifest-audit`, `vault-kit`, `memory-server-rescue`, `tachi-delivery-tests`, `memory-node`, `memory-server-hub-cli`, `memory-server-capture-gate`, `memory-server-prompt-envelope` |

### Files over 2,000 lines (61)

| LOC | File |
| ---: | --- |
| 5340 | `crates/memory-server-runtime/src/lib.rs` |
| 4731 | `crates/tachi-params/src/work_read_model/tests.rs` |
| 4610 | `crates/memcore/src/db/migrations/classification_history.rs` |
| 4391 | `crates/tachi-llm/src/llm/tests/chat_lanes.rs` |
| 4313 | `crates/memcore/src/db/schema.rs` |
| 4257 | `crates/memcore/src/db/memory_crud.rs` |
| 4101 | `crates/tachi-server/src/vault_ops/tests.rs` |
| 4087 | `crates/tachi-server/src/bootstrap/wiki_corpus/tests.rs` |
| 3781 | `crates/tachi-server/src/foundry_runtime_ops/handlers/capture_session.rs` |
| 3729 | `crates/tachi-params/src/current_truth/tests.rs` |
| 3712 | `crates/memcore/src/store/immutable_supersession.rs` |
| 3676 | `crates/memcore/src/db/migrations.rs` |
| 3645 | `crates/tachi-server/src/complete_ops/handler.rs` |
| 3582 | `crates/memcore/src/db/schema/ddl.rs` |
| 3577 | `crates/tachi-exec-env-reaper/src/tests.rs` |
| 3508 | `crates/tachi-server/src/bootstrap/backfill.rs` |
| 3497 | `crates/tachi-server/src/bootstrap/serve/stdio/tests.rs` |
| 3463 | `crates/tachi-server/src/tests/facade_tests/memory_actions/consolidate_lifecycle.rs` |
| 3394 | `crates/tachi-server/src/staffing_ops/mod.rs` |
| 3295 | `crates/tachi-exec-env-reaper/src/lib.rs` |
| 3231 | `crates/tachi-server/src/server_handler.rs` |
| 3172 | `crates/tachi-contract-tests/src/tests/docs_tests/store_trigger_ddl_census.rs` |
| 3099 | `crates/memcore/src/store/open.rs` |
| 3086 | `crates/tachi-server/src/complete_ops/dispatch_outcome.rs` |
| 2962 | `crates/tachi-server/src/dispatch_ops/subprocess.rs` |
| 2943 | `crates/tachi-server/src/bootstrap/serve.rs` |
| 2925 | `crates/tachi-server/src/verify_ops/run/tests.rs` |
| 2900 | `crates/tachi-server/src/tests/facade_tests/memory_actions/recall_proposals.rs` |
| 2884 | `crates/tachi-server/src/dispatch_ops/dispatch/execution.rs` |
| 2871 | `crates/tachi-server/src/exec_env_ops.rs` |
| 2809 | `crates/tachi-server/src/project_db_ops.rs` |
| 2795 | `crates/tachi-server/src/foundry_runtime_ops/wiki_evolver.rs` |
| 2791 | `crates/tachi-server/src/enrichment.rs` |
| 2766 | `crates/memcore/src/db/exec_env_resources.rs` |
| 2693 | `crates/tachi-server/src/tests/docs_tests/organize/provenance.rs` |
| 2676 | `crates/memcore/src/db/delivery_intents.rs` |
| 2674 | `crates/memcore/src/db/session_claims.rs` |
| 2561 | `crates/tachi-server/src/tests/skill_tests/builtin_ingest/auto_ingest.rs` |
| 2504 | `crates/memcore/src/db/tests/graph.rs` |
| 2435 | `crates/tachi-server/src/tune_ops/recall_proposal_ops.rs` |
| 2428 | `crates/memcore/src/db/outbox.rs` |
| 2420 | `crates/memcore/src/store/memory_lifecycle.rs` |
| 2402 | `crates/tachi-server/src/tests/mod.rs` |
| 2381 | `crates/memcore/src/store/exact_dedupe.rs` |
| 2371 | `crates/tachi-server/src/memory_search_ops/auto_link.rs` |
| 2364 | `crates/memcore/src/db/harness_session_events.rs` |
| 2312 | `crates/tachi-server/src/verified_admission.rs` |
| 2296 | `crates/tachi-lesson-forge/src/runner.rs` |
| 2276 | `crates/tachi-server/src/provider_config.rs` |
| 2244 | `crates/tachi-server/src/repair/memory_maintenance.rs` |
| 2204 | `crates/memcore/src/private_partition.rs` |
| 2179 | `crates/tachi-server/src/bootstrap/vault_cli/providers_doctor.rs` |
| 2161 | `crates/tachi-server/src/bootstrap/serve/stdio/tests/rmcp_compat.rs` |
| 2156 | `crates/memcore/src/store/sticky_cutover.rs` |
| 2142 | `crates/memcore/src/search/ranking.rs` |
| 2141 | `crates/tachi-params/src/work_read_model/projector.rs` |
| 2118 | `crates/tachi-server/src/bootstrap/vault_cli/intake.rs` |
| 2078 | `crates/tachi-server/src/managed_run_control.rs` |
| 2073 | `crates/tachi-server/src/dispatch_ops/dispatch/tests/resolution_grants.rs` |
| 2039 | `crates/memcore/src/db/filename.rs` |
| 2024 | `crates/tachi-server/src/tests/dispatch_tests/completion_eval/candidate_projection.rs` |

### memcore open entry points (`crates/memcore/src/store/open.rs`)

`open`, `open_with_label`, `open_with_context`, `open_with_context_and_busy_timeout`,
`open_and_vault_upsert_key_health_with_context_and_busy_timeout`,
`open_and_vault_upsert_key_health_retaining_store`, `open_with_label_and_context`,
`open_private_image` (crate-private), `open_read_only`, `open_read_only_immutable`,
`open_read_only_with_label`, `open_read_only_existing_schema_compat`,
`open_existing_read_write`, `open_in_memory`: 14 entry points.

Fan-out: `MemoryStore::open` 1,188 hits in 220 files; `open_with_label` 157 in 49;
`resolve_generic_open_path` 12 in 4; `busy_timeout` 79 in 32; `superseded_by` 658 in 80.

### `resolve_write_scope(` callers (23 hits)

| Hits | File |
| ---: | --- |
| 11 | `crates/tachi-server/src/complete_ops/dispatch_outcome.rs` |
| 1 each | `server_methods/db.rs` (definition), `complete_ops/lessons.rs`, `hub_ops/register.rs`, `foundry_runtime_ops/handlers/target.rs`, `pipeline_ops/auto_ingest.rs`, `pipeline_ops/ingest/event.rs`, `pipeline_ops/ingest/structured_event.rs`, `pipeline_ops/ingest/source.rs`, `pipeline_ops/ingest/extract.rs`, `dispatch_ops/dispatch/recovery.rs`, `dispatch_ops/dispatch/tests.rs`, `memory_search_ops/save_memory/handler.rs` |

### CLI `#[arg(` counts (`crates/tachi-bootstrap`, 452 total)

| Count | File |
| ---: | --- |
| 204 | `src/cli/maintenance_actions.rs` |
| 135 | `src/cli/vault_actions.rs` |
| 96 | `src/cli/commands.rs` |
| 10 | `src/cli.rs` |
| 5 | `src/cli/broker_actions.rs` |
| 2 | `src/cli/a2a_actions.rs` |

### Open umbrellas (17, before #2143)

#734, #748, #749, #770, #894, #954, #1094, #1096, #1111, #1129, #1299, #1316, #1467,
#1519, #1531, #1956, #1987.
