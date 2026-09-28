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

/// Calls `on_token` with every token [`tokenize`] would emit for `s`, in the
/// same order, as a borrowed slice of the lowercased text instead of an owned
/// `String`. Stops early (and returns `false`) when `on_token` returns
/// `false`.
///
/// The token rules are exactly [`tokenize`]'s: the whole string is lowercased
/// with `str::to_lowercase` first (so context-sensitive mappings such as the
/// Greek final sigma match), a CJK character is its own token, an
/// alphanumeric run is one token when it is at least 2 **bytes** long, and
/// everything else separates. A pure-ASCII string is lowercased byte-wise,
/// which is what `str::to_lowercase` does for ASCII, and borrowed when it has
/// no uppercase letters.
fn for_each_token(s: &str, mut on_token: impl FnMut(&str) -> bool) -> bool {
    let lower: std::borrow::Cow<'_, str> = if s.is_ascii() {
        if s.bytes().any(|b| b.is_ascii_uppercase()) {
            std::borrow::Cow::Owned(s.to_ascii_lowercase())
        } else {
            std::borrow::Cow::Borrowed(s)
        }
    } else {
        std::borrow::Cow::Owned(s.to_lowercase())
    };
    let lower = lower.as_ref();
    // Byte offset where the pending alphanumeric run starts, if any.
    let mut run_start: Option<usize> = None;
    for (offset, ch) in lower.char_indices() {
        if is_cjk(ch) {
            if let Some(start) = run_start.take() {
                if offset - start >= 2 && !on_token(&lower[start..offset]) {
                    return false;
                }
            }
            if !on_token(&lower[offset..offset + ch.len_utf8()]) {
                return false;
            }
        } else if ch.is_alphanumeric() {
            run_start.get_or_insert(offset);
        } else if let Some(start) = run_start.take() {
            if offset - start >= 2 && !on_token(&lower[start..offset]) {
                return false;
            }
        }
    }
    if let Some(start) = run_start {
        if lower.len() - start >= 2 && !on_token(&lower[start..]) {
            return false;
        }
    }
    true
}

/// Above this many distinct query tokens a hash lookup replaces the linear
/// scan in [`SymbolicQuery::position`].
const SYMBOLIC_QUERY_LINEAR_SCAN_MAX: usize = 16;

/// A query pre-tokenized once for symbolic (bag-of-words) scoring.
///
/// This is the single implementation behind [`symbolic_score`],
/// [`symbolic_score_entry`], the `tachi_symbolic_score` SQLite function and
/// the final ranker, so candidate selection and ranking cannot drift
/// (tachi#1144). The score is `distinct query tokens present in the entry /
/// distinct query tokens`, the same set-intersection count as before, but it
/// is computed by streaming each field's tokens against the query set and
/// stops as soon as every query token has been seen. No per-entry token set
/// is built.
pub(crate) struct SymbolicQuery {
    /// Distinct query tokens, in first-occurrence order.
    tokens: Vec<String>,
    /// Token -> index into `tokens`; only built for large queries.
    index: Option<std::collections::HashMap<String, usize>>,
}

impl SymbolicQuery {
    pub(crate) fn new(query: &str) -> Self {
        let mut tokens: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for token in tokenize(query) {
            if seen.insert(token.clone()) {
                tokens.push(token);
            }
        }
        let index = (tokens.len() > SYMBOLIC_QUERY_LINEAR_SCAN_MAX).then(|| {
            tokens
                .iter()
                .enumerate()
                .map(|(i, token)| (token.clone(), i))
                .collect()
        });
        Self { tokens, index }
    }

    /// Number of distinct query tokens: the score's denominator.
    pub(crate) fn distinct_token_count(&self) -> usize {
        self.tokens.len()
    }

    fn position(&self, token: &str) -> Option<usize> {
        match &self.index {
            Some(index) => index.get(token).copied(),
            None => self.tokens.iter().position(|t| t == token),
        }
    }

    pub(crate) fn score_entry(&self, entry: &MemoryEntry) -> f64 {
        self.score_fields(
            &[
                entry.id.as_str(),
                entry.path.as_str(),
                entry.topic.as_str(),
                entry.summary.as_str(),
                entry.text.as_str(),
            ],
            entry.keywords.iter().map(String::as_str),
            entry.entities.iter().map(String::as_str),
        )
    }

