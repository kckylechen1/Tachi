use rusqlite::Connection;
use std::collections::HashMap;

use crate::{
    db::{search_fts, search_fts_raw_match},
    error::MemoryError,
    recall_config::RecallConfig,
    scorer::tokenize,
};

const MAX_SYMBOLIC_EXPANSION_TERMS: usize = 16;

fn push_unique(out: &mut Vec<String>, term: &str) {
    let term = term.trim().to_ascii_lowercase();
    if term.is_empty() || out.iter().any(|existing| existing == &term) {
        return;
    }
    out.push(term);
}

fn token_expansion_variants(token: &str) -> &'static [&'static [&'static str]] {
    match token {
        "mcp" => &[&["model", "context", "protocol"]],
        "llm" => &[&["language", "model"]],
        "rag" => &[&["retrieval", "augmented", "generation"]],
        "fts" => &[&["full", "text", "search"]],
        "rrf" => &[&["reciprocal", "rank", "fusion"]],
        "mmr" => &[&["maximal", "marginal", "relevance"], &["diversity"]],
        "ci" => &[&["workflow"], &["checks"], &["github", "actions"]],
        "pr" => &[&["pull", "request"]],
        "db" => &[&["database"], &["sqlite"]],
        "auth" => &[&["authentication"], &["authorization"]],
        "api" => &[&["endpoint"], &["interface"]],
        "cli" => &[&["command"], &["terminal"]],
        "repo" => &[&["repository"]],
        "vec" | "vector" => &[&["embedding"], &["semantic"]],
        "embedding" | "embeddings" => &[&["vector"], &["semantic"]],
        "semantic" => &[&["embedding"], &["vector"]],
        "recall" => &[&["retrieval"], &["search"]],
        "retrieval" => &[&["recall"], &["search"]],
        "bug" => &[&["error"], &["failure"], &["crash"]],
        "error" => &[&["bug"], &["failure"]],
        "failure" => &[&["error"], &["bug"]],
        "crash" => &[&["failure"], &["panic"]],
        "panic" => &[&["crash"], &["failure"]],
        _ => &[],
    }
}

fn phrase_expansion_variants(tokens: &[String]) -> Vec<String> {
    const PHRASES: &[(&[&str], &[&str])] = &[
        (&["model", "context", "protocol"], &["mcp"]),
        (&["language", "model"], &["llm"]),
        (&["retrieval", "augmented", "generation"], &["rag"]),
        (&["full", "text", "search"], &["fts"]),
        (&["reciprocal", "rank", "fusion"], &["rrf"]),
        (&["pull", "request"], &["pr"]),
        (&["github", "actions"], &["ci"]),
    ];

    let mut variants = Vec::new();
    for (phrase, replacement) in PHRASES {
        if phrase.len() > tokens.len() {
            continue;
        }
        for start in 0..=tokens.len() - phrase.len() {
            if phrase
                .iter()
                .enumerate()
                .all(|(idx, part)| tokens[start + idx] == *part)
            {
                let mut expanded =
                    Vec::with_capacity(tokens.len() - phrase.len() + replacement.len());
                expanded.extend(tokens[..start].iter().cloned());
                expanded.extend(replacement.iter().map(|part| (*part).to_string()));
                expanded.extend(tokens[start + phrase.len()..].iter().cloned());
                variants.push(expanded.join(" "));
            }
        }
    }
    variants
}

fn expanded_fts_queries(query: &str, max_queries: usize) -> Vec<String> {
    let tokens = tokenize(query);
    if tokens.is_empty() {
        return Vec::new();
    }
    let max_queries = max_queries.max(1);

    let mut queries = Vec::new();
    push_unique(&mut queries, query.trim());
    for (idx, token) in tokens.iter().enumerate() {
        for replacement in token_expansion_variants(token) {
            let mut expanded = Vec::with_capacity(tokens.len() + replacement.len());
            expanded.extend(tokens[..idx].iter().cloned());
            expanded.extend(replacement.iter().map(|part| (*part).to_string()));
            expanded.extend(tokens[idx + 1..].iter().cloned());
            push_unique(&mut queries, &expanded.join(" "));
            if queries.len() >= max_queries {
                return queries;
            }
        }
    }

    for variant in phrase_expansion_variants(&tokens) {
        push_unique(&mut queries, &variant);
        if queries.len() >= max_queries {
            break;
        }
    }

    queries
}

fn fts_or_fallback_match_query(query: &str, max_terms: usize) -> Option<String> {
    let max_terms = max_terms.max(1);
    let mut terms = Vec::new();
    for token in tokenize(query) {
        if token.len() < 2 || !token.chars().all(|ch| ch.is_ascii_alphanumeric()) {
            continue;
        }
        push_unique(&mut terms, &token);
        if terms.len() >= max_terms {
            break;
        }
    }
    if terms.len() < 2 {
        return None;
    }
    Some(
        terms
            .into_iter()
            .map(|term| format!("{term}*"))
            .collect::<Vec<_>>()
            .join(" OR "),
    )
}

pub(super) fn symbolic_query_with_expansion(query: &str) -> String {
    let tokens = tokenize(query);
    if tokens.is_empty() {
        return query.to_string();
    }

    let mut terms = tokens.clone();
    for token in &tokens {
        for replacement in token_expansion_variants(token) {
            for part in *replacement {
                push_unique(&mut terms, part);
                if terms.len() >= tokens.len() + MAX_SYMBOLIC_EXPANSION_TERMS {
                    return terms.join(" ");
                }
            }
        }
    }
    for variant in phrase_expansion_variants(&tokens) {
        for part in tokenize(&variant) {
            push_unique(&mut terms, &part);
            if terms.len() >= tokens.len() + MAX_SYMBOLIC_EXPANSION_TERMS {
                return terms.join(" ");
            }
        }
    }
    terms.join(" ")
}

#[allow(clippy::too_many_arguments)]
pub(super) fn search_fts_with_expansion_config(
    conn: &Connection,
    query: &str,
    limit: usize,
    include_archived: bool,
    include_superseded: bool,
    path_prefix: Option<&str>,
    as_of: Option<&str>,
    recall_config: &RecallConfig,
) -> Result<HashMap<String, f64>, MemoryError> {
    let mut merged = HashMap::new();
    for (idx, fts_query) in expanded_fts_queries(query, recall_config.max_expanded_fts_queries)
        .into_iter()
        .enumerate()
    {
        let factor = if idx == 0 {
            1.0
        } else {
            recall_config.expanded_fts_score_factor
        };
        for (id, score) in search_fts(
            conn,
            &fts_query,
            limit,
            include_archived,
            include_superseded,
            path_prefix,
            as_of,
        )? {
            let adjusted = score * factor;
            merged
                .entry(id)
                .and_modify(|existing: &mut f64| *existing = existing.max(adjusted))
                .or_insert(adjusted);
        }
    }
    if recall_config.or_fallback_fts_score_factor > 0.0 {
        if let Some(or_query) =
            fts_or_fallback_match_query(query, recall_config.or_fallback_fts_max_terms)
        {
            for (id, score) in search_fts_raw_match(
                conn,
                &or_query,
                limit,
                include_archived,
                include_superseded,
                path_prefix,
                as_of,
            )? {
                let adjusted = score * recall_config.or_fallback_fts_score_factor;
                merged
                    .entry(id)
                    .and_modify(|existing: &mut f64| *existing = existing.max(adjusted))
                    .or_insert(adjusted);
            }
        }
    }
    Ok(merged)
}
