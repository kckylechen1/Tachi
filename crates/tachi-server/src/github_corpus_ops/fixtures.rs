//! Synthetic gh issue/PR/event JSON for RED→GREEN discrimination tests.

use serde_json::{json, Value};

use super::parse::{
    assemble_case_bundle, CaseCorpusBundle, ProvenanceEventKindV1, ProvenanceEventV1,
};
use super::pilot::CorpusCaseV1;
use super::reader::FixtureCorpusReader;

/// Structured-marker comment so `parse_issue_snapshot_from_gh_json` selects it.
pub const STRUCTURED_COMMENT_BODY: &str =
    "Spec-Ref: docs/engineering/architecture/issue-refinery-memory-lanes.md\nRelated to #1002";

pub fn sample_issue_json(number: u64, state: &str, body: &str, updated_at: &str) -> Value {
    json!({
        "number": number,
        "title": format!("Corpus pilot issue {number}"),
        "body": body,
        "state": state,
        "labels": [{ "name": "type:bug" }],
        "milestone": null,
        "updatedAt": updated_at,
        "comments": [{
            "id": format!("c-{number}"),
            "body": STRUCTURED_COMMENT_BODY,
            "updatedAt": updated_at,
            "createdAt": updated_at,
            "author": { "login": "pilot" }
        }]
    })
}

pub fn sample_pr_json(number: u64, state: &str, merged: bool, updated_at: &str) -> Value {
    json!({
        "number": number,
        "title": format!("Corpus pilot PR {number}"),
        "body": format!("Fixes the gate. See https://example.com/external-link (text only)."),
        "state": state,
        "headRefOid": format!("head{number:04x}"),
        "baseRefOid": "base0001",
        "updatedAt": updated_at,
        "merged": merged,
        "mergeCommit": if merged {
            json!({ "oid": format!("merge{number:04x}") })
        } else {
            Value::Null
        },
        "reviews": [{
            "author": { "login": "reviewer" },
            "state": "APPROVED",
            "submittedAt": updated_at
        }],
        "statusCheckRollup": [{
            "name": "ci",
            "conclusion": "SUCCESS",
            "status": "COMPLETED"
        }]
    })
}

/// Closed issue + merged PR chain. When `with_overturn`, APPENDs reopen+revert
/// events while keeping the issue state **CLOSED** and the PR snapshot
/// unchanged — so a pure overturn cannot hide behind a flipped issue state
/// (C1 discrimination).
pub fn chain_bundle(case_id: &str, with_overturn: bool) -> CaseCorpusBundle {
    let repo = "owner/repo";
    let issue_number = 42u64;
    let pr_number = 77u64;
    // Issue state stays CLOSED regardless of overturn — do not flip OPEN.
    let issue_json = sample_issue_json(
        issue_number,
        "CLOSED",
        "Pilot body with https://evil.example/payload (must stay text).",
        "2026-07-13T00:00:00Z",
    );
    let pr_json = sample_pr_json(pr_number, "MERGED", true, "2026-07-13T01:00:00Z");

    // Assemble once to obtain real snapshot hashes, then bind events to them.
    let skeleton = assemble_case_bundle(
        case_id,
        repo,
        issue_number,
        &issue_json,
        Some((pr_number, &pr_json)),
        vec![],
        "2026-07-14T02:00:00Z",
    )
    .expect("chain fixture must assemble");

    let issue_hash = skeleton.issue.issue_snapshot_hash.clone();
    let pr_hash = skeleton
        .pull_request
        .as_ref()
        .expect("chain fixture has PR")
        .pr_snapshot_hash
        .clone();
    let merge_sha = skeleton
        .pull_request
        .as_ref()
        .and_then(|p| p.merge_commit_sha.clone())
        .unwrap_or_else(|| format!("merge{pr_number:04x}"));
    // The structured-marker comment MUST have been selected — bind its REAL
    // body_hash. Defaulting to "" here would silently mask a regression in
    // comment selection (empty provenance passing as if it were real).
    let comment_body_hash = skeleton
        .issue
        .selected_comment_revisions
        .first()
        .map(|c| c.body_hash.clone())
        .filter(|h| !h.is_empty())
        .expect(
            "chain fixture issue must select its structured comment with a non-empty body_hash",
        );

    let mut events = vec![
        ProvenanceEventV1 {
            kind: ProvenanceEventKindV1::IssueOpened,
            revision_hash: issue_hash.clone(),
            target_ref: format!("{repo}#{issue_number}"),
            occurred_at: "2026-07-10T00:00:00Z".to_string(),
        },
        ProvenanceEventV1 {
            kind: ProvenanceEventKindV1::CommentSelected,
            revision_hash: comment_body_hash,
            target_ref: format!("c-{issue_number}"),
            occurred_at: "2026-07-11T00:00:00Z".to_string(),
        },
        ProvenanceEventV1 {
            kind: ProvenanceEventKindV1::PrOpened,
            revision_hash: pr_hash,
            target_ref: format!("{repo}#{pr_number}"),
            occurred_at: "2026-07-12T00:00:00Z".to_string(),
        },
        ProvenanceEventV1 {
            kind: ProvenanceEventKindV1::PrMerged,
            revision_hash: merge_sha,
            target_ref: format!("{repo}#{pr_number}"),
            occurred_at: "2026-07-13T01:00:00Z".to_string(),
        },
        ProvenanceEventV1 {
            kind: ProvenanceEventKindV1::IssueClosed,
            revision_hash: issue_hash,
            target_ref: format!("{repo}#{issue_number}"),
            occurred_at: "2026-07-13T02:00:00Z".to_string(),
        },
    ];
    if with_overturn {
        // Reopen event does NOT flip the issue snapshot state — the point of
        // C1 is that overturn evidence appends while snapshots stay equal.
        events.push(ProvenanceEventV1 {
            kind: ProvenanceEventKindV1::Reopen,
            revision_hash: skeleton.issue.issue_snapshot_hash.clone(),
            target_ref: format!("{repo}#{issue_number}"),
            occurred_at: "2026-07-14T00:00:00Z".to_string(),
        });
        events.push(ProvenanceEventV1 {
            kind: ProvenanceEventKindV1::Revert,
            revision_hash: "revertsha01".to_string(),
            target_ref: format!("{repo}#{pr_number}"),
            occurred_at: "2026-07-14T01:00:00Z".to_string(),
        });
    }

    let mut bundle = skeleton;
    bundle.events = events;
    bundle
}

