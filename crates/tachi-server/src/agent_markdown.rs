//! Human-readable Markdown formatting for agent-facing MCP tools.

use crate::utils::compact_text_line;
use serde_json::Value;

mod alerts;
mod briefing;
mod search;
mod shared;
mod wiki;

use shared::{format_section_rows, md_escape};

pub(crate) use alerts::format_alerts;
pub(crate) use briefing::format_briefing;
pub(crate) use search::format_search_sections;
pub(crate) use wiki::{
    format_wiki_browse_category, format_wiki_browse_stats, format_wiki_read, format_wiki_search,
};

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

        let cross = serde_json::json!([]);
        let out = format_briefing(
            "q",
            Some("sigil"),
            &memories,
            &wiki,
            &cross,
            &health,
            &serde_json::json!([]),
            &kanban,
            &checkpoints,
            &[],
            &serde_json::json!({"matches": []}),
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

        let cross = serde_json::json!([]);
        let empty_gov = serde_json::json!({"matches": []});
        let compact = format_briefing(
            "q",
            None,
            &serde_json::json!(memories),
            &serde_json::json!(wiki),
            &cross,
            &health,
            &serde_json::json!([]),
            &kanban,
            &serde_json::json!(checkpoints),
            &[],
            &empty_gov,
            &serde_json::json!({}),
            true,
        );
        let full = format_briefing(
            "q",
            None,
            &serde_json::json!(memories),
            &serde_json::json!(wiki),
            &cross,
            &health,
            &serde_json::json!([]),
            &kanban,
            &serde_json::json!(checkpoints),
            &[],
            &empty_gov,
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
}
