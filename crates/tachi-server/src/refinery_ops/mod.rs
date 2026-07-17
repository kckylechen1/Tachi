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
//! [`build_refinery_packet_with_live_signals`] is the pure core (validate →
//! parse → resolve anchors via an injected [`doc_resolver::DocRefResolver`]
//! → compile evidence → propose disposition) used by the live
//! `handle_refine_issues` action (via `doc_resolver::GitRefResolver`, real
//! git calls, plus the #1105 [`live_signals`] async collection described
//! below). [`build_refinery_packet`] is a thin v1 back-compat wrapper kept
//! for every existing fixture test — equivalent to calling the
//! `_with_live_signals` core with `LiveRelationSignals::default()`.
//!
//! R5-2 correction (sol arbitration, codex-r8f4e, archived on issue #1002):
//! an earlier version of this doc comment claimed "every fixture test ...
//! via a fixture resolver" exercises `build_refinery_packet` — that was
//! inaccurate. Two DIFFERENT fixture tiers exist in `refinery_ops::tests`,
//! and they exercise different amounts of this module:
//! - `tests::grounding` and `tests::replay_and_safety`'s F1-F3/R4-*-named
//!   tests DO go through the full `build_refinery_packet` pipeline (via
//!   `doc_resolver::FixtureDocResolver`, zero I/O) — parse → resolve →
//!   compile → propose, the same code path the live action runs.
//! - `tests::disposition_rules`'s per-failure-class and 5-historical-case
//!   fixtures call `disposition::propose_disposition` DIRECTLY (classifier-
//!   level replay only) — they construct `RefinerySignalsV1`/evidence by
//!   hand and never touch `parse`/`compiler`/`doc_resolver` at all. This is
//!   still a legitimate, real regression replay of the classifier's rules
//!   (and is what #1002 acceptance criterion 4 actually asks for), but it is
//!   NOT the same code path as the live action's anchor-resolution/coverage
//!   machinery — see that module's own doc comment for which of its tests
//!   (a handful, tagged "through the real pipeline") DO call
//!   `build_refinery_packet` instead.
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
//! #1105 (closes the scope gap the paragraph above used to describe):
//! related-issue state (canon doc §4.1 input-order step 4) IS now a live
//! per-relation GitHub cross-reference — see [`live_signals`]'s own module
//! doc for the full derivation (per-relation `issue_read` state + the #1000
//! zombie-scan's merged-PR-reference text search + commit-reachability via
//! `DocRefResolver::is_commit_reachable`, canon doc §5's delivery-state
//! ownership table). `collect_live_relation_signals` (in this module) is the
//! async orchestrator; [`build_refinery_packet_with_live_signals`] stays a
//! PURE consumer of its output, so a `gh`/`git` failure anywhere in
//! collection degrades that one signal back to v1's conservative default
//! (`Unknown` state / empty `scope_collisions` / `None` shipped evidence) —
//! it never blocks or corrupts the proposal-only disposition, and never
//! propagates as an `Err` out of the pure pipeline itself. An owner-authored
//! prose `[state]` annotation is STILL never authority — it is now checked
//! against the live-verified state (a disagreement is a contradiction; an
//! unverifiable target stays advisory) instead of being unconditionally
//! discarded.

mod compiler;
mod disposition;
mod doc_resolver;
mod live_signals;
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
use live_signals::{LiveRelationSignals, RelationLookupTarget};

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
    let repo_root = resolve_known_repo_root(&target.repo);
    let resolver: Box<dyn DocRefResolver> = match repo_root.clone() {
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

    // #1105: collect live per-relation evidence and this issue's own
    // shipped-evidence BEFORE running the pure pipeline — every step here is
    // best-effort; a failure degrades that one signal back to v1's
    // conservative default (never blocks, never corrupts the proposal-only
    // disposition) and is surfaced in `live_signal_errors` for visibility,
    // never silently swallowed. Reuses the SAME `resolver`/`repo_root`
    // already established above for Spec-Ref anchors — one verified repo
    // root, one fail-closed posture, for both concerns.
    let (live_signals, live_signal_errors) = collect_live_relation_signals(
        server,
        &target.repo,
        target.number,
        &result,
        repo_root.as_deref(),
        resolver.as_ref(),
        &captured_at,
    )
    .await;

    let (evidence, proposal) = build_refinery_packet_with_live_signals(
        &target.repo,
        target.number,
        &result,
        resolver.as_ref(),
        &captured_at,
        &live_signals,
    )?;

    serde_json::to_string(&serde_json::json!({
        "tool": "tachi_task_refine_issues",
        "issue_ref": evidence.issue_ref,
        "evidence": evidence,
        "proposal": proposal,
        "live_signal_errors": live_signal_errors,
    }))
    .map_err(|e| format!("serialize refine_issues result: {e}"))
}

