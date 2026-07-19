//! #1285 P0 — campaign (session-boundary) handoff publication.
//!
//! **Domain split (does not rename anything else, #1037's reserved slot):**
//! `orchestrator_ops::HandoffPacket` (`tachi_orchestrator(action='handoff_write'|
//! 'handoff_read')`) is the `task_id`-keyed resumable baton; `tachi_gh(action=
//! 'pr_handoff')` (`task_lifecycle::handle_task_pr_handoff`) is the `flow_id`-keyed
//! PR-handoff artifact; `sticky_ops` is the short read-once agent memo. This
//! module is the third, previously-unimplemented domain: a **campaign/session**
//! boundary publication, addressed by neither a `task_id` nor a `flow_id` —
//! a GitHub issue (durable, commentable, closeable) mirrored into `/wiki` for
//! typed-evidence retrieval.
//!
//! Three actions, all wired through `tachi_gh`:
//!   - `handoff_draft`   — read-only, assembles the four-section skeleton.
//!   - `handoff_publish` — issue create → wiki mirror → supersede (target
//!     verify + comment + restricted close + label swap) → continuity
//!     event, in that order. #1285 codex review: the restricted close gate
//!     re-verifies the `handoff` label live immediately before closing —
//!     close MUST run before the label swap, or the gate is checking a
//!     label the swap already removed and refuses every normal-flow close.
//!   - `handoff_repair`  — idempotent mirror-only rebuild for an existing
//!     handoff issue (does not create a second issue or a second mirror).
//!
//! §3 ("next steps") is **always leader-authored**: `handoff_draft` returns
//! raw material only (open issues + the previous handoff's own §3 text, on a
//! best-effort basis), never a synthesized recommendation, and
//! `handoff_publish` only ever writes the leader's own verbatim `body`.

use super::*;
use crate::tool_params::{TachiEventParams, WikiBrowseParams, WikiWriteParams};
use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};

/// GitHub label marking an open campaign-handoff issue. The ONLY place in
/// this crate that closes an issue programmatically (`close_superseded_handoff_issue`
/// below) refuses to act on anything not carrying this label — router.rs's
/// frozen "closing an issue is a leader/owner action on GitHub itself"
/// posture (see `handle_issue_freshness_scan`'s module doc) continues to
/// hold for every issue outside this exact, non-widenable call site.
const HANDOFF_LABEL: &str = "handoff";
const HANDOFF_SUPERSEDED_LABEL: &str = "handoff-superseded";
const HANDOFF_SCHEMA_VERSION: u64 = 1;
/// Lookback window when no previous handoff exists to anchor `since` on
/// (first-ever handoff for a repo).
const DEFAULT_HANDOFF_WINDOW_DAYS: i64 = 7;
/// R2c concurrency advisory: a mirror published within this window for the
/// same repo surfaces a (non-blocking) warning.
const CONCURRENT_PUBLISH_WARNING_MINUTES: i64 = 10;

// ─────────────────────────── handoff_draft (read-only) ───────────────────────────

pub(crate) fn handle_gh_handoff_draft(
    server: &MemoryServer,
    params: &TachiGhParams,
    repo: String,
) -> Result<String, String> {
    validate_repo(&repo)?;
    let previous = discover_previous_handoff_for_draft(&repo, server);
    let since = resolve_since(params.since.as_deref(), previous.published_at)?;

    let mut draft = serde_json::Map::new();
    draft.insert("ok".to_string(), json!(true));
    draft.insert("action".to_string(), json!("handoff_draft"));
    draft.insert("repo".to_string(), json!(repo));
    draft.insert("since".to_string(), json!(since.to_rfc3339()));
    draft.insert("previous_handoff".to_string(), previous.to_json());
    draft.insert(
        "1_delivery_ledger".to_string(),
        assemble_ledger_section(server, &repo, &since),
    );
    draft.insert(
        "2_current_state".to_string(),
        assemble_current_state_section(server),
    );
    draft.insert(
        "3_next_steps".to_string(),
        json!({
            "note": "LEADER AUTHORED — this section is raw material only; tachi_gh(action='handoff_publish') never synthesizes it",
            "next_steps_raw_material": assemble_next_steps_raw_material(server, &repo, previous.mirror_body.as_deref()),
        }),
    );
    draft.insert(
        "4_pitfalls".to_string(),
        assemble_pitfalls_section(server, since),
    );
    serde_json::to_string(&Value::Object(draft))
        .map_err(|e| format!("serialize handoff_draft: {e}"))
}

/// §1: merged-PR ledger over the G3 window + best-effort dispatch outcomes.
/// `merged_prs` failing (gh transport unavailable) degrades the WHOLE §1 to
/// `unavailable` per the frozen contract ("G3 时间窗必须对账...不可用时 §1
/// 整段标 unavailable, 宁缺不错") rather than a half-populated ledger.
fn assemble_ledger_section(server: &MemoryServer, repo: &str, since: &DateTime<Utc>) -> Value {
    let since_str = since.to_rfc3339();
    match fetch_merged_prs_since(server, repo, &since_str, 100) {
        Ok(prs) => json!({
            "status": "ok",
            "since": since_str,
            "merged_prs": {
                "count": prs.len(),
                "prs": prs.iter().map(|pr| json!({
                    "number": pr.number,
                    "title": pr.title,
                    "merged_at": pr.merged_at,
                })).collect::<Vec<_>>(),
            },
            // #1285 P0 explicitly excludes P2 (G2 windowed dispatch-outcomes
            // query). What already exists (memcore::list_outcomes_by_vendor_window,
            // list_outcomes_by_issue_ref) is per-vendor / per-issue, not
            // per-repo/all-vendor-in-window — there is no honest way to
            // assemble a repo-scoped window from those without either a
            // hardcoded vendor enumeration (a fabricated completeness claim)
            // or a new memcore query (P2's job, not this leaf's). Marked
            // unavailable rather than guessed.
            "dispatch_outcomes": {
                "status": "unavailable",
                "reason": "G2 windowed dispatch-outcomes query not implemented (tracked #1285 P2); per-vendor (list_outcomes_by_vendor_window) and per-issue (list_outcomes_by_issue_ref) queries exist but no per-repo/all-vendor window query yet.",
            },
        }),
        Err(err) => json!({
            "status": "unavailable",
            "reason": format!("gh transport: {err}"),
        }),
    }
}

