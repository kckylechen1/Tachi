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
//! Three actions, wired through `tachi_gh` (this commit ships `handoff_draft`
//! only; `handoff_publish`/`handoff_repair` land in follow-up commits of the
//! same #1285 P0 change):
//!   - `handoff_draft`   — read-only, assembles the four-section skeleton.
//!   - `handoff_publish` — issue create → wiki mirror → supersede (comment +
//!     label swap + restricted close) → continuity event, in that order.
//!   - `handoff_repair`  — idempotent mirror-only rebuild for an existing
//!     handoff issue (does not create a second issue or a second mirror).
//!
//! §3 ("next steps") is **always leader-authored**: `handoff_draft` returns
//! raw material only (open issues + the previous handoff's own §3 text, on a
//! best-effort basis), never a synthesized recommendation.

use super::*;
use crate::tool_params::WikiBrowseParams;
use chrono::{DateTime, Duration as ChronoDuration, Utc};

/// GitHub label marking an open campaign-handoff issue. The restricted
/// `issue_close` primitive that closes issues carrying this label lands with
/// `handoff_publish` in a follow-up commit; this commit only ever *reads*
/// (`--label` filter) issues carrying it.
const HANDOFF_LABEL: &str = "handoff";
/// Lookback window when no previous handoff exists to anchor `since` on
/// (first-ever handoff for a repo).
const DEFAULT_HANDOFF_WINDOW_DAYS: i64 = 7;

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

// ─────────────────────────── tests (pure logic only — no gh, no DB) ───────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
}
