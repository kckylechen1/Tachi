//! Human-readable Markdown formatting for agent-facing MCP tools.

use crate::memory_search_ops::scrub_secrets;
use crate::utils::compact_text_line;
use serde_json::Value;

mod alerts;
mod briefing;
mod search;
mod shared;
mod wiki;

use shared::{format_section_rows, md_escape};

pub(crate) use alerts::format_alerts;
pub(crate) use briefing::{format_briefing, render_issue_freshness_section};
pub(crate) use search::{format_search_memory_markdown, format_search_sections};
pub(crate) use wiki::{
    format_wiki_browse_category, format_wiki_browse_stats, format_wiki_read, format_wiki_search,
};

/// Format polarity for the raw `search_memory` / `tachi_status` MCP tools
/// (tachi#1201 k3): omitted/empty/anything-other-than-"json" means markdown;
/// only an exact (trimmed, case-insensitive) "json" opts into the full JSON
/// shape. This is the OPPOSITE default direction from
/// `facade_memory_ops::evidence_format::wants_json`, which defaults an
/// omitted facade `format` to JSON — do not swap the two helpers between
/// surfaces, the sibling `tachi_memory`/`tachi_search` facades must keep
/// their existing (JSON-default) polarity untouched.
pub(crate) fn wants_explicit_json(format: Option<&str>) -> bool {
    format
        .map(str::trim)
        .filter(|format| !format.is_empty())
        .is_some_and(|format| format.eq_ignore_ascii_case("json"))
}

/// Render an arbitrary status/diagnostic JSON object as a compact markdown
/// bullet digest (tachi#1201 k3's `tachi_status` default when `format` is
/// omitted). No fixed schema is assumed beyond "top-level JSON object" so
/// this stays correct as the underlying status response payload evolves.
pub(crate) fn format_status_markdown(value: &Value) -> String {
    let mut out = vec!["## Tachi status".to_string()];
    match value.as_object() {
        Some(obj) if !obj.is_empty() => {
            for (key, v) in obj {
                render_status_field(&mut out, key, v, 0);
            }
        }
        _ => out.push("_No status data._".to_string()),
    }
    out.join("\n")
}

fn render_status_field(out: &mut Vec<String>, key: &str, value: &Value, depth: usize) {
    let indent = "  ".repeat(depth);
    match value {
        Value::Object(map) => {
            if map.is_empty() {
                out.push(format!("{indent}- **{}**: {{}}", md_escape(key)));
                return;
            }
            out.push(format!("{indent}- **{}**:", md_escape(key)));
            for (k, v) in map {
                render_status_field(out, k, v, depth + 1);
            }
        }
        Value::Array(items) => {
            if items.is_empty() {
                out.push(format!("{indent}- **{}**: []", md_escape(key)));
                return;
            }
            let noun = if items.len() == 1 { "item" } else { "items" };
            out.push(format!(
                "{indent}- **{}** ({} {noun}):",
                md_escape(key),
                items.len()
            ));
            const MAX_ITEMS: usize = 20;
            for (idx, item) in items.iter().enumerate().take(MAX_ITEMS) {
                out.push(format!(
                    "{indent}  {}. {}",
                    idx + 1,
                    scalar_or_compact_line(item)
                ));
            }
            if items.len() > MAX_ITEMS {
                out.push(format!(
                    "{indent}  … +{} more",
                    items.len() - MAX_ITEMS
                ));
            }
        }
        _ => out.push(format!(
            "{indent}- **{}**: {}",
            md_escape(key),
            scalar_or_compact_line(value)
        )),
    }
}

