use std::collections::HashSet;

use crate::noise::is_cjk;
use crate::recall_config::RecallConfig;
use crate::types::MemoryEntry;

/// Simple tokeniser for symbolic (bag-of-words) scoring.
/// - Latin/ASCII: splits on non-alphanumeric, filters tokens < 2 chars.
/// - CJK (Chinese/Japanese/Korean): emits each character as an individual token.
pub fn tokenize(s: &str) -> Vec<String> {
    let lower = s.to_lowercase();
    let mut tokens = Vec::new();
    let mut current = String::new();

    for ch in lower.chars() {
        if is_cjk(ch) {
            // Flush any pending ASCII token
            if current.len() >= 2 {
                tokens.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
            // Emit each CJK character as its own token
            tokens.push(ch.to_string());
        } else if ch.is_alphanumeric() {
            current.push(ch);
        } else {
            // Separator: flush pending ASCII token
            if current.len() >= 2 {
                tokens.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }
    // Flush trailing
    if current.len() >= 2 {
        tokens.push(current);
    }
    tokens
}

/// Compute a normalised query-token recall score [0, 1].
/// Measures what fraction of query tokens appear in the entry's text/keywords/entities.
pub fn symbolic_score(
    query: &str,
    entry_text: &str,
    keywords: &[String],
    entities: &[String],
) -> f64 {
    let query_tokens: HashSet<String> = tokenize(query).into_iter().collect();
    if query_tokens.is_empty() {
        return 0.0;
    }

    let mut text_tokens: HashSet<String> = tokenize(entry_text).into_iter().collect();
    for kw in keywords {
        text_tokens.extend(tokenize(kw));
    }
    for ent in entities {
        let trimmed = ent.trim();
        if trimmed.is_empty() {
            continue;
        }
        text_tokens.extend(tokenize(trimmed));
        text_tokens.insert(trimmed.to_ascii_lowercase());
    }

    let overlap = query_tokens.intersection(&text_tokens).count();
    (overlap as f64) / (query_tokens.len().max(1) as f64)
}

pub fn symbolic_score_entry(query: &str, entry: &MemoryEntry) -> f64 {
    let query_tokens: HashSet<String> = tokenize(query).into_iter().collect();
    if query_tokens.is_empty() {
        return 0.0;
    }

    let mut text_tokens: HashSet<String> = HashSet::new();
    for field in [
        entry.id.as_str(),
        entry.path.as_str(),
        entry.topic.as_str(),
        entry.summary.as_str(),
        entry.text.as_str(),
    ] {
        text_tokens.extend(tokenize(field));
    }
    for kw in &entry.keywords {
        text_tokens.extend(tokenize(kw));
    }
    for ent in &entry.entities {
        let trimmed = ent.trim();
        if trimmed.is_empty() {
            continue;
        }
        text_tokens.extend(tokenize(trimmed));
        text_tokens.insert(trimmed.to_ascii_lowercase());
    }

    let overlap = query_tokens.intersection(&text_tokens).count();
    (overlap as f64) / (query_tokens.len().max(1) as f64)
}

/// A caller-injected, domain-specific precision booster.
///
/// A host project registers matchers through `SearchOptions::precision_matchers`
/// to express "if this (query, entry) pair is an exact match in my domain,
/// multiply its hybrid score". The engine never inspects the domain — it only
/// applies whatever boost a matcher returns, under the same RRF clamp and
/// symbolic-floor mechanics as the generic id-like boost.
pub trait PrecisionMatcher: Send + Sync {
    /// Return `Some(boost)` (expected `>= 1.0`) when `entry` is an exact
    /// precision match for `query` in this matcher's domain; `None` to abstain.
    fn boost(&self, query: &str, entry: &MemoryEntry) -> Option<f64>;
}

pub fn is_id_like_exact_query(query: &str) -> bool {
    let query = query.trim();
    if query.len() < 8 || query.chars().any(char::is_whitespace) {
        return false;
    }
    let has_precision_marker = query
        .chars()
        .any(|ch| ch == '_' || ch == '-' || ch == ':' || ch.is_ascii_digit());
    has_precision_marker && tokenize(query).len() >= 2
}

pub fn entry_has_exact_query_token(entry: &MemoryEntry, query: &str) -> bool {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return false;
    }
    if entry.id.to_ascii_lowercase().contains(&query)
        || entry.path.to_ascii_lowercase().contains(&query)
        || entry.topic.to_ascii_lowercase().contains(&query)
        || entry.summary.to_ascii_lowercase().contains(&query)
        || entry.text.to_ascii_lowercase().contains(&query)
    {
        return true;
    }
    entry
        .keywords
        .iter()
        .chain(entry.entities.iter())
        .any(|value| value.to_ascii_lowercase().contains(&query))
}

/// Generic, domain-agnostic precision boost.
///
/// Returns the configured id-like exact-match boost when `query` is a long, structured,
/// identifier-like string that exactly matches a token in `entry`; otherwise
/// `1.0`. Domain-specific boosts are layered on top by the caller via the
/// [`PrecisionMatcher`] list on `SearchOptions` — see the precision-boost loop
/// in `hybrid_search`.
pub fn generic_precision_multiplier(query: &str, entry: &MemoryEntry) -> f64 {
    generic_precision_multiplier_impl(is_id_like_exact_query(query), query, entry)
}

/// Same as [`generic_precision_multiplier`], but takes a precomputed
/// `is_id_like` so the query-constant `is_id_like_exact_query` check (which
/// tokenizes and allocates) isn't repeated for every candidate in the search
/// hot loop.
pub(crate) fn generic_precision_multiplier_impl(
    is_id_like: bool,
    query: &str,
    entry: &MemoryEntry,
) -> f64 {
    generic_precision_multiplier_impl_with_config(is_id_like, query, entry, RecallConfig::get())
}

pub(crate) fn generic_precision_multiplier_impl_with_config(
    is_id_like: bool,
    query: &str,
    entry: &MemoryEntry,
    recall_config: &RecallConfig,
) -> f64 {
    if is_id_like && entry_has_exact_query_token(entry, query) {
        recall_config.id_like_exact_match_boost
    } else {
        1.0
    }
}
