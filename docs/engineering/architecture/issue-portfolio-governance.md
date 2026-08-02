# Issue portfolio governance

Status: canonical repository governance for issue triage, creation, disposition, and closure; live root set reconciled 2026-08-02.

The open portfolio is a GitHub-native parent/sub-issue tree, not a flat backlog. Normally only these five current roots may be open without a parent:

| Router | Authority |
| --- | --- |
| #734 | memory continuity, Soul, judgment, precedent, and growth |
| #749 | task ownership, staffing, claims, receipts, adjudication, and federation |
| #1299 | performance, concurrency, CI, deployment, database/data health, and recovery |
| #1316 | identity and delegation authority, credentials, data partitioning, and egress |
| #1467 | four-core product boundary: Memory, Work Control, ACP Staffing, and A2A |

#745 and #868 are closed and superseded by #1467. Their surviving bounded children keep their history but route through #1467 or another current domain owner; a closed former router is never revived implicitly by an old body or label.

Known bounded drift: until #1553 lands, the existing `UniversalLaw` lane-card rejection string still names `#868/#871`; treat that output as stale remediation, not authority. #1553 owns replacing it with the current #871/#1467 route and removing the remaining active #868/#787 references.

## Parentage and triage

- Every other open issue has exactly one native primary parent. Attach a new leaf to the nearest coherent child umbrella or router that owns its acceptance criteria; a parentless non-router is governance drift and must be attached, absorbed, or closed.
- Cross-domain work still chooses one primary parent. Express secondary relationships with `Related to #…` or an issue comment; do not duplicate the issue or invent multi-headed ownership. Thin instruction compatibility projection belongs under #871 beneath #1467; a worker-launch credential leak belongs under #1316 with #749 related.
- Start review from the five current roots and expand their native children. Do not use a flat open-issue list as the default planning view. New leaf, design, or bug issues must not create a sixth root; labels aid search but never replace native parentage.
- Before designing or dispatching from an issue, read its latest disposition and inspect current code. A historical body marked `DESIGN-SPLIT`, `PREMISE-COLLAPSED`, `ABSORBED`, or superseded is not an executable contract.

## Current-state vocabulary

Record code/current state honestly as one of: `STALE-COMPLETE`, `STALE-BODY / VALID-REMAINDER`, `PREMISE-COLLAPSED / SUPERSEDED`, `STILL-VALID`, or `UNVERIFIED`. Code presence never proves host deployment or live-data repair. Reconcile typed issue, ref, test, deployment, and owner state before changing disposition.

## Umbrellas and protected closure

- Do not open a new umbrella for a renderer, project-manager persona, summary cache, carrier integration, or shared helper. When an umbrella is genuinely needed, make it a child of one of the five current roots and move or attach its coherent leaves beneath it.
- An umbrella marked `type:umbrella` or `agent:no-close` is owner/adjudicator-close only. Implementation PRs use `Refs` or `Related`, never `Closes`, and must update the parent ledger, child disposition, and relevant umbrella comments when scope or completion changes.

## #1319 staffing contraction

#1319 is the contraction umbrella beneath #749 for staffing surfaces. It converges Task dispatch, Shell, and Arena onto one internal staffing kernel and one receipt lifecycle, organized around Lead, ephemeral Worker, and Ops. It is not a sixth root. Work under it must reduce model-facing tools, facades, schemas, duplicate ledgers, and production code; hiding old surfaces behind profiles, renaming them, or adding wrapper facades is not completion.
