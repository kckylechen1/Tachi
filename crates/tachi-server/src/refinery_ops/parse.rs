//! Deterministic, parser-first extraction of the structured markers Issue
//! Refinery understands (canon doc §2.2: "The evidence compiler uses
//! deterministic parsers ... No issue workflow may reuse the daily distill
//! behavior"). No free-text/substring keyword classifiers are used here —
//! the canon doc explicitly abandons that approach (§10: "free-text
//! substring verdict classifiers").
//!
//! Recognized line conventions (this leaf's documented syntax choice for the
//! relation lines; `Spec-Ref:` itself is the canon doc §5 frozen syntax):
//!
//! ```text
//! Spec-Ref: owner/repo:docs/path.md@<commit_sha>/<blob_sha>#<section>
//! Blocks: #123, #124
//! Depends-On: #45
//! Duplicate-Of: #67
//! Supersedes: #89
//! Parent-Of: #12
//! Related: #34
//! ```

use tachi_params::{CommentRevisionV1, IssueRelationKindV1, IssueSnapshotV1, SourceSpanV1};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpecRefLine {
    pub(crate) repo: String,
    pub(crate) path: String,
    pub(crate) commit_sha: String,
    pub(crate) blob_sha: String,
    pub(crate) section: String,
    pub(crate) span: SourceSpanV1,
}

/// Scan `text` line by line (byte-offset tracked) for `Spec-Ref:` lines.
/// Malformed lines are silently skipped (not a hard parse error) — an
/// unresolvable Spec-Ref simply never becomes a linked spec, which the
/// caller must treat as a missing anchor if that was the issue's only
/// grounding path.
pub(crate) fn parse_spec_ref_lines(text: &str) -> Vec<SpecRefLine> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']).trim_start();
        if let Some(rest) = trimmed.strip_prefix("Spec-Ref:") {
            if let Some(mut parsed) = parse_spec_ref_value(rest.trim()) {
                parsed.span = SourceSpanV1 {
                    start_byte: offset,
                    end_byte: offset + line.len(),
                };
                out.push(parsed);
            }
        }
        offset += line.len();
    }
    out
}

