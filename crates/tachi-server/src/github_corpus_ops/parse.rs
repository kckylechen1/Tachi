//! Pure parsers: already-fetched gh JSON → typed corpus snapshots.
//!
//! No I/O. Issue parsing reuses
//! [`crate::refinery_ops::parse::parse_issue_snapshot_from_gh_json`].

use serde_json::Value;
use tachi_params::{
    normalize_line_endings, sha256_hex, IssueSnapshotV1, PrCheckV1, PrReviewV1,
    PullRequestSnapshotV1,
};

use crate::refinery_ops::parse::parse_issue_snapshot_from_gh_json;

/// Closed provenance vocabulary for a corpus case's append-only event chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceEventKindV1 {
    IssueOpened,
    CommentSelected,
    PrOpened,
    PrMerged,
    IssueClosed,
    Reopen,
    Revert,
}

impl ProvenanceEventKindV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IssueOpened => "issue_opened",
            Self::CommentSelected => "comment_selected",
            Self::PrOpened => "pr_opened",
            Self::PrMerged => "pr_merged",
            Self::IssueClosed => "issue_closed",
            Self::Reopen => "reopen",
            Self::Revert => "revert",
        }
    }
}

/// One append-only provenance hop. Carries its own revision hash so later
/// overturn events can append without rewriting earlier rows.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProvenanceEventV1 {
    pub kind: ProvenanceEventKindV1,
    /// Immutable revision hash bound to this hop (issue/PR/comment/commit).
    /// When labeled as an issue/PR snapshot hash, this MUST be the real
    /// `compute_snapshot_hash` output — never a synthesized label.
    pub revision_hash: String,
    /// Optional target ref (issue_ref / pr_ref / comment_id / commit sha).
    pub target_ref: String,
    pub occurred_at: String,
}

/// Assembled per-case corpus: issue + optional PR + ordered provenance events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseCorpusBundle {
    pub case_id: String,
    pub issue: IssueSnapshotV1,
    pub pull_request: Option<PullRequestSnapshotV1>,
    pub events: Vec<ProvenanceEventV1>,
    pub captured_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    MissingProvenanceField { field: &'static str, pr_ref: String },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingProvenanceField { field, pr_ref } => write!(
                f,
                "refusing PR snapshot for {pr_ref}: required provenance field `{field}` is missing or empty"
            ),
        }
    }
}

fn require_nonempty_str<'a>(
    result: &'a Value,
    keys: &[&str],
    field: &'static str,
    pr_ref: &str,
) -> Result<&'a str, ParseError> {
    for key in keys {
        if let Some(s) = result.get(*key).and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return Ok(s);
            }
        }
    }
    Err(ParseError::MissingProvenanceField {
        field,
        pr_ref: pr_ref.to_string(),
    })
}

/// Parse a `gh pr view --json ...` (or fixture-equivalent) payload into
/// [`PullRequestSnapshotV1`]. Pure: no I/O.
///
/// Required provenance fields (`head_sha` / `base_sha` / `updated_at`) are
/// refused when absent or empty — never defaulted to `""` and hashed.
pub fn parse_pr_snapshot_from_gh_json(
    repo: &str,
    number: u64,
    result: &Value,
) -> Result<PullRequestSnapshotV1, ParseError> {
    let pr_ref = format!("{repo}#{number}");
    let title = result
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let raw_body = result
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let body = normalize_line_endings(&raw_body);
    let state = result
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("UNKNOWN")
        .to_ascii_uppercase();
    let head_sha =
        require_nonempty_str(result, &["headRefOid", "head_sha"], "head_sha", &pr_ref)?.to_string();
    let base_sha =
        require_nonempty_str(result, &["baseRefOid", "base_sha"], "base_sha", &pr_ref)?.to_string();
    let updated_at =
        require_nonempty_str(result, &["updatedAt", "updated_at"], "updated_at", &pr_ref)?
            .to_string();

    let merged = result
        .get("merged")
        .and_then(|v| v.as_bool())
        .unwrap_or_else(|| state == "MERGED");
    let merge_commit_sha = result
        .get("mergeCommit")
        .and_then(|m| m.get("oid"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            result
                .get("merge_commit_sha")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        })
        .map(str::to_string);

    let reviews = parse_reviews(result)?;
    let checks = parse_checks(result);

    let pr_body_hash = sha256_hex(body.as_bytes());
    let mut snapshot = PullRequestSnapshotV1 {
        pr_ref,
        repo: repo.to_string(),
        number,
        title,
        body,
        state,
        head_sha,
        base_sha,
        reviews,
        checks,
        merged,
        merge_commit_sha,
        updated_at,
        pr_body_hash,
        pr_snapshot_hash: String::new(),
    };
    snapshot.pr_snapshot_hash =
        snapshot
            .compute_snapshot_hash()
            .map_err(|_| ParseError::MissingProvenanceField {
                field: "pr_snapshot_hash",
                pr_ref: format!("{repo}#{number}"),
            })?;
    Ok(snapshot)
}

