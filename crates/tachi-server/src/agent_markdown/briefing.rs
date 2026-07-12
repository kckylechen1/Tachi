use super::*;

pub(crate) fn format_briefing(
    query: &str,
    project_label: Option<&str>,
    stickies: &Value,
    memories: &Value,
    wiki: &Value,
    cross_project: &Value,
    health_summary: &Value,
    verification: &Value,
    kanban: &Value,
    checkpoints: &Value,
    open_loops: &[Value],
    component_governance: &Value,
    issue_freshness: &Value,
    compact: bool,
) -> String {
    let memory_cap = if compact { 6 } else { 12 };
    let wiki_cap = if compact { 3 } else { 5 };
    let kanban_cap = if compact { 3 } else { 5 };
    let checkpoint_cap = if compact { 2 } else { 3 };
    let cross_cap = if compact { 3 } else { 5 };
    let verification_cap = if compact { 3 } else { 6 };

    let mut out = vec!["## Tachi briefing".to_string(), format!("Query: {query}")];
    if let Some(project) = project_label.filter(|p| !p.is_empty()) {
        out.push(format!(
            "Project focus: `{project}` (memories/wiki from this repo)"
        ));
    } else {
        out.push(
            "Project focus: _unscoped_ (no git project detected; pass `project='name'` for a named DB or `scope='all'` when looking for global/wiki entries)"
                .to_string(),
        );
    }
    out.push(
        "Layer authority: [AUTHORITY: docs/specs > guide/SOP > wiki > memory/eval]. This compatibility briefing shows memory/wiki/evidence; use `tachi_task(action='briefing')` for feature-scoped canonical docs/specs."
            .to_string(),
    );

    // #964: unread stickies render FIRST (frozen semantics #4 — surfaced at
    // the top of the briefing, ahead of open loops/cross-project/everything
    // else). Each row shown here was already atomically claimed as a side
    // effect of this briefing call.
    if let Some(rows) = stickies.as_array() {
        if !rows.is_empty() {
            out.push("\n### 📌 Sticky notes (unread) [AUTHORITY: WORKFLOW STATE]".to_string());
            out.push(
                "_Read-once notes addressed to you. Already marked read by this briefing call — use `tachi_memory(action='sticky_check', include_read=true)` to see the archive._"
                    .to_string(),
            );
            for row in rows {
                let from = row.get("from_agent").and_then(Value::as_str).unwrap_or("?");
                let text = row.get("text").and_then(Value::as_str).unwrap_or("");
                // CP4 belt-and-suspenders, round-3 (codex final review of
                // #964/PR #1003): the PRIMARY scrub now lives at the single
                // row-load choke point (`sticky_ops::pending::
                // scrub_sticky_text_for_read`, applied to every row before
                // it ever reaches this `stickies` JSON value or either JSON
                // route), so `text` here should already be masked. Re-scrub
                // here anyway, at the render boundary, as defense-in-depth —
                // a row reaching this renderer through some future path
                // that bypasses the choke point still cannot leak a live
                // secret into rendered briefing output.
                let (scrubbed_text, _redactions) = scrub_secrets(text);
                out.push(format!(
                    "- from **{from}**: {}",
                    md_escape(&compact_text_line(&scrubbed_text, 200))
                ));
            }
        }
    }

    if !open_loops.is_empty() {
        out.push("\n### ⚠️ Open Loops (closure debt) [AUTHORITY: WORKFLOW STATE]".to_string());
        out.push(
            "_Work done but the loop never closed. Close cheaply now — it won't resurface on its own._".to_string(),
        );
        for item in open_loops {
            let detail = item.get("detail").and_then(Value::as_str).unwrap_or("");
            match item.get("action").and_then(Value::as_str) {
                Some(action) => out.push(format!("- {detail} → `{action}`")),
                None => out.push(format!("- {detail}")),
            }
        }
    }

    if let Some(section) = render_issue_freshness_section(issue_freshness) {
        out.push(section);
    }

    if let Some(handoffs) = cross_project.as_array() {
        if !handoffs.is_empty() {
            out.push(
                "\n### Cross-project (global handoffs) [AUTHORITY: WORKFLOW STATE]".to_string(),
            );
            out.push(
                "_Pending memos from other repos/agents. Ack with `tachi_handoff(action='check')` or leave via `tachi_handoff(action='leave')` (deprecated, #1016 — prefer `tachi_memory(action='sticky_leave'|'sticky_check')` for new notes)._".to_string(),
            );
            for row in handoffs.iter().take(cross_cap) {
                let from = row.get("from_agent").and_then(Value::as_str).unwrap_or("?");
                let summary = row
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("(handoff)");
                let path = row
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or("/handoff");
                out.push(format!(
                    "- [handoff] `{path}` from **{from}**: {}",
                    md_escape(&compact_text_line(summary, 120))
                ));
            }
        }
    }

    out.push("\n### Memories (this project) [AUTHORITY: LOW-MEDIUM]".to_string());
    out.push(format_section_rows(memories, memory_cap));

    if wiki.as_array().is_some_and(|rows| !rows.is_empty()) {
        out.push("\n### Wiki [AUTHORITY: MEDIUM-HIGH]".to_string());
        out.push(format_section_rows(wiki, wiki_cap));
    }

    if let Some(score) = health_summary.get("health_score") {
        out.push(format!(
            "\n### Health snapshot (score {score}) [AUTHORITY: OPS]"
        ));
        if let Some(warnings) = health_summary.get("warnings").and_then(Value::as_array) {
            if warnings.is_empty() {
                out.push("- No active warnings".to_string());
            } else {
                for warning in warnings.iter().take(6) {
                    if let Some(text) = warning.as_str() {
                        out.push(format!("- {text}"));
                    }
                }
            }
        }
        if let Some(wiki_h) = health_summary.get("wiki") {
            out.push(format!(
                "- Wiki hygiene: {} orphan(s), {} stale, {} duplicate(s)",
                wiki_h.get("orphans").and_then(Value::as_u64).unwrap_or(0),
                wiki_h
                    .get("stale_nodes")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                wiki_h
                    .get("duplicates")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            ));
        }
    }

    if let Some(matches) = component_governance
        .get("matches")
        .and_then(Value::as_array)
    {
        if !matches.is_empty() {
            out.push("\n### Component governance [AUTHORITY: GOVERNANCE REGISTRY]".to_string());
            out.push(
                "_Declared registry only — stale/unknown are not memory truth. Use `tachi_component(action='show'|'plan')` for detail._"
                    .to_string(),
            );
            let cap = if compact { 3 } else { 6 };
            for m in matches.iter().take(cap) {
                let id = m.get("component_id").and_then(Value::as_str).unwrap_or("?");
                let ctype = m
                    .get("component_type")
                    .and_then(Value::as_str)
                    .unwrap_or("?");
                let freshness = m
                    .get("freshness")
                    .and_then(|f| f.get("state"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let category = m.get("category").and_then(Value::as_str).unwrap_or("?");
                out.push(format!(
                    "- `{id}` ({ctype}) category=`{category}` freshness=`{freshness}`"
                ));
                if let Some(drift) = m.get("known_drift").and_then(Value::as_array) {
                    for d in drift.iter().take(2) {
                        let area = d.get("area").and_then(Value::as_str).unwrap_or("?");
                        let class = d
                            .get("classification")
                            .and_then(Value::as_str)
                            .unwrap_or("?");
                        out.push(format!("  - drift `{area}` — {class}"));
                    }
                }
                if let Some(prereqs) = m.get("upstream_prereqs").and_then(Value::as_array) {
                    if !prereqs.is_empty() && freshness != "current" {
                        out.push(format!(
                            "  - {} upstream prereq(s) on record (verify before cutover)",
                            prereqs.len()
                        ));
                    }
                }
            }
        }
    }

    if let Some(rows) = verification.as_array() {
        if !rows.is_empty() {
            out.push("\n### Verification gates [AUTHORITY: EVAL EVIDENCE]".to_string());
            for row in rows.iter().take(verification_cap) {
                let flow_id = row.get("flow_id").and_then(Value::as_str).unwrap_or("?");
                let overall = row
                    .get("overall")
                    .and_then(Value::as_str)
                    .unwrap_or("pending");
                let total = row.get("total").and_then(Value::as_u64).unwrap_or(0);
                let failed = row.get("failed").and_then(Value::as_u64).unwrap_or(0);
                let pending = row.get("pending").and_then(Value::as_u64).unwrap_or(0);
                let pr_ref = row.get("pr_ref").and_then(Value::as_str).unwrap_or("");
                out.push(format!(
                    "- [{overall}] `{flow_id}`{} checks={total} failed={failed} pending={pending}",
                    if pr_ref.is_empty() {
                        String::new()
                    } else {
                        format!(" `{}`", md_escape(&compact_text_line(pr_ref, 80)))
                    }
                ));
            }
        }
    }

    if let Some(tasks) = kanban.get("tasks").and_then(Value::as_array) {
        if !tasks.is_empty() {
            out.push("\n### Kanban [AUTHORITY: WORKFLOW STATE]".to_string());
            for task in tasks.iter().take(kanban_cap) {
                let summary = task
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("(task)");
                let state = task
                    .get("state")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                out.push(format!("- [{state}] {summary}"));
            }
        }
    }

    if let Some(cps) = checkpoints.as_array() {
        if !cps.is_empty() {
            out.push("\n### Recent checkpoints [AUTHORITY: MEMORY FRAGMENTS]".to_string());
            for cp in cps.iter().take(checkpoint_cap) {
                let raw_title = cp
                    .get("title")
                    .or_else(|| cp.get("summary"))
                    .and_then(Value::as_str)
                    .unwrap_or("(checkpoint)");
                out.push(format!("- {}", compact_text_line(raw_title, 140)));
            }
        }
    }

    out.push("\n### Suggested next step".to_string());
    out.push(format!(
        "- {}",
        briefing_next_step(
            query,
            wiki,
            health_summary,
            verification,
            kanban,
            component_governance,
        )
    ));
    out.push(
        "\n> Save only new decisions/outcomes as they happen → `tachi_memory(action='save', text=…, keywords=[…], project='…')`. Windsurf/Cursor have no auto-capture."
            .to_string(),
    );

    out.join("\n")
}

/// Render the "Issue freshness" section shared by `tachi_memory`'s
/// compatibility briefing (`format_briefing`, above) and `tachi_task`'s
/// feature briefing markdown (#1000 round-3 codex review finding 5: the
/// `tachi_task` markdown renderer never rendered this section at all, even
/// though the JSON response already carried `issue_freshness` — only
/// `tachi_memory`'s renderer did). Returns `None` when there is nothing to
/// show (both queues empty) so callers can skip the section entirely rather
/// than emit an empty header.
///
/// Wording (#1000 round-3 codex review finding 7, reworded round-4): these
/// rows are review candidates, never verdicts — the blurb below used to
/// describe them as settled judgments, which said the opposite of what the
/// module's own frozen posture is ("圈候选不判决" — circle the candidate, do
/// not judge it).
pub(crate) fn render_issue_freshness_section(issue_freshness: &Value) -> Option<String> {
    let zombie_count = issue_freshness["zombies"]["count"].as_u64().unwrap_or(0);
    let stale_count = issue_freshness["stale_candidates"]["count"]
        .as_u64()
        .unwrap_or(0);
    if zombie_count == 0 && stale_count == 0 {
        return None;
    }
    let mut out = vec!["\n### Issue freshness [AUTHORITY: WORKFLOW STATE]".to_string()];
    out.push(
        "_GitHub is truth for content; these are judgment-free review candidates, not verdicts or auto-closes._"
            .to_string(),
    );
    if zombie_count > 0 {
        out.push(format!(
            "- **{zombie_count} zombie(s)** (fixed, still open) — top: {}",
            issue_freshness["zombies"]["items"]
                .as_array()
                .map(|items| items
                    .iter()
                    .filter_map(|i| i.get("issue_ref").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join(", "))
                .unwrap_or_default()
        ));
    }
    if stale_count > 0 {
        out.push(format!(
            "- **{stale_count} stale-spec candidate(s)** (review, no verdict) — top: {}",
            issue_freshness["stale_candidates"]["items"]
                .as_array()
                .map(|items| items
                    .iter()
                    .filter_map(|i| i.get("issue_ref").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join(", "))
                .unwrap_or_default()
        ));
    }
    Some(out.join("\n"))
}

fn briefing_next_step(
    query: &str,
    wiki: &Value,
    health_summary: &Value,
    verification: &Value,
    kanban: &Value,
    component_governance: &Value,
) -> String {
    if verification.as_array().is_some_and(|rows| {
        rows.iter().any(|row| {
            row.get("overall")
                .and_then(Value::as_str)
                .is_some_and(|s| matches!(s, "failed" | "pending"))
        })
    }) {
        return "`tachi_verify(action='board')` to inspect background verification gates before merge."
            .to_string();
    }
    if component_governance
        .get("matches")
        .and_then(Value::as_array)
        .is_some_and(|rows| {
            rows.iter().any(|m| {
                matches!(
                    m.get("freshness")
                        .and_then(|f| f.get("state"))
                        .and_then(Value::as_str),
                    Some("stale" | "unknown")
                ) || m
                    .get("blocked_forks")
                    .and_then(Value::as_array)
                    .is_some_and(|b| !b.is_empty())
            })
        })
    {
        return "`tachi_component(action='show' or 'plan')` to inspect stale/blocked shared-component governance before cutover."
            .to_string();
    }
    let has_warnings = health_summary
        .get("warnings")
        .and_then(Value::as_array)
        .is_some_and(|warnings| !warnings.is_empty());
    if has_warnings {
        return "`tachi_memory(action='alerts')` to inspect operational warnings before deeper work."
            .to_string();
    }
    if kanban
        .get("tasks")
        .and_then(Value::as_array)
        .is_some_and(|tasks| !tasks.is_empty())
    {
        return "`tachi_task(action='board')` to review active work before dispatching new tasks."
            .to_string();
    }
    if wiki.as_array().is_some_and(|rows| !rows.is_empty()) {
        return format!(
            "`tachi_wiki(action='search', query='{}')` for reusable lessons related to this briefing.",
            compact_text_line(query, 80).replace('\'', "")
        );
    }
    format!(
        "`tachi_memory(action='search', scope='all', query='{}')` if project-scoped results look sparse.",
        compact_text_line(query, 80).replace('\'', "")
    )
}
