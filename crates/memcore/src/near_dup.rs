//! Near-duplicate detection for raw memory rows (RomanBath light-sleep port).
//!
//! Pairwise token-set Jaccard over entry `text` for raw-tier rows only.
//! Uses the same tokenizer as hybrid scoring — see [`crate::scorer::tokenize`].
//!
//! ## Scan window (provisional)
//!
//! Callers typically pass the first [`NEAR_DUP_RAW_SCAN_CAP`] rows from
//! `list_by_path(prefix, limit)`, whose order is **path lexicographic ASC,
//! then timestamp DESC** — not "newest N overall". Rows under late path
//! prefixes can fall outside the window even when recent. A recency-first
//! window is a deliberate follow-up; do not silently reorder the shared
//! consolidate scan here (it also feeds same-path / archive / promote
//! generators).

use std::collections::HashSet;

use crate::scorer::tokenize;
use crate::types::MemoryEntry;

/// Provisional hard cap on raw rows compared in one near-duplicate scan
/// (RomanBath parity). The effective window inherits the caller's list order
/// — typically `list_by_path` path-ASC / timestamp-DESC, not newest-first.
pub const NEAR_DUP_RAW_SCAN_CAP: usize = 500;

fn token_set_jaccard(ta: &HashSet<String>, tb: &HashSet<String>) -> f64 {
    if ta.is_empty() && tb.is_empty() {
        return 0.0;
    }
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let intersection = ta.intersection(tb).count() as f64;
    let union = ta.union(tb).count() as f64;
    intersection / union.max(1.0)
}

/// Token-set Jaccard similarity between two texts via [`tokenize`].
pub fn text_token_jaccard(a: &str, b: &str) -> f64 {
    let ta: HashSet<String> = tokenize(a).into_iter().collect();
    let tb: HashSet<String> = tokenize(b).into_iter().collect();
    token_set_jaccard(&ta, &tb)
}

/// Pairwise near-duplicate pairs among raw-tier entries.
///
/// Returns `(i, j, similarity)` with `i < j` (indices into `entries`), only when
/// both rows are raw-tier and text Jaccard is `>= threshold`. Only the first
/// [`NEAR_DUP_RAW_SCAN_CAP`] raw rows in input order are compared (O(n²) cap;
/// each entry is tokenized once).
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

    let token_sets: Vec<HashSet<String>> = raw_indices
        .iter()
        .map(|&index| tokenize(&entries[index].text).into_iter().collect())
        .collect();

    let mut pairs = Vec::new();
    for (left, &i) in raw_indices.iter().enumerate() {
        for (right, &j) in raw_indices.iter().enumerate().skip(left + 1) {
            let similarity = token_set_jaccard(&token_sets[left], &token_sets[right]);
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
            scored_count: 0,
            last_access: None,
            last_use_at: None,
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

    #[test]
    fn near_duplicate_raw_pairs_detects_chinese_near_twins() {
        // Char-level CJK tokenize: shared ≥37 unique chars + 2-char unique
        // tails → Jaccard shared/(shared+4) > 0.9.
        let shared =
            "数据库迁移需要先备份再执行脚本检查索引状态确认无误后再同步配置并记录变更摘要完毕";
        let a = raw_entry("zh-a", &format!("{shared}提交"));
        let b = raw_entry("zh-b", &format!("{shared}归档"));
        let jaccard = text_token_jaccard(&a.text, &b.text);
        assert!(jaccard + f64::EPSILON >= 0.9, "jaccard={jaccard}");
        let pairs = near_duplicate_raw_pairs(&[a, b], 0.9);
        assert_eq!(pairs.len(), 1);
        assert!(pairs[0].2 + f64::EPSILON >= 0.9);
    }

    #[test]
    fn near_duplicate_raw_pairs_ignores_distinct_chinese() {
        let entries = [
            raw_entry("zh-a", "今天讨论了交易策略的回测框架和风控阈值"),
            raw_entry("zh-b", "厨房冰箱里还剩半盒豆腐和一把青菜"),
        ];
        assert!(text_token_jaccard(&entries[0].text, &entries[1].text) < 0.9);
        assert!(near_duplicate_raw_pairs(&entries, 0.9).is_empty());
    }
}
