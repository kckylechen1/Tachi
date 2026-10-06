# Staff results and the deterministic test worker

`tachi_staff(action="result", dispatch_id=..., format="json")` reads the existing
canonical receipt and adds `result`. It does not launch a worker or rewrite the
receipt. It needs no `staffing_reason`; Observe and delegate profiles may read it.
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
`staffing_ops::tests::staff_start_launches_fake_worker_through_canonical_receipt_lifecycle`
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