/// Pure-revert pair: identical CLOSED issue + MERGED PR snapshots; the only
/// difference is a trailing Revert event. Used to prove candidate_id does
/// not collide when snapshots are unchanged (C1).
pub fn pure_revert_pair(case_id: &str) -> (CaseCorpusBundle, CaseCorpusBundle) {
    let base = chain_bundle(case_id, false);
    let mut with_revert = base.clone();
    with_revert.events.push(ProvenanceEventV1 {
        kind: ProvenanceEventKindV1::Revert,
        revision_hash: "revertsha01".to_string(),
        target_ref: format!(
            "{}#{}",
            base.pull_request
                .as_ref()
                .map(|p| p.repo.as_str())
                .unwrap_or("owner/repo"),
            base.pull_request.as_ref().map(|p| p.number).unwrap_or(77)
        ),
        occurred_at: "2026-07-14T01:00:00Z".to_string(),
    });
    // Snapshots must remain byte-identical.
    assert_eq!(base.issue, with_revert.issue);
    assert_eq!(base.pull_request, with_revert.pull_request);
    (base, with_revert)
}

/// Bundle for a frozen manifest case (matches `valid_20_cases` numbering).
pub fn fixture_bundle_for_case(case: &CorpusCaseV1) -> CaseCorpusBundle {
    let issue_json = sample_issue_json(
        case.issue_number,
        "CLOSED",
        &format!("Body for {}", case.case_id),
        "2026-07-13T00:00:00Z",
    );
    let pr_json = case
        .pr_number
        .map(|n| (n, sample_pr_json(n, "MERGED", true, "2026-07-13T01:00:00Z")));
    let pr_arg = pr_json.as_ref().map(|(n, j)| (*n, j));
    let skeleton = assemble_case_bundle(
        &case.case_id,
        &case.repo,
        case.issue_number,
        &issue_json,
        pr_arg,
        vec![],
        "2026-07-14T00:00:00Z",
    )
    .expect("fixture case must assemble");

    let mut events = vec![
        ProvenanceEventV1 {
            kind: ProvenanceEventKindV1::IssueOpened,
            revision_hash: skeleton.issue.issue_snapshot_hash.clone(),
            target_ref: format!("{}#{}", case.repo, case.issue_number),
            occurred_at: "2026-07-10T00:00:00Z".to_string(),
        },
        ProvenanceEventV1 {
            kind: ProvenanceEventKindV1::IssueClosed,
            revision_hash: skeleton.issue.issue_snapshot_hash.clone(),
            target_ref: format!("{}#{}", case.repo, case.issue_number),
            occurred_at: "2026-07-13T02:00:00Z".to_string(),
        },
    ];
    if let Some(pr) = skeleton.pull_request.as_ref() {
        let merge_sha = pr
            .merge_commit_sha
            .clone()
            .unwrap_or_else(|| pr.head_sha.clone());
        events.insert(
            1,
            ProvenanceEventV1 {
                kind: ProvenanceEventKindV1::PrOpened,
                revision_hash: pr.pr_snapshot_hash.clone(),
                target_ref: pr.pr_ref.clone(),
                occurred_at: "2026-07-12T00:00:00Z".to_string(),
            },
        );
        events.insert(
            2,
            ProvenanceEventV1 {
                kind: ProvenanceEventKindV1::PrMerged,
                revision_hash: merge_sha,
                target_ref: pr.pr_ref.clone(),
                occurred_at: "2026-07-13T01:00:00Z".to_string(),
            },
        );
    }

    let mut bundle = skeleton;
    bundle.events = events;
    bundle
}

pub fn fixture_reader_for_case(case: &CorpusCaseV1) -> FixtureCorpusReader {
    let mut reader = FixtureCorpusReader::new();
    reader.insert_issue(
        &case.repo,
        case.issue_number,
        sample_issue_json(
            case.issue_number,
            "OPEN",
            &format!("Body for {}", case.case_id),
            "2026-07-13T00:00:00Z",
        ),
    );
    if let Some(pr) = case.pr_number {
        reader.insert_pr(
            &case.repo,
            pr,
            sample_pr_json(pr, "OPEN", false, "2026-07-13T01:00:00Z"),
        );
    }
    reader
}