fn parse_spec_ref_value(value: &str) -> Option<SpecRefLine> {
    // owner/repo:path@commit_sha/blob_sha#section
    let (repo_and_path, rest) = value.split_once('@')?;
    let (repo, path) = repo_and_path.split_once(':')?;
    let (shas, section) = rest.split_once('#')?;
    let (commit_sha, blob_sha) = shas.split_once('/')?;
    if repo.matches('/').count() != 1 || repo.is_empty() || path.is_empty() {
        return None;
    }
    if commit_sha.is_empty() || blob_sha.is_empty() || section.is_empty() {
        return None;
    }
    Some(SpecRefLine {
        repo: repo.to_string(),
        path: path.to_string(),
        commit_sha: commit_sha.to_string(),
        blob_sha: blob_sha.to_string(),
        section: section.to_string(),
        span: SourceSpanV1 {
            start_byte: 0,
            end_byte: 0,
        },
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelationLine {
    pub(crate) kind: IssueRelationKindV1,
    pub(crate) target_ref: String,
    pub(crate) span: SourceSpanV1,
}

const RELATION_PREFIXES: &[(&str, IssueRelationKindV1)] = &[
    ("Blocks:", IssueRelationKindV1::Blocks),
    ("Depends-On:", IssueRelationKindV1::DependsOn),
    ("Duplicate-Of:", IssueRelationKindV1::DuplicateOf),
    ("Supersedes:", IssueRelationKindV1::Supersedes),
    ("Parent-Of:", IssueRelationKindV1::ParentOf),
    ("Related:", IssueRelationKindV1::Related),
];

pub(crate) fn parse_relation_lines(text: &str) -> Vec<RelationLine> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']).trim_start();
        let span = SourceSpanV1 {
            start_byte: offset,
            end_byte: offset + line.len(),
        };
        if let Some((kind, rest)) = match_relation_prefix(trimmed) {
            for target in rest.split(',') {
                let target = target.trim();
                if !target.is_empty() {
                    out.push(RelationLine {
                        kind,
                        target_ref: target.to_string(),
                        span,
                    });
                }
            }
        }
        offset += line.len();
    }
    out
}

fn match_relation_prefix(line: &str) -> Option<(IssueRelationKindV1, &str)> {
    for &(prefix, kind) in RELATION_PREFIXES {
        if let Some(rest) = line.strip_prefix(prefix) {
            return Some((kind, rest.trim()));
        }
    }
    None
}

fn comment_has_structured_marker(body: &str) -> bool {
    body.lines().any(|line| {
        let t = line.trim_start();
        t.starts_with("Spec-Ref:") || match_relation_prefix(t).is_some()
    })
}

/// Parse a `gh issue view --json ...` result payload (already fetched by the
/// caller via the existing `tachi_gh(action='issue_read')` read facility)
/// into an `IssueSnapshotV1`. Pure: no I/O, no gh/git calls — takes the
/// already-parsed JSON `Value` so tests can exercise this with fixture JSON
/// and never touch the network (#1002 acceptance criterion 7).
pub(crate) fn parse_issue_snapshot_from_gh_json(
    repo: &str,
    number: u64,
    result: &serde_json::Value,
) -> IssueSnapshotV1 {
    let issue_ref = format!("{repo}#{number}");
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
    let body = tachi_params::normalize_line_endings(&raw_body);
    let state = result
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("UNKNOWN")
        .to_ascii_uppercase();
    let labels = result
        .get("labels")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.get("name")
                        .and_then(|v| v.as_str())
                        .or_else(|| item.as_str())
                })
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let milestone = result
        .get("milestone")
        .and_then(|m| m.get("title"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let updated_at = result
        .get("updatedAt")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let comments = result
        .get("comments")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let selected_comment_revisions: Vec<CommentRevisionV1> = comments
        .iter()
        .filter_map(|c| {
            let raw_comment_body = c.get("body").and_then(|v| v.as_str())?;
            if !comment_has_structured_marker(raw_comment_body) {
                return None;
            }
            let normalized_body = tachi_params::normalize_line_endings(raw_comment_body);
            let comment_id = c
                .get("id")
                .and_then(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .or_else(|| v.as_u64().map(|n| n.to_string()))
                })
                .unwrap_or_default();
            let updated_at = c
                .get("createdAt")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let author = c
                .get("author")
                .and_then(|a| a.get("login"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let body_hash = tachi_params::sha256_hex(normalized_body.as_bytes());
            Some(CommentRevisionV1 {
                comment_id,
                author,
                updated_at,
                body: normalized_body,
                body_hash,
            })
        })
        .collect();

    let dependency_refs: Vec<String> = {
        let mut all_text = body.clone();
        for c in &selected_comment_revisions {
            all_text.push('\n');
            all_text.push_str(&c.body);
        }
        let mut refs: Vec<String> = parse_relation_lines(&all_text)
            .into_iter()
            .filter(|r| {
                matches!(
                    r.kind,
                    IssueRelationKindV1::Blocks | IssueRelationKindV1::DependsOn
                )
            })
            .map(|r| r.target_ref)
            .collect();
        refs.sort();
        refs.dedup();
        refs
    };

    let issue_body_hash = tachi_params::compute_issue_body_hash(&raw_body);
    let mut snapshot = IssueSnapshotV1 {
        issue_ref,
        repo: repo.to_string(),
        number,
        title,
        body,
        state,
        labels,
        milestone,
        dependency_refs,
        selected_comment_revisions,
        updated_at,
        issue_body_hash,
        issue_snapshot_hash: String::new(),
    };
    snapshot.issue_snapshot_hash = snapshot.compute_snapshot_hash().unwrap_or_default();
    snapshot
}