/// §2: local-only, always available even with `gh` offline — health
/// snapshot + doctor build-resource/worktree patrol + best-effort
/// known-reds. No writer for `/wiki/known-reds` exists anywhere in this
/// codebase yet (confirmed by repo-wide grep) — an empty/failed lookup
/// degrades to `unavailable` rather than inventing a shape for it.
fn assemble_current_state_section(server: &MemoryServer) -> Value {
    let app_home = server.tachi_home_dir();
    let global_db = server.global_db_path_buf();
    let project_db = server.project_db_path_buf();
    let snapshot_json = {
        let snapshot =
            crate::status_ops::collect_snapshot(&app_home, &global_db, project_db.as_deref());
        serde_json::to_value(&snapshot).unwrap_or_else(|err| {
            json!({ "status": "unavailable", "reason": format!("snapshot serialize failed: {err}") })
        })
    };
    let orphan_warnings = crate::doctor::scan_orphan_build_resources(
        crate::doctor::DEFAULT_ORPHAN_MAX_AGE_DAYS,
        &global_db,
    );
    let worktree_warnings =
        crate::doctor::worktree_inspection_report(crate::doctor::DEFAULT_WORKTREE_STALE_DAYS);
    json!({
        "health_snapshot": snapshot_json,
        "orphan_build_resources": orphan_warnings.iter().map(doctor_warning_json).collect::<Vec<_>>(),
        "worktree_inspection": worktree_warnings.iter().map(doctor_warning_json).collect::<Vec<_>>(),
        "known_reds": known_reds_section(server),
    })
}

fn doctor_warning_json(warning: &crate::doctor::DoctorWarning) -> Value {
    json!({
        "code": warning.code,
        "path": warning.path,
        "message": warning.message,
        "remediation": warning.remediation,
    })
}

fn known_reds_section(server: &MemoryServer) -> Value {
    let params = WikiBrowseParams {
        category: Some("/wiki/known-reds".to_string()),
        limit: 20,
        project: "wiki".to_string(),
        lifecycle: None,
    };
    match crate::wiki_ops::collect_wiki_browse_value(server, params) {
        Ok(value) => {
            let count = value.get("count").and_then(Value::as_u64).unwrap_or(0);
            if count == 0 {
                json!({ "status": "unavailable", "reason": "no /wiki/known-reds entries found" })
            } else {
                value
            }
        }
        Err(err) => json!({ "status": "unavailable", "reason": err }),
    }
}

/// §3: raw material only — open issues (best-effort, needs `gh`) + the
/// previous handoff's own §3 text extracted verbatim (best-effort, needs a
/// previous mirror). Never synthesizes a recommendation.
fn assemble_next_steps_raw_material(
    server: &MemoryServer,
    repo: &str,
    previous_body: Option<&str>,
) -> Value {
    let open_issues = match fetch_open_issues_brief(server, repo) {
        Ok(issues) => json!({ "status": "ok", "issues": issues }),
        Err(err) => json!({ "status": "unavailable", "reason": format!("gh transport: {err}") }),
    };
    let previous_section = match previous_body {
        Some(body) => match extract_next_steps_section(body) {
            Some(text) => json!({ "status": "ok", "text": text }),
            None => json!({
                "status": "unavailable",
                "reason": "no §3/next-steps heading found in previous handoff mirror",
            }),
        },
        None => json!({ "status": "unavailable", "reason": "no previous handoff found" }),
    };
    json!({
        "open_issues": open_issues,
        "previous_handoff_next_steps": previous_section,
    })
}

/// §4: feedback-category memories + continuity events since the G3 window.
fn assemble_pitfalls_section(server: &MemoryServer, since: DateTime<Utc>) -> Value {
    let feedback = match collect_feedback_since(server, since, 20) {
        Ok(rows) => json!({ "status": "ok", "count": rows.len(), "entries": rows }),
        Err(err) => json!({ "status": "unavailable", "reason": err }),
    };
    let events = match collect_continuity_events_since(server, since, 20) {
        Ok(rows) => json!({ "status": "ok", "count": rows.len(), "entries": rows }),
        Err(err) => json!({ "status": "unavailable", "reason": err }),
    };
    json!({
        "feedback_memories": feedback,
        "continuity_events": events,
    })
}

fn collect_feedback_since(
    server: &MemoryServer,
    since: DateTime<Utc>,
    cap: usize,
) -> Result<Vec<Value>, String> {
    let rows = server.with_global_store_read(|store| {
        store
            .list_memories_by_category_and_path_prefix("feedback", "%")
            .map_err(|e| format!("list feedback memories: {e}"))
    })?;
    let mut recent: Vec<_> = rows
        .into_iter()
        .filter(|row| {
            DateTime::parse_from_rfc3339(&row.timestamp)
                .map(|dt| dt.with_timezone(&Utc) >= since)
                .unwrap_or(false)
        })
        .collect();
    recent.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    recent.truncate(cap);
    let mut out = Vec::with_capacity(recent.len());
    for row in recent {
        let entry = server.with_global_store_read(|store| {
            store
                .get(&row.id)
                .map_err(|e| format!("fetch feedback memory {}: {e}", row.id))
        })?;
        if let Some(entry) = entry {
            out.push(json!({
                "id": entry.id,
                "path": entry.path,
                "summary": entry.summary,
                "timestamp": entry.timestamp,
            }));
        }
    }
    Ok(out)
}

fn collect_continuity_events_since(
    server: &MemoryServer,
    since: DateTime<Utc>,
    cap: usize,
) -> Result<Vec<Value>, String> {
    let route = server.event_db_route(None);
    let query = memcore::TachiEventQuery {
        project: None,
        domain: None,
        event_type: None,
        session_id: None,
        source_repo: None,
        adapter: None,
        limit: 200,
    };
    let events = server.with_event_route_store_read(&route, |store| {
        store
            .list_tachi_events(&query)
            .map_err(|e| format!("list tachi events: {e}"))
    })?;
    let mut recent: Vec<_> = events
        .into_iter()
        .filter(|event| {
            DateTime::parse_from_rfc3339(&event.created_at)
                .map(|dt| dt.with_timezone(&Utc) >= since)
                .unwrap_or(false)
        })
        .collect();
    recent.truncate(cap);
    Ok(recent
        .into_iter()
        .map(|event| {
            json!({
                "id": event.id,
                "event_type": event.event_type,
                "created_at": event.created_at,
                "actor": event.actor,
            })
        })
        .collect())
}

fn fetch_open_issues_brief(server: &MemoryServer, repo: &str) -> Result<Vec<Value>, String> {
    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args(["issue", "list"])
        .args(["--repo", repo])
        .args(["--state", "open"])
        .args(["--json", "number,title,labels"])
        .args(["--limit", "30"]);
    let output = run_gh_json(cmd, &token)?;
    let value: Value =
        serde_json::from_str(&output).map_err(|e| format!("parse issue list json: {e}"))?;
    Ok(value.as_array().cloned().unwrap_or_default())
}