fn scalar_or_compact_line(value: &Value) -> String {
    match value {
        Value::String(s) => compact_text_line(s, 200),
        Value::Null => "null".to_string(),
        Value::Object(_) | Value::Array(_) => {
            compact_text_line(&value.to_string(), 200)
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn long_title() -> String {
        // 250 chars of 'a' with newlines
        let body = "a".repeat(220);
        format!("{body}\n\nNext paragraph that should be truncated by the cap.")
    }

    #[test]
    fn format_briefing_truncates_long_checkpoint_titles() {
        let checkpoints = serde_json::json!([
            {"id": "c1", "title": long_title(), "summary": "fallback"}
        ]);
        let memories = serde_json::json!([]);
        let wiki = serde_json::json!([]);
        let health = serde_json::json!({"health_score": 95, "warnings": [], "wiki": {}});
        let kanban = serde_json::json!({"tasks": []});

        let out = format_briefing(
            "q",
            Some("sigil"),
            &serde_json::json!([]),
            &memories,
            &wiki,
            &health,
            &serde_json::json!([]),
            &kanban,
            &checkpoints,
            &[],
            &serde_json::json!({"matches": []}),
            &serde_json::json!({"zombies": {"count": 0}, "stale_candidates": {"count": 0}}),
            &serde_json::json!({}),
            false,
        );

        // Compact-style truncation kicks in for checkpoints: must be capped.
        // We can't assert an exact char count because compact_text_line adds "…",
        // but the raw newline must not survive, and the line must be short.
        let line = out
            .lines()
            .find(|l| l.starts_with("- "))
            .expect("at least one bullet");
        assert!(!line.contains('\n'), "checkpoint title must be single-line");
        assert!(
            line.len() < 200,
            "checkpoint title should be truncated; got len {}",
            line.len()
        );
    }

    #[test]
    fn format_briefing_compact_caps_section_rows() {
        let memories: Vec<serde_json::Value> = (0..20)
            .map(|i| {
                serde_json::json!({
                    "id": format!("m{i}"),
                    "summary": format!("row {i}"),
                    "topic": "t",
                    "path": "/p",
                })
            })
            .collect();
        let wiki: Vec<serde_json::Value> = (0..20)
            .map(|i| {
                serde_json::json!({
                    "id": format!("w{i}"),
                    "summary": format!("wiki {i}"),
                    "topic": "t",
                    "path": "/wiki/x",
                })
            })
            .collect();
        let checkpoints: Vec<serde_json::Value> = (0..5)
            .map(|i| serde_json::json!({"id": format!("c{i}"), "title": format!("cp {i}")}))
            .collect();
        let health = serde_json::json!({"health_score": 95, "warnings": [], "wiki": {}});
        let kanban = serde_json::json!({"tasks": []});

        let empty_gov = serde_json::json!({"matches": []});
        let compact = format_briefing(
            "q",
            None,
            &serde_json::json!([]),
            &serde_json::json!(memories),
            &serde_json::json!(wiki),
            &health,
            &serde_json::json!([]),
            &kanban,
            &serde_json::json!(checkpoints),
            &[],
            &empty_gov,
            &serde_json::json!({"zombies": {"count": 0}, "stale_candidates": {"count": 0}}),
            &serde_json::json!({}),
            true,
        );
        let full = format_briefing(
            "q",
            None,
            &serde_json::json!([]),
            &serde_json::json!(memories),
            &serde_json::json!(wiki),
            &health,
            &serde_json::json!([]),
            &kanban,
            &serde_json::json!(checkpoints),
            &[],
            &empty_gov,
            &serde_json::json!({"zombies": {"count": 0}, "stale_candidates": {"count": 0}}),
            &serde_json::json!({}),
            false,
        );

        // `format_section_rows` emits lines like "1. **topic** ..." — count
        // numbered rows under a section header.
        fn numbered_rows(s: &str, section_header: &str) -> usize {
            let mut in_section = false;
            let mut n = 0;
            for line in s.lines() {
                if line.starts_with("### ") {
                    in_section = line.contains(section_header);
                    continue;
                }
                if in_section
                    && !line.starts_with("> ")
                    && !line.trim().is_empty()
                    && line
                        .trim_start()
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_digit())
                {
                    n += 1;
                }
            }
            n
        }

        let compact_mem = numbered_rows(&compact, "Memories");
        let full_mem = numbered_rows(&full, "Memories");
        assert_eq!(
            compact_mem, 6,
            "compact must cap memories at 6, got {compact_mem}"
        );
        assert!(
            full_mem >= compact_mem,
            "full must show at least as many memories as compact (compact={compact_mem} full={full_mem})"
        );

        let compact_cp = numbered_rows(&compact, "Recent checkpoints");
        assert!(
            compact_cp <= 2,
            "compact must cap checkpoints at 2, got {compact_cp}"
        );

        // Compact output should be substantially shorter than full output.
        assert!(
            compact.len() < full.len(),
            "compact ({} bytes) should be smaller than full ({} bytes)",
            compact.len(),
            full.len()
        );
    }

    #[test]
    fn format_briefing_compact_caps_verification_gates() {
        let verification: Vec<serde_json::Value> = (0..10)
            .map(|i| {
                serde_json::json!({
                    "flow_id": format!("flow_{i}"),
                    "overall": if i == 0 { "failed" } else { "passed" },
                    "total": 3,
                    "failed": if i == 0 { 1 } else { 0 },
                    "pending": 0,
                })
            })
            .collect();
        let empty = serde_json::json!([]);
        let health = serde_json::json!({"health_score": 95, "warnings": [], "wiki": {}});
        let kanban = serde_json::json!({"tasks": []});
        let compact = format_briefing(
            "q",
            Some("sigil"),
            &empty,
            &empty,
            &empty,
            &health,
            &serde_json::json!(verification),
            &kanban,
            &empty,
            &[],
            &serde_json::json!({"matches": []}),
            &serde_json::json!({"zombies": {"count": 0}, "stale_candidates": {"count": 0}}),
            &serde_json::json!({}),
            true,
        );
        let gate_rows = compact
            .lines()
            .filter(|line| line.starts_with("- [") && line.contains("`flow_"))
            .count();
        assert_eq!(gate_rows, 3);
        assert!(compact.contains("### Verification gates"));
        assert!(compact.contains("tachi_verify(action='board')"));
    }

    #[test]
    fn format_briefing_renders_issue_freshness_section_when_nonempty() {
        let empty = serde_json::json!([]);
        let health = serde_json::json!({"health_score": 95, "warnings": [], "wiki": {}});
        let kanban = serde_json::json!({"tasks": []});
        let issue_freshness = serde_json::json!({
            "zombies": {"count": 2, "items": [{"issue_ref": "o/r#979"}, {"issue_ref": "o/r#947"}]},
            "stale_candidates": {"count": 1, "items": [{"issue_ref": "o/r#500"}]},
        });
        let out = format_briefing(
            "q",
            Some("sigil"),
            &empty,
            &empty,
            &empty,
            &health,
            &empty,
            &kanban,
            &empty,
            &[],
            &serde_json::json!({"matches": []}),
            &issue_freshness,
            &serde_json::json!({}),
            false,
        );
        assert!(out.contains("### Issue freshness"));
        assert!(out.contains("2 zombie(s)"));
        assert!(out.contains("o/r#979"));
        assert!(out.contains("o/r#947"));
        assert!(out.contains("1 stale-spec candidate(s)"));
        assert!(out.contains("o/r#500"));
    }

    #[test]
    fn format_briefing_omits_issue_freshness_section_when_empty() {
        let empty = serde_json::json!([]);
        let health = serde_json::json!({"health_score": 95, "warnings": [], "wiki": {}});
        let kanban = serde_json::json!({"tasks": []});
        let issue_freshness =
            serde_json::json!({"zombies": {"count": 0}, "stale_candidates": {"count": 0}});
        let out = format_briefing(
            "q",
            Some("sigil"),
            &empty,
            &empty,
            &empty,
            &health,
            &empty,
            &kanban,
            &empty,
            &[],
            &serde_json::json!({"matches": []}),
            &issue_freshness,
            &serde_json::json!({}),
            false,
        );
        assert!(!out.contains("### Issue freshness"));
    }

    // CP4 belt-and-suspenders (codex final review of #964/PR #1003): a
    // pre-merge DB cannot contain a raw-secret sticky row (write-time
    // scrubbing in `sticky_ops::handlers::handle_sticky_leave` already
    // covers that — the feature never shipped without it), but this proves
    // the render boundary itself is a second, independent line of defense —
    // a row that somehow bypassed write-time scrubbing (hand-inserted here,
    // simulating a migrated/legacy/out-of-band row) still cannot leak a live
    // secret into the rendered briefing markdown.
    #[test]
    fn format_briefing_scrubs_secrets_in_sticky_text_even_if_row_bypassed_write_scrub() {
        let stickies = serde_json::json!([
            {
                "id": "s-raw-token",
                "from_agent": "wizard",
                "to": serde_json::Value::Null,
                "text": "here is the key: sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ012345",
                "created_at": "2026-07-11T00:00:00Z",
                "kind": "sticky",
            }
        ]);
        let empty = serde_json::json!([]);
        let health = serde_json::json!({"health_score": 95, "warnings": [], "wiki": {}});
        let kanban = serde_json::json!({"tasks": []});
        let issue_freshness =
            serde_json::json!({"zombies": {"count": 0}, "stale_candidates": {"count": 0}});

        let out = format_briefing(
            "q",
            Some("sigil"),
            &stickies,
            &empty,
            &empty,
            &health,
            &empty,
            &kanban,
            &empty,
            &[],
            &serde_json::json!({"matches": []}),
            &issue_freshness,
            &serde_json::json!({}), // presence (empty for this sticky-scrub test)
            false,
        );

        assert!(
            !out.contains("sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ012345"),
            "rendered briefing must never contain the raw secret token:\n{out}"
        );
        // `format_briefing` runs sticky text through `md_escape` (escapes
        // `[`/`]`/`*`/`_`), so the masked marker survives as the escaped
        // `\[REDACTED\]`, not the raw `[REDACTED]` — mirrors the existing
        // assertion shape in `sticky_ops::tests::
        // sticky_leave_scrubs_secrets_in_storage_and_briefing_render`.
        assert!(
            out.contains("\\[REDACTED\\]"),
            "rendered briefing must show the redaction marker in place of the secret:\n{out}"
        );
    }
}
