use super::*;
use crate::hub_helpers::sanitize_skill_tool_name;
use memory_core::{AgentProjection, HubCapability, Pack};
use serde::Serialize;
use serde_json::json;

#[derive(Debug, Clone)]
struct CapabilityRecord {
    cap: HubCapability,
    db: &'static str,
    visibility: CapabilityVisibility,
    callable: bool,
}

#[derive(Debug, Clone, Serialize)]
struct CapabilityRecommendation {
    id: String,
    cap_type: String,
    name: String,
    description: String,
    db: String,
    visibility: String,
    callable: bool,
    score: f64,
    reasons: Vec<String>,
    uses: u64,
    avg_rating: f64,
    suggested_tool_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct PackRecommendation {
    id: String,
    name: String,
    description: String,
    version: String,
    projected_to_host: bool,
    projected_path: Option<String>,
    score: f64,
    reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct CapabilityBundleSection {
    title: String,
    estimated_tokens: usize,
    block: String,
}

#[derive(Debug, Clone, Serialize)]
struct CapabilityBundle {
    primary_skill: Option<CapabilityRecommendation>,
    supporting_capabilities: Vec<CapabilityRecommendation>,
    packs: Vec<PackRecommendation>,
    host_tools: Vec<String>,
    activation_steps: Vec<String>,
    rationale: Vec<String>,
    section: Option<CapabilityBundleSection>,
}

fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn dedup_strings(values: Vec<String>) -> Vec<String> {
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

fn normalize_host_label(raw: Option<&str>) -> Option<String> {
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
        "opencode" => "opencode",
        "gemini" => "gemini",
        "trae" => "trae",
        "antigravity" => "antigravity",
        "kiro" => "kiro",
        other => other,
    };
    Some(normalized.to_string())
}

fn tokenize_query(query: &str) -> Vec<String> {
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

fn token_overlap_ratio(query_tokens: &[String], candidate_tokens: &[String]) -> f64 {
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

fn recommend_capabilities_inner(
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

fn collect_enabled_packs(server: &MemoryServer) -> Result<Vec<Pack>, String> {
    server.with_global_store_read(|store| {
        store.pack_list(true).map_err(|e| format!("pack_list: {e}"))
    })
}

fn collect_projections_for_host(
    server: &MemoryServer,
    host: Option<&str>,
) -> Result<Vec<AgentProjection>, String> {
    server.with_global_store_read(|store| {
        store
            .projection_list(host, None)
            .map_err(|e| format!("projection_list: {e}"))
    })
}

fn pack_score(
    pack: &Pack,
    query: &str,
    projected: Option<&AgentProjection>,
    host: Option<&str>,
) -> Option<(f64, Vec<String>)> {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return None;
    }
    let tokens = tokenize_query(&query);
    let haystack = format!(
        "{} {} {} {}",
        pack.id.to_ascii_lowercase(),
        pack.name.to_ascii_lowercase(),
        pack.description.to_ascii_lowercase(),
        pack.metadata.to_ascii_lowercase()
    );

    let mut score = 0.0;
    let mut reasons = Vec::new();

    if haystack.contains(&query) {
        score += 6.0;
        reasons.push("pack metadata matches query".to_string());
    }
    for token in &tokens {
        if haystack.contains(token) {
            score += 1.5;
        }
    }
    if let Some(projection) = projected {
        score += 2.0;
        reasons.push(format!("already projected to {}", projection.agent));
    } else if let Some(host) = host {
        if pack.metadata.to_ascii_lowercase().contains(host) {
            score += 0.8;
            reasons.push(format!("metadata mentions host '{}'", host));
        }
    }

    if score <= 0.0 {
        None
    } else {
        Some((round3(score), dedup_strings(reasons)))
    }
}

fn recommend_packs_inner(
    server: &MemoryServer,
    query: &str,
    host: Option<&str>,
    limit: usize,
) -> Result<Vec<PackRecommendation>, String> {
    let host = normalize_host_label(host);
    let packs = collect_enabled_packs(server)?;
    let projections = collect_projections_for_host(server, host.as_deref())?;
    let by_pack = projections
        .into_iter()
        .map(|projection| (projection.pack_id.clone(), projection))
        .collect::<HashMap<_, _>>();

    let mut ranked = packs
        .into_iter()
        .filter_map(|pack| {
            let projection = by_pack.get(&pack.id);
            let (score, reasons) = pack_score(&pack, query, projection, host.as_deref())?;
            Some(PackRecommendation {
                id: pack.id.clone(),
                name: pack.name.clone(),
                description: pack.description.clone(),
                version: pack.version.clone(),
                projected_to_host: projection.is_some(),
                projected_path: projection.map(|p| p.projected_path.clone()),
                score,
                reasons,
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

fn infer_host_tools(query: &str) -> Vec<String> {
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

fn build_bundle_section(
    query: &str,
    primary_skill: Option<&CapabilityRecommendation>,
    capabilities: &[CapabilityRecommendation],
    packs: &[PackRecommendation],
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
    if !packs.is_empty() {
        lines.push(format!(
            "Relevant packs: {}",
            packs
                .iter()
                .map(|pack| pack.id.clone())
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

pub(crate) async fn handle_recommend_capability(
    server: &MemoryServer,
    params: RecommendCapabilityParams,
) -> Result<String, String> {
    let results = recommend_capabilities_inner(
        server,
        &params.query,
        params.host.as_deref(),
        params.cap_type.as_deref(),
        params.limit.max(1),
        params.include_hidden,
        params.include_uncallable,
    )?;

    serde_json::to_string(&json!({
        "query": params.query,
        "host": normalize_host_label(params.host.as_deref()),
        "recommendations": results,
        "count": results.len(),
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_recommend_skill(
    server: &MemoryServer,
    params: RecommendSkillParams,
) -> Result<String, String> {
    let results = recommend_capabilities_inner(
        server,
        &params.query,
        params.host.as_deref(),
        Some("skill"),
        params.limit.max(1),
        false,
        params.include_uncallable,
    )?;

    serde_json::to_string(&json!({
        "query": params.query,
        "host": normalize_host_label(params.host.as_deref()),
        "skills": results,
        "count": results.len(),
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_recommend_toolchain(
    server: &MemoryServer,
    params: RecommendToolchainParams,
) -> Result<String, String> {
    let host = normalize_host_label(params.host.as_deref());
    let skills = recommend_capabilities_inner(
        server,
        &params.query,
        host.as_deref(),
        Some("skill"),
        params.skill_limit.max(1),
        false,
        false,
    )?;
    let capabilities = recommend_capabilities_inner(
        server,
        &params.query,
        host.as_deref(),
        None,
        params.capability_limit.max(1),
        false,
        false,
    )?
    .into_iter()
    .filter(|rec| rec.cap_type != "skill")
    .take(params.capability_limit.max(1))
    .collect::<Vec<_>>();
    let packs = recommend_packs_inner(server, &params.query, host.as_deref(), params.pack_limit)?;
    let host_tools = infer_host_tools(&params.query);

    let mut rationale = Vec::new();
    if let Some(top) = skills.first() {
        rationale.push(format!("Top skill match: {}", top.id));
    }
    if let Some(top) = capabilities.first() {
        rationale.push(format!("Supporting capability: {}", top.id));
    }
    if let Some(top) = packs.first() {
        rationale.push(format!("Relevant pack: {}", top.id));
    }
    if !host_tools.is_empty() {
        rationale.push(format!("Suggested host tools: {}", host_tools.join(", ")));
    }

    serde_json::to_string(&json!({
        "query": params.query,
        "host": host,
        "skills": skills,
        "capabilities": capabilities,
        "packs": packs,
        "host_tools": host_tools,
        "rationale": rationale,
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_prepare_capability_bundle(
    server: &MemoryServer,
    params: PrepareCapabilityBundleParams,
) -> Result<String, String> {
    let host = normalize_host_label(params.host.as_deref());
    let skills = recommend_capabilities_inner(
        server,
        &params.query,
        host.as_deref(),
        Some("skill"),
        params.skill_limit.max(1),
        false,
        false,
    )?;
    let primary_skill = skills.first().cloned();
    let supporting_capabilities = recommend_capabilities_inner(
        server,
        &params.query,
        host.as_deref(),
        None,
        params.capability_limit.max(1) + 1,
        false,
        false,
    )?
    .into_iter()
    .filter(|rec| rec.cap_type != "skill")
    .take(params.capability_limit.max(1))
    .collect::<Vec<_>>();
    let packs = recommend_packs_inner(server, &params.query, host.as_deref(), params.pack_limit)?;
    let host_tools = infer_host_tools(&params.query);

    let mut activation_steps = Vec::new();
    if let Some(skill) = primary_skill.as_ref() {
        if let Some(tool_name) = skill.suggested_tool_name.as_ref() {
            activation_steps.push(format!(
                "Load or call {tool_name} as the primary skill path."
            ));
        } else {
            activation_steps.push(format!("Start with skill {}.", skill.id));
        }
    }
    if let Some(pack) = packs.first() {
        if pack.projected_to_host {
            activation_steps.push(format!(
                "Use projected pack {} at {}.",
                pack.id,
                pack.projected_path
                    .clone()
                    .unwrap_or_else(|| "(projected path unknown)".to_string())
            ));
        } else {
            activation_steps.push(format!(
                "Project or activate pack {} before running the task.",
                pack.id
            ));
        }
    }
    if !host_tools.is_empty() {
        activation_steps.push(format!(
            "Grant or prepare host tools: {}.",
            host_tools.join(", ")
        ));
    }
    for capability in supporting_capabilities.iter().take(2) {
        activation_steps.push(format!(
            "Keep {} available as a supporting capability.",
            capability.id
        ));
    }

    let mut rationale = Vec::new();
    if let Some(skill) = primary_skill.as_ref() {
        rationale.push(format!("Primary skill match: {}", skill.id));
    }
    if let Some(pack) = packs.first() {
        rationale.push(format!("Best pack candidate: {}", pack.id));
    }
    if !host_tools.is_empty() {
        rationale.push(format!("Host tool fit: {}", host_tools.join(", ")));
    }
    rationale.extend(
        supporting_capabilities
            .iter()
            .take(2)
            .map(|cap| format!("Supporting capability: {}", cap.id)),
    );

    let section = if params.include_section {
        Some(build_bundle_section(
            &params.query,
            primary_skill.as_ref(),
            &supporting_capabilities,
            &packs,
            &host_tools,
            &activation_steps,
        ))
    } else {
        None
    };

    let bundle = CapabilityBundle {
        primary_skill,
        supporting_capabilities,
        packs,
        host_tools,
        activation_steps,
        rationale,
        section,
    };

    serde_json::to_string(&json!({
        "query": params.query,
        "host": host,
        "bundle": bundle,
    }))
    .map_err(|e| format!("serialize: {e}"))
}
