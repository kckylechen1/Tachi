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

/// Closed issue + merged PR + reopen/revert chain for provenance tests.
pub fn chain_bundle(case_id: &str, with_overturn: bool) -> CaseCorpusBundle {
    let repo = "owner/repo";
    let issue_number = 42u64;
    let pr_number = 77u64;
    let issue_json = sample_issue_json(
        issue_number,
        if with_overturn { "OPEN" } else { "CLOSED" },
        "Pilot body with https://evil.example/payload (must stay text).",
        "2026-07-13T00:00:00Z",
    );
    let pr_json = sample_pr_json(pr_number, "MERGED", true, "2026-07-13T01:00:00Z");

    let mut events = vec![
        ProvenanceEventV1 {
            kind: ProvenanceEventKindV1::IssueOpened,
            revision_hash: "rev-open".to_string(),
            target_ref: format!("{repo}#{issue_number}"),
            occurred_at: "2026-07-10T00:00:00Z".to_string(),
        },
        ProvenanceEventV1 {
            kind: ProvenanceEventKindV1::CommentSelected,
            revision_hash: "rev-comment".to_string(),
            target_ref: format!("c-{issue_number}"),
            occurred_at: "2026-07-11T00:00:00Z".to_string(),
        },
        ProvenanceEventV1 {
            kind: ProvenanceEventKindV1::PrOpened,
            revision_hash: "rev-pr-open".to_string(),
            target_ref: format!("{repo}#{pr_number}"),
            occurred_at: "2026-07-12T00:00:00Z".to_string(),
        },
        ProvenanceEventV1 {
            kind: ProvenanceEventKindV1::PrMerged,
            revision_hash: format!("merge{pr_number:04x}"),
            target_ref: format!("{repo}#{pr_number}"),
            occurred_at: "2026-07-13T01:00:00Z".to_string(),
        },
        ProvenanceEventV1 {
            kind: ProvenanceEventKindV1::IssueClosed,
            revision_hash: "rev-closed".to_string(),
            target_ref: format!("{repo}#{issue_number}"),
            occurred_at: "2026-07-13T02:00:00Z".to_string(),
        },
    ];
    if with_overturn {
        events.push(ProvenanceEventV1 {
            kind: ProvenanceEventKindV1::Reopen,
            revision_hash: "rev-reopen".to_string(),
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

    assemble_case_bundle(
        case_id,
        repo,
        issue_number,
        &issue_json,
        Some((pr_number, &pr_json)),
        events,
        "2026-07-14T02:00:00Z",
    )
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
    let mut events = vec![
        ProvenanceEventV1 {
            kind: ProvenanceEventKindV1::IssueOpened,
            revision_hash: format!("open-{}", case.issue_number),
            target_ref: format!("{}#{}", case.repo, case.issue_number),
            occurred_at: "2026-07-10T00:00:00Z".to_string(),
        },
        ProvenanceEventV1 {
            kind: ProvenanceEventKindV1::IssueClosed,
            revision_hash: format!("closed-{}", case.issue_number),
            target_ref: format!("{}#{}", case.repo, case.issue_number),
            occurred_at: "2026-07-13T02:00:00Z".to_string(),
        },
    ];
    if let Some(pr) = case.pr_number {
        events.insert(
            1,
            ProvenanceEventV1 {
                kind: ProvenanceEventKindV1::PrOpened,
                revision_hash: format!("pr-open-{pr}"),
                target_ref: format!("{}#{pr}", case.repo),
                occurred_at: "2026-07-12T00:00:00Z".to_string(),
            },
        );
        events.insert(
            2,
            ProvenanceEventV1 {
                kind: ProvenanceEventKindV1::PrMerged,
                revision_hash: format!("merge{pr:04x}"),
                target_ref: format!("{}#{pr}", case.repo),
                occurred_at: "2026-07-13T01:00:00Z".to_string(),
            },
        );
    }
    assemble_case_bundle(
        &case.case_id,
        &case.repo,
        case.issue_number,
        &issue_json,
        pr_arg,
        events,
        "2026-07-14T00:00:00Z",
    )
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
