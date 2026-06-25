use crate::tool_params::{TachiEventParams, TachiProfileDocumentParams, TachiProfileParams};
use crate::MemoryServer;
use memory_core::{
    AgentProfileIdentity, AgentProfilePack, AgentProfileRule, AgentProfileSource,
    RenderedAgentProfile,
};
use serde_json::{json, Value};
use std::collections::HashSet;

const DEFAULT_AGENT_ID: &str = "default";

pub(crate) async fn handle_tachi_profile(
    server: &MemoryServer,
    params: TachiProfileParams,
) -> Result<String, String> {
    if !params.dry_run {
        return Err(
            "tachi_profile is read-only in this release; dry_run=false is not supported"
                .to_string(),
        );
    }

    match params.action.trim().to_ascii_lowercase().as_str() {
        "import" => {
            let pack = resolve_pack(&params)?;
            serialize_profile_response(json!({
                "status": "completed",
                "action": "import",
                "dry_run": true,
                "pack": pack,
            }))
        }
        "render" => {
            let pack = resolve_pack(&params)?;
            let targets = render_targets(&params);
            let rendered = targets
                .iter()
                .map(|target| render_profile_target(&pack, target, true))
                .collect::<Result<Vec<_>, String>>()?;
            serialize_profile_response(json!({
                "status": "completed",
                "action": "render",
                "dry_run": true,
                "pack_summary": pack_summary(&pack),
                "documents": rendered,
            }))
        }
        "context" => {
            let pack = resolve_pack(&params)?;
            let context = render_profile_context(server, &pack, &params);
            serialize_profile_response(json!({
                "status": "completed",
                "action": "context",
                "dry_run": true,
                "pack_summary": pack_summary(&pack),
                "context": context,
                "prepend_context": context,
            }))
        }
        other => Err(format!(
            "Unsupported tachi_profile action '{other}'. Expected import|render|context"
        )),
    }
}

fn serialize_profile_response(value: Value) -> Result<String, String> {
    serde_json::to_string_pretty(&value)
        .map_err(|e| format!("Failed to serialize tachi_profile response: {e}"))
}

fn resolve_pack(params: &TachiProfileParams) -> Result<AgentProfilePack, String> {
    if let Some(pack) = params.pack.clone() {
        return serde_json::from_value::<AgentProfilePack>(pack)
            .map_err(|e| format!("Invalid AgentProfilePack JSON: {e}"));
    }

    let mut docs = params.documents.clone();
    for doc in &params.document_paths {
        let content = std::fs::read_to_string(&doc.path)
            .map_err(|e| format!("Failed to read profile document '{}': {e}", doc.path))?;
        docs.push(TachiProfileDocumentParams {
            kind: doc.kind.clone(),
            path: Some(doc.path.clone()),
            content,
        });
    }

    if docs.is_empty() {
        return Err("tachi_profile requires pack or documents/document_paths".to_string());
    }

    let agent_id = params
        .agent_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_AGENT_ID)
        .to_string();
    let mut pack = AgentProfilePack::new(agent_id, params.display_name.clone());
    import_documents_into_pack(&mut pack, &docs);
    if pack.identity.name.is_none() {
        pack.identity.name = pack.display_name.clone();
    }
    Ok(pack)
}

fn import_documents_into_pack(pack: &mut AgentProfilePack, docs: &[TachiProfileDocumentParams]) {
    let mut seen_sources = HashSet::<String>::new();
    let mut seen_rules = HashSet::<String>::new();

    for doc in docs {
        let kind = normalize_kind(&doc.kind);
        let source_key = format!("{}:{}", kind, doc.path.as_deref().unwrap_or(""));
        if seen_sources.insert(source_key) {
            pack.provenance.push(AgentProfileSource {
                kind: kind.clone(),
                path: doc.path.clone(),
                section: None,
            });
        }
        if kind == "identity" {
            merge_identity(&mut pack.identity, &doc.content);
        }
        for candidate in extract_rule_candidates(doc) {
            let bucket = classify_rule_bucket(&kind, candidate.section.as_deref(), &candidate.text);
            let dedupe_key = format!("{bucket}:{}", normalize_dedupe_text(&candidate.text));
            if !seen_rules.insert(dedupe_key) {
                continue;
            }
            let rule = AgentProfileRule {
                id: stable_rule_id(bucket, &candidate.text),
                text: candidate.text,
                tags: rule_tags(bucket),
                source: Some(AgentProfileSource {
                    kind: kind.clone(),
                    path: doc.path.clone(),
                    section: candidate.section,
                }),
            };
            push_rule(pack, bucket, rule);
        }
    }
}

