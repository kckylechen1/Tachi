use super::*;
use crate::tool_params::GetMemoryParams;
use serde_json::{json, Value};

// ─── Prompt assembly (v2) ─────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub(crate) struct PromptAssembly {
    pub prompt: String,
    pub capability_bundle: Value,
}

/// Resolve effective skills list, applying stage-based defaults when the caller
/// did not explicitly provide skills.
pub(super) fn resolve_effective_skills(
    params: &TachiDispatchParams,
) -> (Vec<String>, Option<String>) {
    let stage_key = crate::skill_policy::dispatch_stage_key(params.stage.as_deref());
    let auto_instruction = crate::skill_policy::dispatch_stage_instruction(&stage_key);

    if !params.skills.is_empty() {
        return (params.skills.clone(), auto_instruction);
    }

    let mut skills = crate::skill_policy::dispatch_stage_skills(&stage_key);

    if stage_key != "brainstorm" {
        let route = crate::copilot_ops::build_task_brief_routing(&params.task, &[]);
        crate::skill_policy::append_builtin_sops(&mut skills, route.selected_sops.into_iter());
    }

    crate::skill_policy::dedupe_preserve_order(&mut skills);
    (skills, auto_instruction)
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

pub(crate) async fn assemble_prompt_with_trace(
    server: &MemoryServer,
    params: &TachiDispatchParams,
) -> PromptAssembly {
    let mut parts: Vec<String> = Vec::new();

    let agent = params.agent.as_deref().unwrap_or("unknown");
    if let Some(overlay) =
        crate::prompt_envelope::render_envelope_overlay(agent, params.stage.as_deref())
    {
        parts.push(overlay);
    }

    let route = crate::copilot_ops::build_task_brief_routing(&params.task, &[]);
    parts.push(render_task_route_overlay(&route));

    if params.profile.is_some()
        || params.tool_profile.is_some()
        || params.mcp_access.is_some()
        || params.issue_ref.is_some()
        || params.pr_ref.is_some()
        || params.flow_id.is_some()
    {
        parts.push(render_dispatch_profile_overlay(server, params));
    }

    // Resolve skills with stage defaults
    let (effective_skills, extra_instruction) = resolve_effective_skills(params);

    let capability_requested = params.auto_capability_bundle.unwrap_or(false);
    let capability_source = if params.auto_capability_bundle.is_some() {
        "params"
    } else {
        "unset"
    };
    let mut capability_bundle = json!({
        "status": if capability_requested { "requested" } else { "disabled" },
        "requested": capability_requested,
        "disabled": !capability_requested,
        "injected": false,
        "host": agent,
        "query": params.task,
        "source": capability_source,
        "primary_skill": Value::Null,
        "supporting_capabilities": [],
        "packs": [],
        "host_tools": [],
        "activation_steps": [],
        "rationale": Value::Null,
        "section": Value::Null,
        "error": Value::Null,
        "reason": if capability_requested {
            "auto_capability_bundle=true"
        } else {
            "auto_capability_bundle=false"
        },
    });
    if capability_requested {
        match crate::capability_ops::handle_prepare_capability_bundle(
            server,
            crate::tool_params::PrepareCapabilityBundleParams {
                query: params.task.clone(),
                host: Some(agent.to_string()),
                skill_limit: 2,
                capability_limit: 2,
                pack_limit: 1,
                include_section: true,
            },
        )
        .await
        {
            Ok(raw) => match serde_json::from_str::<Value>(&raw) {
                Ok(value) => {
                    let block = value
                        .get("bundle")
                        .and_then(|bundle| bundle.get("section"))
                        .and_then(|section| section.get("block"))
                        .and_then(|block| block.as_str());
                    if let Some(block) = block {
                        parts.push(block.to_string());
                    }
                    capability_bundle = json!({
                        "status": if block.is_some() { "injected" } else { "prepared" },
                        "requested": true,
                        "disabled": false,
                        "injected": block.is_some(),
                        "host": value.get("host").cloned().unwrap_or_else(|| json!(agent)),
                        "query": value.get("query").cloned().unwrap_or_else(|| json!(params.task)),
                        "source": capability_source,
                        "primary_skill": value.pointer("/bundle/primary_skill").cloned().unwrap_or(Value::Null),
                        "supporting_capabilities": value.pointer("/bundle/supporting_capabilities").cloned().unwrap_or_else(|| json!([])),
                        "packs": value.pointer("/bundle/packs").cloned().unwrap_or_else(|| json!([])),
                        "host_tools": value.pointer("/bundle/host_tools").cloned().unwrap_or_else(|| json!([])),
                        "activation_steps": value.pointer("/bundle/activation_steps").cloned().unwrap_or_else(|| json!([])),
                        "rationale": value.pointer("/bundle/rationale").cloned().unwrap_or(Value::Null),
                        "section": value.pointer("/bundle/section").cloned().unwrap_or(Value::Null),
                        "error": Value::Null,
                        "reason": if block.is_some() {
                            "capability bundle section injected into prompt"
                        } else {
                            "capability bundle prepared without section block"
                        },
                    });
                }
                Err(error) => {
                    capability_bundle = json!({
                        "status": "failed",
                        "requested": true,
                        "disabled": false,
                        "injected": false,
                        "host": agent,
                        "query": params.task,
                        "source": capability_source,
                        "primary_skill": Value::Null,
                        "supporting_capabilities": [],
                        "packs": [],
                        "host_tools": [],
                        "activation_steps": [],
                        "rationale": Value::Null,
                        "section": Value::Null,
                        "error": {
                            "kind": "parse_error",
                            "message": error.to_string(),
                        },
                        "reason": "capability bundle JSON parse failed",
                    });
                }
            },
            Err(error) => {
                capability_bundle = json!({
                    "status": "failed",
                    "requested": true,
                    "disabled": false,
                    "injected": false,
                    "host": agent,
                    "query": params.task,
                    "source": capability_source,
                    "primary_skill": Value::Null,
                    "supporting_capabilities": [],
                    "packs": [],
                    "host_tools": [],
                    "activation_steps": [],
                    "rationale": Value::Null,
                    "section": Value::Null,
                    "error": {
                        "kind": "prepare_error",
                        "message": error,
                    },
                    "reason": "capability bundle preparation failed",
                });
            }
        }
    }

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
        if let Ok(cap) = server.get_capability(skill_id) {
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
    parts.push(
        "- Call `tachi_task(action=\"complete\")` when done, including dispatch_id if provided."
            .to_string(),
    );
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

    PromptAssembly {
        prompt,
        capability_bundle,
    }
}

#[cfg(test)]
pub(crate) async fn assemble_prompt(server: &MemoryServer, params: &TachiDispatchParams) -> String {
    assemble_prompt_with_trace(server, params).await.prompt
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

fn render_dispatch_profile_overlay(server: &MemoryServer, params: &TachiDispatchParams) -> String {
    let mut lines = vec!["## Dispatch profile".to_string()];
    if let Some(profile) = params.profile.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("- profile: {profile}"));
        if let Some(profile_def) = crate::dispatch_profile::resolve_dispatch_profile(profile) {
            lines.push("- skill_loadout:".to_string());
            match crate::dispatch_profile::profile_skill_loadout_json_for_server(
                server,
                profile_def,
            ) {
                Ok(loadout) => {
                    for label in [
                        "common_skills",
                        "signature_skills",
                        "projected_signature_skills",
                        "passive_traits",
                        "projected_passive_traits",
                        "forbidden_skills",
                    ] {
                        let items = loadout
                            .get(label)
                            .and_then(|value| value.as_array())
                            .into_iter()
                            .flatten()
                            .filter_map(|value| value.as_str())
                            .collect::<Vec<_>>();
                        if !items.is_empty() {
                            lines.push(format!("  - {label}: {}", items.join(", ")));
                        }
                    }
                    if let Some(status) = loadout
                        .get("projection")
                        .and_then(|projection| projection.get("status"))
                        .and_then(|status| status.as_str())
                    {
                        lines.push(format!("  - projection_status: {status}"));
                    }
                }
                Err(err) => {
                    lines.push(format!("  - loadout_error: {err}"));
                }
            }
            match crate::dispatch_profile::profile_evidence_contract_json_for_server(
                server,
                profile_def,
            ) {
                Ok(contract) => {
                    lines.push("- evidence_contract:".to_string());
                    for label in ["required", "projected_required"] {
                        let items = contract
                            .get(label)
                            .and_then(|value| value.as_array())
                            .into_iter()
                            .flatten()
                            .filter_map(|value| value.as_str())
                            .collect::<Vec<_>>();
                        if !items.is_empty() {
                            lines.push(format!("  - {label}: {}", items.join(", ")));
                        }
                    }
                    if let Some(status) = contract
                        .get("projection")
                        .and_then(|projection| projection.get("status"))
                        .and_then(|status| status.as_str())
                    {
                        lines.push(format!("  - evidence_projection_status: {status}"));
                    }
                }
                Err(err) => {
                    lines.push(format!("  - evidence_contract_error: {err}"));
                }
            }
        }
    }
    if let Some(agent) = params.agent.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("- backend: {agent}"));
    }
    if let Some(stage) = params.stage.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("- stage: {stage}"));
    }
    if let Some(tool_profile) = params
        .tool_profile
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        lines.push(format!("- tachi_tool_profile: {tool_profile}"));
    }
    if let Some(flow_id) = params.flow_id.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("- flow_id: {flow_id}"));
    }
    if let Some(issue_ref) = params.issue_ref.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("- issue_ref: {issue_ref}"));
    }
    if let Some(pr_ref) = params.pr_ref.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("- pr_ref: {pr_ref}"));
    }
    if let Some(access) = params.mcp_access.as_ref() {
        if let Ok(compact) = serde_json::to_string(access) {
            lines.push(format!("- tool_access: {compact}"));
        }
    }
    if !params.allowed_mcp_servers.is_empty() {
        lines.push(format!(
            "- allowed_mcp_servers: {}",
            params.allowed_mcp_servers.join(", ")
        ));
    }
    lines.push("- completion_report: report files changed, tests run, blockers, and any unavailable MCP/GitHub context explicitly.".to_string());
    lines.join("\n")
}
