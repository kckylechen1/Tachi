use super::*;
use crate::tool_params::GetMemoryParams;

// ─── Prompt assembly (v2) ─────────────────────────────────────────────────

/// Resolve effective skills list, applying stage-based defaults when the caller
/// did not explicitly provide skills.
pub(super) fn resolve_effective_skills(
    params: &TachiDispatchParams,
) -> (Vec<String>, Option<String>) {
    let stage = params.stage.as_deref().unwrap_or("").to_ascii_lowercase();
    let stage_key = stage.split(':').next().unwrap_or("").trim();
    let auto_instruction = if stage_key == "auto" {
        Some(
            "IMPORTANT: Produce a plan first. Do NOT execute directly. \
             Wait for the operator to review the plan and trigger the execute stage."
                .to_string(),
        )
    } else {
        None
    };

    if !params.skills.is_empty() {
        return (params.skills.clone(), auto_instruction);
    }

    let mut skills = match stage_key {
        "brainstorm" => vec!["skill:superpowers-brainstorming".to_string()],
        "plan" => vec![
            "skill:superpowers-writing-plans".to_string(),
            "skill:waza-think".to_string(),
        ],
        "dispatch" | "execute" => vec!["skill:superpowers-executing-plans".to_string()],
        "review" => vec![
            "skill:superpowers-requesting-code-review".to_string(),
            "skill:waza-check".to_string(),
        ],
        "ship" => vec![
            "skill:superpowers-finishing-a-development-branch".to_string(),
            "skill:waza-check".to_string(),
        ],
        "auto" => vec![
            "skill:superpowers-writing-plans".to_string(),
            "skill:waza-think".to_string(),
        ],
        _ => Vec::new(),
    };

    if stage_key != "brainstorm" {
        let route = crate::copilot_ops::build_task_brief_routing(&params.task, &[]);
        for sop in route.selected_sops {
            let Some(id) = sop.get("id").and_then(|value| value.as_str()) else {
                continue;
            };
            if id.starts_with("skill:") && skill_id_is_dispatch_builtin(id) {
                skills.push(id.to_string());
            }
        }
    }

    dedupe_preserve_order(&mut skills);
    (skills, auto_instruction)
}

fn dedupe_preserve_order(items: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| seen.insert(item.clone()));
}

fn skill_id_is_dispatch_builtin(id: &str) -> bool {
    matches!(
        id,
        "skill:waza-check"
            | "skill:waza-design"
            | "skill:waza-health"
            | "skill:waza-hunt"
            | "skill:waza-learn"
            | "skill:waza-read"
            | "skill:waza-tachi"
            | "skill:waza-think"
            | "skill:waza-write"
            | "skill:coding-refactor-checklist"
            | "skill:coding-test-strategy"
            | "skill:coding-architecture-decision"
    )
}

fn compact_example_text(text: &str, max_chars: usize) -> String {
    let mut out = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if out.chars().count() > max_chars {
        out = out.chars().take(max_chars).collect::<String>();
        out.push_str("...");
    }
    out
}

