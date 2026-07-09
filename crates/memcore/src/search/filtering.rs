use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};

use crate::types::{MemoryEntry, SearchResult};

pub(super) fn env_truthy(key: &str) -> bool {
    matches!(
        std::env::var(key).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("on")
    )
}

pub(super) fn scoped_path_can_surface_superseded(path_prefix: Option<&str>) -> bool {
    let Some(prefix) = path_prefix
        .map(str::trim)
        .filter(|prefix| !prefix.is_empty())
    else {
        return false;
    };
    if prefix == "/"
        || prefix == "/wiki"
        || prefix.starts_with("/wiki/")
        || prefix == "/kanban"
        || prefix.starts_with("/kanban/")
    {
        return false;
    }
    prefix.trim_matches('/').split('/').count() >= 3
}

fn parse_utc_timestamp(ts: &str) -> Option<DateTime<Utc>> {
    let raw = ts.trim();
    if raw.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|_| raw.parse::<DateTime<Utc>>())
        .ok()
}

pub(super) fn valid_at(entry: &MemoryEntry, as_of: Option<&str>) -> bool {
    let Some(as_of) = as_of else {
        return true;
    };
    let valid_from = if entry.valid_from.trim().is_empty() {
        entry.timestamp.as_str()
    } else {
        entry.valid_from.as_str()
    };
    let Some(as_of_dt) = parse_utc_timestamp(as_of) else {
        return false;
    };

    let starts_before_as_of = parse_utc_timestamp(valid_from)
        .map(|valid_from_dt| valid_from_dt <= as_of_dt)
        .unwrap_or_else(|| valid_from <= as_of);
    let ends_after_as_of = entry
        .valid_until
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|until| {
            parse_utc_timestamp(until)
                .map(|until_dt| until_dt > as_of_dt)
                .unwrap_or_else(|| until > as_of)
        })
        .unwrap_or(true);

    starts_before_as_of && ends_after_as_of
}

pub(super) fn newest_by_shared_entity(entries: &HashMap<String, &MemoryEntry>) -> HashSet<String> {
    let mut by_entity: HashMap<&str, Vec<&MemoryEntry>> = HashMap::new();
    for entry in entries.values() {
        for entity in &entry.entities {
            let entity = entity.trim();
            if !entity.is_empty() {
                by_entity.entry(entity).or_default().push(*entry);
            }
        }
    }

    by_entity
        .into_values()
        .filter(|items| items.len() > 1)
        .filter_map(|items| {
            items
                .into_iter()
                // Pick the newest by parsed instant, tie-broken by id, so an
                // exact-time tie boosts the same target run to run (tachi#718);
                // HashMap order fed this before, and a lexical timestamp compare
                // would mis-order mixed formats (CP2). `max_by_key` evaluates the
                // key once per element — no per-comparison parse.
                .max_by_key(|entry| {
                    (
                        crate::scorer::timestamp_epoch_millis(&entry.timestamp),
                        entry.id.clone(),
                    )
                })
                .map(|entry| entry.id.clone())
        })
        .collect()
}

fn metadata_bool(entry: &MemoryEntry, key: &str) -> bool {
    entry
        .metadata
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn is_sft_training_entry(entry: &MemoryEntry) -> bool {
    metadata_bool(entry, "training_sample")
        || entry.path.starts_with("/sft/")
        || entry.topic.eq_ignore_ascii_case("sft-memory")
}

fn is_openclaw_low_signal_entry(entry: &MemoryEntry) -> bool {
    entry.path == "/openclaw/legacy"
        || entry.path.contains("/unnamed")
        || entry.topic.trim().is_empty() && entry.path.starts_with("/openclaw/")
}

pub(super) fn is_search_noise_entry(entry: &MemoryEntry, path_prefix: Option<&str>) -> bool {
    crate::namespace::is_namespace_search_noise(entry, path_prefix)
}

/// Path-scoped quality multipliers (tachi#708 Phase D / ops-audit same-store).
///
/// Wiki ×1.15 and guide ×1.12 apply only when the search is scoped to that
/// bucket via `path_prefix`. Unscoped mixed search (`path_prefix = None`) keeps
/// them at 1.0 so older wiki/roadmap pages do not systematically bury fresh
/// project decisions (ops-audit rank-dilution / adjacent-wiki shapes).
pub(super) fn quality_multiplier(
    entry: &MemoryEntry,
    path_prefix: Option<&str>,
) -> f64 {
    let wiki_scoped = path_prefix.is_some_and(|p| p == "/wiki" || p.starts_with("/wiki/"));
    let guide_scoped = path_prefix.is_some_and(|p| p == "/guide" || p.starts_with("/guide/"));

    let base = if is_sft_training_entry(entry) {
        0.45
    } else if is_openclaw_low_signal_entry(entry) {
        0.55
    } else if entry.is_foundry_distill() {
        0.75
    } else if entry.is_wiki() {
        if wiki_scoped {
            1.15
        } else {
            1.0
        }
    } else if entry.is_guide() {
        if guide_scoped {
            1.12
        } else {
            1.0
        }
    } else if entry.is_kanban() || entry.is_handoff() {
        0.65
    } else {
        1.0
    };
    // High-importance entries get a floor of 1.0 so they aren't suppressed,
    // but foundry_distill and SFT training examples stay penalized regardless
    // of importance. Training examples are useful references when explicitly
    // scoped, but they should not crowd out distilled operational memory.
    if entry.importance >= 0.9
        && base < 1.0
        && !entry.is_foundry_distill()
        && !is_sft_training_entry(entry)
        && !is_openclaw_low_signal_entry(entry)
    {
        1.0
    } else {
        base
    }
}

pub(super) fn normalized_seed_weights(results: &[SearchResult]) -> HashMap<String, f64> {
    let max_score = results
        .iter()
        .map(|result| result.score.final_score)
        .filter(|score| score.is_finite() && *score > 0.0)
        .fold(0.0_f64, f64::max);

    results
        .iter()
        .map(|result| {
            let weight = if max_score > 0.0 {
                result.score.final_score / max_score
            } else {
                1.0
            };
            (result.entry.id.clone(), weight.clamp(0.05, 1.0))
        })
        .collect()
}
