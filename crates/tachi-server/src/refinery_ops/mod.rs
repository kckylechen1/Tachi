//! Issue Refinery (#1002): a manually triggered, read-only, proposal-only
//! semantic refinement of one GitHub issue. Orchestrates the existing GitHub
//! read facility (`tachi_gh(action='issue_read')`) and local git state into
//! typed evidence/disposition packets — see
//! `docs/engineering/architecture/issue-refinery-memory-lanes.md` §4/§5 for
//! the frozen design authority.
//!
//! Safety boundary (canon doc §4.3), unconditionally true of everything in
//! this module: never closes/reopens an issue, never edits an issue body or
//! canonical doc, never establishes a precedent, never treats an issue
//! attachment/external download as evidence, never invents an execution
//! backend, and is only reachable on demand (no cron/resident scheduling).
//! `IssueDispositionProposalV1::preview_only` is unconditionally `true` in
//! this leaf.
//!
//! [`build_refinery_packet`] is the pure core (validate → parse → resolve
//! anchors via an injected [`doc_resolver::DocRefResolver`] → compile
//! evidence → propose disposition) used by BOTH the live
//! `handle_refine_issues` action (via `doc_resolver::GitRefResolver`, real
//! git calls) and every fixture test in `refinery_ops::tests` (via a
//! fixture resolver, zero I/O) — the same production code path is what the
//! acceptance tests exercise, not a parallel re-implementation.
//!
//! Fail-closed grounding (F1/R4-1, build-seat REQUEST-CHANGES): grounding
//! degrades to `missing_anchor` — never silently stays `Grounded` — when
//! ANY of: (a) the fetched issue result isn't a well-formed, identity-
//! matching object (catches a truncated/malformed `gh` response that a
//! lenient JSON fallback turned into a stray string, AND wrong-typed/
//! mismatched-number results — see `validate_gh_issue_result`); (b) a
//! `Spec-Ref:` line is present but fails to parse against the frozen syntax
//! (a claimed-but-broken anchor, not silently skipped — see
//! `parse::MalformedSpecRefLine`); (c) a `Spec-Ref:` line parses but the
//! resolver can't verify it — including the declared `#section` not
//! actually existing in the doc's content (R4-1); (d) the resolver's repo
//! identity doesn't match the checkout it's asked to verify against
//! (`doc_resolver::GitRefResolver::known_repo`).
//!
//! Repo-root trust (R4-3, build-seat REQUEST-CHANGES, REGRESSION finding):
//! `handle_refine_issues` never hands `GitRefResolver` an ambient
//! `current_dir()` and calls it a day — daemon-mode CWD can be the
//! runtime/daemon directory, not this repo's checkout. [`resolve_known_repo_root`]
//! cross-checks the candidate checkout's `origin` remote against the
//! requested repo; when it can't establish a verified match, the action
//! uses [`doc_resolver::NullDocResolver`] instead of guessing — every
//! anchor comes back unresolved and no repo revision is ever reported,
//! which (R4-2) forces a real "repo revision unavailable" contradiction and
//! makes the resulting proposal fail `check_proposal_replay`'s repo axis.
//!
//! Honestly-declared signal gaps (F7): `dispatch_packet_complete` is NOT
//! derived in this leaf — there is no defined, canon-backed criterion yet
//! for what makes a dispatch packet "complete" from a raw issue body, so it
//! stays `None` (never defaults to a measured-looking `Some(true)`).
//! `stale_body_signal` IS derived, from real local data: a resolver
//! `Unresolved` reason indicating blob-sha drift (the doc still exists and
//! is still reachable, but its content changed) means the issue's own
//! Spec-Ref pin refers to a superseded doc snapshot — a real, zero-extra-IO
//! staleness signal, not a placeholder default.
//!
//! Known scope gap (tracked here, not hidden): related-issue state (canon
//! doc §4.1 input-order step 4) is wired ONLY through each relation line's
//! own optional `[state]` annotation (see `parse::parse_related_state_suffix`)
//! — a bounded, zero-extra-IO signal an issue author/dispatch tool can set
//! explicitly. It is NOT yet a live per-relation GitHub cross-reference
//! (fetching the target issue's real state/labels); that fuller version,
//! and scope-collision detection, remain a follow-up slice.
//! `shipped_evidence` live derivation is under active contract-interpretation
//! dispute (sent to arbitration) and deliberately untouched this round.

