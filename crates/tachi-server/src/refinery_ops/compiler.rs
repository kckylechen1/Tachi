//! Evidence compiler (canon doc §2.1 "Evidence compiler" seat): atomizes an
//! `IssueSnapshotV1` body into `ClaimV1`s with byte-precise source spans and
//! full coverage accounting. Pure — no I/O — so every acceptance fixture can
//! exercise it directly with constructed data.
//!
//! Claim atomization is paragraph-based (blank-line separated blocks). Every
//! byte of the body lands in exactly one of `claims[].source_span` or
//! `coverage.omitted_spans` — no per-claim length cap and no dropped tail,
//! unlike the daily-distill 800-char truncation this workflow must not
//! repeat (canon doc §2.2).

use tachi_params::{
    AnchorKindV1, AnchorV1, CanonicalDocRefV1, ClaimV1, ClaimVerificationV1, CoverageV1,
    GroundingStatusV1, IssueEvidenceV1, IssueRelationV1, IssueSnapshotV1, SourceSpanV1,
};

/// Split `body` into non-blank paragraph spans (claims) and the blank-line
/// separator spans between them (omitted, but still accounted for — never
/// silently dropped). Every returned span's byte offsets sit on ASCII
/// newline/space/tab boundaries, which are always valid UTF-8 char
/// boundaries regardless of surrounding multibyte content.
pub(crate) fn split_paragraphs(body: &str) -> (Vec<SourceSpanV1>, Vec<SourceSpanV1>) {
    let bytes = body.as_bytes();
    let len = bytes.len();
    if len == 0 {
        return (Vec::new(), Vec::new());
    }

    let mut separators: Vec<(usize, usize)> = Vec::new();
    let mut idx = 0usize;
    while idx + 1 < len {
        if bytes[idx] == b'\n' {
            let mut k = idx + 1;
            while k < len && matches!(bytes[k], b' ' | b'\t' | b'\r') {
                k += 1;
            }
            if k < len && bytes[k] == b'\n' {
                let sep_start = idx;
                let mut sep_end = k + 1;
                loop {
                    let mut k2 = sep_end;
                    while k2 < len && matches!(bytes[k2], b' ' | b'\t' | b'\r') {
                        k2 += 1;
                    }
                    if k2 < len && bytes[k2] == b'\n' {
                        sep_end = k2 + 1;
                    } else {
                        break;
                    }
                }
                separators.push((sep_start, sep_end));
                idx = sep_end;
                continue;
            }
        }
        idx += 1;
    }

    let mut claim_spans = Vec::new();
    let mut omitted_spans = Vec::new();
    let mut cursor = 0usize;
    for (sep_start, sep_end) in separators {
        if sep_start > cursor {
            claim_spans.push(SourceSpanV1 {
                start_byte: cursor,
                end_byte: sep_start,
            });
        }
        omitted_spans.push(SourceSpanV1 {
            start_byte: sep_start,
            end_byte: sep_end,
        });
        cursor = sep_end;
    }
    if cursor < len {
        claim_spans.push(SourceSpanV1 {
            start_byte: cursor,
            end_byte: len,
        });
    }
    (claim_spans, omitted_spans)
}

/// Build an [`IssueEvidenceV1`] from an already-hashed snapshot plus
/// pre-resolved anchors. `doc_anchors_by_span`/`issue_ref_anchors_by_span`
/// carry the byte span of the `Spec-Ref:`/relation line each anchor was
/// parsed from (see `refinery_ops::parse`), so a claim only receives the
/// anchors whose declaring line falls inside that claim's paragraph.
///
/// Every claim's `verification` is `ClaimVerificationV1::ModelOnly` — this
/// builder never has repo-tool evidence in hand, and the type itself has no
/// zero-evidence `Verified` constructor (#1002 acceptance criterion 6).
pub(crate) fn build_issue_evidence(
    snapshot: IssueSnapshotV1,
    linked_specs: Vec<CanonicalDocRefV1>,
    relations: Vec<IssueRelationV1>,
    grounding_status: GroundingStatusV1,
    doc_anchors_by_span: &[(SourceSpanV1, CanonicalDocRefV1)],
    issue_ref_anchors_by_span: &[(SourceSpanV1, String)],
) -> IssueEvidenceV1 {
    let (claim_spans, omitted_spans) = split_paragraphs(&snapshot.body);
    let source_bytes = snapshot.body.len();
    let mut covered_bytes = 0usize;

    let claims: Vec<ClaimV1> = claim_spans
        .into_iter()
        .enumerate()
        .map(|(i, span)| {
            covered_bytes += span.len();
            let text = snapshot.body[span.start_byte..span.end_byte].to_string();
            let mut anchors = Vec::new();
            for (anchor_span, doc_ref) in doc_anchors_by_span {
                if span.overlaps(anchor_span) {
                    anchors.push(AnchorV1 {
                        kind: AnchorKindV1::CanonicalDoc,
                        doc_ref: Some(doc_ref.clone()),
                        issue_ref: None,
                    });
                }
            }
            for (anchor_span, target_ref) in issue_ref_anchors_by_span {
                if span.overlaps(anchor_span) {
                    anchors.push(AnchorV1 {
                        kind: AnchorKindV1::IssueRef,
                        doc_ref: None,
                        issue_ref: Some(target_ref.clone()),
                    });
                }
            }
            ClaimV1 {
                claim_id: format!("{}-claim-{i}", snapshot.issue_ref),
                text,
                source_span: span,
                anchors,
                verification: ClaimVerificationV1::ModelOnly,
            }
        })
        .collect();

    let coverage = CoverageV1 {
        source_bytes,
        covered_bytes,
        omitted_spans,
    };

    IssueEvidenceV1 {
        issue_ref: snapshot.issue_ref.clone(),
        issue_body_hash: snapshot.issue_body_hash.clone(),
        issue_snapshot_hash: snapshot.issue_snapshot_hash.clone(),
        issue_updated_at: snapshot.updated_at.clone(),
        grounding_status,
        issue_snapshot: snapshot,
        claims,
        relations,
        linked_specs,
        coverage,
    }
}