fn fetch_open_handoff_issues(server: &MemoryServer, repo: &str) -> Result<Vec<Value>, String> {
    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args(["issue", "list"])
        .args(["--repo", repo])
        .args(["--label", HANDOFF_LABEL])
        .args(["--state", "open"])
        .args(["--json", "number,title,createdAt,labels"])
        .args(["--limit", "20"]);
    let output = run_gh_json(cmd, &token)?;
    let value: Value =
        serde_json::from_str(&output).map_err(|e| format!("parse issue list json: {e}"))?;
    Ok(value.as_array().cloned().unwrap_or_default())
}

/// Heuristic, best-effort §3 extraction from a previous handoff mirror's
/// free-form leader-authored body: finds a markdown/§ heading line
/// containing "next step"/"§3"/"下一步" (case-insensitive) and returns
/// everything up to the next heading line (or EOF). `None` (never a guess)
/// when no such heading exists.
fn extract_next_steps_section(body: &str) -> Option<String> {
    let lines: Vec<&str> = body.lines().collect();
    let is_heading =
        |line: &str| line.trim_start().starts_with('#') || line.trim_start().starts_with('§');
    let matches_marker = |line: &str| {
        let lower = line.to_ascii_lowercase();
        lower.contains("next step") || lower.contains("§3") || lower.contains("下一步")
    };
    let start = lines
        .iter()
        .position(|line| is_heading(line) && matches_marker(line))?;
    let mut end = lines.len();
    for (idx, line) in lines.iter().enumerate().skip(start + 1) {
        if is_heading(line) {
            end = idx;
            break;
        }
    }
    let section = lines[start..end].join("\n").trim().to_string();
    if section.is_empty() {
        None
    } else {
        Some(section)
    }
}

fn resolve_since(
    explicit: Option<&str>,
    previous_published_at: Option<DateTime<Utc>>,
) -> Result<DateTime<Utc>, String> {
    if let Some(explicit) = explicit {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() {
            return DateTime::parse_from_rfc3339(trimmed)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| format!("invalid 'since' RFC3339 timestamp '{trimmed}': {e}"));
        }
    }
    Ok(previous_published_at
        .unwrap_or_else(|| Utc::now() - ChronoDuration::days(DEFAULT_HANDOFF_WINDOW_DAYS)))
}

/// Previous-handoff discovery result for `handoff_draft` — offline-safe:
/// the wiki side is always attempted (local, never fails hard); the gh side
/// is best-effort and its own failure is captured (`gh_error`) rather than
/// failing the whole draft (offline degrade, R1).
struct PreviousHandoffForDraft {
    issue: Option<u64>,
    published_at: Option<DateTime<Utc>>,
    mirror_body: Option<String>,
    source: &'static str,
    gh_error: Option<String>,
}

impl PreviousHandoffForDraft {
    fn to_json(&self) -> Value {
        json!({
            "issue": self.issue,
            "published_at": self.published_at.map(|dt| dt.to_rfc3339()),
            "source": self.source,
            "gh_lookup_error": self.gh_error,
        })
    }
}

fn discover_previous_handoff_for_draft(
    repo: &str,
    server: &MemoryServer,
) -> PreviousHandoffForDraft {
    let wiki_mirrors =
        crate::wiki_ops::list_handoff_mirrors_for_repo(server, repo).unwrap_or_default();
    let wiki_latest = wiki_mirrors.first();

    let (gh_issue, gh_error) = match fetch_open_handoff_issues(server, repo) {
        Ok(issues) => {
            let newest = issues
                .iter()
                .filter_map(|issue| {
                    let number = issue.get("number")?.as_u64()?;
                    let created_at = issue.get("createdAt").and_then(Value::as_str)?;
                    let ts = DateTime::parse_from_rfc3339(created_at)
                        .ok()?
                        .with_timezone(&Utc);
                    Some((number, ts))
                })
                .max_by_key(|(_, ts)| *ts);
            (newest, None)
        }
        Err(err) => (None, Some(err)),
    };

    // "两处都空 = 首张,链空转"; "两造并察, 按时间取新" otherwise.
    let use_gh = match (&gh_issue, wiki_latest) {
        (Some((_, gh_ts)), Some(mirror)) => {
            mirror.published_at.map(|wp| *gh_ts > wp).unwrap_or(true)
        }
        (Some(_), None) => true,
        _ => false,
    };

    if use_gh {
        if let Some((number, ts)) = gh_issue {
            return PreviousHandoffForDraft {
                issue: Some(number),
                published_at: Some(ts),
                mirror_body: wiki_mirrors
                    .iter()
                    .find(|mirror| mirror.issue == number)
                    .map(|mirror| mirror.text.clone()),
                source: "gh",
                gh_error,
            };
        }
    }
    if let Some(mirror) = wiki_latest {
        return PreviousHandoffForDraft {
            issue: Some(mirror.issue),
            published_at: mirror.published_at,
            mirror_body: Some(mirror.text.clone()),
            source: "wiki",
            gh_error,
        };
    }
    PreviousHandoffForDraft {
        issue: None,
        published_at: None,
        mirror_body: None,
        source: "none",
        gh_error,
    }
}

// ─────────────────────────── handoff_publish ───────────────────────────