mod compiler;
mod disposition;
mod doc_resolver;
mod parse;

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod tests;

use crate::gh_ops::handle_tachi_gh;
use crate::task_lifecycle::parse_issue_ref;
use crate::tool_params::{
    CanonicalDocRefV1, GroundingStatusV1, IssueDispositionProposalV1, IssueEvidenceV1,
    IssueRelationV1, RepoRevisionV1, SourceSpanV1, TachiGhParams, TachiTaskParams,
};
use crate::MemoryServer;
use doc_resolver::{DocRefResolver, DocResolution, GitRefResolver, NullDocResolver};

const TRUSTED_REF: &str = "origin/main";

pub(crate) async fn handle_refine_issues(
    server: &MemoryServer,
    params: &TachiTaskParams,
) -> Result<String, String> {
    let raw_ref = params
        .issue_ref
        .clone()
        .or_else(|| {
            let repo = params.repo.clone()?;
            let number = params.number?;
            Some(format!("{repo}#{number}"))
        })
        .ok_or_else(|| {
            "issue_ref (or repo+number) is required for action='refine_issues'".to_string()
        })?;
    let target = parse_issue_ref(&raw_ref, params.repo.as_deref())
        .ok_or_else(|| format!("could not parse issue_ref '{raw_ref}'"))?;

    let raw = handle_tachi_gh(
        server,
        TachiGhParams {
            action: "issue_read".to_string(),
            repo: Some(target.repo.clone()),
            number: Some(target.number),
            dry_run: Some(true),
            ..Default::default()
        },
    )
    .await?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse issue_read: {e}"))?;
    let result = value
        .get("result")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    // R4-3: never hand GitRefResolver an unverified ambient current_dir().
    let resolver: Box<dyn DocRefResolver> = match resolve_known_repo_root(&target.repo) {
        Some(repo_root) => Box::new(GitRefResolver {
            repo_root,
            known_repo: target.repo.clone(),
        }),
        None => Box::new(NullDocResolver {
            reason: format!(
                "no reliable repo root found for '{}' — refusing to resolve Spec-Ref anchors \
                 or repo revisions against an unverified ambient cwd",
                target.repo
            ),
        }),
    };
    let captured_at = chrono::Utc::now().to_rfc3339();
    let (evidence, proposal) = build_refinery_packet(
        &target.repo,
        target.number,
        &result,
        resolver.as_ref(),
        &captured_at,
    )?;

    serde_json::to_string(&serde_json::json!({
        "tool": "tachi_task_refine_issues",
        "issue_ref": evidence.issue_ref,
        "evidence": evidence,
        "proposal": proposal,
    }))
    .map_err(|e| format!("serialize refine_issues result: {e}"))
}

