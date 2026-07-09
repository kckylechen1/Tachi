use super::scoring::{dedup_strings, normalize_host_label, round3, tokenize_query};
use super::types::PackRecommendation;
use crate::MemoryServer;
use memcore::{AgentProjection, Pack};
use std::collections::HashMap;

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

pub(super) fn recommend_packs_inner(
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
