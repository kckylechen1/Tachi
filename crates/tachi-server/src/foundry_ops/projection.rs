use crate::tool_params::AgentEvolutionDocumentParams;

use super::synthesis::parse_document_kind;

fn markdown_heading_info(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim();
    if !trimmed.starts_with('#') {
        return None;
    }
    let level = trimmed.chars().take_while(|&ch| ch == '#').count();
    let title = trimmed[level..].trim();
    if level == 0 || title.is_empty() {
        None
    } else {
        Some((level, title.to_string()))
    }
}

pub(super) fn apply_markdown_section_update(
    content: &str,
    section: Option<&str>,
    suggested_value: &str,
) -> String {
    let replacement = suggested_value.trim();
    if replacement.is_empty() {
        return content.to_string();
    }

    let Some(section) = section.map(str::trim).filter(|section| !section.is_empty()) else {
        if content.trim().is_empty() {
            return format!("{replacement}\n");
        }
        return format!("{}\n\n{}\n", content.trim_end(), replacement);
    };

    let lines = content.lines().collect::<Vec<_>>();
    let mut heading_index = None;
    let mut heading_level = None;
    for (idx, line) in lines.iter().enumerate() {
        if markdown_heading_info(line)
            .map(|(level, title)| {
                if title == section {
                    heading_level = Some(level);
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false)
        {
            heading_index = Some(idx);
            break;
        }
    }

    if let Some(start_idx) = heading_index {
        let start_level = heading_level.unwrap_or(2);
        let mut end_idx = lines.len();
        for idx in (start_idx + 1)..lines.len() {
            if let Some((level, _)) = markdown_heading_info(lines[idx]) {
                if level <= start_level {
                    end_idx = idx;
                    break;
                }
            }
        }

        let mut rebuilt = Vec::new();
        rebuilt.extend_from_slice(&lines[..=start_idx]);
        rebuilt.push("");
        rebuilt.extend(replacement.lines());
        if end_idx < lines.len() {
            rebuilt.push("");
            rebuilt.extend_from_slice(&lines[end_idx..]);
        }
        return rebuilt.join("\n").trim_end().to_string() + "\n";
    }

    let mut suffix = String::new();
    if !content.trim().is_empty() {
        suffix.push_str(content.trim_end());
        suffix.push_str("\n\n");
    }
    suffix.push_str(&format!("## {section}\n\n{replacement}\n"));
    suffix
}

fn document_target_aliases(doc: &AgentEvolutionDocumentParams) -> Vec<String> {
    let mut aliases = Vec::new();
    if let Some(path) = &doc.path {
        if let Some(name) = std::path::Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
        {
            aliases.push(name.to_ascii_lowercase());
        }
    }
    let kind_alias = match parse_document_kind(&doc.kind) {
        Ok(memcore::AgentProfileDocumentKind::Identity) => Some("identity"),
        Ok(memcore::AgentProfileDocumentKind::Agents) => Some("agents"),
        Ok(memcore::AgentProfileDocumentKind::LatestTruths) => Some("latest_truths"),
        Ok(memcore::AgentProfileDocumentKind::RoutingPolicy) => Some("routing_policy"),
        Ok(memcore::AgentProfileDocumentKind::ToolPolicy) => Some("tool_policy"),
        Ok(memcore::AgentProfileDocumentKind::MemoryPolicy) => Some("memory_policy"),
        _ => None,
    };
    if let Some(alias) = kind_alias {
        aliases.push(alias.to_string());
        aliases.push(format!("{alias}.md"));
    }
    aliases.sort();
    aliases.dedup();
    aliases
}

pub(super) fn proposal_targets_document(
    proposal: &memcore::AgentEvolutionProposal,
    doc: &AgentEvolutionDocumentParams,
) -> bool {
    let target = proposal.target.trim().to_ascii_lowercase();
    if target.is_empty() {
        return false;
    }
    document_target_aliases(doc)
        .into_iter()
        .any(|alias| alias == target)
}
