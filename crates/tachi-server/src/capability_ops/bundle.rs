use super::types::{CapabilityBundleSection, CapabilityRecommendation};

pub(super) fn infer_host_tools(query: &str) -> Vec<String> {
    let query = query.to_ascii_lowercase();
    let mut tools = Vec::new();

    let push = |tools: &mut Vec<String>, name: &str| {
        if !tools.iter().any(|existing| existing == name) {
            tools.push(name.to_string());
        }
    };

    if ["excel", "spreadsheet", "csv", "sheet", "table"]
        .iter()
        .any(|needle| query.contains(needle))
    {
        push(&mut tools, "python");
        push(&mut tools, "filesystem");
    }
    if ["browser", "scrape", "crawl", "website", "web"]
        .iter()
        .any(|needle| query.contains(needle))
    {
        push(&mut tools, "browser");
        push(&mut tools, "filesystem");
    }
    if ["code", "test", "refactor", "build", "debug"]
        .iter()
        .any(|needle| query.contains(needle))
    {
        push(&mut tools, "filesystem");
        push(&mut tools, "shell");
    }
    if ["image", "screenshot", "vision", "pdf"]
        .iter()
        .any(|needle| query.contains(needle))
    {
        push(&mut tools, "browser");
        push(&mut tools, "filesystem");
    }
    if tools.is_empty() {
        push(&mut tools, "filesystem");
    }

    tools
}

fn estimate_tokens(text: &str) -> usize {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        0
    } else {
        trimmed.chars().count().div_ceil(4)
    }
}

pub(super) fn build_bundle_section(
    query: &str,
    primary_skill: Option<&CapabilityRecommendation>,
    capabilities: &[CapabilityRecommendation],
    host_tools: &[String],
    activation_steps: &[String],
) -> CapabilityBundleSection {
    let mut lines = vec![
        "<!-- tachi:section kind=capability_bundle layer=live cache_boundary=turn -->".to_string(),
        "## Capability Bundle".to_string(),
        String::new(),
        format!("Task: {}", query.trim()),
    ];

    if let Some(skill) = primary_skill {
        lines.push(format!(
            "Primary skill: {}{}",
            skill.id,
            skill
                .suggested_tool_name
                .as_ref()
                .map(|name| format!(" ({name})"))
                .unwrap_or_default()
        ));
    }
    if !capabilities.is_empty() {
        lines.push(format!(
            "Supporting capabilities: {}",
            capabilities
                .iter()
                .map(|cap| cap.id.clone())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !host_tools.is_empty() {
        lines.push(format!("Suggested host tools: {}", host_tools.join(", ")));
    }
    if !activation_steps.is_empty() {
        lines.push(String::new());
        for step in activation_steps {
            lines.push(format!("- {step}"));
        }
    }
    lines.push("<!-- /tachi:section -->".to_string());

    let block = lines.join("\n");
    CapabilityBundleSection {
        title: "Capability Bundle".to_string(),
        estimated_tokens: estimate_tokens(&block),
        block,
    }
}
