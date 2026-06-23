use std::error::Error;
use std::path::{Path, PathBuf};

pub(super) const AGENT_RULES_START: &str = "<!-- BEGIN TACHI MEMORY RULES -->";
pub(super) const AGENT_RULES_END: &str = "<!-- END TACHI MEMORY RULES -->";

/// MCP `initialize` instructions (also mirrored in agent rule blocks).
pub(crate) fn mcp_server_instructions() -> String {
    "Tachi — memory + Hub copilot for coding agents. \
WORKFLOW: (1) briefing at session start: tachi_memory(action='briefing'). \
(2) save PROACTIVELY after any meaningful milestone — decision made, root cause found, sub-task done, key command confirmed. Do NOT wait until session end: tachi_memory(action='save', text=…, path='/scratch/…' or '/code-review/…', keywords=[tags], entities=[repos/modules], project='<git-project>'). \
(3) checkpoint for mid-task pause or handoff (not a substitute for save): action='checkpoint'. \
(4) extract_facts to atomize raw text/logs via LLM into N searchable facts: action='extract_facts'. \
Cursor/Windsurf have no auto-capture — you must call save explicitly. OpenClaw auto-captures on agent_end. \
Wiki for stable reusable knowledge: tachi_wiki(action='write'). \
Skills: tachi_skill(action='discover') before solving complex problems. \
Diagnostics: tachi_status."
        .to_string()
}

pub(super) fn agent_memory_rules_block() -> String {
    format!(
        "{AGENT_RULES_START}\n\
## Tachi Memory Rules\n\n\
### Session start (non-trivial work)\n\
- Call `tachi_memory` with `action=\"briefing\"` (or `tachi_task` `action=\"plan\"`).\n\
- Optionally `tachi_status` when DB health, vectors, or Foundry jobs may matter.\n\n\
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
- Treat `tachi_status` warnings (keys, vector coverage, failed Foundry jobs) as active context.\n\
{AGENT_RULES_END}\n"
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

pub(super) fn merge_managed_block(existing: &str, block: &str) -> String {
    if let Some(start) = existing.find(AGENT_RULES_START) {
        if let Some(rel_end) = existing[start..].find(AGENT_RULES_END) {
            let end = start + rel_end + AGENT_RULES_END.len();
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
    }

    let mut out = existing.trim_end().to_string();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(block.trim_end());
    out.push('\n');
    out
}

pub(super) fn install_agent_memory_rules(home: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let markdown_candidates = [
        home.join(".claude").join("CLAUDE.md"),
        home.join(".codex").join("AGENTS.md"),
        home.join(".gemini").join("GEMINI.md"),
    ];
    let block = agent_memory_rules_block();
    let body_only = block
        .trim()
        .trim_start_matches(AGENT_RULES_START)
        .trim_end_matches(AGENT_RULES_END)
        .trim();
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
        }
        updated.push(path);
    }

    let cursor_dir = home.join(".cursor");
    if cursor_dir.exists() {
        let rules_dir = cursor_dir.join("rules");
        std::fs::create_dir_all(&rules_dir)?;
        let path = rules_dir.join("tachi-memory.mdc");
        let mdc = cursor_memory_rules_mdc(body_only);
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if existing != mdc {
            std::fs::write(&path, mdc)?;
        }
        updated.push(path);
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
            }
            updated.push(windsurf_global);
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
                let md = windsurf_rule_markdown(body_only);
                let existing = std::fs::read_to_string(&windsurf_system).unwrap_or_default();
                if existing != md {
                    std::fs::write(&windsurf_system, md)?;
                }
                updated.push(windsurf_system);
            }
        }
    }

    Ok(updated)
}
