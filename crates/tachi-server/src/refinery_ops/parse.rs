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
//!
//! A relation line may carry an optional trailing `[state]` annotation
//! applying to every target on that line — also this leaf's own convention,
//! not canon-frozen, e.g. `Supersedes: #89 [closed_shipped]`. The parser
//! preserves that prose annotation, but the live pipeline must treat it as
//! advisory until #1105 supplies an authenticated cross-reference lookup.

use super::disposition::RelatedIssueStateV1;
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

/// A line that declared itself a `Spec-Ref:` but failed to parse against
/// the frozen syntax (canon doc §5). This must NOT be silently skipped —
/// the issue explicitly claimed a canonical anchor here and failed to
/// supply a usable one, which is exactly the "requested anchor cannot be
/// resolved" case canon doc §4.1 requires to degrade the whole packet
/// (F1: previously a malformed line was simply invisible to the grounding
/// check, leaving the packet falsely `Grounded`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MalformedSpecRefLine {
    pub(crate) raw: String,
    pub(crate) span: SourceSpanV1,
}

/// Scan `text` line by line (byte-offset tracked) for `Spec-Ref:` lines,
/// returning both successfully parsed refs and malformed ones (a line that
/// started with `Spec-Ref:` but didn't match the frozen syntax) — the
/// caller must treat every malformed line as a missing anchor, not ignore
/// it (F1).
pub(crate) fn parse_spec_ref_lines(text: &str) -> (Vec<SpecRefLine>, Vec<MalformedSpecRefLine>) {
    let mut ok = Vec::new();
    let mut malformed = Vec::new();
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']).trim_start();
        if let Some(rest) = trimmed.strip_prefix("Spec-Ref:") {
            let span = SourceSpanV1 {
                start_byte: offset,
                end_byte: offset + line.len(),
            };
            match parse_spec_ref_value(rest.trim()) {
                Some(mut parsed) => {
                    parsed.span = span;
                    ok.push(parsed);
                }
                None => malformed.push(MalformedSpecRefLine {
                    raw: rest.trim().to_string(),
                    span,
                }),
            }
        }
        offset += line.len();
    }
    (ok, malformed)
}

pub(super) fn parse_spec_ref_value(value: &str) -> Option<SpecRefLine> {
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
    pub(crate) state: RelatedIssueStateV1,
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
            let (state, ref_list) = parse_related_state_suffix(rest);
            for target in ref_list.split(',') {
                let target = target.trim();
                if !target.is_empty() {
                    out.push(RelationLine {
                        kind,
                        target_ref: target.to_string(),
                        span,
                        state,
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

/// Peel an optional trailing `[state]` annotation off a relation line's
/// target-ref list (see module docs for the recognized values). Returns the
/// parsed state (defaulting to `Unknown`) and the remaining text to split on
/// commas for target refs.
fn parse_related_state_suffix(rest: &str) -> (RelatedIssueStateV1, &str) {
    let trimmed = rest.trim_end();
    if let Some(bracket_start) = trimmed.rfind('[') {
        if trimmed.ends_with(']') && bracket_start < trimmed.len() - 1 {
            let inner = &trimmed[bracket_start + 1..trimmed.len() - 1];
            let state = match inner.trim().to_ascii_lowercase().as_str() {
                "open" => RelatedIssueStateV1::Open,
                "closed_shipped" => RelatedIssueStateV1::ClosedShipped,
                "closed_unshipped" => RelatedIssueStateV1::ClosedUnshipped,
                _ => RelatedIssueStateV1::Unknown,
            };
            return (state, trimmed[..bracket_start].trim_end());
        }
    }
    (RelatedIssueStateV1::Unknown, rest)
}

fn comment_has_structured_marker(body: &str) -> bool {
    body.contains("Updated pin: Spec-Ref:")
        || body.lines().any(|line| {
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
            // F5: prefer `updatedAt` — a comment edited without changing its
            // text (unusual, but possible) must still produce a different
            // semantic snapshot hash. `createdAt` never changes after an
            // edit and would make such an edit invisible to
            // `compute_snapshot_hash`. Unverified whether `gh issue view
            // --json comments` actually exposes `updatedAt` in this gh CLI
            // version (no network access to confirm) — if it doesn't, this
            // falls back to `createdAt` (today's behavior, not a
            // regression); Oz/a live smoke test should confirm the real
            // field name.
            let updated_at = c
                .get("updatedAt")
                .and_then(|v| v.as_str())
                .or_else(|| c.get("createdAt").and_then(|v| v.as_str()))
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