/// R4-3: resolve a repo root we can TRUST corresponds to `expected_repo` —
/// never by blindly trusting ambient `current_dir()` (daemon-mode CWD can
/// be the runtime/daemon directory, not this repo's checkout — codex-p25b2
/// REGRESSION finding). Cross-checks the candidate toplevel's `origin`
/// remote URL against `expected_repo` via [`parse_owner_repo_from_git_url`];
/// any failure at any step (no git checkout here, no origin remote, origin
/// points elsewhere) returns `None` rather than guessing. The live
/// git-shelling in this function is NOT unit-tested (same policy as
/// `GitRefResolver`'s own git calls); the URL-parsing/comparison logic it
/// depends on is pure and IS unit-tested (see this file's own tests).
fn resolve_known_repo_root(expected_repo: &str) -> Option<std::path::PathBuf> {
    let start = std::env::current_dir().ok()?;
    let toplevel_output = std::process::Command::new("git")
        .arg("-C")
        .arg(&start)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()
        .ok()?;
    if !toplevel_output.status.success() {
        return None;
    }
    let toplevel = std::path::PathBuf::from(
        String::from_utf8_lossy(&toplevel_output.stdout)
            .trim()
            .to_string(),
    );
    let origin_output = std::process::Command::new("git")
        .arg("-C")
        .arg(&toplevel)
        .arg("remote")
        .arg("get-url")
        .arg("origin")
        .output()
        .ok()?;
    if !origin_output.status.success() {
        return None;
    }
    let origin_url = String::from_utf8_lossy(&origin_output.stdout)
        .trim()
        .to_string();
    let actual_repo = parse_owner_repo_from_git_url(&origin_url)?;
    if actual_repo.eq_ignore_ascii_case(expected_repo) {
        Some(toplevel)
    } else {
        None
    }
}

/// Pure: extract `"owner/repo"` from a git remote URL, handling both
/// `https://github.com/owner/repo.git` and `git@github.com:owner/repo.git`
/// forms. `None` for anything that doesn't parse as a plausible GitHub
/// remote — the caller treats that as "cannot verify", never a match.
fn parse_owner_repo_from_git_url(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches(".git").trim_end_matches('/');
    let after_host = trimmed
        .rsplit_once("github.com/")
        .map(|(_, rest)| rest)
        .or_else(|| trimmed.rsplit_once("github.com:").map(|(_, rest)| rest))?;
    if after_host.is_empty() || after_host.matches('/').count() != 1 {
        return None;
    }
    Some(after_host.to_string())
}

/// F1/R4-1: the fetched `gh issue view --json ...` result must be a
/// well-formed object, identifying the REQUESTED issue by number, with
/// correctly-typed core fields, before anything downstream may treat it as
/// grounding-safe. Catches (among other things) `gh_ops::issues::
/// handle_gh_issue_read`'s truncation fallback, which wraps a
/// non-JSON-parseable (truncated) response as a bare JSON *string* — an
/// object-shaped `.get()` on that string silently returns `None` for every
/// field, producing an all-empty-but-technically-valid snapshot that must
/// NOT be reported as `Grounded`.
fn validate_gh_issue_result(
    result: &serde_json::Value,
    expected_number: u64,
) -> Result<(), String> {
    let Some(obj) = result.as_object() else {
        return Err(format!(
            "issue_read result is not a JSON object (got {}) — likely a truncated/malformed gh response",
            match result {
                serde_json::Value::String(_) => "string",
                serde_json::Value::Null => "null",
                serde_json::Value::Array(_) => "array",
                serde_json::Value::Bool(_) => "bool",
                serde_json::Value::Number(_) => "number",
                serde_json::Value::Object(_) => unreachable!(),
            }
        ));
    };
    match obj.get("number") {
        Some(v) => match v.as_u64() {
            Some(n) if n == expected_number => {}
            Some(n) => {
                return Err(format!(
                    "issue_read result 'number' is {n}, expected {expected_number}"
                ))
            }
            None => {
                return Err(format!(
                    "issue_read result 'number' field is not a number (got {v})"
                ))
            }
        },
        None => return Err("issue_read result is missing 'number'".to_string()),
    }
    match obj.get("title") {
        Some(v) if v.is_string() => {}
        Some(v) => return Err(format!("issue_read result 'title' is not a string (got {v})")),
        None => return Err("issue_read result is missing 'title'".to_string()),
    }
    match obj.get("state") {
        Some(v) if v.is_string() => {}
        Some(v) => return Err(format!("issue_read result 'state' is not a string (got {v})")),
        None => return Err("issue_read result is missing 'state'".to_string()),
    }
    if let Some(v) = obj.get("body") {
        if !v.is_string() && !v.is_null() {
            return Err(format!("issue_read result 'body' is not a string (got {v})"));
        }
    }
    Ok(())
}

