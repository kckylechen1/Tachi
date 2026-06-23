use super::types::{CapabilityRecommendation, CapabilityRecord};
use crate::hub_helpers::{
    capability_callable, capability_visibility_for_cap, sanitize_skill_tool_name,
    CapabilityVisibility,
};
use crate::MemoryServer;
use memory_core::HubCapability;
use std::collections::HashSet;

pub(super) fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

pub(super) fn dedup_strings(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        let key = trimmed.to_ascii_lowercase();
        if seen.insert(key) {
            out.push(trimmed.to_string());
        }
    }
    out
}

pub(super) fn normalize_host_label(raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    let binding = raw.to_ascii_lowercase();
    let normalized = match binding.as_str() {
        "claude-code" | "claude" => "claude",
        "cursor" => "cursor",
        "codex" => "codex",
        "openclaw" => "openclaw",
        "gemini" => "gemini",
        "trae" => "trae",
        "antigravity" => "antigravity",
        "kiro" => "kiro",
        other => other,
    };
    Some(normalized.to_string())
}

pub(super) fn tokenize_query(query: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in query.chars() {
        if ch.is_ascii_alphanumeric() {
            current.push(ch.to_ascii_lowercase());
        } else if !current.is_empty() {
            if current.len() >= 2 {
                tokens.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }
    if !current.is_empty() && current.len() >= 2 {
        tokens.push(current);
    }
    tokens.sort();
    tokens.dedup();
    tokens
}

pub(super) fn token_overlap_ratio(query_tokens: &[String], candidate_tokens: &[String]) -> f64 {
    if query_tokens.is_empty() || candidate_tokens.is_empty() {
        return 0.0;
    }

    let query = query_tokens
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let candidate = candidate_tokens
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let intersection = query.intersection(&candidate).count();
    if intersection == 0 {
        return 0.0;
    }
    let union = query.union(&candidate).count();
    if union == 0 {
        return 0.0;
    }
    intersection as f64 / union as f64
}

fn telemetry_bonus(cap: &HubCapability, reasons: &mut Vec<String>) -> f64 {
    let mut bonus = 0.0;
    if cap.uses > 0 {
        bonus += ((cap.uses as f64 + 1.0).ln()).min(2.0) * 0.2;
        reasons.push("existing usage telemetry".to_string());
    }
    if cap.uses > 0 && cap.successes > 0 {
        let success_rate = cap.successes as f64 / cap.uses as f64;
        if success_rate >= 0.75 {
            bonus += (success_rate - 0.5).min(0.4);
            reasons.push(format!("success rate {:.0}%", success_rate * 100.0));
        }
    }
    if cap.avg_rating > 0.0 {
        bonus += (cap.avg_rating / 5.0) * 0.4;
        reasons.push(format!("rating {:.1}/5", cap.avg_rating));
    }
    bonus
}

fn capability_score(
    cap: &HubCapability,
    visibility: CapabilityVisibility,
    callable: bool,
    query: &str,
    host: Option<&str>,
) -> Option<(f64, Vec<String>)> {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return None;
    }
    let query_tokens = tokenize_query(&query);
    if query_tokens.is_empty() {
        return None;
    }
    let id = cap.id.to_ascii_lowercase();
    let name = cap.name.to_ascii_lowercase();
    let desc = cap.description.to_ascii_lowercase();
    let definition = cap.definition.to_ascii_lowercase();
    let id_tokens = tokenize_query(&id);
    let name_tokens = tokenize_query(&name);
    let desc_tokens = tokenize_query(&desc);
    let definition_tokens = tokenize_query(&definition);

    let mut score = 0.0;
    let mut matched = false;
    let mut reasons = Vec::new();

    if id == query || name == query {
        score += 10.0;
        matched = true;
        reasons.push("exact id/name match".to_string());
    } else {
        if name.contains(&query) {
            score += 4.0;
            matched = true;
            reasons.push("name matches query".to_string());
        }
        if id.contains(&query) {
            score += 3.5;
            matched = true;
            reasons.push("id matches query".to_string());
        }
        if desc.contains(&query) {
            score += 3.0;
            matched = true;
            reasons.push("description matches query".to_string());
        }
    }

    let name_overlap = token_overlap_ratio(&query_tokens, &name_tokens);
    let id_overlap = token_overlap_ratio(&query_tokens, &id_tokens);
    let desc_overlap = token_overlap_ratio(&query_tokens, &desc_tokens);
    let definition_overlap = token_overlap_ratio(&query_tokens, &definition_tokens);

    if name_overlap > 0.0 {
        score += name_overlap * 10.0;
        matched = true;
        reasons.push(format!("name token overlap {:.3}", name_overlap));
    }
    if id_overlap > 0.0 {
        score += id_overlap * 8.0;
        matched = true;
        reasons.push(format!("id token overlap {:.3}", id_overlap));
    }
    if desc_overlap > 0.0 {
        score += desc_overlap * 12.0;
        matched = true;
        reasons.push(format!("description token overlap {:.3}", desc_overlap));
    }
    if definition_overlap > 0.0 {
        score += definition_overlap * 2.5;
        matched = true;
        reasons.push(format!(
            "definition token overlap {:.3}",
            definition_overlap
        ));
    }

    if !matched {
        return None;
    }

    if let Some(host) = host {
        if id.contains(host)
            || name.contains(host)
            || desc.contains(host)
            || definition.contains(host)
        {
            score += 1.2;
            reasons.push(format!("mentions host '{}'", host));
        }
        if cap.cap_type.eq_ignore_ascii_case("skill") {
            score += 0.2;
        }
    }

    score += match visibility {
        CapabilityVisibility::Listed => 0.3,
        CapabilityVisibility::Discoverable => 0.15,
        CapabilityVisibility::Hidden => -0.25,
    };
    if callable {
        score += 0.35;
    }

    score += telemetry_bonus(cap, &mut reasons);

    if score <= 0.0 {
        None
    } else {
        Some((round3(score), dedup_strings(reasons)))
    }
}

fn collect_capabilities(
    server: &MemoryServer,
    cap_type: Option<&str>,
    include_hidden: bool,
    include_uncallable: bool,
) -> Result<Vec<CapabilityRecord>, String> {
    let global_caps = server.with_global_store_read(|store| {
        store
            .hub_list(cap_type, true)
            .map_err(|e| format!("hub list global: {e}"))
    })?;
    let project_caps = if server.has_project_db() {
        server.with_project_store_read(|store| {
            store
                .hub_list(cap_type, true)
                .map_err(|e| format!("hub list project: {e}"))
        })?
    } else {
        Vec::new()
    };

    let mut out = Vec::new();
    let mut seen = HashSet::<String>::new();

    for (db, caps) in [("project", project_caps), ("global", global_caps)] {
        for cap in caps {
            if !seen.insert(cap.id.clone()) {
                continue;
            }
            let visibility = capability_visibility_for_cap(&cap);
            let callable = capability_callable(&cap);
            if !include_hidden && visibility == CapabilityVisibility::Hidden {
                continue;
            }
            if !include_uncallable && !callable {
                continue;
            }
            out.push(CapabilityRecord {
                cap,
                db,
                visibility,
                callable,
            });
        }
    }

    Ok(out)
}

pub(super) fn recommend_capabilities_inner(
    server: &MemoryServer,
    query: &str,
    host: Option<&str>,
    cap_type: Option<&str>,
    limit: usize,
    include_hidden: bool,
    include_uncallable: bool,
) -> Result<Vec<CapabilityRecommendation>, String> {
    let host = normalize_host_label(host);
    let mut ranked = collect_capabilities(server, cap_type, include_hidden, include_uncallable)?
        .into_iter()
        .filter_map(|record| {
            let (score, reasons) = capability_score(
                &record.cap,
                record.visibility,
                record.callable,
                query,
                host.as_deref(),
            )?;
            Some(CapabilityRecommendation {
                id: record.cap.id.clone(),
                cap_type: record.cap.cap_type.clone(),
                name: record.cap.name.clone(),
                description: record.cap.description.clone(),
                db: record.db.to_string(),
                visibility: record.visibility.as_str().to_string(),
                callable: record.callable,
                score,
                reasons,
                uses: record.cap.uses,
                avg_rating: round3(record.cap.avg_rating),
                suggested_tool_name: if record.cap.cap_type.eq_ignore_ascii_case("skill") {
                    sanitize_skill_tool_name(&record.cap.id)
                } else {
                    None
                },
            })
        })
        .collect::<Vec<_>>();

    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
    ranked.truncate(limit.max(1));
    Ok(ranked)
}