/// #1105: async orchestration for the live signal derivation described in
/// `live_signals`'s own module doc. Fetches every `gh` read this leaf needs
/// (per-relation `issue_read`, the shared merged-PR fetch for the
/// commit-reachability shipped check), then hands the results to the pure
/// `live_signals::derive_live_relation_signals`. Returns the derived
/// signals PLUS a list of non-fatal collection errors (empty on full
/// success) — every error here degrades its own signal to the v1-default,
/// never the whole packet.
async fn collect_live_relation_signals(
    server: &MemoryServer,
    repo: &str,
    number: u64,
    gh_issue_result: &serde_json::Value,
    repo_root: Option<&std::path::Path>,
    reachability_resolver: &dyn DocRefResolver,
    captured_at: &str,
) -> (LiveRelationSignals, Vec<String>) {
    let mut errors = Vec::new();
    let relation_lines = relation_targets_for_issue(repo, number, gh_issue_result);

    // Resolve + dedupe relation targets into concrete (repo, number) pairs —
    // a target_ref that fails to parse is simply omitted (stays Unknown,
    // same as v1; not a collection error, since a malformed target_ref is
    // the issue body's own problem, already surfaced elsewhere as prose).
    let mut targets: Vec<RelationLookupTarget> = Vec::new();
    let mut seen: std::collections::HashSet<(String, u64)> = std::collections::HashSet::new();
    for relation in &relation_lines {
        let Some(resolved) = parse_issue_ref(&relation.target_ref, Some(repo)) else {
            continue;
        };
        targets.push(RelationLookupTarget {
            target_ref: relation.target_ref.clone(),
            repo: resolved.repo.clone(),
            number: resolved.number,
        });
        seen.insert((resolved.repo, resolved.number));
    }

    // One `issue_read` per unique (repo, number) target — sequential is
    // fine (manual/on-demand workflow, canon doc §4.3, never resident).
    let mut related_live_states: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for (lookup_repo, lookup_number) in seen {
        match fetch_live_issue_state(server, &lookup_repo, lookup_number).await {
            Ok(Some(state)) => {
                for target in targets
                    .iter()
                    .filter(|t| t.repo == lookup_repo && t.number == lookup_number)
                {
                    related_live_states.insert(target.target_ref.clone(), state.clone());
                }
            }
            Ok(None) => errors.push(format!(
                "{lookup_repo}#{lookup_number}: issue_read returned a malformed/typeless result"
            )),
            Err(reason) => errors.push(format!("{lookup_repo}#{lookup_number}: {reason}")),
        }
    }

    // One merged-PR fetch for `repo` — the shared shipped-evidence candidate
    // pool for both this issue's OWN shipped_evidence and every same-repo
    // CLOSED relation's shipped-ness. Bounded to the most-recently-merged
    // PRs (canon doc §4.3 posture: manual/on-demand, not an unbounded scan);
    // an issue whose shipping PR is older than this window degrades to "no
    // evidence found" — identical to v1, never a false claim.
    let merged_prs = match crate::gh_ops::fetch_merged_prs(
        server,
        repo,
        LIVE_SIGNAL_MERGED_PR_SCAN_LIMIT,
        repo_root,
    ) {
        Ok(prs) => prs,
        Err(reason) => {
            errors.push(format!("fetch_merged_prs({repo}): {reason}"));
            Vec::new()
        }
    };

    let signals = live_signals::derive_live_relation_signals(
        repo,
        number,
        TRUSTED_REF,
        &targets,
        &related_live_states,
        &merged_prs,
        reachability_resolver,
        captured_at,
    );
    (signals, errors)
}

