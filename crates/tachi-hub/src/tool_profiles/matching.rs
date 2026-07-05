use super::patterns::{
    COORDINATE_TOOL_PATTERNS, DELEGATE_MINIMAL_TOOL_PATTERNS, OBSERVE_TOOL_PATTERNS,
    OPERATE_TOOL_PATTERNS, REMEMBER_TOOL_PATTERNS, STANDARD_MINIMAL_TOOL_PATTERNS,
};
use super::types::{default_tool_profile, ToolBundle, ToolProfile};
use rmcp::model::Tool;

pub fn parse_tool_profile(raw: &str) -> Option<ToolProfile> {
    let mut resolved: Option<ToolProfile> = None;

    for token in raw
        .split([',', '+'])
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        let token_profile = match token.to_ascii_lowercase().as_str() {
            "observe" | "read" | "reader" => ToolProfile::observe(),
            "remember" | "write" | "writer" | "agent" => ToolProfile::remember(),
            "standard" | "ide" | "cursor" | "trae" | "windsurf" | "antigravity" | "claude"
            | "claude-code" | "codex" => ToolProfile::standard(),
            "delegate" | "worker" | "subagent" => ToolProfile::delegate(),
            "coordinate" => ToolProfile::coordinate(),
            "companion" | "copilot" | "coach" => ToolProfile::remember()
                .merge(ToolProfile::coordinate())
                .merge(ToolProfile::operate()),
            "workflow" => ToolProfile::coordinate().merge(ToolProfile::operate()),
            "operate" | "runtime" | "openclaw" | "hermes" | "adapter" | "ops" => {
                ToolProfile::operate()
            }
            "admin" | "full" => ToolProfile::admin(),
            _ => return None,
        };
        resolved = Some(match resolved {
            Some(profile) => profile.merge(token_profile),
            None => token_profile,
        });
    }

    resolved
}

pub fn parse_tool_patterns_csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

pub fn filter_tool_defs(
    tools: Vec<Tool>,
    profile: Option<ToolProfile>,
    env_patterns: Option<&[String]>,
) -> Vec<Tool> {
    tools
        .into_iter()
        .filter(|tool| {
            let name = tool.name.as_ref();
            tool_visible(name, profile, env_patterns)
        })
        .collect()
}

pub fn tool_matches_bundle(tool_name: &str, bundle: ToolBundle) -> bool {
    matches_any_pattern(
        tool_name,
        match bundle {
            ToolBundle::Observe => OBSERVE_TOOL_PATTERNS.iter().copied(),
            ToolBundle::Remember => REMEMBER_TOOL_PATTERNS.iter().copied(),
            ToolBundle::Coordinate => COORDINATE_TOOL_PATTERNS.iter().copied(),
            ToolBundle::Operate => OPERATE_TOOL_PATTERNS.iter().copied(),
        },
    )
}

pub fn tool_visible(
    tool_name: &str,
    profile: Option<ToolProfile>,
    env_patterns: Option<&[String]>,
) -> bool {
    if let Some(patterns) = env_patterns {
        if !matches_any_pattern(tool_name, patterns.iter().map(String::as_str)) {
            return false;
        }
    }

    let profile = profile.unwrap_or_else(default_tool_profile);
    if profile.is_admin() {
        return true;
    }

    // Curated minimal allow-lists: standard daily surface > delegate worker surface.
    if profile.uses_standard_allow_list() {
        if !matches_any_pattern(tool_name, STANDARD_MINIMAL_TOOL_PATTERNS.iter().copied()) {
            return false;
        }
    } else if profile.uses_delegate_allow_list()
        && !matches_any_pattern(tool_name, DELEGATE_MINIMAL_TOOL_PATTERNS.iter().copied())
    {
        return false;
    }

    profile.allows(ToolBundle::Observe)
        && matches_any_pattern(tool_name, OBSERVE_TOOL_PATTERNS.iter().copied())
        || profile.allows(ToolBundle::Remember)
            && matches_any_pattern(tool_name, REMEMBER_TOOL_PATTERNS.iter().copied())
        || profile.allows(ToolBundle::Coordinate)
            && matches_any_pattern(tool_name, COORDINATE_TOOL_PATTERNS.iter().copied())
        || profile.allows(ToolBundle::Operate)
            && matches_any_pattern(tool_name, OPERATE_TOOL_PATTERNS.iter().copied())
}

fn matches_any_pattern<'a>(tool_name: &str, mut patterns: impl Iterator<Item = &'a str>) -> bool {
    patterns.any(|pattern| tool_name_matches_pattern(tool_name, pattern))
}

pub fn tool_name_matches_pattern(tool_name: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return tool_name == pattern;
    }

    let anchored_start = !pattern.starts_with('*');
    let anchored_end = !pattern.ends_with('*');
    let segments: Vec<&str> = pattern
        .split('*')
        .filter(|segment| !segment.is_empty())
        .collect();

    if segments.is_empty() {
        return true;
    }

    let mut cursor = 0usize;
    let mut first = true;

    for segment in segments {
        let Some(found_at) = tool_name[cursor..].find(segment) else {
            return false;
        };
        let absolute = cursor + found_at;
        if first && anchored_start && absolute != 0 {
            return false;
        }
        cursor = absolute + segment.len();
        first = false;
    }

    if anchored_end {
        cursor == tool_name.len()
    } else {
        true
    }
}