pub(crate) async fn handle_gh_handoff_publish(
    server: &MemoryServer,
    params: &TachiGhParams,
    repo: String,
) -> Result<String, String> {
    validate_repo(&repo)?;
    let title = params
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| "handoff_publish requires a non-empty 'title'".to_string())?;
    let body = params
        .body
        .clone()
        .filter(|b| !b.trim().is_empty())
        .ok_or_else(|| {
            "handoff_publish requires a non-empty 'body' (leader-authored, verbatim)".to_string()
        })?;
    let refs = params.refs.clone();
    crate::wiki_ops::validate_references(&refs)?;

    let mut receipt = serde_json::Map::new();
    receipt.insert("action".to_string(), json!("handoff_publish"));
    receipt.insert("repo".to_string(), json!(repo));
    if let Some(warning) = concurrent_publish_warning(server, &repo) {
        receipt.insert("concurrent_publish_warning".to_string(), warning);
    }

    // Discover the previous open handoff issue (if any) BEFORE creating the
    // new one — this determines the supersede target, scoped to `repo` only
    // (multi-repo isolation: `fetch_open_handoff_issues` passes `--repo`).
    // An explicit `params.supersedes` short-circuits auto-discovery entirely
    // (leader-supplied override, per the frozen contract).
    let previous_issue = match params.supersedes {
        Some(explicit) => Ok(Some(explicit)),
        None => fetch_open_handoff_issues(server, &repo).map(|issues| {
            issues
                .iter()
                .filter_map(|issue| {
                    let number = issue.get("number")?.as_u64()?;
                    let created_at = issue.get("createdAt").and_then(Value::as_str)?;
                    Some((number, created_at.to_string()))
                })
                .max_by(|a, b| a.1.cmp(&b.1))
                .map(|(number, _)| number)
        }),
    };
    let previous_issue = match previous_issue {
        Ok(previous_issue) => previous_issue,
        Err(err) => {
            receipt.insert("ok".to_string(), json!(false));
            receipt.insert(
                "error".to_string(),
                json!(format!(
                    "could not discover previous handoff (gh transport): {err}"
                )),
            );
            return serde_json::to_string(&Value::Object(receipt))
                .map_err(|e| format!("serialize handoff_publish receipt: {e}"));
        }
    };

    // Step 1: issue create.
    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args(["issue", "create"])
        .args(["--repo", &repo])
        .args(["--title", &title])
        .args(["--label", HANDOFF_LABEL]);
    let _body_file = attach_gh_body_file(&mut cmd, &body)?;
    let create_output = run_gh(cmd, &token)?;
    let issue_number = parse_issue_number_from_gh_url(&create_output).ok_or_else(|| {
        format!("could not parse issue number from `gh issue create` output: {create_output}")
    })?;
    let published_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);

    receipt.insert("ok".to_string(), json!(true));
    receipt.insert("issue".to_string(), json!(issue_number));
    receipt.insert("issue_url".to_string(), json!(create_output.trim()));
    receipt.insert("published_at".to_string(), json!(published_at));
    let mut steps = serde_json::Map::new();
    steps.insert("issue_create".to_string(), json!("ok"));
    receipt.insert("steps".to_string(), Value::Object(steps));

    // Step 2: wiki mirror.
    let path = mirror_path_for(&repo, &published_at, &title);
    match write_handoff_mirror(
        server,
        &repo,
        issue_number,
        &title,
        &body,
        &refs,
        &published_at,
        previous_issue,
        &path,
    )
    .await
    {
        Ok(mirror_receipt) => {
            set_step(&mut receipt, "mirror", json!("ok"));
            receipt.insert("mirror".to_string(), mirror_receipt);
        }
        Err(err) => {
            receipt.insert("ok".to_string(), json!(false));
            set_step(
                &mut receipt,
                "mirror",
                json!({ "status": "failed", "error": err }),
            );
            receipt.insert(
                "next_step".to_string(),
                json!(format!(
                    "tachi_gh(action='handoff_repair', repo='{repo}', number={issue_number}) to retry the wiki mirror"
                )),
            );
            return serde_json::to_string(&Value::Object(receipt))
                .map_err(|e| format!("serialize partial handoff_publish receipt: {e}"));
        }
    }

    // Step 3: supersede (only if a previous open handoff exists for this repo).
    if let Some(prev_issue) = previous_issue {
        match supersede_previous_handoff(server, &repo, prev_issue, issue_number) {
            Ok(supersede_receipt) => {
                set_step(&mut receipt, "supersede", json!("ok"));
                receipt.insert("supersedes".to_string(), supersede_receipt);
            }
            Err(err) => {
                receipt.insert("ok".to_string(), json!(false));
                set_step(
                    &mut receipt,
                    "supersede",
                    json!({ "status": "failed", "error": err }),
                );
                // #1285 codex review: `verify_supersede_target`'s rejection
                // (un-widenable — the previous_issue is not open/handoff-labeled)
                // must NOT be answered with a "fix by hand" suggestion that
                // tells the caller how to force the comment/close/label-edit
                // through anyway — that would hand back, in the receipt
                // itself, a manual recipe for bypassing the gate that just
                // fired. Only the genuine partial-mutation failure modes
                // (comment posted but the API call itself errored, etc.) get
                // the manual-completion fallback.
                let next_step = if err.starts_with("refusing to supersede") {
                    format!(
                        "supersede target verification refused {repo}#{prev_issue} — {err}. \
                         No mutation was attempted (comment/close/label-swap were never called). \
                         This is not a transient failure: re-check the `supersedes` argument (or \
                         auto-discovery result) against an actually-open, handoff-labeled issue."
                    )
                } else {
                    format!(
                        "supersede comment/label on {repo}#{prev_issue} did not complete — {err}. \
                         Fix by hand: `gh issue comment {prev_issue} --repo {repo} --body \"superseded by {repo}#{issue_number}\"` \
                         then `gh issue edit {prev_issue} --repo {repo} --remove-label {HANDOFF_LABEL} --add-label {HANDOFF_SUPERSEDED_LABEL}`."
                    )
                };
                receipt.insert("next_step".to_string(), json!(next_step));
                return serde_json::to_string(&Value::Object(receipt))
                    .map_err(|e| format!("serialize partial handoff_publish receipt: {e}"));
            }
        }
    } else {
        set_step(&mut receipt, "supersede", json!("skipped_no_previous"));
    }

    // Step 4: continuity event — best-effort, never fails the publish.
    let event_result =
        emit_handoff_published_event(server, &repo, issue_number, &published_at, previous_issue)
            .await;
    match event_result {
        Ok(_) => set_step(&mut receipt, "event", json!("ok")),
        Err(err) => set_step(
            &mut receipt,
            "event",
            json!({ "status": "failed_non_blocking", "error": err }),
        ),
    }

    serde_json::to_string(&Value::Object(receipt))
        .map_err(|e| format!("serialize handoff_publish receipt: {e}"))
}

fn set_step(receipt: &mut serde_json::Map<String, Value>, step: &str, value: Value) {
    if let Some(steps) = receipt.get_mut("steps").and_then(Value::as_object_mut) {
        steps.insert(step.to_string(), value);
    }
}

/// R2c: best-effort advisory only — never blocks publish. A mirror
/// published in the last `CONCURRENT_PUBLISH_WARNING_MINUTES` for the same
/// repo is surfaced so a leader can notice a likely-concurrent publish
/// race.
fn concurrent_publish_warning(server: &MemoryServer, repo: &str) -> Option<Value> {
    let mirrors = crate::wiki_ops::list_handoff_mirrors_for_repo(server, repo).ok()?;
    let latest = mirrors.first()?;
    let published_at = latest.published_at?;
    let age = Utc::now().signed_duration_since(published_at);
    if age >= ChronoDuration::zero()
        && age < ChronoDuration::minutes(CONCURRENT_PUBLISH_WARNING_MINUTES)
    {
        Some(json!({
            "other_issue": latest.issue,
            "other_published_at": published_at.to_rfc3339(),
            "age_seconds": age.num_seconds(),
        }))
    } else {
        None
    }
}