/// Pure pipeline: `gh issue view --json ...` result → typed evidence +
/// disposition proposal. Takes an injected [`DocRefResolver`] so tests never
/// shell real git/GitHub (#1002 acceptance criterion 7).
pub(crate) fn build_refinery_packet(
    repo: &str,
    number: u64,
    gh_issue_result: &serde_json::Value,
    resolver: &dyn DocRefResolver,
    captured_at: &str,
) -> Result<(IssueEvidenceV1, IssueDispositionProposalV1), String> {
    let mut grounding_status = GroundingStatusV1::Grounded;
    // Reasons folded into the proposal's `contradictions` verbatim — each
    // entry already carries its own descriptive prefix (see
    // `disposition::propose_disposition`'s doc comment).
    let mut contradiction_reasons: Vec<String> = Vec::new();

    if let Err(reason) = validate_gh_issue_result(gh_issue_result, number) {
        grounding_status = GroundingStatusV1::MissingAnchor;
        contradiction_reasons.push(reason);
    }

    let snapshot = parse::parse_issue_snapshot_from_gh_json(repo, number, gh_issue_result);

    // F3: coverage must account for every input source byte, not only the
    // issue body — every `selected_comment_revisions` entry (already fed
    // into Spec-Ref/relation parsing below) is folded into the SAME source
    // text `compiler::build_issue_evidence` claim-splits and covers. A
    // double-newline join keeps each comment its own paragraph(s), never
    // merged with the body's last paragraph.
    let mut source_text = snapshot.body.clone();
    for c in &snapshot.selected_comment_revisions {
        source_text.push_str("\n\n");
        source_text.push_str(&c.body);
    }

    let (spec_refs, malformed_spec_refs) = parse::parse_spec_ref_lines(&source_text);
    let relation_lines = parse::parse_relation_lines(&source_text);

    // F1: a Spec-Ref line that failed to parse is a claimed-but-broken
    // anchor, not a silently-skipped one.
    for malformed in &malformed_spec_refs {
        grounding_status = GroundingStatusV1::MissingAnchor;
        contradiction_reasons.push(format!(
            "malformed Spec-Ref line (does not match the frozen syntax): {}",
            malformed.raw
        ));
    }

    let mut linked_specs: Vec<CanonicalDocRefV1> = Vec::new();
    let mut doc_anchors_by_span: Vec<(SourceSpanV1, CanonicalDocRefV1)> = Vec::new();
    for spec_ref in &spec_refs {
        match resolver.resolve(
            &spec_ref.repo,
            &spec_ref.path,
            &spec_ref.commit_sha,
            &spec_ref.blob_sha,
            &spec_ref.section,
            TRUSTED_REF,
        ) {
            DocResolution::Resolved(doc_ref) => {
                doc_anchors_by_span.push((spec_ref.span, doc_ref.clone()));
                linked_specs.push(doc_ref);
            }
            DocResolution::Unresolved { reason } => {
                // Any requested anchor that fails to resolve degrades the
                // whole packet (canon doc §4.1) — specs that DID resolve are
                // still reported, but the packet as a whole is not "high
                // confidence". The reason is not discarded: it becomes a
                // real contradiction on the proposal (see
                // `disposition::propose_disposition`), not silently dropped.
                grounding_status = GroundingStatusV1::MissingAnchor;
                contradiction_reasons.push(format!("unresolved canonical doc anchor: {reason}"));
            }
        }
    }

    // F7: a blob-sha-drift reason means the doc still exists and is still
    // reachable, but its content changed — the issue's own Spec-Ref pin
    // refers to a superseded snapshot. This is a real, local,
    // zero-extra-IO derivation of `stale_body_signal`, not a default.
    let stale_body_signal = contradiction_reasons
        .iter()
        .any(|r| r.contains("blob sha drift"));

    let issue_ref_anchors_by_span: Vec<(SourceSpanV1, String)> = relation_lines
        .iter()
        .map(|r| (r.span, r.target_ref.clone()))
        .collect();

    // Real (non-live-lookup) related-issue-state signals: each relation
    // line's own optional `[state]` annotation (see
    // `parse::parse_related_state_suffix`), defaulting to `Unknown` when
    // absent — `classify` treats `Unknown` as a no-op (fail open).
    let related_signals: Vec<disposition::RelatedSignalV1> = relation_lines
        .iter()
        .map(|r| disposition::RelatedSignalV1 {
            target_ref: r.target_ref.clone(),
            kind: r.kind,
            state: r.state,
        })
        .collect();

    let relations: Vec<IssueRelationV1> = relation_lines
        .into_iter()
        .map(|r| IssueRelationV1 {
            kind: r.kind,
            target_ref: r.target_ref,
            evidence_refs: Vec::new(),
        })
        .collect();

    let evidence = compiler::build_issue_evidence(
        snapshot,
        &source_text,
        linked_specs.clone(),
        relations,
        grounding_status,
        &doc_anchors_by_span,
        &issue_ref_anchors_by_span,
    );

    let signals = disposition::RefinerySignalsV1 {
        related: related_signals,
        stale_body_signal,
        // `dispatch_packet_complete` is honestly left uncollected (`None`,
        // its default) — see module docs; `classify` treats `None` as a
        // no-op, never a false `DECISION_REQUIRED`.
        ..Default::default()
    };

    // F2/R4-2: pin the repo's real current revision when the resolver can
    // supply one; when it can't, the reason becomes a real contradiction
    // instead of a silently-empty replay axis (`check_proposal_replay`
    // itself now also refuses to vacuously pass an empty axis).
    let mut repo_revisions: Vec<RepoRevisionV1> = Vec::new();
    match resolver.current_repo_revision(repo, TRUSTED_REF) {
        Ok(revision) => repo_revisions.push(revision),
        Err(reason) => {
            contradiction_reasons.push(format!("repo revision unavailable: {reason}"));
        }
    }
    let doc_revisions: Vec<CanonicalDocRefV1> = linked_specs;
    let proposal = disposition::propose_disposition(
        &evidence,
        &signals,
        repo_revisions,
        doc_revisions,
        &contradiction_reasons,
        captured_at,
    )?;

    Ok((evidence, proposal))
}