#[derive(Debug)]
struct RuleCandidate {
    text: String,
    section: Option<String>,
}

fn extract_rule_candidates(doc: &TachiProfileDocumentParams) -> Vec<RuleCandidate> {
    let mut candidates = Vec::new();
    let mut current_section = None::<String>;
    let mut in_code = false;
    let mut in_frontmatter = false;

    for (idx, line) in doc.content.lines().enumerate() {
        let trimmed = line.trim();
        if idx == 0 && trimmed == "---" {
            in_frontmatter = true;
            continue;
        }
        if in_frontmatter {
            if trimmed == "---" {
                in_frontmatter = false;
            }
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code = !in_code;
            continue;
        }
        if in_code || trimmed.is_empty() || trimmed.starts_with("<!--") {
            continue;
        }
        if let Some(section) = markdown_heading(trimmed) {
            current_section = Some(section);
            continue;
        }
        if let Some(text) = normalize_rule_line(trimmed) {
            candidates.push(RuleCandidate {
                text,
                section: current_section.clone(),
            });
        }
    }

    candidates
}

fn markdown_heading(line: &str) -> Option<String> {
    if !line.starts_with('#') {
        return None;
    }
    let level = line.chars().take_while(|ch| *ch == '#').count();
    if level == 0 {
        return None;
    }
    let title = line[level..].trim();
    (!title.is_empty()).then(|| title.to_string())
}

fn normalize_rule_line(line: &str) -> Option<String> {
    let mut text = line.trim();
    if text.starts_with("- ") || text.starts_with("* ") {
        text = text[2..].trim();
    } else if let Some(rest) = ordered_list_rest(text) {
        text = rest;
    } else if text.starts_with("**") && text.contains("**") {
        text = text.trim_matches('*').trim();
    } else if text.len() > 220 {
        return None;
    }

    let text = text
        .trim_matches('_')
        .trim_matches('*')
        .trim()
        .replace("**", "");
    if text.len() < 8 || text.starts_with('|') || text.starts_with('`') {
        return None;
    }
    Some(text)
}

fn ordered_list_rest(line: &str) -> Option<&str> {
    let mut chars = line.char_indices();
    let mut last_digit_end = None;
    for (idx, ch) in &mut chars {
        if ch.is_ascii_digit() {
            last_digit_end = Some(idx + ch.len_utf8());
            continue;
        }
        if (ch == '.' || ch == ')') && last_digit_end.is_some() {
            let rest = &line[idx + ch.len_utf8()..];
            return Some(rest.trim_start());
        }
        break;
    }
    None
}

fn merge_identity(identity: &mut AgentProfileIdentity, content: &str) {
    for line in content.lines() {
        let Some((label, value)) = parse_label_value(line) else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() || value.starts_with("_(") {
            continue;
        }
        match label.as_str() {
            "name" => {
                identity.name.get_or_insert_with(|| value.to_string());
            }
            "emoji" => {
                identity.emoji.get_or_insert_with(|| value.to_string());
            }
            "vibe" => {
                identity.vibe.get_or_insert_with(|| value.to_string());
            }
            "avatar" => {
                identity.avatar.get_or_insert_with(|| value.to_string());
            }
            _ => {}
        }
    }
}

fn parse_label_value(line: &str) -> Option<(String, String)> {
    let cleaned = line
        .trim()
        .trim_start_matches("- ")
        .trim_start_matches("* ")
        .replace("**", "");
    let (label, value) = cleaned.split_once(':')?;
    Some((label.trim().to_ascii_lowercase(), value.trim().to_string()))
}