/// Fetch a target issue/PR's live `state` field via the existing
/// `tachi_gh(issue_read)` read surface. `Ok(None)` when the fetch succeeded
/// but the result wasn't a well-formed object with a string `state` (same
/// fail-closed posture as `validate_gh_issue_result`, but scoped to only
/// the one field this leaf needs from a RELATED issue — it does not require
/// the full primary-issue field set).
async fn fetch_live_issue_state(
    server: &MemoryServer,
    repo: &str,
    number: u64,
) -> Result<Option<String>, String> {
    let raw = handle_tachi_gh(
        server,
        TachiGhParams {
            action: "issue_read".to_string(),
            repo: Some(repo.to_string()),
            number: Some(number),
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
        .unwrap_or(serde_json::Value::Null);
    Ok(result
        .get("state")
        .and_then(|v| v.as_str())
        .map(str::to_string))
}

/// #1105: bounds the merged-PR fetch used for the shipped-evidence
/// commit-reachability check — see `collect_live_relation_signals`'s doc
/// comment for what "older than this window" degrades to.
const LIVE_SIGNAL_MERGED_PR_SCAN_LIMIT: u32 = 100;

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

/// Pure: extract `"owner/repo"` from an explicitly supported github.com
/// URL/SCP form. Host-looking substrings embedded in another URL are never
/// authority: the caller treats every unsupported form as "cannot verify".
fn parse_owner_repo_from_git_url(url: &str) -> Option<String> {
    const URL_PREFIXES: &[&str] = &[
        "https://github.com/",
        "http://github.com/",
        "ssh://git@github.com/",
        "git://github.com/",
    ];
    const SCP_PREFIX: &str = "git@github.com:";

    let trimmed = url.trim();
    let path = URL_PREFIXES
        .iter()
        .find_map(|prefix| trimmed.strip_prefix(prefix))
        .or_else(|| trimmed.strip_prefix(SCP_PREFIX))?
        .trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.split_once('/')?;
    if owner.is_empty()
        || repo.is_empty()
        || repo.contains('/')
        || !owner.chars().all(is_github_repo_component_char)
        || !repo.chars().all(is_github_repo_component_char)
    {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

fn is_github_repo_component_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')
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
        Some(v) => {
            return Err(format!(
                "issue_read result 'title' is not a string (got {v})"
            ))
        }
        None => return Err("issue_read result is missing 'title'".to_string()),
    }
    match obj.get("state") {
        Some(v) if v.is_string() => {}
        Some(v) => {
            return Err(format!(
                "issue_read result 'state' is not a string (got {v})"
            ))
        }
        None => return Err("issue_read result is missing 'state'".to_string()),
    }
    match obj.get("body") {
        Some(v) if v.is_string() || v.is_null() => {}
        Some(v) => {
            return Err(format!(
                "issue_read result 'body' is neither a string nor null (got {v})"
            ));
        }
        None => return Err("issue_read result is missing 'body'".to_string()),
    }
    match obj.get("labels") {
        Some(serde_json::Value::Array(labels)) => {
            for (index, label) in labels.iter().enumerate() {
                if label.get("name").and_then(|v| v.as_str()).is_none() {
                    return Err(format!(
                        "issue_read result 'labels[{index}].name' is missing or not a string (got {label})"
                    ));
                }
            }
        }
        Some(v) => {
            return Err(format!(
                "issue_read result 'labels' is not an array (got {v})"
            ));
        }
        None => return Err("issue_read result is missing 'labels'".to_string()),
    }
    match obj.get("milestone") {
        Some(v) if v.is_null() => {}
        Some(v) if v.get("title").and_then(|title| title.as_str()).is_some() => {}
        Some(v) => {
            return Err(format!(
                "issue_read result 'milestone' is neither null nor an object with string 'title' (got {v})"
            ));
        }
        None => return Err("issue_read result is missing 'milestone'".to_string()),
    }
    match obj.get("updatedAt") {
        Some(v) if v.is_string() => {}
        Some(v) => {
            return Err(format!(
                "issue_read result 'updatedAt' is not a string (got {v})"
            ));
        }
        None => return Err("issue_read result is missing 'updatedAt'".to_string()),
    }
    match obj.get("comments") {
        Some(serde_json::Value::Array(comments)) => {
            for (index, comment) in comments.iter().enumerate() {
                validate_gh_comment(comment, index)?;
            }
        }
        Some(v) => {
            return Err(format!(
                "issue_read result 'comments' is not an array (got {v})"
            ));
        }
        None => return Err("issue_read result is missing 'comments'".to_string()),
    }
    Ok(())
}

fn validate_gh_comment(comment: &serde_json::Value, index: usize) -> Result<(), String> {
    let Some(obj) = comment.as_object() else {
        return Err(format!(
            "issue_read result 'comments[{index}]' is not an object (got {comment})"
        ));
    };
    match obj.get("id") {
        Some(v) if v.is_string() || v.as_u64().is_some() => {}
        Some(v) => {
            return Err(format!(
                "issue_read result 'comments[{index}].id' is neither a string nor an unsigned number (got {v})"
            ));
        }
        None => {
            return Err(format!(
                "issue_read result is missing 'comments[{index}].id'"
            ))
        }
    }
    match obj.get("body") {
        Some(v) if v.is_string() => {}
        Some(v) => {
            return Err(format!(
                "issue_read result 'comments[{index}].body' is not a string (got {v})"
            ));
        }
        None => {
            return Err(format!(
                "issue_read result is missing 'comments[{index}].body'"
            ))
        }
    }
    match obj.get("author") {
        Some(v) if v.is_null() => {}
        Some(v) if v.get("login").and_then(|login| login.as_str()).is_some() => {}
        Some(v) => {
            return Err(format!(
                "issue_read result 'comments[{index}].author' is neither null nor an object with string 'login' (got {v})"
            ));
        }
        None => {
            return Err(format!(
                "issue_read result is missing 'comments[{index}].author'"
            ));
        }
    }
    match obj.get("createdAt") {
        Some(v) if v.is_string() => {}
        Some(v) => {
            return Err(format!(
                "issue_read result 'comments[{index}].createdAt' is not a string (got {v})"
            ));
        }
        None => {
            return Err(format!(
                "issue_read result is missing 'comments[{index}].createdAt'"
            ));
        }
    }
    if let Some(v) = obj.get("updatedAt") {
        if !v.is_string() {
            return Err(format!(
                "issue_read result 'comments[{index}].updatedAt' is not a string (got {v})"
            ));
        }
    }
    Ok(())
}

fn shift_spec_ref_span(spec_ref: &mut parse::SpecRefLine, offset: usize) {
    spec_ref.span.start_byte += offset;
    spec_ref.span.end_byte += offset;
}

fn shift_malformed_spec_ref_span(malformed: &mut parse::MalformedSpecRefLine, offset: usize) {
    malformed.span.start_byte += offset;
    malformed.span.end_byte += offset;
}

fn parse_updated_pin_amendment(
    body: &str,
    source_offset: usize,
) -> Option<Result<parse::SpecRefLine, parse::MalformedSpecRefLine>> {
    const PREFIX: &str = "Updated pin: Spec-Ref: ";
    let prefix_start = body.rfind(PREFIX)?;
    let value_start = prefix_start + PREFIX.len();
    let value = body[value_start..].split_whitespace().next().unwrap_or("");
    let span = SourceSpanV1 {
        start_byte: source_offset + value_start,
        end_byte: source_offset + value_start + value.len(),
    };
    Some(match parse::parse_spec_ref_value(value) {
        Some(mut parsed) => {
            parsed.span = span;
            Ok(parsed)
        }
        None => Err(parse::MalformedSpecRefLine {
            raw: value.to_string(),
            span,
        }),
    })
}

/// Build the combined source text (issue body + every selected comment,
/// double-newline joined) that Spec-Ref/relation parsing and coverage
/// claim-splitting operate over (F3). Factored out so [`relation_targets_for_issue`]
/// (#1105: the live async orchestrator needs to know which relation targets
/// to look up BEFORE calling the pure pipeline) parses from the EXACT same
/// text `build_refinery_packet_with_live_signals` will parse, without
/// duplicating or drifting from the join logic. Returns the text plus each
/// comment's start byte offset within it (needed for span-shifting
/// Spec-Ref/malformed-Spec-Ref matches found inside a comment).
fn build_source_text_with_comment_offsets(
    snapshot: &tachi_params::IssueSnapshotV1,
) -> (String, Vec<usize>) {
    let mut source_text = snapshot.body.clone();
    let mut comment_offsets = Vec::with_capacity(snapshot.selected_comment_revisions.len());
    for c in &snapshot.selected_comment_revisions {
        source_text.push_str("\n\n");
        comment_offsets.push(source_text.len());
        source_text.push_str(&c.body);
    }
    (source_text, comment_offsets)
}

/// #1105: the relation lines (`Blocks:`/`Depends-On:`/`Duplicate-Of:`/
/// `Supersedes:`/`Parent-Of:`/`Related:`) this issue's body+comments
/// declare, parsed the SAME way `build_refinery_packet_with_live_signals`
/// will — exposed so the live async orchestrator
/// (`handle_refine_issues`/`collect_live_relation_signals`) can resolve
/// live evidence for exactly these targets BEFORE calling the pure
/// pipeline, without a second (and possibly drifting) parse of the issue
/// body.
pub(crate) fn relation_targets_for_issue(
    repo: &str,
    number: u64,
    gh_issue_result: &serde_json::Value,
) -> Vec<parse::RelationLine> {
    let snapshot = parse::parse_issue_snapshot_from_gh_json(repo, number, gh_issue_result);
    let (source_text, _comment_offsets) = build_source_text_with_comment_offsets(&snapshot);
    parse::parse_relation_lines(&source_text)
}

/// Back-compat wrapper: v1's pure pipeline entry point, unchanged for every
/// existing caller/fixture test — equivalent to calling
/// [`build_refinery_packet_with_live_signals`] with
/// `LiveRelationSignals::default()` (#1105's `Default` is BY CONSTRUCTION
/// v1's own conservative behavior: every relation `Unknown`, empty
/// `scope_collisions`, `None` shipped evidence — see that type's doc
/// comment).
#[cfg(test)]
pub(crate) fn build_refinery_packet(
    repo: &str,
    number: u64,
    gh_issue_result: &serde_json::Value,
    resolver: &dyn DocRefResolver,
    captured_at: &str,
) -> Result<(IssueEvidenceV1, IssueDispositionProposalV1), String> {
    build_refinery_packet_with_live_signals(
        repo,
        number,
        gh_issue_result,
        resolver,
        captured_at,
        &LiveRelationSignals::default(),
    )
}

/// Pure pipeline: `gh issue view --json ...` result → typed evidence +
/// disposition proposal. Takes an injected [`DocRefResolver`] so tests never
/// shell real git/GitHub (#1002 acceptance criterion 7). `live_signals`
/// (#1105) carries the already-fetched, already-verified per-relation live
/// state and this issue's own shipped-evidence — `LiveRelationSignals::default()`
/// (v1's conservative behavior) when the caller has none.
pub(crate) fn build_refinery_packet_with_live_signals(
    repo: &str,
    number: u64,
    gh_issue_result: &serde_json::Value,
    resolver: &dyn DocRefResolver,
    captured_at: &str,
    live_signals: &LiveRelationSignals,
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
    let (source_text, comment_offsets) = build_source_text_with_comment_offsets(&snapshot);

    // The issue body is the baseline authority. A later owner-authored
    // `Updated pin: Spec-Ref: ...` amendment supersedes it deterministically;
    // untrusted comments cannot introduce or replace canonical authority.
    let owner = repo.split_once('/').map(|(owner, _)| owner).unwrap_or("");
    let mut amendments = snapshot
        .selected_comment_revisions
        .iter()
        .zip(&comment_offsets)
        .filter(|(comment, _)| {
            comment
                .author
                .as_deref()
                .is_some_and(|author| author.eq_ignore_ascii_case(owner))
        })
        .filter_map(|(comment, offset)| {
            parse_updated_pin_amendment(&comment.body, *offset).map(|parsed| {
                (
                    comment.updated_at.clone(),
                    comment.comment_id.clone(),
                    parsed,
                )
            })
        })
        .collect::<Vec<_>>();
    amendments.sort_by(|left, right| (&left.0, &left.1).cmp(&(&right.0, &right.1)));

    let (spec_refs, malformed_spec_refs) = if let Some((_, _, amendment)) = amendments.pop() {
        match amendment {
            Ok(spec_ref) => (vec![spec_ref], Vec::new()),
            Err(malformed) => (Vec::new(), vec![malformed]),
        }
    } else {
        let (mut refs, mut malformed) = parse::parse_spec_ref_lines(&snapshot.body);
        for (comment, offset) in snapshot
            .selected_comment_revisions
            .iter()
            .zip(&comment_offsets)
            .filter(|(comment, _)| {
                comment
                    .author
                    .as_deref()
                    .is_some_and(|author| author.eq_ignore_ascii_case(owner))
            })
        {
            let (mut comment_refs, mut comment_malformed) =
                parse::parse_spec_ref_lines(&comment.body);
            for spec_ref in &mut comment_refs {
                shift_spec_ref_span(spec_ref, *offset);
            }
            for malformed in &mut comment_malformed {
                shift_malformed_spec_ref_span(malformed, *offset);
            }
            refs.extend(comment_refs);
            malformed.extend(comment_malformed);
        }
        (refs, malformed)
    };
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

    // #1105: `live_signals.related_states` (populated by the async
    // orchestrator's live cross-reference lookups) now REPLACES the v1
    // always-`Unknown` default — a target_ref absent from that map (lookup
    // never attempted or failed) still falls back to `Unknown`, identical
    // to v1's own behavior (fail-closed by construction, not by a special
    // case here). A prose `[state]` annotation is still never authority: an
    // owner-authored annotation that DISAGREES with the live-verified state
    // is surfaced as a contradiction — agreement is silent (not a
    // contradiction), and an annotation on a target whose live state
    // couldn't be determined stays advisory-only, same as pre-#1105.
    let mut related_signals: Vec<disposition::RelatedSignalV1> =
        Vec::with_capacity(relation_lines.len());
    for relation in &relation_lines {
        let live_state = live_signals
            .related_states
            .get(&relation.target_ref)
            .copied()
            .unwrap_or(disposition::RelatedIssueStateV1::Unknown);
        if relation.state != disposition::RelatedIssueStateV1::Unknown
            && relation.state != live_state
        {
            contradiction_reasons.push(format!(
                "prose [state] annotation for {} ({:?}) disagrees with the live-verified state ({:?})",
                relation.target_ref, relation.state, live_state
            ));
        }
        related_signals.push(disposition::RelatedSignalV1 {
            target_ref: relation.target_ref.clone(),
            kind: relation.kind,
            state: live_state,
        });
    }

    // #1105 scope_collision (this leaf's own convention, not canon-frozen —
    // see `live_signals` module doc): a `Duplicate-Of:` relation whose
    // live-verified target state is `Open` — both issues still live,
    // genuinely competing for the same scope. Structured (keyed off the
    // already-frozen relation kind + a real cross-reference), never a
    // free-text/title-similarity classifier (canon doc §10).
    let scope_collisions: Vec<String> = related_signals
        .iter()
        .filter(|r| {
            r.kind == tachi_params::IssueRelationKindV1::DuplicateOf
                && r.state == disposition::RelatedIssueStateV1::Open
        })
        .map(|r| r.target_ref.clone())
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
        // #1105: live-derived (empty/`None` when the caller supplied
        // `LiveRelationSignals::default()`, i.e. v1 behavior).
        scope_collisions,
        shipped_evidence: live_signals.own_shipped_evidence.clone(),
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
        assert_eq!(
            parse_owner_repo_from_git_url("ssh://git@github.com/kckylechen1/tachi.git"),
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
    fn rejects_embedded_or_lookalike_github_hosts() {
        for hostile in [
            "https://example.com/github.com/kckylechen1/tachi.git",
            "https://github.com.evil/kckylechen1/tachi.git",
            "git@github.com.evil:kckylechen1/tachi.git",
            "https://evil.invalid/?next=https://github.com/kckylechen1/tachi.git",
        ] {
            assert_eq!(
                parse_owner_repo_from_git_url(hostile),
                None,
                "host lookalike must not establish repo authority: {hostile}"
            );
        }
    }

    #[test]
    fn mismatched_repo_is_never_treated_as_a_match() {
        let actual =
            parse_owner_repo_from_git_url("https://github.com/someone-else/other-repo.git")
                .expect("parses");
        assert!(!actual.eq_ignore_ascii_case("kckylechen1/tachi"));
    }
}
