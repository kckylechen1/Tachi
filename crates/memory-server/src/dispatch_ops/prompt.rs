use super::*;

// ─── Prompt assembly (v2) ─────────────────────────────────────────────────

/// Resolve effective skills list, applying stage-based defaults when the caller
/// did not explicitly provide skills.
pub(super) fn resolve_effective_skills(
    params: &TachiDispatchParams,
) -> (Vec<String>, Option<String>) {
    let stage = params.stage.as_deref().unwrap_or("").to_ascii_lowercase();
    let auto_instruction = if stage == "auto" {
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

    let skills = match stage.as_str() {
        "plan" => vec!["skill:superpowers-writing-plans".to_string()],
        "execute" => vec!["skill:superpowers-executing-plans".to_string()],
        "auto" => vec!["skill:superpowers-writing-plans".to_string()],
        _ => Vec::new(),
    };

    (skills, auto_instruction)
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
                    if let Some(text) = row.get("text").and_then(|v| v.as_str()) {
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
    }

    // 2. Skill definitions (effective = explicit + stage defaults)
    for skill_id in &effective_skills {
        if let Ok(cap) = server.get_capability(skill_id).map_err(|e| format!("{e}")) {
            let def: serde_json::Value = serde_json::from_str(&cap.definition).unwrap_or_default();
            if let Some(prompt) = def.get("prompt").and_then(|v| v.as_str()) {
                parts.push(format!("## Skill: {}\n{}", skill_id, prompt));
            }
        }
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