fn classify_rule_bucket(kind: &str, section: Option<&str>, text: &str) -> &'static str {
    let haystack = format!("{} {} {}", kind, section.unwrap_or(""), text).to_ascii_lowercase();

    if matches!(kind, "soul")
        || contains_any(
            &haystack,
            &["voice", "style", "tone", "vibe", "communication"],
        )
    {
        return "voice";
    }
    if matches!(kind, "user")
        || contains_any(
            &haystack,
            &["user", "human", "preference", "annoys", "cares about"],
        )
    {
        return "user_model";
    }
    if contains_any(
        &haystack,
        &[
            "tachi",
            "memory",
            "briefing",
            "recall",
            "checkpoint",
            "wiki",
            "save durable",
        ],
    ) {
        return "memory_policy";
    }
    if contains_any(
        &haystack,
        &[
            "test",
            "verify",
            "verification",
            "lint",
            "typecheck",
            "quality",
            "completion",
        ],
    ) {
        return "quality_bar";
    }
    if contains_any(
        &haystack,
        &[
            "tool",
            "mcp",
            "shell",
            "cli",
            "sandbox",
            "permission",
            "approval",
            "external action",
        ],
    ) {
        return "tool_policy";
    }
    if contains_any(
        &haystack,
        &[
            "role",
            "hat",
            "mode",
            "purpose",
            "responsibility",
            "orchestrator",
            "reviewer",
        ],
    ) {
        return "role_hats";
    }
    if contains_any(&haystack, &["project overlay", "repo", "workspace root"]) {
        return "project_overlays";
    }
    "operating_contract"
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn push_rule(pack: &mut AgentProfilePack, bucket: &str, rule: AgentProfileRule) {
    match bucket {
        "voice" => pack.voice.push(rule),
        "quality_bar" => pack.quality_bar.push(rule),
        "tool_policy" => pack.tool_policy.push(rule),
        "memory_policy" => pack.memory_policy.push(rule),
        "user_model" => pack.user_model.push(rule),
        "role_hats" => pack.role_hats.push(rule),
        "project_overlays" => pack.project_overlays.push(rule),
        "runtime_bindings" => pack.runtime_bindings.push(rule),
        _ => pack.operating_contract.push(rule),
    }
}

fn rule_tags(bucket: &str) -> Vec<String> {
    vec![bucket.to_string()]
}

fn stable_rule_id(bucket: &str, text: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in format!("{bucket}:{text}").as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{bucket}-{hash:016x}")
}

fn normalize_dedupe_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn normalize_kind(kind: &str) -> String {
    match kind.trim().to_ascii_lowercase().as_str() {
        "agents.md" | "agent" | "agents" | "ag" => "agents",
        "claude.md" | "claude" => "claude",
        "gemini.md" | "gemini" => "gemini",
        "cursor" | "mdc" | ".mdc" => "cursor",
        "soul.md" | "soul" => "soul",
        "identity.md" | "identity" => "identity",
        "user.md" | "user" => "user",
        "tools.md" | "tools" => "tools",
        "memory_policy" | "memory" => "memory_policy",
        "tool_policy" | "tooling_policy" => "tool_policy",
        _ => "other",
    }
    .to_string()
}

fn render_targets(params: &TachiProfileParams) -> Vec<String> {
    let mut targets = params
        .targets
        .iter()
        .map(|target| target.trim().to_ascii_lowercase())
        .filter(|target| !target.is_empty())
        .collect::<Vec<_>>();
    if targets.is_empty() {
        if let Some(target) = params
            .target
            .as_deref()
            .map(str::trim)
            .filter(|target| !target.is_empty())
        {
            targets.push(target.to_ascii_lowercase());
        }
    }
    if targets.is_empty() {
        targets.push("codex_agents".to_string());
    }
    targets
}

fn render_profile_target(
    pack: &AgentProfilePack,
    target: &str,
    dry_run: bool,
) -> Result<RenderedAgentProfile, String> {
    let target = normalize_target(target);
    let filename = filename_for_target(&target)?;
    let content = match target.as_str() {
        "codex_agents" => render_agents_like(pack, "AGENTS.md", "Codex"),
        "claude_md" => render_agents_like(pack, "CLAUDE.md", "Claude"),
        "gemini_md" => render_agents_like(pack, "GEMINI.md", "Gemini"),
        "cursor_mdc" => render_cursor_mdc(pack),
        "openclaw_agents" => render_agents_like(pack, "AGENTS.md", "OpenClaw"),
        "openclaw_soul" => render_openclaw_soul(pack),
        "openclaw_identity" => render_openclaw_identity(pack),
        "openclaw_user" => render_openclaw_user(pack),
        "openclaw_tools" => render_openclaw_tools(pack),
        _ => {
            return Err(format!(
                "Unsupported profile render target '{}'. Expected codex_agents|claude_md|gemini_md|cursor_mdc|openclaw_agents|openclaw_soul|openclaw_identity|openclaw_user|openclaw_tools",
                target
            ));
        }
    };
    Ok(RenderedAgentProfile {
        target,
        filename,
        content,
        dry_run,
    })
}

