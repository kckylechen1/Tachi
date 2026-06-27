use std::collections::HashSet;

use crate::tool_params::{TachiProfileDocumentParams, TachiProfileParams};
use memory_core::{AgentProfileIdentity, AgentProfilePack, AgentProfileRule, AgentProfileSource};
use serde_json::{json, Value};

const DEFAULT_AGENT_ID: &str = "default";

pub(super) fn resolve_pack(params: &TachiProfileParams) -> Result<AgentProfilePack, String> {
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

pub(super) fn identity_name(pack: &AgentProfilePack) -> String {
    pack.identity
        .name
        .clone()
        .or_else(|| pack.display_name.clone())
        .unwrap_or_else(|| pack.agent_id.clone())
}

pub(super) fn pack_summary(pack: &AgentProfilePack) -> Value {
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