    pub(crate) fn score_fields<'k, 'e>(
        &self,
        fields: &[&str],
        keywords: impl IntoIterator<Item = &'k str>,
        entities: impl IntoIterator<Item = &'e str>,
    ) -> f64 {
        let total = self.tokens.len();
        if total == 0 {
            return 0.0;
        }
        let mut matched = vec![false; total];
        let mut remaining = total;
        let mut mark = |token: &str| {
            if let Some(i) = self.position(token) {
                if !matched[i] {
                    matched[i] = true;
                    remaining -= 1;
                }
            }
            remaining > 0
        };
        // Every `break` below means all query tokens are already matched, so
        // the rest of the entry cannot change the overlap.
        'scan: {
            for field in fields {
                if !for_each_token(field, &mut mark) {
                    break 'scan;
                }
            }
            for keyword in keywords {
                if !for_each_token(keyword, &mut mark) {
                    break 'scan;
                }
            }
            for entity in entities {
                let trimmed = entity.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if !for_each_token(trimmed, &mut mark) {
                    break 'scan;
                }
                // The whole entity is also a token, lowercased ASCII-only.
                if !mark(&trimmed.to_ascii_lowercase()) {
                    break 'scan;
                }
            }
        }
        let overlap = total - remaining;
        (overlap as f64) / (total.max(1) as f64)
    }
}

/// Compute a normalised query-token recall score [0, 1].
/// Measures what fraction of query tokens appear in the entry's text/keywords/entities.
pub fn symbolic_score(
    query: &str,
    entry_text: &str,
    keywords: &[String],
    entities: &[String],
) -> f64 {
    SymbolicQuery::new(query).score_fields(
        &[entry_text],
        keywords.iter().map(String::as_str),
        entities.iter().map(String::as_str),
    )
}

pub fn symbolic_score_entry(query: &str, entry: &MemoryEntry) -> f64 {
    SymbolicQuery::new(query).score_entry(entry)
}

/// Scores the text columns stored in SQLite with exactly the token semantics
/// used by [`symbolic_score_entry`]. The symbolic candidate query calls this
/// through a SQLite scalar function before applying its cap, so a LIKE
/// substring cannot outrank an exact token match (tachi#1144).
pub(crate) fn symbolic_score_stored_entry(
    query: &SymbolicQuery,
    fields: &[&str; 5],
    keywords_json: &str,
    entities_json: &str,
) -> f64 {
    let keywords = serde_json::from_str::<Vec<String>>(keywords_json).unwrap_or_default();
    let entities = serde_json::from_str::<Vec<String>>(entities_json).unwrap_or_default();
    query.score_fields(
        fields,
        keywords.iter().map(String::as_str),
        entities.iter().map(String::as_str),
    )
}

/// The pre-streaming implementation (per-entry `HashSet<String>` of every
/// token), kept only as the reference the differential tests compare
/// [`SymbolicQuery`] against.
#[cfg(test)]
pub(crate) fn symbolic_score_fields_reference(
    query: &str,
    fields: &[&str],
    keywords: &[String],
    entities: &[String],
) -> f64 {
    let query_tokens: HashSet<String> = tokenize(query).into_iter().collect();
    if query_tokens.is_empty() {
        return 0.0;
    }

    let mut text_tokens: HashSet<String> = HashSet::new();
    for field in fields {
        text_tokens.extend(tokenize(field));
    }
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

    /// Whether [`boost`](Self::boost) reads `entry.vector`. See
    /// [`crate::scorer::DecayPolicy::reads_entry_vector`]: the default `true`
    /// keeps every candidate's embedding loaded during ranking.
    fn reads_entry_vector(&self) -> bool {
        true
    }
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
    // tachi#1585 D5: pure default, not the process-wide `RecallConfig::get()`.
    // This helper takes no `MemoryStore`/`SearchOptions` to draw a
    // host-injected policy from; the crate's hot search path calls
    // `generic_precision_multiplier_impl_with_config` directly with an
    // explicit config (`search/ranking.rs`), so this default is reached only
    // by the standalone `generic_precision_multiplier` convenience API.
    generic_precision_multiplier_impl_with_config(
        is_id_like,
        query,
        entry,
        &RecallConfig::default(),
    )
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