fn normalize_target(target: &str) -> String {
    match target
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_")
        .as_str()
    {
        "agents" | "agent_md" | "codex" | "codex_agents_md" => "codex_agents",
        "claude" | "claudemd" | "claude_md" => "claude_md",
        "gemini" | "geminimd" | "gemini_md" => "gemini_md",
        "cursor" | "cursor_mdc" => "cursor_mdc",
        other => other,
    }
    .to_string()
}

fn filename_for_target(target: &str) -> Result<String, String> {
    Ok(match target {
        "codex_agents" | "openclaw_agents" => "AGENTS.md",
        "claude_md" => "CLAUDE.md",
        "gemini_md" => "GEMINI.md",
        "cursor_mdc" => "tachi-profile.mdc",
        "openclaw_soul" => "SOUL.md",
        "openclaw_identity" => "IDENTITY.md",
        "openclaw_user" => "USER.md",
        "openclaw_tools" => "TOOLS.md",
        _ => return Err(format!("Unsupported profile render target '{target}'")),
    }
    .to_string())
}

fn render_agents_like(pack: &AgentProfilePack, filename: &str, host: &str) -> String {
    let mut lines = render_header(pack, filename, host);
    append_identity(&mut lines, pack);
    append_rules(&mut lines, "Voice", &pack.voice);
    append_rules(&mut lines, "Operating Contract", &pack.operating_contract);
    append_rules(&mut lines, "Quality Bar", &pack.quality_bar);
    append_tachi_mcp_rules(&mut lines);
    append_rules(&mut lines, "Tool Policy", &pack.tool_policy);
    append_rules(&mut lines, "Memory Policy", &pack.memory_policy);
    append_rules(&mut lines, "Role Hats", &pack.role_hats);
    append_rules(&mut lines, "Project Overlays", &pack.project_overlays);
    lines.join("\n").trim_end().to_string() + "\n"
}

fn render_cursor_mdc(pack: &AgentProfilePack) -> String {
    let mut lines = vec![
        "---".to_string(),
        "description: Tachi-rendered agent profile projection".to_string(),
        "globs:".to_string(),
        "alwaysApply: true".to_string(),
        "---".to_string(),
        String::new(),
    ];
    lines.extend(
        render_agents_like(pack, "tachi-profile.mdc", "Cursor")
            .lines()
            .map(String::from),
    );
    lines.join("\n").trim_end().to_string() + "\n"
}

fn render_openclaw_soul(pack: &AgentProfilePack) -> String {
    let mut lines = render_header(pack, "SOUL.md", "OpenClaw");
    append_identity(&mut lines, pack);
    append_rules(&mut lines, "Voice", &pack.voice);
    append_rules(&mut lines, "Boundaries", &pack.operating_contract);
    lines.join("\n").trim_end().to_string() + "\n"
}

fn render_openclaw_identity(pack: &AgentProfilePack) -> String {
    let mut lines = render_header(pack, "IDENTITY.md", "OpenClaw");
    lines.push(format!("- Name: {}", identity_name(pack)));
    if let Some(emoji) = &pack.identity.emoji {
        lines.push(format!("- Emoji: {emoji}"));
    }
    if let Some(vibe) = &pack.identity.vibe {
        lines.push(format!("- Vibe: {vibe}"));
    }
    if let Some(avatar) = &pack.identity.avatar {
        lines.push(format!("- Avatar: {avatar}"));
    }
    lines.join("\n").trim_end().to_string() + "\n"
}

fn render_openclaw_user(pack: &AgentProfilePack) -> String {
    let mut lines = render_header(pack, "USER.md", "OpenClaw");
    append_rules(&mut lines, "User Model", &pack.user_model);
    lines.join("\n").trim_end().to_string() + "\n"
}

fn render_openclaw_tools(pack: &AgentProfilePack) -> String {
    let mut lines = render_header(pack, "TOOLS.md", "OpenClaw");
    append_rules(&mut lines, "Tool Policy", &pack.tool_policy);
    append_tachi_mcp_rules(&mut lines);
    lines.join("\n").trim_end().to_string() + "\n"
}

