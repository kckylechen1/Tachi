# Staff results and the deterministic test worker

`tachi_staff(action="result", dispatch_id=..., format="json")` reads the existing
canonical receipt and adds `result`. It does not launch a worker or rewrite the
receipt. If the receipt already carries its own top-level `result`, the response
keeps that value under `receipt_result`; a receipt that already uses both keys is
refused rather than dropping either. It needs no `staffing_reason`; Observe and
delegate profiles may read it.
The same run-root and regular-file containment checks apply as for Task results.

`result.body` contains the full UTF-8 `result.md`, up to the existing **64 KiB file
limit**. `truncated` is false, and `full_size_chars` / `full_size_bytes` describe
that report. Files above 64 KiB and invalid UTF-8 are rejected; this is not an
unbounded artifact download. A missing report returns `body: null` with a note,
which does not prove a failed run. The canonical receipt and optional managed
`read_projection` retain their existing execution/control/outcome meanings.

Task `status(include_result=true)` continues to return an 8,000-character preview
within the same file limit. Controllers needing the remainder of an admitted
report should use Staff `result`. For reports larger than 64 KiB, the worker must
produce a bounded summary or arrange a separate artifact delivery; this action
will not accept a caller-supplied filesystem path.

## Existing test backend

The Unix integration fixture
`staffing_ops::tests::native_lifecycle::staff_start_launches_fake_worker_through_canonical_receipt_lifecycle`
uses the real Staff adapter and canonical background launcher with a deterministic
`codex` executable at the front of a **temporary** `PATH`. It uses the existing
`codex_55_review` profile, `worker="codex"`, and
`staffing_reason="durable_cross_session"`. No production fake profile is added.

The executable implements `--version` as `codex-cli 0.144.1`, waits for a
`codex.release` marker beside itself, prints `staff fake worker`, and exits zero.
The release marker makes admission-before-execution observable. The fixture
isolates home, run root, repository, worktrees, database, and Git environment;
it waits for terminal completion and workspace cleanup before restoring them.
It verifies the single canonical receipt/trajectory and reads the actual report
back through Staff `result`, including unchanged receipt bytes.

Run this existing fixture rather than launching a real authenticated harness:

```sh
cargo nextest run -p tachi-server --lib --locked --profile census \
  -E 'test(staff_start_launches_fake_worker_through_canonical_receipt_lifecycle)'
```

Use the owning host's normal Cargo target policy and serialize shared build
resources. A fake worker proves the adapter/result lifecycle; it does not prove
provider authentication, remote caller identity, CLI cancellation, or event watch.
Those remain separate contracts (#1676, #1677, #1262, #1316).

## Harness sandbox and workspace retention

Ordinary Staff read-only jobs with an admitted harness sandbox and no
`declared_file_scope` use that harness's native sandbox. Tachi keeps its exclusive
workspace lease and cancellation/timeout lifecycle, without adding an outer OS
sandbox that would prevent nested sandbox initialization on macOS. An explicit
scope, including an empty list, retains the strict parent-owned postflight path.
Direct dispatch and caller-supplied workspaces retain their existing policy.

The native path does not certify the entire descendant tree as reaped. It retains
the managed worktree and fences its resources against reuse. The terminal receipt
reports `exec_env_postflight.status="delegated_to_harness"`,
`workspace_cleanup="retained"`, the fence outcome, and descendant liveness.
A failed fence makes the run fail; a successful result remains untrusted harness
output. Retained worktrees require the normal certified cleanup path after their
process owners have been resolved.

The `ordinary_staff_runs_inner_sandbox_and_retains_unproven_workspace` fixture
checks that the actual Staff launcher permits an inner macOS sandbox to read its
worktree, then retains and fences that workspace. The strict lifecycle fixture
passes an explicit empty scope and still requires postflight-certified cleanup.

## Harness observations and the work board

Use `tachi_staff(action="roster", format="json")` for the current bound project,
or read the identical projection under `tachi_task(action="board").staff`.
`project` can select an explicit project for an unbound local caller; a bound
session cannot select another project. `flow_id` filters the assignments and
worker links. Board `limit` also bounds the roster; zero returns no observations.
Observe and delegate profiles can read the roster without a staffing reason.

Tachi automatically observes incoming initialized MCP clients and tool/resource
requests. A client is `recently_seen` for five minutes, then `stale`; this is a
request sighting, not proof of an open persistent socket or idle worker. Modern
stateless requests use the existing resolved request identity bucket. Names and
identity labels are caller assertions. WorkClaims describe reported work and
never refresh connection evidence.

Managed worker observations are registered on canonical dispatch handoff. They
start as `starting`; a successful CLI spawn becomes `busy`. Native ACP records
`ready_observed_at` only after initialize and session setup, then `busy` when
sending the prompt. Runner return, cancellation or owner drop disconnects the
observation. This means Tachi no longer controls that runner connection, not that
all escaped descendants have been certified dead. Canonical run receipts still
own task success/failure and cancellation/cleanup authority.

Rows carry the harness, dispatch id, admitted task summary, issue/flow refs and
project-scoped active WorkClaims. Client rows link their managed dispatches.
`accepts_new_work=null` is deliberate: a recent MCP request or ACP session cannot
prove capacity to accept another assignment. CLI installation remains a separate
`preflight` probe. An independently opened harness with no Tachi traffic is not
auto-discovered; this view does not scan processes or read credentials.

Observations are volatile and shared by this daemon's session clones. Restart
starts a new `controller_epoch` with an empty registry; historical claims cannot
resurrect online state. The registry holds at most 512 rows and displays at most
100; overflow, capacity loss and claim read errors are explicit. No database
migration, external heartbeat service or new task/result store is introduced.
