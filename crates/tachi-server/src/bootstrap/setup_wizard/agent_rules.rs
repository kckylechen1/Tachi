use std::error::Error;
use std::path::{Path, PathBuf};

pub(crate) const HARNESS_MARKER_START: &str = "<!-- TACHI:HARNESS:START -->";
pub(crate) const HARNESS_MARKER_END: &str = "<!-- TACHI:HARNESS:END -->";
pub(crate) const AGENT_RULES_START: &str = "<!-- BEGIN TACHI MEMORY RULES -->";
pub(crate) const AGENT_RULES_END: &str = "<!-- END TACHI MEMORY RULES -->";

pub(crate) fn is_managed_agent_rules(content: &str) -> bool {
    content.contains(HARNESS_MARKER_START)
        || (content.contains("TACHI:HARNESS") && content.contains("<!--"))
}

pub(crate) fn is_installed_agent_rules(content: &str) -> bool {
    (content.contains(HARNESS_MARKER_START) && content.contains(HARNESS_MARKER_END))
        || (content.contains(AGENT_RULES_START) && content.contains(AGENT_RULES_END))
}

pub(crate) fn has_legacy_tachi_block(content: &str) -> bool {
    if is_managed_agent_rules(content) {
        return false;
    }
    if content.contains(AGENT_RULES_START) || content.contains(AGENT_RULES_END) {
        return true;
    }
    let lower = content.to_ascii_lowercase();
    lower.contains("## tachi memory rules")
        || lower.contains("### tachi memory rules")
        || lower.contains("tachi rules")
}

pub(crate) fn agent_memory_rules_body() -> String {
    format!(
        "## Tachi Memory Rules\n\n\
### Session start (non-trivial work)\n\
- Call `tachi_memory` with `action=\"briefing\"`.\n\
- Call `tachi_memory` with `action=\"alerts\"` when operational warnings may matter.\n\n\
### Project lifecycle (issue/PR/flow work)\n\
- When a `flow_id`, `issue_ref`, or `pr_ref` exists, run `tachi_task` with `action=\"status\"` before PR handoff, release notes, or close-loop.\n\
- Treat the nested `status.cycle` view as read-only lifecycle state; follow its `next_action`.\n\n\
### Delegation — native subagent first\n\
- Use the host harness's native subagent for ordinary local delegation. Tachi remains memory, policy, claims, ledger, receipts, and eval.\n\
- `tachi_task` no longer owns worker launch (`action=\"dispatch\"`/`wait`/`cancel` were removed): use the host harness's native subagent, or `tachi_staff(action=\"start\", task=..., staffing_reason=\"durable_cross_session\")` for an explicit durable/remote exception. `staffing_reason` is a REQUIRED typed value: explicit_user_request | durable_cross_session | cross_device_remote | native_subagent_unavailable. Tachi availability, parallelism, tracking, or vendor choice alone is not a reason.\n\n\
### Save — call proactively after any meaningful milestone\n\
- **Do NOT wait until session end.** Save after: decision made, root cause found, sub-task done, key command confirmed.\n\
- **`tachi_memory` `action=\"save\"`**: your own concise conclusion. Pass `project` (git repo), `path` under `/scratch/…` or `/code-review/…`, `keywords` + `entities`.\n\
- **`action=\"extract_facts\"`**: feed raw undigested text/logs/docs — LLM atomizes into N searchable entries.\n\
- **`action=\"checkpoint\"`**: mid-task pause or handoff (progress + next steps). Not a substitute for save when facts are final.\n\
- **Do not rely on chat history** — Cursor/Windsurf have no `agent_end` auto-capture (OpenClaw does via `capture_session`).\n\
- Skip save only for one-off trivia with nothing worth recalling next session.\n\n\
### Handoff without finishing\n\
- `action=\"checkpoint\"` with concise summary + next steps (not a substitute for `save` when facts are final).\n\n\
### While working\n\
- Stuck / repeated failures → `action=\"alerts\"` or `action=\"ask\"` before more patches.\n\
- Never save secrets, tokens, or raw transcripts.\n\
- Treat warnings returned by `tachi_memory(action=\"alerts\")` as active context.\n"
    )
}