async fn write_handoff_mirror(
    server: &MemoryServer,
    repo: &str,
    issue: u64,
    title: &str,
    body: &str,
    refs: &[String],
    published_at: &str,
    supersedes_issue: Option<u64>,
    path: &str,
) -> Result<Value, String> {
    let now = Utc::now().to_rfc3339();
    let metadata = json!({
        "repo": repo,
        "issue": issue,
        "published_at": published_at,
        "supersedes_issue": supersedes_issue,
        "handoff_schema_version": HANDOFF_SCHEMA_VERSION,
        // #1285 owner-ratified: publish IS the leader-authored approval —
        // stamping `review_receipt.decision = "approved"` here is what
        // makes `wiki_layer_metadata` (copilot_ops/support/wiki.rs) compute
        // `lifecycle: "active"` instead of the ordinary agent-write default
        // `pending_review`, through the SAME mechanism every other active
        // wiki entry uses (no special-casing of the lifecycle field).
        "review_receipt": {
            "approver": "tachi_gh:handoff_publish",
            "decision": "approved",
            "decided_at": now,
        },
    });
    let write_params = WikiWriteParams {
        title: title.to_string(),
        text: body.to_string(),
        path: Some(path.to_string()),
        topic: None,
        summary: None,
        category: "handoff".to_string(),
        keywords: vec!["handoff".to_string()],
        entities: vec![],
        importance: 0.9,
        scope: "global".to_string(),
        retention_policy: "permanent".to_string(),
        domain: Some("handoff".to_string()),
        project: Some("wiki".to_string()),
        metadata: Some(metadata),
        force: true,
        references: refs.to_vec(),
        include_patterns: false,
        pattern_query: None,
        pattern_top_k: None,
    };
    let raw = crate::copilot_ops::handle_tachi_wiki_write(server, write_params).await?;
    serde_json::from_str(&raw).map_err(|e| format!("parse wiki mirror write response: {e}"))
}

/// Verify target → comment → restricted close → label swap. #1285 codex
/// review, two fixes:
///
/// 1. **Ordering** (close before swap, not after): `attempt_restricted_close`
///    re-verifies the `handoff` label LIVE (not a stale discovery snapshot)
///    immediately before closing — that is the entire point of the
///    restricted-close gate (only ever close a handoff-labeled issue). If
///    the label swap ran first, the live re-fetch would see
///    `handoff-superseded` (the swap's own output) and `restricted_close_allowed`
///    would refuse every single normal-flow close — the gate would never
///    once fire in production. Running close first means the live re-fetch
///    still observes the pre-swap `handoff` label, preserving both the
///    gate's live-re-verify semantics AND making it actually reachable.
/// 2. **Target verification is un-widenable**: `verify_supersede_target`
///    runs before ANY mutation (comment/close/label-swap), for both the
///    auto-discovered path and an explicit caller-supplied
///    `params.supersedes` — this function is the single call site for
///    both (see `handle_gh_handoff_publish`), so gating here covers both by
///    construction. A caller-supplied override does not get to skip the
///    open+handoff-labeled check that auto-discovery already enforces via
///    `--label handoff --state open`.
///
/// Comment and label-swap remain the two load-bearing steps ("comment+label
/// 两笔仍算成功") — a close failure/refusal degrades to a manual fallback
/// command inside the returned `Value`, never fails the overall supersede
/// step.
fn supersede_previous_handoff(
    server: &MemoryServer,
    repo: &str,
    previous_issue: u64,
    new_issue: u64,
) -> Result<Value, String> {
    verify_supersede_target(server, repo, previous_issue)?;

    let comment_body = format!("superseded by {repo}#{new_issue}");
    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args(["issue", "comment", &previous_issue.to_string()])
        .args(["--repo", repo]);
    let _body_file = attach_gh_body_file(&mut cmd, &comment_body)?;
    run_gh(cmd, &token).map_err(|e| format!("supersede comment failed: {e}"))?;

    // Close BEFORE the label swap — see the fn doc above for why the order
    // matters (the close gate's live re-fetch must not observe our own swap).
    let close_result = attempt_restricted_close(server, repo, previous_issue);

    swap_handoff_label(server, repo, previous_issue)
        .map_err(|e| format!("supersede label swap failed (comment/close already done): {e}"))?;

    Ok(json!({
        "previous_issue": previous_issue,
        "comment": "ok",
        "close": close_result,
        "label_swap": "ok",
    }))
}