#[cfg(test)]
mod url_tests {
    use super::parse_owner_repo_from_git_url;

    #[test]
    fn parses_https_github_url() {
        assert_eq!(
            parse_owner_repo_from_git_url("https://github.com/kckylechen1/tachi.git"),
            Some("kckylechen1/tachi".to_string())
        );
    }

    #[test]
    fn parses_ssh_github_url() {
        assert_eq!(
            parse_owner_repo_from_git_url("git@github.com:kckylechen1/tachi.git"),
            Some("kckylechen1/tachi".to_string())
        );
    }

    #[test]
    fn parses_url_without_dot_git_suffix() {
        assert_eq!(
            parse_owner_repo_from_git_url("https://github.com/kckylechen1/tachi"),
            Some("kckylechen1/tachi".to_string())
        );
    }

    /// R4-3: a non-GitHub / malformed remote must not be silently treated
    /// as a match for anything — this is the pure logic half of "wrong CWD
    /// must not mislabel", proven without touching ambient `current_dir()`.
    #[test]
    fn returns_none_for_a_non_github_remote() {
        assert_eq!(
            parse_owner_repo_from_git_url("https://gitlab.com/someone/other.git"),
            None
        );
        assert_eq!(parse_owner_repo_from_git_url("not a url at all"), None);
    }

    #[test]
    fn mismatched_repo_is_never_treated_as_a_match() {
        let actual = parse_owner_repo_from_git_url("https://github.com/someone-else/other-repo.git")
            .expect("parses");
        assert!(!actual.eq_ignore_ascii_case("kckylechen1/tachi"));
    }
}
