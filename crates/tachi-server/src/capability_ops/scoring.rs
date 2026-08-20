use super::types::{CapabilityRecommendation, CapabilityRecord};
use crate::MemoryServer;
use memcore::{HubCapability, MemoryEntry};
use serde_json::Value;
use std::collections::HashSet;
use tachi_hub::{
    capability_callable, capability_visibility_for_cap, sanitize_skill_tool_name,
    CapabilityVisibility,
};

#[derive(Debug, Clone)]
struct PatternSignal {
    pattern_ref: Value,
    projection_key: String,
    tokens: Vec<String>,
}

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

/// Runtime-resolved filesystem locations identify where a capability was
/// loaded, not what it can do. They must not alter recommendation scores.
fn scoring_definition(definition: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<Value>(definition) else {
        return definition.to_string();
    };
    strip_resolved_paths(&mut value);
    value.to_string()
}

fn strip_resolved_paths(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            fields.remove("resolved_path");
            for child in fields.values_mut() {
                strip_resolved_paths(child);
            }
        }
        Value::Array(values) => {
            for child in values {
                strip_resolved_paths(child);
            }
        }
        _ => {}
    }
}

/// Declared-metadata keys a capability author can use to state host
/// affinity. Host bonus matching is scoped to these keys only — never to
/// free-text fields (`content`, `prompt`, …) or runtime-resolved paths.
/// See kckylechen1/tachi#1140 fix direction B: "match `host` against
/// declared metadata (tags/host fields), not raw substring over the whole
/// definition blob." `tags` is the metadata key builtin capabilities
/// already populate (`crates/tachi-server/src/builtins/*.rs`); `host` /
/// `hosts` are included for capabilities that declare host affinity
/// directly.
const HOST_METADATA_KEYS: &[&str] = &["host", "hosts", "tags"];

/// True when `host` appears in one of `cap`'s declared metadata fields
/// (`host`, `hosts`, or `tags`) rather than anywhere in its free-text
/// content. This is the scoped replacement for the old
/// `definition.contains(host)` substring test, which matched runtime
/// filesystem paths and prose alike (#1140).
fn definition_declares_host(definition: &str, host: &str) -> bool {
    let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(definition) else {
        return false;
    };
    // Review checkpoint 1.1 (round-2, codex cross-review of #1166): only the
    // top-level `host`/`hosts`/`tags` keys are consulted here — a nested
    // object under one of those keys is not walked. This is intentional, not
    // an oversight: a value nested arbitrarily deep inside declared metadata
    // is structurally closer to free-text content than to an author's
    // explicit host declaration, so it gets no bonus, matching the same
    // fail-safe default `HOST_METADATA_KEYS` exists to enforce (#1140).
    HOST_METADATA_KEYS
        .iter()
        .filter_map(|key| fields.get(*key))
        .any(|field| host_value_matches(field, host))
}

fn host_value_matches(value: &Value, host: &str) -> bool {
    match value {
        Value::String(text) => text.to_ascii_lowercase().contains(host),
        Value::Array(values) => values.iter().any(|entry| host_value_matches(entry, host)),
        _ => false,
    }
}

fn tokens_from_parts(parts: &[&str]) -> Vec<String> {
    let mut tokens = parts
        .iter()
        .flat_map(|part| tokenize_query(part))
        .collect::<Vec<_>>();
    tokens.sort();
    tokens.dedup();
    tokens
}

fn is_bridge_token(token: &str) -> bool {
    if token.chars().count() < 4 {
        return false;
    }
    const STOPWORDS: &[&str] = &[
        "about",
        "after",
        "agent",
        "alignment",
        "asks",
        "before",
        "bridge",
        "completion",
        "context",
        "durable",
        "project",
        "pattern",
        "record",
        "records",
        "skill",
        "through",
        "workflow",
        "user",
        "when",
        "with",
        "write",
    ];
    !STOPWORDS.contains(&token)
}

fn bridge_tokens_from_parts(parts: &[&str]) -> Vec<String> {
    tokens_from_parts(parts)
        .into_iter()
        .filter(|token| is_bridge_token(token))
        .collect()
}