/// Un-widenable pre-mutation gate: `previous_issue` must be open AND carry
/// the `handoff` label before `supersede_previous_handoff` posts a comment,
/// closes it, or swaps its label. Applies identically whether
/// `previous_issue` came from `fetch_open_handoff_issues` auto-discovery
/// (already `--label handoff --state open` filtered, so this is normally a
/// no-op re-check) or an explicit caller-supplied `params.supersedes` (#1285
/// codex review — an explicit override must not bypass this: it is the ONLY
/// path that can name an arbitrary same-repo issue number, so it is exactly
/// the path this gate exists for). Reuses `restricted_close_allowed`'s label
/// check rather than a second label-matching implementation.
fn verify_supersede_target(server: &MemoryServer, repo: &str, number: u64) -> Result<(), String> {
    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args(["issue", "view", &number.to_string()])
        .args(["--repo", repo])
        .args(["--json", "state,labels"]);
    let output = run_gh_json(cmd, &token)
        .map_err(|e| format!("could not verify supersede target {repo}#{number}: {e}"))?;
    let value: Value = serde_json::from_str(&output)
        .map_err(|e| format!("parse issue view json for {repo}#{number}: {e}"))?;

    let state = value
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !state.eq_ignore_ascii_case("open") {
        return Err(format!(
            "refusing to supersede {repo}#{number}: issue is not open (state={state})"
        ));
    }
    let labels: Vec<String> = value
        .get("labels")
        .and_then(Value::as_array)
        .map(|labels| {
            labels
                .iter()
                .filter_map(|l| l.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    restricted_close_allowed(&labels, number, number)
        .map_err(|e| format!("refusing to supersede {repo}#{number}: {e}"))
}

fn swap_handoff_label(server: &MemoryServer, repo: &str, number: u64) -> Result<String, String> {
    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args(["issue", "edit", &number.to_string()])
        .args(["--repo", repo])
        .args(["--remove-label", HANDOFF_LABEL])
        .args(["--add-label", HANDOFF_SUPERSEDED_LABEL]);
    run_gh(cmd, &token)
}

/// Re-verifies label state live (not the earlier discovery snapshot) right
/// before closing, then runs the restricted close gate. Never propagates an
/// `Err` up to the caller — a refused/failed close degrades to a manual
/// fallback command inside the returned `Value` (see module doc).
fn attempt_restricted_close(server: &MemoryServer, repo: &str, number: u64) -> Value {
    let fallback = format!("gh issue close {number} --repo {repo} --reason completed");
    let labels = match fetch_issue_labels(server, repo, number) {
        Ok(labels) => labels,
        Err(err) => {
            return json!({ "status": "failed", "error": err, "fallback": fallback });
        }
    };
    if let Err(gate_err) = restricted_close_allowed(&labels, number, number) {
        return json!({ "status": "refused", "error": gate_err, "fallback": fallback });
    }
    match close_superseded_handoff_issue(server, repo, number) {
        Ok(_) => json!({ "status": "ok" }),
        Err(err) => json!({ "status": "failed", "error": err, "fallback": fallback }),
    }
}

fn fetch_issue_labels(
    server: &MemoryServer,
    repo: &str,
    number: u64,
) -> Result<Vec<String>, String> {
    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args(["issue", "view", &number.to_string()])
        .args(["--repo", repo])
        .args(["--json", "labels"]);
    let output = run_gh_json(cmd, &token)?;
    let value: Value =
        serde_json::from_str(&output).map_err(|e| format!("parse issue view json: {e}"))?;
    Ok(value
        .get("labels")
        .and_then(Value::as_array)
        .map(|labels| {
            labels
                .iter()
                .filter_map(|l| l.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default())
}

/// The restricted-close gate itself — pure, unit-testable without `gh`.
/// Refuses to close anything that does not carry the `handoff` label, and
/// refuses (as a belt-and-suspenders invariant check) any number that does
/// not match what supersede discovery selected.
fn restricted_close_allowed(
    labels: &[String],
    expected_issue: u64,
    actual_issue: u64,
) -> Result<(), String> {
    if actual_issue != expected_issue {
        return Err(format!(
            "refusing to close #{actual_issue}: not the issue this supersede discovered (#{expected_issue})"
        ));
    }
    if !labels.iter().any(|label| label == HANDOFF_LABEL) {
        return Err(format!(
            "refusing to close #{actual_issue}: missing '{HANDOFF_LABEL}' label — restricted issue_close only closes handoff-labeled issues"
        ));
    }
    Ok(())
}

/// The ONLY call site in this crate that runs `gh issue close`. NOT exposed
/// as a standalone `tachi_gh` action — callers reach it exclusively through
/// `attempt_restricted_close`, which re-verifies the `handoff` label live
/// immediately beforehand.
fn close_superseded_handoff_issue(
    server: &MemoryServer,
    repo: &str,
    number: u64,
) -> Result<String, String> {
    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args(["issue", "close", &number.to_string()])
        .args(["--repo", repo])
        .args(["--reason", "completed"]);
    run_gh(cmd, &token)
}

async fn emit_handoff_published_event(
    server: &MemoryServer,
    repo: &str,
    issue: u64,
    published_at: &str,
    supersedes_issue: Option<u64>,
) -> Result<String, String> {
    let params = TachiEventParams {
        action: "emit".to_string(),
        format: None,
        id: None,
        source_repo: Some(repo.to_string()),
        adapter: Some("tachi-server".to_string()),
        project: None,
        project_explicit: false,
        domain: Some("handoff".to_string()),
        session_id: None,
        actor: Some("leader".to_string()),
        event_type: Some("handoff.published".to_string()),
        authority: Some("collect_only".to_string()),
        effects: vec!["memory_write".to_string()],
        projection_hints: vec![],
        payload: Some(json!({
            "repo": repo,
            "issue": issue,
            "published_at": published_at,
            "supersedes_issue": supersedes_issue,
        })),
        provenance: Some(json!({ "source": "tachi_gh:handoff_publish" })),
        created_at: None,
        limit: 20,
        path_prefix: None,
        dry_run: false,
    };
    crate::event_ops::handle_tachi_event(server, params).await
}

fn parse_issue_number_from_gh_url(output: &str) -> Option<u64> {
    output.trim().rsplit('/').next()?.parse::<u64>().ok()
}

fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in input.trim().chars() {
        if ch.is_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_end_matches('-').to_string();
    if trimmed.is_empty() {
        "handoff".to_string()
    } else {
        trimmed
    }
}

/// Repo folded into the path (not just the date) so two different repos
/// publishing a same-dated, similarly-titled handoff can never collide onto
/// the same `/wiki` path — a collision there would silently UPDATE the
/// wrong repo's mirror in place (the write path's find-by-path idempotency
/// is exactly what `handoff_repair` relies on being repo-scoped).
fn mirror_path_for(repo: &str, published_at: &str, title: &str) -> String {
    let date = published_at.get(0..10).unwrap_or("undated");
    let repo_slug = repo.replace('/', "-");
    format!("/wiki/handoffs/{date}-{repo_slug}-{}", slugify(title))
}

// ─────────────────────────── handoff_repair ───────────────────────────

/// Idempotent mirror-only rebuild for an existing `handoff`-labeled issue:
/// reads the issue itself (source of truth for title/body), reuses the
/// existing mirror's wiki path when one already exists (repeat calls
/// update the SAME entry, never create a second one), or derives a fresh
/// deterministic path from the issue's own `createdAt` otherwise. Never
/// creates a second GitHub issue.
pub(crate) async fn handle_gh_handoff_repair(
    server: &MemoryServer,
    repo: String,
    number: u64,
) -> Result<String, String> {
    validate_repo(&repo)?;
    let (mut cmd, token) = build_gh_command(server)?;
    cmd.args(["issue", "view", &number.to_string()])
        .args(["--repo", &repo])
        .args(["--json", "number,title,body,labels,createdAt"]);
    let output = run_gh_json(cmd, &token)?;
    let issue: Value =
        serde_json::from_str(&output).map_err(|e| format!("parse gh issue view json: {e}"))?;

    let labels: Vec<String> = issue
        .get("labels")
        .and_then(Value::as_array)
        .map(|labels| {
            labels
                .iter()
                .filter_map(|l| l.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if !labels.iter().any(|label| label == HANDOFF_LABEL) {
        return Err(format!(
            "{repo}#{number} does not carry the '{HANDOFF_LABEL}' label — handoff_repair only rebuilds handoff mirrors"
        ));
    }
    let title = issue
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let body = issue
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let created_at = issue
        .get("createdAt")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let existing = crate::wiki_ops::list_handoff_mirrors_for_repo(server, &repo)
        .unwrap_or_default()
        .into_iter()
        .find(|mirror| mirror.issue == number);

    let (path, published_at, supersedes_issue, refs) = match &existing {
        Some(mirror) => (
            mirror.path.clone(),
            mirror
                .published_at
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_else(|| created_at.clone()),
            mirror.supersedes_issue,
            mirror.references.clone(),
        ),
        None => (
            mirror_path_for(&repo, &created_at, &title),
            created_at.clone(),
            None,
            Vec::new(),
        ),
    };

    let mirror = write_handoff_mirror(
        server,
        &repo,
        number,
        &title,
        &body,
        &refs,
        &published_at,
        supersedes_issue,
        &path,
    )
    .await?;

    serde_json::to_string(&json!({
        "ok": true,
        "action": "handoff_repair",
        "repo": repo,
        "issue": number,
        "wiki_write_mode": mirror.get("wiki_write_mode").cloned().unwrap_or(Value::Null),
        "mirror": mirror,
    }))
    .map_err(|e| format!("serialize handoff_repair receipt: {e}"))
}

// ─────────────────────────── tests ───────────────────────────
// Most of these are pure logic (no gh, no DB). The `supersede_*` tests at
// the bottom (#1285 codex review regression coverage) are the exception:
// they drive `supersede_previous_handoff` against a fake `gh` shim on PATH
// (same pattern as `tests::gh_comment_tests`), because the two bugs they
// guard against are about the ORDER/gating of real `gh` calls, which a
// pure-logic test of `restricted_close_allowed` alone cannot discriminate.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restricted_close_refuses_missing_handoff_label() {
        let err = restricted_close_allowed(&["bug".to_string()], 42, 42).unwrap_err();
        assert!(err.contains("handoff"), "err: {err}");
    }

    #[test]
    fn restricted_close_refuses_number_mismatch() {
        let err = restricted_close_allowed(&[HANDOFF_LABEL.to_string()], 42, 43).unwrap_err();
        assert!(err.contains("not the issue"), "err: {err}");
    }

    #[test]
    fn restricted_close_allows_matching_handoff_labeled_issue() {
        restricted_close_allowed(&[HANDOFF_LABEL.to_string(), "other".to_string()], 42, 42)
            .expect("handoff-labeled matching issue should be allowed");
    }

    #[test]
    fn parse_issue_number_from_gh_url_extracts_trailing_number() {
        assert_eq!(
            parse_issue_number_from_gh_url("https://github.com/kckylechen1/tachi/issues/1285\n"),
            Some(1285)
        );
        assert_eq!(parse_issue_number_from_gh_url("not a url"), None);
    }

    #[test]
    fn slugify_lowercases_and_collapses_separators() {
        assert_eq!(slugify("Session Wrap-Up: #1285!"), "session-wrap-up-1285");
        assert_eq!(slugify(""), "handoff");
    }

    #[test]
    fn mirror_path_for_folds_repo_and_date_to_avoid_cross_repo_collision() {
        let a = mirror_path_for("owner/repo-a", "2026-07-19T00:00:00Z", "Session wrap");
        let b = mirror_path_for("owner/repo-b", "2026-07-19T00:00:00Z", "Session wrap");
        assert_ne!(a, b);
        assert!(a.starts_with("/wiki/handoffs/2026-07-19-owner-repo-a-"));
        assert!(b.starts_with("/wiki/handoffs/2026-07-19-owner-repo-b-"));
    }

    #[test]
    fn extract_next_steps_section_finds_heading_case_insensitively() {
        let body = "# Handoff\n\nSome ledger text.\n\n## Next Steps\n\nDo the thing.\nAnd another thing.\n\n## Pitfalls\n\nWatch out.";
        let section = extract_next_steps_section(body).expect("section found");
        assert!(section.contains("Do the thing."));
        assert!(!section.contains("Watch out."));
    }

    #[test]
    fn extract_next_steps_section_returns_none_without_heading() {
        assert_eq!(extract_next_steps_section("no headings here at all"), None);
    }

    #[test]
    fn resolve_since_prefers_explicit_override_over_previous_published_at() {
        let previous = Some(Utc::now() - ChronoDuration::days(1));
        let resolved =
            resolve_since(Some("2020-01-01T00:00:00Z"), previous).expect("valid override");
        assert_eq!(resolved.to_rfc3339(), "2020-01-01T00:00:00+00:00");
    }

    #[test]
    fn resolve_since_falls_back_to_previous_published_at_when_no_override() {
        let previous = Utc::now() - ChronoDuration::days(3);
        let resolved = resolve_since(None, Some(previous)).expect("resolves");
        assert_eq!(resolved, previous);
    }

    #[test]
    fn resolve_since_falls_back_to_default_window_when_nothing_available() {
        let resolved = resolve_since(None, None).expect("resolves");
        let expected_floor = Utc::now() - ChronoDuration::days(DEFAULT_HANDOFF_WINDOW_DAYS + 1);
        assert!(resolved > expected_floor);
    }

    #[test]
    fn resolve_since_rejects_invalid_override() {
        let err = resolve_since(Some("not-a-date"), None).unwrap_err();
        assert!(err.contains("invalid 'since'"), "err: {err}");
    }

    // ─── #1285 codex review regression tests: fake-`gh`-shim integration ───
    // (see the module-header note above for why these are here rather than
    // folded into the pure-logic tests above them).

    struct PathEnvGuard {
        original: Option<std::ffi::OsString>,
    }

    impl PathEnvGuard {
        fn prepend(dir: &std::path::Path) -> Self {
            let original = std::env::var_os("PATH");
            let mut paths = vec![dir.to_path_buf()];
            if let Some(value) = original.as_ref() {
                paths.extend(std::env::split_paths(value));
            }
            let joined = std::env::join_paths(paths).expect("join PATH");
            std::env::set_var("PATH", joined);
            Self { original }
        }
    }

    impl Drop for PathEnvGuard {
        fn drop(&mut self) {
            if let Some(path) = self.original.as_ref() {
                std::env::set_var("PATH", path);
            } else {
                std::env::remove_var("PATH");
            }
        }
    }

    fn write_executable(path: &std::path::Path, contents: &str) {
        std::fs::write(path, contents).expect("write shim");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(path)
                .expect("shim metadata")
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(path, perms).expect("chmod shim");
        }
    }

    /// A fake `gh` whose `issue view` answer tracks mutable state in
    /// `state_path` (format `"<OPEN|CLOSED>:<label>"`), so a test can prove
    /// whether a LATER `issue view` call observes a label an EARLIER step
    /// in the same `supersede_previous_handoff` invocation already changed
    /// — exactly the condition BUG1 was about (the close gate's live
    /// re-fetch seeing the swap's own output). `issue comment` is a no-op
    /// success; `issue close` flips status to CLOSED; `issue edit
    /// --add-label X` overwrites the label to X. Anything else is an
    /// unexpected call and fails loudly.
    fn stateful_gh_shim_script(state_path: &std::path::Path) -> String {
        format!(
            r#"#!/bin/sh
STATE="{state}"
if [ "$1" = "issue" ] && [ "$2" = "view" ]; then
  cur=$(cat "$STATE")
  status=$(echo "$cur" | cut -d: -f1)
  label=$(echo "$cur" | cut -d: -f2)
  printf '{{"number":%s,"state":"%s","labels":[{{"name":"%s"}}]}}\n' "$3" "$status" "$label"
  exit 0
fi
if [ "$1" = "issue" ] && [ "$2" = "comment" ]; then
  exit 0
fi
if [ "$1" = "issue" ] && [ "$2" = "close" ]; then
  cur=$(cat "$STATE")
  label=$(echo "$cur" | cut -d: -f2)
  echo "CLOSED:$label" > "$STATE"
  exit 0
fi
if [ "$1" = "issue" ] && [ "$2" = "edit" ]; then
  add=""
  while [ $# -gt 0 ]; do
    if [ "$1" = "--add-label" ]; then
      shift
      add="$1"
    fi
    shift
  done
  cur=$(cat "$STATE")
  status=$(echo "$cur" | cut -d: -f1)
  echo "$status:$add" > "$STATE"
  exit 0
fi
echo "unhandled/unexpected gh mutation attempted: $@" >&2
exit 1
"#,
            state = state_path.display()
        )
    }

    /// A fake `gh` whose `issue view` answer is FIXED (never mutated) and
    /// whose every other subcommand fails loudly — used by the BUG2 tests,
    /// where a correct fix must never reach comment/close/edit at all.
    fn readonly_gh_shim_script(fixed_view_json: &str) -> String {
        format!(
            r#"#!/bin/sh
if [ "$1" = "issue" ] && [ "$2" = "view" ]; then
  echo '{json}'
  exit 0
fi
echo "unhandled/unexpected gh mutation attempted: $@" >&2
exit 1
"#,
            json = fixed_view_json
        )
    }

    /// #1285 codex review, BUG 1: on origin/main, `swap_handoff_label` ran
    /// BEFORE `attempt_restricted_close`, whose live label re-fetch would
    /// then observe `handoff-superseded` (the swap's own output) and
    /// refuse — `close.status` was therefore ALWAYS `"refused"` in the
    /// normal supersede flow, never `"ok"`. This is red against that order
    /// (the fake gh's `issue edit` call flips the state file to
    /// `OPEN:handoff-superseded` before the close gate's `issue view` runs)
    /// and green against the fixed order (close's live re-fetch runs while
    /// the state file still reads back the pre-swap `handoff` label).
    #[test]
    fn supersede_close_actually_executes_in_normal_flow() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let fake_bin = tempfile::tempdir().expect("fake bin dir");
        let gh_path = fake_bin.path().join("gh");
        let state_dir = tempfile::tempdir().expect("state dir");
        let state_path = state_dir.path().join("issue-100-state");
        std::fs::write(&state_path, "OPEN:handoff").expect("seed state");
        write_executable(&gh_path, &stateful_gh_shim_script(&state_path));
        let _path_guard = PathEnvGuard::prepend(fake_bin.path());

        let server = crate::tests::make_server();
        let result = supersede_previous_handoff(&server, "owner/repo", 100, 101)
            .expect("supersede should succeed in the normal flow");

        assert_eq!(
            result["close"]["status"],
            json!("ok"),
            "close must actually execute (not be refused) when the previous \
             issue was open+handoff-labeled at the start of supersede — got: {result}"
        );
        let final_state = std::fs::read_to_string(&state_path).expect("read final state");
        assert_eq!(
            final_state.trim(),
            "CLOSED:handoff-superseded",
            "both the close AND the label swap must have run, in that order"
        );
    }

    /// #1285 codex review, BUG 2: `verify_supersede_target` must reject a
    /// `previous_issue` missing the `handoff` label BEFORE any mutation —
    /// this fake gh fails loudly on anything other than `issue view`, so if
    /// the fix regressed and a comment/close/edit call were attempted, the
    /// test would fail on the shim's own error text instead of the expected
    /// refusal message.
    #[test]
    fn supersede_refuses_non_handoff_labeled_target_with_zero_mutation() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let fake_bin = tempfile::tempdir().expect("fake bin dir");
        let gh_path = fake_bin.path().join("gh");
        write_executable(
            &gh_path,
            &readonly_gh_shim_script(r#"{"number":999,"state":"OPEN","labels":[{"name":"bug"}]}"#),
        );
        let _path_guard = PathEnvGuard::prepend(fake_bin.path());

        let server = crate::tests::make_server();
        let err = supersede_previous_handoff(&server, "owner/repo", 999, 101)
            .expect_err("a non-handoff-labeled target must be refused");

        assert!(err.contains("refusing to supersede"), "err: {err}");
        assert!(err.contains("handoff"), "err: {err}");
        assert!(
            !err.contains("unexpected gh mutation attempted"),
            "verification must reject BEFORE any comment/close/edit call — err: {err}"
        );
    }

    /// Same gate, closed-issue clause: a target pointing at an
    /// already-closed (even if still handoff-labeled) issue must also be
    /// refused before any mutation.
    #[test]
    fn supersede_refuses_closed_target_with_zero_mutation() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let fake_bin = tempfile::tempdir().expect("fake bin dir");
        let gh_path = fake_bin.path().join("gh");
        write_executable(
            &gh_path,
            &readonly_gh_shim_script(
                r#"{"number":999,"state":"CLOSED","labels":[{"name":"handoff"}]}"#,
            ),
        );
        let _path_guard = PathEnvGuard::prepend(fake_bin.path());

        let server = crate::tests::make_server();
        let err = supersede_previous_handoff(&server, "owner/repo", 999, 101)
            .expect_err("a closed target must be refused");

        assert!(err.contains("refusing to supersede"), "err: {err}");
        assert!(err.contains("not open"), "err: {err}");
        assert!(
            !err.contains("unexpected gh mutation attempted"),
            "verification must reject BEFORE any comment/close/edit call — err: {err}"
        );
    }
}
