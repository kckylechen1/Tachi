//! Near-duplicate detection for raw memory rows (RomanBath light-sleep port).
//!
//! Pairwise token-set Jaccard over entry `text` for raw-tier rows only.
//! Uses the same tokenizer as hybrid scoring — see [`crate::scorer::tokenize`].

use std::collections::HashSet;

use crate::scorer::tokenize;
use crate::types::MemoryEntry;

/// Maximum raw rows considered in one near-duplicate scan (RomanBath parity).
pub const NEAR_DUP_RAW_SCAN_CAP: usize = 500;

/// Token-set Jaccard similarity between two texts via [`tokenize`].
pub fn text_token_jaccard(a: &str, b: &str) -> f64 {
    let ta: HashSet<String> = tokenize(a).into_iter().collect();
    let tb: HashSet<String> = tokenize(b).into_iter().collect();
    if ta.is_empty() && tb.is_empty() {
        return 0.0;
    }
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let intersection = ta.intersection(&tb).count() as f64;
    let union = ta.union(&tb).count() as f64;
    intersection / union.max(1.0)
}

/// Pairwise near-duplicate pairs among raw-tier entries.
///
/// Returns `(i, j, similarity)` with `i < j` (indices into `entries`), only when
/// both rows are raw-tier and text Jaccard is `>= threshold`. Only the first
/// [`NEAR_DUP_RAW_SCAN_CAP`] raw rows in input order are compared (O(n²) cap).
pub fn near_duplicate_raw_pairs(
    entries: &[MemoryEntry],
    threshold: f64,
) -> Vec<(usize, usize, f64)> {
    let raw_indices: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.tier.eq_ignore_ascii_case("raw"))
        .map(|(index, _)| index)
        .take(NEAR_DUP_RAW_SCAN_CAP)
        .collect();

    let mut pairs = Vec::new();
    for left in 0..raw_indices.len() {
        let i = raw_indices[left];
        for &j in &raw_indices[(left + 1)..] {
            let similarity = text_token_jaccard(&entries[i].text, &entries[j].text);
            if similarity + f64::EPSILON >= threshold {
                pairs.push((i, j, similarity));
            }
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MemoryEntry;
    use serde_json::json;

    fn raw_entry(id: &str, text: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/test".to_string(),
            summary: text.chars().take(60).collect(),
            text: text.to_string(),
            importance: 0.5,
            timestamp: "2026-07-05T00:00:00Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "test".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: json!({}),
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    fn consolidated_entry(id: &str, text: &str) -> MemoryEntry {
        let mut entry = raw_entry(id, text);
        entry.tier = "consolidated".to_string();
        entry
    }

    #[test]
    fn near_duplicate_raw_pairs_detects_high_similarity_twins() {
        let shared = "alpha bravo charlie delta echo foxtrot golf hotel india juliet \
                      kilo lima mike november oscar papa quebec romeo sierra";
        let a = raw_entry("a", &format!("{shared} tango"));
        let b = raw_entry("b", &format!("{shared} uniform"));
        assert!(text_token_jaccard(&a.text, &b.text) > 0.9);

        let entries = vec![a, b];
        let pairs = near_duplicate_raw_pairs(&entries, 0.9);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, 0);
        assert_eq!(pairs[0].1, 1);
        assert!(pairs[0].2 > 0.9);
    }

    #[test]
    fn near_duplicate_raw_pairs_ignores_distinct_texts() {
        let entries = vec![
            raw_entry("a", "completely different topic about database migrations"),
            raw_entry("b", "unrelated notes on frontend styling conventions"),
        ];
        assert!(near_duplicate_raw_pairs(&entries, 0.9).is_empty());
    }

    #[test]
    fn near_duplicate_raw_pairs_excludes_non_raw_tier() {
        let shared = "alpha bravo charlie delta echo foxtrot golf hotel india juliet \
                      kilo lima mike november oscar papa quebec romeo sierra";
        let raw = raw_entry("raw", &format!("{shared} tango"));
        let consolidated = consolidated_entry("cons", &format!("{shared} uniform"));
        assert!(text_token_jaccard(&raw.text, &consolidated.text) > 0.9);

        let entries = vec![raw, consolidated];
        assert!(
            near_duplicate_raw_pairs(&entries, 0.9).is_empty(),
            "non-raw rows must not participate"
        );
    }

    #[test]
    fn near_duplicate_raw_pairs_respects_scan_cap() {
        let shared = "alpha bravo charlie delta echo foxtrot golf hotel india juliet \
                      kilo lima mike november oscar papa quebec romeo sierra";
        let mut entries = Vec::new();
        for index in 0..=NEAR_DUP_RAW_SCAN_CAP {
            let suffix = if index % 2 == 0 { "tango" } else { "uniform" };
            entries.push(raw_entry(
                &format!("entry-{index}"),
                &format!("{shared} {suffix}"),
            ));
        }
        assert_eq!(entries.len(), NEAR_DUP_RAW_SCAN_CAP + 1);
        let pairs = near_duplicate_raw_pairs(&entries, 0.9);
        assert!(
            pairs
                .iter()
                .all(|(i, j, _)| *i < NEAR_DUP_RAW_SCAN_CAP && *j < NEAR_DUP_RAW_SCAN_CAP),
            "pairs must stay within the capped raw window: {pairs:?}"
        );
        assert!(
            !pairs.iter().any(|(i, j, _)| {
                (*i == 0 && *j == NEAR_DUP_RAW_SCAN_CAP) || (*i == NEAR_DUP_RAW_SCAN_CAP && *j == 0)
            }),
            "row beyond the cap must not pair with capped rows"
        );
    }
}