fn projection_key(entry: &MemoryEntry) -> String {
    entry
        .metadata
        .get("projection_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(entry.id.as_str())
        .to_string()
}

fn pattern_signal_from_entry(entry: &MemoryEntry) -> PatternSignal {
    let key = projection_key(entry);
    let tokens = bridge_tokens_from_parts(&[
        entry.id.as_str(),
        entry.path.as_str(),
        entry.summary.as_str(),
        entry.text.as_str(),
        key.as_str(),
    ]);
    PatternSignal {
        pattern_ref: crate::continuity_ops::pattern_ref_json(entry),
        projection_key: key,
        tokens,
    }
}

fn collect_pattern_signals(server: &MemoryServer, query: &str) -> Vec<PatternSignal> {
    crate::continuity_ops::list_active_patterns(server, None, Some(query), 5)
        .unwrap_or_default()
        .iter()
        .map(pattern_signal_from_entry)
        .filter(|signal| !signal.tokens.is_empty())
        .collect()
}

fn pattern_signal_bonus(
    cap: &HubCapability,
    query_tokens: &[String],
    pattern_signals: &[PatternSignal],
    reasons: &mut Vec<String>,
) -> (f64, Vec<Value>) {
    if pattern_signals.is_empty() {
        return (0.0, Vec::new());
    }
    let definition = scoring_definition(&cap.definition);
    let cap_tokens = bridge_tokens_from_parts(&[
        cap.id.as_str(),
        cap.name.as_str(),
        cap.description.as_str(),
        definition.as_str(),
    ]);
    if cap_tokens.is_empty() {
        return (0.0, Vec::new());
    }

    let mut bonus = 0.0;
    let mut refs = Vec::new();
    for signal in pattern_signals {
        let query_overlap = token_overlap_ratio(query_tokens, &signal.tokens);
        let cap_overlap = token_overlap_ratio(&cap_tokens, &signal.tokens);
        if query_overlap <= 0.0 || cap_overlap <= 0.0 {
            continue;
        }
        let signal_bonus = (query_overlap * 4.0 + cap_overlap * 6.0).min(2.0);
        if signal_bonus <= 0.0 {
            continue;
        }
        bonus += signal_bonus;
        refs.push(signal.pattern_ref.clone());
        reasons.push(format!(
            "active pattern '{}' bridges query to skill",
            signal.projection_key
        ));
    }
    (round3(bonus.min(3.0)), refs)
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
    pattern_signals: &[PatternSignal],
) -> Option<(f64, Vec<String>, Vec<Value>)> {
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
    let definition = scoring_definition(&cap.definition).to_ascii_lowercase();
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

    let (pattern_bonus, pattern_refs) =
        pattern_signal_bonus(cap, &query_tokens, pattern_signals, &mut reasons);
    if pattern_bonus > 0.0 {
        score += pattern_bonus;
        matched = true;
    }

    if !matched {
        return None;
    }

    if let Some(host) = host {
        if id.contains(host)
            || name.contains(host)
            || desc.contains(host)
            || definition_declares_host(&cap.definition, host)
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
        Some((round3(score), dedup_strings(reasons), pattern_refs))
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
            if crate::builtins::is_retired_builtin_capability_id(&cap.id) {
                continue;
            }
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

pub(crate) fn recommend_capabilities_inner(
    server: &MemoryServer,
    query: &str,
    host: Option<&str>,
    cap_type: Option<&str>,
    limit: usize,
    include_hidden: bool,
    include_uncallable: bool,
) -> Result<Vec<CapabilityRecommendation>, String> {
    let host = normalize_host_label(host);
    let pattern_signals = collect_pattern_signals(server, query);
    let mut ranked = collect_capabilities(server, cap_type, include_hidden, include_uncallable)?
        .into_iter()
        .filter_map(|record| {
            let (score, reasons, pattern_refs) = capability_score(
                &record.cap,
                record.visibility,
                record.callable,
                query,
                host.as_deref(),
                &pattern_signals,
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
                pattern_refs,
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

#[cfg(test)]
mod pattern_signal_tests {
    use super::*;

    fn skill(id: &str, definition: Value) -> HubCapability {
        HubCapability {
            id: id.to_string(),
            cap_type: "skill".to_string(),
            name: "neutral".to_string(),
            version: 1,
            description: "neutral description".to_string(),
            definition: definition.to_string(),
            enabled: true,
            review_status: "approved".to_string(),
            health_status: "healthy".to_string(),
            last_error: None,
            last_success_at: None,
            last_failure_at: None,
            fail_streak: 0,
            active_version: None,
            exposure_mode: "direct".to_string(),
            uses: 0,
            successes: 0,
            failures: 0,
            avg_rating: 0.0,
            last_used: None,
            created_at: "2026-07-16T00:00:00Z".to_string(),
            updated_at: "2026-07-16T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn pattern_bonus_ignores_resolved_path_tokens() {
        let alpha = skill(
            "skill:alpha",
            serde_json::json!({
                "content": "neutral",
                "resolved_path": "/work/sigil/skills/fixture/SKILL.md"
            }),
        );
        let zeta = skill(
            "skill:zeta",
            serde_json::json!({
                "content": "neutral",
                "resolved_path": "/work/codex-issue/skills/fixture/SKILL.md"
            }),
        );
        let signals = [PatternSignal {
            pattern_ref: serde_json::json!({"projection_key": "path-bridge"}),
            projection_key: "path-bridge".to_string(),
            tokens: vec!["query".to_string(), "codex".to_string()],
        }];
        let query_tokens = vec!["query".to_string()];
        let bonus = |cap: &HubCapability| {
            let mut reasons = Vec::new();
            pattern_signal_bonus(cap, &query_tokens, &signals, &mut reasons).0
        };

        assert_eq!(
            bonus(&alpha),
            bonus(&zeta),
            "an active pattern must not bridge through a runtime-resolved path"
        );
    }
}
