# #1059 exact-20 read-only adapter PilotReport

Captured at: `2026-07-24T04:56:05Z`

Frozen owner manifest: [1059-exact20-manifest.json](1059-exact20-manifest.json)
Manifest SHA-256: `224858da1443006fe9bdc84ce10aea84a2ccf2cec3da8aa2d8d041dc4df3fbe8`

## Result

**PARTIAL / preview-only.** The read-only runner emitted exactly 20 pending
candidates after checking every live PR merge commit against the owner-pinned
SHA. It made no GitHub mutation, database write, bulk ingestion, model request,
or credential read.

- Candidate yield: 20/20 emitted; 20/20 are `pending` and
  `preview_only`.
- Merge provenance: 20/20 expected and observed merge SHAs match.
- Selected comment revisions: 0/20 cases selected a structured comment. Each
  report row therefore encodes `selected_comment_revisions: []` rather than
  inventing an id, timestamp, or hash.
- Source coverage: 0 full / 20 partial. This is an honest current-adapter
  result: its derived candidate situation contains the issue title/body, while
  coverage also counts the PR title/body. It is a behavior-test handoff, not a
  silently waived gate.
- Cost and latency: no model invocation, so per-case `cost_usd` is absent with
  `not_applicable_no_model_invocation`; total in-process adapter latency was
  2 ms after the read-only metadata fetches completed.
- Engine receipt: no provider, Vault, Keychain, or model resolver path was
  entered. Effective provider/model/version remain explicitly unknown and the
  report carries a degraded preview-only receipt.
- Current provenance drift from the prior owner baseline was limited to issue
  snapshot hashes for `gh-1379-1397` and `gh-1278-1390`. All 20 PR snapshot
  hashes, merge SHAs, case IDs, and manifest order remained unchanged.

The machine-readable per-case fields — candidate yield, source coverage,
cost/latency, selected-comment revision receipts, effective engine receipt,
and behavior-test handoff — are in the owner-approved
[no-spend baseline artifact](1059-exact20-pilot-report.json). Execution
preflight reads only its immutable provenance fields through the narrow
compatibility type; it is not an output destination. New executions emit the
current report schema to a distinct result path.

Baseline SHA-256 changed from
`3bbb067f90c57009eeb45e9129becf7bf28062a718735686f088761e68e2765e` to
`df262db0eeb65e60491c61bce2c271a6553dff99656ea78f992aee2edafd9a33`.
The manifest SHA-256 remains
`224858da1443006fe9bdc84ce10aea84a2ccf2cec3da8aa2d8d041dc4df3fbe8`.

## Scope constraints retained in the receipt

- `gh-1393-l1-1398` is L1-only; #1411 remains open for full credential
  custody.
- `gh-1355-1384` retains #1391 context and #1413's open fail-open follow-up.
- `gh-1278-1390` remains the step-2 partial case and retains #1413's later
  known-reds regex follow-up.
- #1073 is not part of this manifest or report.

## Reproduce

```sh
CARGO_TARGET_DIR=/private/tmp/sigil-target-1059-cde718ec \
  cargo run -p tachi-server --bin github-corpus-pilot -- \
  --manifest docs/engineering/receipts/1059-exact20-manifest.json \
  --baseline-report docs/engineering/receipts/1059-exact20-pilot-report.json \
  --baseline-sha256 df262db0eeb65e60491c61bce2c271a6553dff99656ea78f992aee2edafd9a33 \
  --report /private/tmp/1059-exact20-phase3-report.json \
  --captured-at 2026-07-24T04:56:05Z
```
