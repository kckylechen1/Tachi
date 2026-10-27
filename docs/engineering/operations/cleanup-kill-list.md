# Cleanup criteria

Durable rules for [#2143](https://github.com/kckylechen1/Tachi/issues/2143).
The issue owns priorities, progress and dated measurements; this page owns deletion
criteria. Apply [AGENTS.md](../../../AGENTS.md) and
[delivery and independent review](../architecture/dispatch-lifecycle.md).

## Eligible changes

1. **Unused implementation.** Show that production callers, public consumers,
   registration/configuration paths and persisted compatibility do not require it.
   An `rg` search identifies candidates; zero matches alone do not prove dead code.
   Verify the clean build and unchanged applicable tests after removal.
2. **Equivalent alias.** Show identical semantics and coverage for every caller.
   Migrate callers before removing an alias; preserve public compatibility.
3. **Workaround.** Remove only after its underlying defect is fixed and a regression
   test exercises the original failure. Account for installed older clients and
   binaries; source correctness alone does not prove deployment compatibility.
4. **Responsibility split.** Move code without changing behavior or public
   identifiers, following the dependency direction in AGENTS.md. Show moved-code
   equivalence and run the unchanged applicable tests. High-risk code still needs
   independent review even when the change only moves it.

## Protected contracts

Preserve these throughout this cleanup:

- Cross-process startup ownership (#2136).
- Live-database header reads through SQLite VFS, without a parallel `std::fs`
  handle on the live database (#2139).
- Private-partition identity, stamping and generic-open refusal.
- Reserved-reference write authorization.
- Supersession atomicity, `as_of` and multi-evidence search behavior.
- The engineering-checkpoint global default, idempotency and legacy-row handling;
  keep its dual-flag recipe while supported installed binaries still need it.
- Public MCP/CLI/configuration compatibility, on-disk schema and migration history.
- The installed-binary freeze; test builds in isolated workspaces are not deployment.

A policy or behavior change requires an explicitly scoped proposal and the relevant
review. A move-only label never waives persistence, concurrency or authority checks.

## Evidence for each change

Keep the following in the PR or its CI artifacts:

- Base and candidate identities, category and affected hotspot from #2143.
- Callers and contracts that change, separating production code, tests and comments.
  Distinguish shared wrappers from independent policy implementations.
- The concrete improvement: fewer independent decisions, a smaller representative
  edit surface, fewer repeated database opens, or measured feedback-time reduction.
- Existing applicable tests and any discriminating regression test. A skipped
  integration test is missing evidence, not a pass.
- A bounded rollback that needs no data migration.
- Risk classification and required independent review.

File length, text matches and issue counts are investigation signals, not deletion
targets. Splitting files alone does not establish a build or test speedup.

## Measurement and CI

Record workload, command, commit, platform, cache state and repetitions. Separate
queueing, compilation and test execution. Parallel test durations may be summed for
workload attribution, but their sum is not wall time and cannot be subtracted from
workflow time to infer compilation.

Use the existing [CI applicability policy](ci-scheduling.md). A reduced check must
retain the actual consumers of the changed inputs and fail closed for unknown
changes; changes to CI policy itself receive full validation.

Keep dated census tables, timing reports and before/after evidence in #2143, the PR
or CI artifacts. Preserve the reproduction commands with that evidence. The original
[Phase 0 census](https://github.com/kckylechen1/Tachi/blob/822946382725f958463641b3b1821af0482ce996/docs/engineering/operations/cleanup-kill-list.md#baseline-snapshot--tip-1793a18-2026-10-27)
remains available as an immutable historical snapshot.