pub(crate) fn agent_memory_rules_block() -> String {
    format!(
        "{HARNESS_MARKER_START}\n\
{AGENT_RULES_START}\n\
{}\
{AGENT_RULES_END}\n\
{HARNESS_MARKER_END}\n",
        agent_memory_rules_body()
    )
}

fn cursor_memory_rules_mdc(block: &str) -> String {
    format!(
        "---\n\
description: Tachi memory workflow — briefing at start, save at task end\n\
globs:\n\
alwaysApply: true\n\
---\n\n\
{block}"
    )
}

#[cfg(target_os = "macos")]
fn windsurf_rule_markdown(block: &str) -> String {
    format!(
        "---\n\
trigger: always_on\n\
description: Tachi memory workflow (briefing + mandatory save at task end)\n\
---\n\n\
{block}"
    )
}

fn find_managed_block_range(existing: &str) -> Option<(usize, usize)> {
    if let Some(start) = existing.find(HARNESS_MARKER_START) {
        if let Some(rel_end) = existing[start..].find(HARNESS_MARKER_END) {
            return Some((start, start + rel_end + HARNESS_MARKER_END.len()));
        }
    }
    if let Some(start) = existing.find(AGENT_RULES_START) {
        if let Some(rel_end) = existing[start..].find(AGENT_RULES_END) {
            return Some((start, start + rel_end + AGENT_RULES_END.len()));
        }
    }
    None
}

pub(super) fn merge_managed_block(existing: &str, block: &str) -> String {
    if let Some((start, end)) = find_managed_block_range(existing) {
        let mut out = String::new();
        out.push_str(existing[..start].trim_end());
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(block.trim_end());
        let tail = existing[end..].trim_start();
        if !tail.is_empty() {
            out.push_str("\n\n");
            out.push_str(tail);
        }
        out.push('\n');
        return out;
    }

    let mut out = existing.trim_end().to_string();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(block.trim_end());
    out.push('\n');
    out
}

pub(crate) fn install_agent_memory_rules(home: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let markdown_candidates = [
        home.join(".claude").join("CLAUDE.md"),
        home.join(".codex").join("AGENTS.md"),
        home.join(".gemini").join("GEMINI.md"),
    ];
    let block = agent_memory_rules_block();
    let mut updated = Vec::new();
    for path in markdown_candidates {
        let Some(parent) = path.parent() else {
            continue;
        };
        if !parent.exists() {
            continue;
        }
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let merged = merge_managed_block(&existing, &block);
        if merged != existing {
            std::fs::write(&path, merged)?;
            updated.push(path);
        }
    }

    let cursor_dir = home.join(".cursor");
    if cursor_dir.exists() {
        let rules_dir = cursor_dir.join("rules");
        std::fs::create_dir_all(&rules_dir)?;
        let path = rules_dir.join("tachi-memory.mdc");
        let mdc = cursor_memory_rules_mdc(block.trim());
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if existing != mdc {
            std::fs::write(&path, mdc)?;
            updated.push(path);
        }
    }

    let windsurf_global = home
        .join(".codeium")
        .join("windsurf")
        .join("memories")
        .join("global_rules.md");
    if let Some(parent) = windsurf_global.parent() {
        if parent.exists() {
            let existing = std::fs::read_to_string(&windsurf_global).unwrap_or_default();
            let merged = merge_managed_block(&existing, &block);
            if merged != existing {
                std::fs::write(&windsurf_global, merged)?;
                updated.push(windsurf_global);
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        let windsurf_system = home
            .join("Library")
            .join("Application Support")
            .join("Windsurf")
            .join("rules")
            .join("tachi-memory.md");
        if let Some(parent) = windsurf_system.parent() {
            if parent.exists() {
                let md = windsurf_rule_markdown(block.trim());
                let existing = std::fs::read_to_string(&windsurf_system).unwrap_or_default();
                if existing != md {
                    std::fs::write(&windsurf_system, md)?;
                    updated.push(windsurf_system);
                }
            }
        }
    }

    Ok(updated)
}