async fn prompt_row_text(
    server: &MemoryServer,
    row: &serde_json::Value,
    project: Option<&str>,
) -> Option<String> {
    if let Some(text) = row.get("text").and_then(|v| v.as_str()) {
        if !text.trim().is_empty() {
            return Some(text.to_string());
        }
    }

    if let Some(id) = row.get("id").and_then(|v| v.as_str()) {
        let raw = crate::memory_ops::handle_get_memory(
            server,
            GetMemoryParams {
                id: id.to_string(),
                include_archived: false,
                project: project.map(str::to_string),
            },
        )
        .await
        .ok()?;
        let full: serde_json::Value = serde_json::from_str(&raw).ok()?;
        if let Some(text) = full.get("text").and_then(|v| v.as_str()) {
            if !text.trim().is_empty() {
                return Some(text.to_string());
            }
        }
    }

    row.get("summary")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

pub(crate) async fn assemble_prompt(server: &MemoryServer, params: &TachiDispatchParams) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(overlay) =
        crate::prompt_envelope::render_envelope_overlay(&params.agent, params.stage.as_deref())
    {
        parts.push(overlay);
    }

    let route = crate::copilot_ops::build_task_brief_routing(&params.task, &[]);
    parts.push(render_task_route_overlay(&route));

    // Resolve skills with stage defaults
    let (effective_skills, extra_instruction) = resolve_effective_skills(params);

    // 1. Context from memory/wiki (v2: default query = task if none provided)
    let context_query = params
        .context_query
        .as_deref()
        .unwrap_or(&params.task)
        .to_string();

    if !context_query.is_empty() {
        if let Ok(rows) = crate::memory_search_ops::search_memory_rows(
            server,
            SearchMemoryParams {
                query: context_query.clone(),
                query_vec: None,
                top_k: 5,
                path_prefix: None,
                include_training: false,
                include_archived: false,
                candidates_per_channel: 20,
                mmr_threshold: Some(0.7),
                graph_expand_hops: 0,
                graph_relation_filter: None,
                weights: None,
                agent_role: None,
                project: params.project.clone(),
                domain: None,
                file_context: None,
                error_context: None,
                enable_rerank: false,
                as_of: None,
                include_metadata: false,
            },
            false,
        )
        .await
        {
            if !rows.is_empty() {
                parts.push("## Relevant context from Tachi memory/wiki".to_string());
                for row in &rows {
                    if let Some(text) =
                        prompt_row_text(server, row, params.project.as_deref()).await
                    {
                        let path = row
                            .get("path")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        parts.push(format!("### {}\n{}", path, text));
                    }
                }
                parts.push(String::new());
            }
        }

        if let Ok(rows) = crate::memory_search_ops::search_memory_rows(
            server,
            SearchMemoryParams {
                query: context_query.clone(),
                query_vec: None,
                top_k: 2,
                path_prefix: Some("/sft".to_string()),
                include_training: true,
                include_archived: false,
                candidates_per_channel: 12,
                mmr_threshold: Some(0.85),
                graph_expand_hops: 0,
                graph_relation_filter: None,
                weights: None,
                agent_role: None,
                project: params.project.clone(),
                domain: None,
                file_context: None,
                error_context: None,
                enable_rerank: false,
                as_of: None,
                include_metadata: false,
            },
            false,
        )
        .await
        {
            if !rows.is_empty() {
                parts.push("## SFT gold examples (style only, not live facts)".to_string());
                parts.push(
                    "Use these as answer-shape references. Do not treat historical SFT samples as current project truth."
                        .to_string(),
                );
                for row in &rows {
                    if let Some(text) =
                        prompt_row_text(server, row, params.project.as_deref()).await
                    {
                        let path = row
                            .get("path")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        parts.push(format!(
                            "### {}\n{}",
                            path,
                            compact_example_text(&text, 900)
                        ));
                    }
                }
                parts.push(String::new());
            }
        }
    }

    // 2. Skill invocation contract (effective = explicit + stage/intent defaults)
    let mut skill_sections = Vec::new();
    for skill_id in &effective_skills {
        if let Ok(cap) = server.get_capability(skill_id).map_err(|e| format!("{e}")) {
            let def: serde_json::Value = serde_json::from_str(&cap.definition).unwrap_or_default();
            skill_sections.push(render_skill_invocation_contract(skill_id, &cap, &def));
        } else {
            skill_sections.push(format!(
                "### {skill_id}\n- registry_status: missing\n- instruction: If `tachi_skill` is available, first run `tachi_skill(action='discover', query='{skill_id}')`; otherwise continue with the task route and report that the skill capability was unavailable."
            ));
        }
    }
    if !skill_sections.is_empty() {
        parts.push("## Required skill invocation".to_string());
        parts.push(
            "Before starting substantive work, apply these skills in order. If the child agent has Tachi MCP, prefer `tachi_skill(action='run', skill_id=...)`; otherwise use the embedded contract below. Start your worker output with `Using skills: <ids>` and follow each skill's hard stops and done condition."
                .to_string(),
        );
        parts.extend(skill_sections);
        parts.push(String::new());
    }

    // 3. Avoidance: search for prior failures related to this task
    let avoidance_query = format!("{} failure OR partial OR watchdog", params.task);
    if let Ok(eval_rows) = crate::memory_search_ops::search_memory_rows(
        server,
        SearchMemoryParams {
            query: avoidance_query,
            query_vec: None,
            top_k: 3,
            path_prefix: Some("/eval".to_string()),
            include_training: false,
            include_archived: false,
            candidates_per_channel: 20,
            mmr_threshold: Some(0.7),
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            agent_role: None,
            project: params.project.clone(),
            domain: None,
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
        },
        false,
    )
    .await
    {
        if !eval_rows.is_empty() {
            parts.push("## Prior pitfalls / avoidance notes".to_string());
            for row in &eval_rows {
                if let Some(text) = row.get("text").and_then(|v| v.as_str()) {
                    let path = row
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    parts.push(format!(
                        "- **{}**: {}",
                        path,
                        text.chars().take(300).collect::<String>()
                    ));
                }
            }
            parts.push(String::new());
        }
    }

    // 4. Operating instructions
    parts.push("## Operating instructions".to_string());
    parts.push("- Use Tachi MCP tools if available for additional context.".to_string());
    parts.push("- Call `tachi_complete` when done, including dispatch_id if provided.".to_string());
    parts.push(String::new());

    // 5. Extra instruction from stage (e.g. auto → "plan first")
    if let Some(ref instr) = extra_instruction {
        parts.push(instr.clone());
        parts.push(String::new());
    }

    // 6. Task itself
    parts.push(format!("## Task\n{}", params.task));

    let prompt = parts.join("\n\n");

    // Token budget check: warn if prompt exceeds 50K characters (~12.5K tokens)
    // This is a conservative limit to avoid exceeding model context windows
    const MAX_PROMPT_CHARS: usize = 50000;
    if prompt.len() > MAX_PROMPT_CHARS {
        tracing::warn!(
            "Generated prompt exceeds budget ({} chars > {} chars). Consider reducing context_query or task length.",
            prompt.len(),
            MAX_PROMPT_CHARS
        );
    }

    prompt
}

