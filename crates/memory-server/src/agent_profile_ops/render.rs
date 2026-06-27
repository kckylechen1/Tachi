use crate::tool_params::TachiProfileParams;
use memory_core::{AgentProfilePack, AgentProfileRule, RenderedAgentProfile};

use super::pack::identity_name;

pub(super) fn render_targets(params: &TachiProfileParams) -> Vec<String> {
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

pub(super) fn render_profile_target(
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