fn render_header(pack: &AgentProfilePack, filename: &str, host: &str) -> Vec<String> {
    vec![
        format!(
            "<!-- tachi:profile-render target={filename} host={host} schema={} dry_run=true -->",
            pack.schema_version
        ),
        format!("# {filename} - Tachi Agent Profile Projection"),
        String::new(),
        "This file is a projection of the canonical Tachi AgentProfilePack.".to_string(),
        "Do not treat this Markdown file as the source of truth.".to_string(),
        String::new(),
    ]
}

fn append_identity(lines: &mut Vec<String>, pack: &AgentProfilePack) {
    lines.push("## Identity".to_string());
    lines.push(String::new());
    lines.push(format!("- agent_id: {}", pack.agent_id));
    lines.push(format!("- name: {}", identity_name(pack)));
    if let Some(vibe) = &pack.identity.vibe {
        lines.push(format!("- vibe: {vibe}"));
    }
    lines.push(String::new());
}

fn identity_name(pack: &AgentProfilePack) -> String {
    pack.identity
        .name
        .clone()
        .or_else(|| pack.display_name.clone())
        .unwrap_or_else(|| pack.agent_id.clone())
}

fn append_rules(lines: &mut Vec<String>, title: &str, rules: &[AgentProfileRule]) {
    if rules.is_empty() {
        return;
    }
    lines.push(format!("## {title}"));
    lines.push(String::new());
    for rule in rules {
        lines.push(format!("- {}", rule.text));
    }
    lines.push(String::new());
}

fn append_tachi_mcp_rules(lines: &mut Vec<String>) {
    lines.push("## Tachi MCP".to_string());
    lines.push(String::new());
    lines.push("- For non-trivial work, call `tachi_status` and `tachi_memory(action=\"briefing\")` before acting.".to_string());
    lines.push(
        "- Prefer Tachi MCP tools over shelling out to `tachi` when MCP tools are available."
            .to_string(),
    );
    lines.push("- Save durable decisions, root causes, commands, file paths, and verified outcomes with project-scoped memory.".to_string());
    lines.push("- Use static profile files for startup alignment and MCP for fresh recall, feedback, and evolution proposals.".to_string());
    lines.push(String::new());
}

fn render_profile_context(
    server: &MemoryServer,
    pack: &AgentProfilePack,
    params: &TachiProfileParams,
) -> String {
    let session_kind = params
        .session_kind
        .as_deref()
        .unwrap_or("main")
        .to_ascii_lowercase();
    let private_session = matches!(session_kind.as_str(), "subagent" | "group" | "external");
    let include_user = params.include_private_user && !private_session;

    let mut lines = vec![
        "## Tachi Profile Context".to_string(),
        String::new(),
        format!("- agent_id: {}", pack.agent_id),
        format!("- name: {}", identity_name(pack)),
    ];
    if let Some(role) = &params.role {
        lines.push(format!("- requested_role: {role}"));
    }
    if let Some(project) = &params.project {
        lines.push(format!("- project: {project}"));
    }
    lines.push(String::new());

    append_rules_limited(&mut lines, "Voice", &pack.voice, 4);
    append_rules_limited(
        &mut lines,
        "Operating Contract",
        &pack.operating_contract,
        6,
    );
    append_rules_limited(&mut lines, "Quality Bar", &pack.quality_bar, 4);
    append_rules_limited(&mut lines, "Tool Policy", &pack.tool_policy, 4);
    append_rules_limited(&mut lines, "Memory Policy", &pack.memory_policy, 4);
    append_rules_limited(&mut lines, "Role Hats", &pack.role_hats, 4);
    if include_user {
        append_rules_limited(&mut lines, "User Model", &pack.user_model, 4);
    } else if !pack.user_model.is_empty() {
        lines.push("## User Model".to_string());
        lines.push(String::new());
        lines.push("- suppressed for this session kind; request include_private_user=true only in trusted main sessions.".to_string());
        lines.push(String::new());
    }
    if params.include_continuity {
        append_continuity_context(server, params, &mut lines);
    }

    let mut context = lines.join("\n").trim_end().to_string() + "\n";
    truncate_context_to_max_chars(&mut context, params.max_chars);
    context
}

