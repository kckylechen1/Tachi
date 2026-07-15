use std::collections::HashSet;

use super::cache::recall_cache_recall_opted_in;
use super::exact::{annotate_exact_token_matches, mark_exact_token_match};
use super::filters::{
    eval_recall_opted_in, is_eval_entry, project_filter_name,
    project_scope_allows_memory_with_config, training_recall_opted_in,
};
use super::store::{with_global_search, with_named_project_search, with_project_search};
use crate::memory_search_ops::auto_link::is_training_seed;
use crate::memory_search_ops::search_helpers::{
    apply_guide_context_boosts, dedup_search_results, infer_search_project,
    named_project_db_exists, normalize_search_relevance,
};
use crate::shared_defs::{slim_l0_rule, slim_search_result};
use crate::tool_params::SearchMemoryParams;
use crate::utils::{is_active_global_rule, parse_env_bool};
use crate::{DbScope, MemoryServer};
use serde_json::json;

const MAX_CONTEXT_SYMBOLS: usize = 32;

fn sandbox_role(params: &SearchMemoryParams) -> Option<&str> {
    params
        .agent_role
        .as_deref()
        .map(str::trim)
        .filter(|role| !role.is_empty())
}

/// Fetch sandbox rules for `role` once, so the caller can evaluate many paths against them
/// in memory instead of issuing a DB query per result row (N+1 → 1).
fn load_sandbox_rules(server: &MemoryServer, role: &str) -> Result<Vec<(String, String)>, String> {
    server.with_global_store_read(|store| {
        store
            .list_sandbox_rules_for_role(role)
            .map_err(|e| format!("sandbox rules load failed for role '{role}': {e}"))
    })
}

fn recall_quality_from_store(store: &memcore::MemoryStore) -> Option<serde_json::Value> {
    let vector = crate::status_ops::vector_health(store.connection()).ok()?;
    if vector.total == 0 || vector.coverage >= 0.9 {
        return None;
    }
    Some(json!({
        "status": "degraded",
        "reason": "vector coverage below 90%; lexical/FTS recall still runs, but semantic recall may miss relevant rows",
        "vector_coverage": (vector.coverage * 1000.0).round() / 1000.0,
        "vector_missing": vector.missing,
        "pending_enrichment": vector.pending_enrichment,
        "memory_total": vector.total,
    }))
}

fn recall_quality_for_global(server: &MemoryServer) -> Option<serde_json::Value> {
    server
        .with_global_store_read(|store| Ok(recall_quality_from_store(store)))
        .ok()
        .flatten()
}

fn recall_quality_for_project(
    server: &MemoryServer,
    params: &SearchMemoryParams,
) -> Option<serde_json::Value> {
    if let Some(project_name) = params.project.as_deref() {
        return server
            .with_named_project_store_read(project_name, |store| {
                Ok(recall_quality_from_store(store))
            })
            .ok()
            .flatten();
    }
    if !server.has_project_db() {
        return None;
    }
    server
        .with_project_store_read(|store| Ok(recall_quality_from_store(store)))
        .ok()
        .flatten()
}

pub(super) fn query_with_context_symbols(query: &str, context_symbols: &[String]) -> String {
    if context_symbols.is_empty() || memcore::scorer::is_id_like_exact_query(query) {
        return query.to_string();
    }

    let query_trimmed = query.trim();
    let query_lower = query_trimmed.to_lowercase();
    let mut seen = HashSet::new();
    let mut missing = Vec::new();

    for raw in context_symbols {
        let symbol = raw.trim();
        if symbol.is_empty() {
            continue;
        }
        let key = symbol.to_lowercase();
        if !seen.insert(key.clone()) || query_lower.contains(&key) {
            continue;
        }
        missing.push(symbol.to_string());
        if missing.len() >= MAX_CONTEXT_SYMBOLS {
            break;
        }
    }

    if missing.is_empty() {
        return query.to_string();
    }
    let prefix = missing.join(" ");
    if query_trimmed.is_empty() {
        prefix
    } else {
        format!("{prefix} {query_trimmed}")
    }
}

pub(crate) async fn search_memory_rows(
    server: &MemoryServer,
    params: SearchMemoryParams,
    project_only: bool,
) -> Result<Vec<serde_json::Value>, String> {
    search_memory_rows_with_access(server, params, project_only, false).await
}