fn parse_reviews(result: &Value) -> Result<Vec<PrReviewV1>, ParseError> {
    let Some(items) = result.get("reviews").and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(items.len());
    for r in items {
        let author = r
            .get("author")
            .and_then(|a| a.get("login").or_else(|| a.as_str().map(|_| a)))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| r.get("author").and_then(|v| v.as_str()).map(str::to_string));
        let state = r
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("UNKNOWN")
            .to_string();
        // submitted_at is part of the review row's semantic contribution to
        // the PR snapshot hash — refuse empty rather than silently hashing "".
        let submitted_at = r
            .get("submittedAt")
            .or_else(|| r.get("submitted_at"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ParseError::MissingProvenanceField {
                field: "review.submitted_at",
                pr_ref: "reviews[]".to_string(),
            })?
            .to_string();
        out.push(PrReviewV1 {
            author,
            state,
            submitted_at,
        });
    }
    // GitHub exposes reviews as a collection. The timestamp carries temporal
    // meaning; transport order does not. Preserve duplicates and sort by the
    // complete semantic row so equivalent API permutations hash identically.
    out.sort_by(|left, right| {
        (&left.submitted_at, &left.author, &left.state).cmp(&(
            &right.submitted_at,
            &right.author,
            &right.state,
        ))
    });
    Ok(out)
}

fn parse_checks(result: &Value) -> Vec<PrCheckV1> {
    let items = result
        .get("statusCheckRollup")
        .or_else(|| result.get("checks"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut checks = items
        .iter()
        .map(|c| {
            let name = c
                .get("name")
                .or_else(|| c.get("context"))
                .and_then(|v| v.as_str())
                .unwrap_or("check")
                .to_string();
            let conclusion = c
                .get("conclusion")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let status = c
                .get("status")
                .or_else(|| c.get("state"))
                .and_then(|v| v.as_str())
                .unwrap_or("UNKNOWN")
                .to_string();
            PrCheckV1 {
                name,
                conclusion,
                status,
            }
        })
        .collect::<Vec<_>>();
    // Status checks are a current-state collection. Preserve duplicate rows,
    // but remove API transport order from the snapshot identity.
    checks.sort_by(|left, right| {
        (&left.name, &left.status, &left.conclusion).cmp(&(
            &right.name,
            &right.status,
            &right.conclusion,
        ))
    });
    checks
}

/// Assemble a [`CaseCorpusBundle`] from already-fetched issue/PR JSON plus
/// an ordered event list. Pure: no I/O. Refuses under-fetched PR JSON.
pub fn assemble_case_bundle(
    case_id: &str,
    repo: &str,
    issue_number: u64,
    issue_json: &Value,
    pr: Option<(u64, &Value)>,
    events: Vec<ProvenanceEventV1>,
    captured_at: &str,
) -> Result<CaseCorpusBundle, ParseError> {
    let mut issue = parse_issue_snapshot_from_gh_json(repo, issue_number, issue_json);
    // Labels are an unordered GitHub set. Keep multiplicity intact while
    // making the typed snapshot and its hash independent of API order.
    issue.labels.sort();
    issue.issue_snapshot_hash = issue.compute_snapshot_hash().unwrap_or_default();
    let pull_request = match pr {
        Some((n, json)) => Some(parse_pr_snapshot_from_gh_json(repo, n, json)?),
        None => None,
    };
    Ok(CaseCorpusBundle {
        case_id: case_id.to_string(),
        issue,
        pull_request,
        events,
        captured_at: captured_at.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_pr_snapshot_computes_stable_hash() {
        let json = json!({
            "title": "Fix gate",
            "body": "see https://example.com/untrusted",
            "state": "MERGED",
            "headRefOid": "deadbeef",
            "baseRefOid": "cafebabe",
            "updatedAt": "2026-07-13T00:00:00Z",
            "merged": true,
            "mergeCommit": { "oid": "merge111" },
            "reviews": [{
                "author": { "login": "alice" },
                "state": "APPROVED",
                "submittedAt": "2026-07-12T00:00:00Z"
            }],
            "statusCheckRollup": [{
                "name": "ci",
                "conclusion": "SUCCESS",
                "status": "COMPLETED"
            }]
        });
        let snap = parse_pr_snapshot_from_gh_json("owner/repo", 42, &json).expect("parse");
        assert_eq!(snap.pr_ref, "owner/repo#42");
        assert!(snap.merged);
        assert_eq!(snap.merge_commit_sha.as_deref(), Some("merge111"));
        assert!(!snap.pr_snapshot_hash.is_empty());
        assert_eq!(snap.pr_snapshot_hash, snap.compute_snapshot_hash().unwrap());
        // External URL stays as text; parser never fetches.
        assert!(snap.body.contains("https://example.com/untrusted"));
    }

    /// E1 discrimination: missing head/base/updated_at must REFUSE, not
    /// silently hash empty defaults into a valid-looking snapshot.
    #[test]
    fn missing_pr_provenance_fields_are_refused() {
        let base = json!({
            "title": "Fix gate",
            "body": "body",
            "state": "OPEN",
            "headRefOid": "deadbeef",
            "baseRefOid": "cafebabe",
            "updatedAt": "2026-07-13T00:00:00Z",
        });

        for field in ["headRefOid", "baseRefOid", "updatedAt"] {
            let mut bad = base.clone();
            bad.as_object_mut().unwrap().remove(field);
            let err = parse_pr_snapshot_from_gh_json("owner/repo", 1, &bad)
                .expect_err(&format!("missing {field} must refuse"));
            assert!(
                matches!(err, ParseError::MissingProvenanceField { .. }),
                "missing {field}: {err}"
            );
        }

        // Present-but-empty is also refused.
        let mut empty_head = base.clone();
        empty_head["headRefOid"] = json!("");
        assert!(parse_pr_snapshot_from_gh_json("owner/repo", 1, &empty_head).is_err());
    }
}