fn append_continuity_context(
    server: &MemoryServer,
    params: &TachiProfileParams,
    lines: &mut Vec<String>,
) {
    let event_params = TachiEventParams {
        action: "context".to_string(),
        format: None,
        id: None,
        source_repo: None,
        adapter: None,
        project: params.project.clone(),
        domain: None,
        session_id: None,
        actor: None,
        event_type: None,
        authority: None,
        effects: Vec::new(),
        projection_hints: Vec::new(),
        payload: None,
        provenance: None,
        created_at: None,
        limit: 8,
        path_prefix: None,
        dry_run: false,
    };
    let context = match crate::continuity_ops::build_continuity_context(server, &event_params) {
        Ok(context) => context,
        Err(error) => {
            lines.push("## Continuity Read Model".to_string());
            lines.push(String::new());
            lines.push(format!("- unavailable: {error}"));
            lines.push(String::new());
            return;
        }
    };

    lines.push("## Continuity Read Model".to_string());
    lines.push(String::new());
    let memory_count = context
        .get("memory_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let event_count = context
        .get("event_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    lines.push(format!(
        "- projected_memories: {memory_count}; recent_events: {event_count}"
    ));
    if let Some(rate) = context
        .pointer("/metrics/session_outcomes/challenge_rate")
        .and_then(Value::as_f64)
    {
        lines.push(format!("- challenge_rate: {rate:.2}"));
    }
    lines.push("- guardrail: continuity context is guidance; facts, tests, and user corrections still dominate.".to_string());
    lines.push("- affect, when present, is tone/reminder only; never scoring, execution, routing, or fact mutation.".to_string());
    append_context_items(lines, "Patterns", context.get("patterns"), 5);
    append_context_items(lines, "Lorebook", context.get("lorebook"), 3);
    append_context_items(lines, "Affect", context.get("affect"), 3);
    lines.push(String::new());
}

fn append_context_items(lines: &mut Vec<String>, title: &str, value: Option<&Value>, limit: usize) {
    let Some(items) = value.and_then(Value::as_array) else {
        return;
    };
    if items.is_empty() {
        return;
    }
    lines.push(format!("### {title}"));
    lines.push(String::new());
    for item in items.iter().take(limit) {
        let summary = item
            .get("summary")
            .or_else(|| item.get("state"))
            .and_then(Value::as_str)
            .unwrap_or("continuity item")
            .trim();
        let path = item
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if path.is_empty() {
            lines.push(format!("- {summary}"));
        } else {
            lines.push(format!("- {summary} ({path})"));
        }
    }
    lines.push(String::new());
}

fn truncate_context_to_max_chars(context: &mut String, max_chars: usize) {
    if context.len() <= max_chars {
        return;
    }
    if max_chars == 0 {
        context.clear();
        return;
    }

    let suffix = "\n[truncated by tachi_profile max_chars]\n";
    if max_chars <= suffix.len() {
        context.truncate(floor_char_boundary(context, max_chars));
        return;
    }

    let cut_at = floor_char_boundary(context, max_chars - suffix.len());
    context.truncate(cut_at);
    context.push_str(suffix);
}

fn floor_char_boundary(value: &str, idx: usize) -> usize {
    let mut boundary = idx.min(value.len());
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

fn append_rules_limited(
    lines: &mut Vec<String>,
    title: &str,
    rules: &[AgentProfileRule],
    limit: usize,
) {
    if rules.is_empty() {
        return;
    }
    lines.push(format!("## {title}"));
    lines.push(String::new());
    for rule in rules.iter().take(limit) {
        lines.push(format!("- {}", rule.text));
    }
    lines.push(String::new());
}

fn pack_summary(pack: &AgentProfilePack) -> Value {
    json!({
        "schema_version": pack.schema_version,
        "agent_id": pack.agent_id,
        "display_name": pack.display_name,
        "identity_name": pack.identity.name,
        "counts": {
            "voice": pack.voice.len(),
            "operating_contract": pack.operating_contract.len(),
            "quality_bar": pack.quality_bar.len(),
            "tool_policy": pack.tool_policy.len(),
            "memory_policy": pack.memory_policy.len(),
            "user_model": pack.user_model.len(),
            "role_hats": pack.role_hats.len(),
            "project_overlays": pack.project_overlays.len(),
            "runtime_bindings": pack.runtime_bindings.len(),
        },
    })
}