pub(crate) async fn search_memory_rows_with_access(
    server: &MemoryServer,
    params: SearchMemoryParams,
    project_only: bool,
    record_access: bool,
) -> Result<Vec<serde_json::Value>, String> {
    search_memory_rows_with_recall_config(server, params, project_only, record_access, None).await
}

pub(crate) async fn search_memory_rows_with_recall_config(
    server: &MemoryServer,
    mut params: SearchMemoryParams,
    project_only: bool,
    record_access: bool,
    recall_config: Option<&memcore::RecallConfig>,
) -> Result<Vec<serde_json::Value>, String> {
    params.query = query_with_context_symbols(&params.query, &params.context_symbols);
    let wiki_path_prefix = params
        .path_prefix
        .as_deref()
        .is_some_and(|prefix| prefix == "/wiki" || prefix.starts_with("/wiki/"));
    if !wiki_path_prefix && memcore::should_skip_query(&params.query) {
        return Ok(vec![]);
    }
    let top_k = params.normalized_top_k();
    params.top_k = top_k;
    params.candidates_per_channel = params.normalized_candidates_per_channel();

    let named_project_vec_available = if let Some(ref project_name) = params.project {
        server
            .with_named_project_store_read(project_name, |store| Ok(store.vec_available))
            .unwrap_or(false)
    } else {
        false
    };
    let default_wiki_vec_available =
        if params.project.is_none() && wiki_path_prefix && named_project_db_exists("wiki") {
            server
                .with_named_project_store_read("wiki", |store| Ok(store.vec_available))
                .unwrap_or(false)
        } else {
            false
        };

    let mut searched_default_wiki = false;
    let routing_config = server.routing_config().get();

    // #926: when query embedding fails/times out we fall back to lexical-only
    // recall. Capture a short reason so the response can carry a machine-
    // readable `recall_quality.degraded = "lexical_only: <reason>"` marker.
    let mut embed_degraded: Option<String> = None;

    if params.query_vec.is_none()
        && !parse_env_bool("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING").unwrap_or(false)
        && (server.global_vec_available()
            || server.project_vec_available()
            || named_project_vec_available
            || default_wiki_vec_available)
    {
        server.ensure_provider_secrets_materialized(&["VOYAGE_API_KEY"]);
        let (scrubbed_query, _) = crate::memory_search_ops::scrub_secrets(&params.query);
        match server.llm.embed_voyage(&scrubbed_query, "query").await {
            Ok(query_vec) => {
                params.query_vec = Some(query_vec);
            }
            Err(e) => {
                embed_degraded = Some(crate::memory_search_ops::recall_short_reason(&e));
                eprintln!(
                    "[search_memory] query embedding failed, falling back to lexical-only search: {e}"
                );
            }
        }
    }

    let pipeline_enabled = server.pipeline_enabled;

    let mut combined_results: Vec<(memcore::SearchResult, DbScope)> = Vec::new();

    let mut searched_named = false;
    if let Some(ref project_name) = params.project {
        if crate::memory_search_ops::search_helpers::named_project_db_exists(project_name) {
            let project_results = with_named_project_search(
                server,
                project_name,
                &params,
                record_access,
                recall_config,
                format!("Search failed in project DB '{project_name}'"),
            )?;
            combined_results.extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
            searched_named = true;
            if !project_only {
                let global_results = with_global_search(
                    server,
                    &params,
                    record_access,
                    recall_config,
                    "Search failed in global DB",
                )?;
                combined_results.extend(global_results.into_iter().map(|r| (r, DbScope::Global)));
            }
        } else if !project_only {
            return Err(format!(
                "Project '{project_name}' not found (expected DB at {})",
                crate::MemoryServer::resolve_named_project_db_path(project_name)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|e| e)
            ));
        }
    }

    if params.project.is_none() && wiki_path_prefix && named_project_db_exists("wiki") {
        match with_named_project_search(
            server,
            "wiki",
            &params,
            record_access,
            recall_config,
            "Search failed in default wiki project DB",
        ) {
            Ok(wiki_results) => {
                combined_results.extend(wiki_results.into_iter().map(|r| (r, DbScope::Project)));
                searched_default_wiki = true;
            }
            Err(e) => {
                tracing::warn!("Search failed in default wiki project DB: {e}");
            }
        }
    }

    if !searched_named {
        if project_only {
            let named_project =
                crate::memory_search_ops::search_helpers::resolve_workspace_named_project();
            if let Some(ref project_name) = named_project {
                if crate::memory_search_ops::search_helpers::named_project_db_exists(project_name)
                    && (project_name != "wiki" || !searched_default_wiki)
                {
                    let workspace_path = server.project_db_path_buf();
                    let named_path =
                        crate::MemoryServer::resolve_named_project_db_path(project_name).ok();
                    let skip_workspace = workspace_path
                        .as_deref()
                        .zip(named_path.as_deref())
                        .map(|(w, n)| w == n)
                        .unwrap_or(false);

                    if !skip_workspace && server.has_project_db() {
                        let project_results = with_project_search(
                            server,
                            &params,
                            record_access,
                            recall_config,
                            "Search failed in workspace project DB",
                        )?;
                        combined_results
                            .extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
                    }

                    if named_path.is_some() {
                        let project_results = with_named_project_search(
                            server,
                            project_name,
                            &params,
                            record_access,
                            recall_config,
                            format!("Search failed in named project DB '{project_name}'"),
                        )?;
                        combined_results
                            .extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
                    }
                } else if server.has_project_db() {
                    let project_results = with_project_search(
                        server,
                        &params,
                        record_access,
                        recall_config,
                        "Search failed in workspace project DB",
                    )?;
                    combined_results
                        .extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
                }
            } else if server.has_project_db() {
                let project_results = with_project_search(
                    server,
                    &params,
                    record_access,
                    recall_config,
                    "Search failed in workspace project DB",
                )?;
                combined_results.extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
            }
        } else {
            let inferred_project = infer_search_project(
                &server.tachi_home_dir(),
                &params.query,
                params.domain.as_deref(),
                &routing_config,
            );
            let inferred_db_path = inferred_project
                .as_deref()
                .and_then(|name| crate::MemoryServer::resolve_named_project_db_path(name).ok());
            let workspace_db_path = server.project_db_path_buf();
            let skip_workspace = inferred_db_path.is_some()
                && workspace_db_path.as_ref() == inferred_db_path.as_ref();

            let global_results = with_global_search(
                server,
                &params,
                record_access,
                recall_config,
                "Search failed in global DB",
            )?;
            combined_results.extend(global_results.into_iter().map(|r| (r, DbScope::Global)));

            if let Some(ref project_name) = inferred_project {
                if project_name == "wiki" && searched_default_wiki {
                    // Already searched the canonical wiki store above for
                    // unscoped /wiki queries.
                } else {
                    match with_named_project_search(
                        server,
                        project_name,
                        &params,
                        record_access,
                        recall_config,
                        format!("Search failed in inferred project DB '{project_name}'"),
                    ) {
                        Ok(project_results) => {
                            combined_results
                                .extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Search failed in inferred project DB '{project_name}': {e}"
                            );
                        }
                    }
                }
            } else if server.has_project_db() && !skip_workspace {
                let project_results = with_project_search(
                    server,
                    &params,
                    record_access,
                    recall_config,
                    "Search failed in project DB",
                )?;
                combined_results.extend(project_results.into_iter().map(|r| (r, DbScope::Project)));
            }
        }
    }

    if !training_recall_opted_in(&params) {
        combined_results.retain(|(result, _)| !is_training_seed(&result.entry));
    }
    if !recall_cache_recall_opted_in(params.path_prefix.as_deref()) {
        combined_results.retain(|(result, _)| !memcore::is_recall_cache_entry(&result.entry));
    }
    if !eval_recall_opted_in(&params) {
        combined_results.retain(|(result, _)| !is_eval_entry(&result.entry));
    }
    if let Some(project_name) = project_filter_name(
        &server.tachi_home_dir(),
        &params,
        project_only,
        &routing_config,
    ) {
        combined_results.retain(|(result, db_scope)| match db_scope {
            DbScope::Project => project_scope_allows_memory_with_config(
                &project_name,
                &params,
                &result.entry,
                &routing_config,
            ),
            DbScope::Global => true,
        });
    }

    apply_guide_context_boosts(
        &mut combined_results,
        params.file_context.as_deref(),
        params.error_context.as_deref(),
    );

    // #899: when global+project both contribute, prefer project rows enough that
    // recent project decisions are not buried under older global noise. Also
    // provides Project-before-Global tie-break. Skips wiki-scoped queries.
    // If preference does not sort (single-scope / wiki / empty), plain score sort.
    if !super::cross_library::apply_cross_library_project_preference(&mut combined_results, &params)
    {
        combined_results.sort_by(|a, b| {
            b.0.score
                .final_score
                .partial_cmp(&a.0.score.final_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.0.entry.timestamp.cmp(&a.0.entry.timestamp))
                .then_with(|| a.0.entry.id.cmp(&b.0.entry.id))
        });
    }

    combined_results = dedup_search_results(combined_results, top_k);

    let mut seen_ids = HashSet::new();
    let mut deduped_results: Vec<(memcore::SearchResult, DbScope)> = Vec::new();
    for (result, db_scope) in combined_results {
        if seen_ids.insert(result.entry.id.clone()) {
            deduped_results.push((result, db_scope));
        }
        if deduped_results.len() >= top_k {
            break;
        }
    }

    normalize_search_relevance(&mut deduped_results);

    // Sandbox enforcement: rules are stored in the global policy DB and apply
    // to rows from every searched DB. This keeps repo/project memories from
    // bypassing role rules just because the result came from another store.
    // Rules are fetched once per search (not once per result row) and evaluated
    // in memory, so a top_k=N search issues 1 sandbox query instead of N.
    if let Some(role) = sandbox_role(&params) {
        let rules = load_sandbox_rules(server, role)?;
        let mut allowed_results = Vec::with_capacity(deduped_results.len());
        for (result, db_scope) in deduped_results {
            let (allowed, matching_rule) =
                memcore::db::evaluate_sandbox_access(&rules, role, &result.entry.path, "read");
            if allowed {
                allowed_results.push((result, db_scope));
            } else {
                tracing::debug!(
                    role,
                    path = %result.entry.path,
                    db_scope = db_scope.as_str(),
                    matching_rule = ?matching_rule,
                    "sandbox denied memory search row"
                );
            }
        }
        deduped_results = allowed_results;
    }

    let global_recall_quality = deduped_results
        .iter()
        .any(|(_, db_scope)| *db_scope == DbScope::Global)
        .then(|| recall_quality_for_global(server))
        .flatten();
    let project_recall_quality = deduped_results
        .iter()
        .any(|(_, db_scope)| *db_scope == DbScope::Project)
        .then(|| recall_quality_for_project(server, &params))
        .flatten();

    let mut output: Vec<serde_json::Value> = deduped_results
        .iter()
        .map(|(r, db_scope)| {
            let mut row = slim_search_result(r, *db_scope, params.include_metadata);
            let recall_quality = match db_scope {
                DbScope::Global => global_recall_quality.as_ref(),
                DbScope::Project => project_recall_quality.as_ref(),
            };
            // #926: when embedding failed the row carries the lexical-only
            // marker (merged with any vector-coverage recall_quality). Absent a
            // provider failure, only the coverage object is attached.
            let effective_recall_quality = match embed_degraded.as_deref() {
                Some(reason) => Some(crate::memory_search_ops::merge_lexical_only_marker(
                    recall_quality.cloned(),
                    reason,
                )),
                None => recall_quality.cloned(),
            };
            if let Some(recall_quality) = effective_recall_quality {
                if let Some(obj) = row.as_object_mut() {
                    obj.insert("recall_quality".to_string(), recall_quality);
                }
            }
            if memcore::scorer::is_id_like_exact_query(&params.query)
                && memcore::scorer::entry_has_exact_query_token(&r.entry, &params.query)
            {
                mark_exact_token_match(&mut row);
            }
            row
        })
        .collect();
    annotate_exact_token_matches(&mut output, &params.query);

    if pipeline_enabled {
        let mut existing_ids: HashSet<String> = deduped_results
            .iter()
            .map(|(r, _)| r.entry.id.clone())
            .collect();

        if server.has_project_db() {
            let project_rules = server.with_project_store_read(|store| {
                Ok(store
                    .list_by_path("/behavior/global_rules", 50, false)
                    .unwrap_or_default())
            })?;
            for rule in project_rules {
                if !is_active_global_rule(&rule) {
                    continue;
                }
                if !existing_ids.insert(rule.id.clone()) {
                    continue;
                }
                output.push(slim_l0_rule(&rule, DbScope::Project));
            }
        }

        let global_rules = server.with_global_store_read(|store| {
            Ok(store
                .list_by_path("/behavior/global_rules", 50, false)
                .unwrap_or_default())
        })?;
        for rule in global_rules {
            if !is_active_global_rule(&rule) {
                continue;
            }
            if !existing_ids.insert(rule.id.clone()) {
                continue;
            }
            output.push(slim_l0_rule(&rule, DbScope::Global));
        }
    }

    Ok(output)
}
