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

## Recovering a lost Staff start response

Before sending a new `tachi_staff(action="start")`, a controller can durably save
an optional `submission_id` alongside its original request. Keys contain 1–128
ASCII letters, digits, periods, underscores or hyphens. The key belongs to the
server-admitted agent identity, canonical project and physical project source;
it is independent of the HTTP session ID and daemon controller epoch. It grants
no launch or result access by itself. A shared connector without an admitted
per-agent identity cannot use this recovery contract.

```json
{
  "action": "start",
  "submission_id": "review-2290-attempt-1",
  "staffing_reason": "durable_cross_session",
  "task": "Review the submitted candidate and report concrete defects.",
  "profile": "codex_55_review"
}
```

If the response is lost, reconnect with the same admitted identity and bound
project, then query the saved key through the existing status action:

```json
{"action":"status","submission_id":"review-2290-attempt-1","format":"json"}
```

| Recovery observation | Meaning and permitted next step |
|---|---|
| `binding_state="bound"`, `dispatch_id` present | The exact submission association and a canonical status receipt are readable. Continue existing Staff status/result/cancel operations using that dispatch ID and their existing authorization. |
| `binding_state="incomplete"`, `dispatch_id` present | The association is durable, but a valid status receipt is unavailable. Execution may not have started or may require reconciliation. Do not launch a replacement automatically. |
| `binding_state="unconfirmed"`, `dispatch_id=null` | No confirmable association is available to this owner. Missing, unreadable, replaced and foreign evidence do not prove that work never started. |

Every recovery response has `retry_safe=false`. Recovery reports an association,
not liveness, control, completion, verification or acceptance. `receipt_available`
and `status_revision`, when present, describe only the receipt observed by that
read. Polling uses the existing read-only status policy and still consumes the
session's normal request budget. It creates no Tasks handle or receipt write.

Submitting the same key with the same canonical input returns the retained
association with `replayed=true`, without provisioning or launching again.
Changing the input under an already bound key returns a conflict without changing
the original association. The input digest freezes the typed staffing reason,
trimmed task, profile/worker/stage hints, execution level, issue/PR/flow references,
canonical project, completion predicate, recommendation reference and declared
file scope before mutable profile defaults are resolved. Omitted values remain
distinct from explicit overrides. A new key expresses a new submission even when
the issue, flow and task text are identical; recovery never chooses the newest
matching run.

The exclusive canonical run directory reserves the key before workspace
provisioning or worker spawn. Its existing `mcp-tasks.json` owner binding retains
the input digest; the ordinary status receipt remains the execution authority.
The association has no automatic expiry, including after terminal completion or
expiration of a result Tasks handle. A crash before the owner binding is written
leaves an unconfirmable reserved directory that is not reused. Failed provisioning
can leave an incomplete association for explicit reconciliation. Removing or
moving canonical run data loses this evidence; a consumer must never treat that
absence as permission to resubmit automatically. Existing starts without a key
retain their previous behavior and cannot be recovered by key retroactively.

The `submission` dogfood scenario discards the initial response, reconnects,
checks replay/conflict and owner isolation, restarts the daemon after completion,
and submits identical text under a distinct key. It uses the deterministic local
worker below and verifies the actual spawn count. Run it against the candidate:

```sh
python3 scripts/dogfood_e2e.py --tachi <candidate-binary> --only submission
```

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
100, with active workers before history. Each row shows at most eight assignments
and dispatch links, prioritizing current work. Overflow, capacity loss and claim
read errors are explicit. No database
migration, external heartbeat service or new task/result store is introduced.

## Project work views and canonical run receipts

In an admitted local project, an explicit WorkClaim with `dispatch_id` can link
that work item's board/status/brief to an existing managed run. Reading the run
also requires its durable result binding to match the current agent identity,
canonical project and physical project source. A claim alone grants no result
access. Ambiguous work links, another owner, historical runs without that binding,
and lifecycle adapters without a managed identity remain `unavailable`.

The three views reuse the bounded canonical receipt reader and Staff's managed
execution/control/outcome projection. Each compact `run_receipts` row carries
`dispatch_id`, `status_revision`, a receipt digest and receipt/result references;
no prompt or report body is included. Controller loss stays `unknown` or
`orphaned`, and a terminal execution never supplies verification or acceptance.
At most eight flow/run/attached-session receipts are admitted per read. No run enumeration,
result handle creation or receipt write occurs.

The section's `revision` remains the project-claims revision, explicitly labeled
by `revision_scope`. A linked item's `revision` covers its projection and observed
run rows; `projection_revision` identifies the shared model input independently.
Exact and full views use the same run set for the same work. If any linked run is
unavailable, that item's typed run section stays unavailable while readable rows
retain their own evidence and the source reports `incomplete`. ExecEnv,
verification and adjudication sources remain unintegrated.

For canonical project claims, attach, exact replay, attachment lookup and
reconnect use the captured local-project admission. The current host must hold
its own active claim in that project; the worker retains its original identity,
freshness, revision and capability-policy checks. The HTTP descriptor must name
the same captured project. Legacy NULL claims continue through their existing
admission path. These operations record host-owned receipts without launching or
controlling the external session.

An attached external session is discovered only through selected project claims
and the current host's admission. The worker identity on the attachment must
still match its claim; the host must hold a current project claim of its own.
The existing session-spine reader rechecks the exact host connection. A worker's
released claim retains historical session evidence, while a reconnected host
must rebind its attachment before these views can read it again. Host presence
alone does not create an attached execution receipt.

`attached_sessions` carries the canonical state and revision, conflict/cleanup
flags and evidence/result references. The same state appears in status and brief
summaries; it does not invent a managed dispatch or change verification,
adjudication or success. Event summaries and result bodies stay out of the views.
The result reference includes the verified host and admission reference needed
to call the existing session-state reader from that current connection.
All selected receipts share the eight-receipt budget, including mixed managed and
attached work. Missing and denied attachments have the same visible shape.

Both execution planes use the existing delivery observations. An attached intent
must match its execution reference, claim, requester agent, host, session and
result reference. Delivery is shown only for a consistent canonical terminal
whose latest eligible terminal payload identity agrees with the intent. Selection
respects the producer's journal-order revision high-water and existing result
revision: a late payload cannot hide settled delivery after cleanup or a
same-revision correction. Unchanged terminal replays preserve delivery, while a
valid corrected payload awaiting its mint stays `unavailable`, including after
cleanup. Managed and attached
intents are combined in one projection snapshot; partial rows remain explicit.
Reading never claims, acknowledges, retries or mints delivery. Per-item revisions
cover attached evidence and delivery changes as well as managed run receipts.

The `real_staff_launch_persists_result_owner_before_returning_acceptance` fixture
now compares the actual launched worker across project board/status/brief, Staff
and MCP Tasks. The `mcp_tasks::tests::project_*` regressions cover controller loss,
ownership/source rejection, partial and missing data, revision drift, no-write
reads, receipt bounds and Unix path replacement.