fn render_skill_invocation_contract(
    skill_id: &str,
    cap: &memory_core::HubCapability,
    def: &serde_json::Value,
) -> String {
    let source_path = def
        .get("source_path")
        .and_then(|value| value.as_str())
        .or_else(|| def.get("skill_path").and_then(|value| value.as_str()))
        .unwrap_or("unknown");
    let prompt = def
        .get("prompt")
        .and_then(|value| value.as_str())
        .map(|value| compact_skill_text(value, 700));
    let content = def
        .get("content")
        .and_then(|value| value.as_str())
        .map(|value| compact_skill_text(strip_frontmatter(value), 1400));

    let mut lines = vec![
        format!("### {skill_id}"),
        format!("- name: {}", cap.name),
        format!("- source_path: {source_path}"),
        format!("- why: {}", cap.description),
        format!(
            "- invocation: `tachi_skill(action='run', skill_id='{skill_id}', args={{\"task\": \"<task>\", \"context\": \"<context>\"}})` when available"
        ),
    ];
    if let Some(prompt) = prompt {
        lines.push(format!("- activation_prompt: {prompt}"));
    }
    if let Some(content) = content {
        lines.push(format!("- embedded_contract: {content}"));
    }
    lines.join("\n")
}

fn strip_frontmatter(text: &str) -> &str {
    let trimmed = text.trim_start();
    if !trimmed.starts_with("---") {
        return trimmed;
    }
    let rest = &trimmed[3..];
    if let Some(end) = rest.find("\n---") {
        return rest[end + 4..].trim_start();
    }
    trimmed
}

fn compact_skill_text(text: &str, max_chars: usize) -> String {
    let mut out = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if out.chars().count() > max_chars {
        out = out.chars().take(max_chars).collect::<String>();
        out.push_str("...");
    }
    out
}

fn render_task_route_overlay(route: &crate::copilot_ops::TaskBriefRouting) -> String {
    let intent = route.intent;
    let mut lines = vec![
        "## Tachi task route".to_string(),
        format!("- intent: {intent}"),
    ];

    let labels = route
        .selected_sops
        .iter()
        .take(4)
        .filter_map(|sop| {
            let id = sop.get("id").and_then(|v| v.as_str())?;
            let reason = sop.get("reason").and_then(|v| v.as_str()).unwrap_or("");
            Some(if reason.is_empty() {
                format!("  - {id}")
            } else {
                format!("  - {id}: {reason}")
            })
        })
        .collect::<Vec<_>>();
    if !labels.is_empty() {
        lines.push("- selected_sops:".to_string());
        lines.extend(labels);
    }

    let steps = route
        .tool_plan
        .iter()
        .take(5)
        .filter_map(|step| {
            let tool = step.get("tool").and_then(|v| v.as_str())?;
            let action = step.get("action").and_then(|v| v.as_str()).unwrap_or("");
            let when = step.get("when").and_then(|v| v.as_str()).unwrap_or("");
            Some(format!("  - {tool}({action}): {when}"))
        })
        .collect::<Vec<_>>();
    if !steps.is_empty() {
        lines.push("- tool_plan:".to_string());
        lines.extend(steps);
    }

    lines.join("\n")
}
