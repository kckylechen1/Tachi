//! Pure hybrid-floor RRF blend for post-model rerank merges.
//!
//! Lives in the portable kernel so HyperTachi / HyperMemory can reuse the same
//! seatbelt + reciprocal-rank-fusion math without `memory-server` / Foundry
//! product deps. LLM I/O (Voyage rerank call) stays in the product crate.
//!
//! Relocated from `memory_search_ops::rerank` (#876 seam, #833 extract path).

use serde_json::{json, Value};

/// Provisional: minimum fraction of `top_k` hybrid-head items guaranteed to
/// survive rerank. Lower = more promotion room for tail items; raise to protect
/// more head items. Named threshold per AGENTS.md; tunable.
pub const HYBRID_HEAD_FRACTION: f64 = 0.5;

/// Stamp the relevance fields so the value rendered into agent context matches
/// the actual sort key. `relevance`/`score.final` reflect `blend_score` (the key
/// the output is ordered by); `rerank_score` keeps the raw provider signal for
/// observability when present.
pub fn apply_blend_relevance(mut row: Value, blend_score: f64, rerank_score: Option<f64>) -> Value {
    let blend = round3(blend_score);
    if let Value::Object(map) = &mut row {
        map.insert("relevance".into(), json!(blend));
        if let Some(raw) = rerank_score {
            map.insert("rerank_score".into(), json!(round3(raw)));
        }
        if let Some(Value::Object(score_map)) = map.get_mut("score") {
            score_map.insert("final".into(), json!(blend));
        }
    }
    row
}

/// Merge a model-produced `(index, score)` order with a hybrid-score floor so
/// the top-K always retains a guaranteed hybrid head while still allowing tail
/// promotions via reciprocal-rank-fusion.
///
/// `order` entries are `(original_row_index, provider_score)`. Invalid indices
/// are ignored. Empty `top_k` or empty `rows` returns an empty vec.
pub fn merge_rerank_order_with_hybrid_floor(
    rows: &[Value],
    order: &[(usize, f64)],
    top_k: usize,
) -> Vec<Value> {
    let output_len = top_k.min(rows.len());
    if output_len == 0 {
        return Vec::new();
    }

    // rerank_by_index: index -> (rerank_rank, provider_score). Keep first
    // occurrence per index; rerank_rank is the 1-based enumerate position.
    let mut rerank_by_index: std::collections::HashMap<usize, (usize, f64)> =
        std::collections::HashMap::new();
    for (rerank_rank, &(index, score)) in order.iter().enumerate() {
        if index < rows.len() {
            rerank_by_index
                .entry(index)
                .or_insert((rerank_rank + 1, score));
        }
    }
    let missing_rerank_rank = rows.len() + 1;

    // blend_score = reciprocal rank fusion of hybrid rank and rerank rank.
    let blend_score = |index: usize| -> f64 {
        let original_rank = index + 1;
        let rerank_rank = rerank_by_index
            .get(&index)
            .map(|(rank, _)| *rank)
            .unwrap_or(missing_rerank_rank);
        (1.0 / original_rank as f64) + (1.0 / rerank_rank as f64)
    };

    // Seatbelt: a guaranteed prefix of the hybrid head always survives, but its
    // position may shift down if tail items out-score it. head_floor is strictly
    // <= output_len, leaving room for tail promotions.
    let head_floor = ((output_len as f64) * HYBRID_HEAD_FRACTION).round() as usize;
    let head_floor = head_floor.min(output_len);

    // Guaranteed head indices 0..head_floor.
    let mut selected: Vec<usize> = (0..head_floor).collect();

    // Remaining slots filled by the top blend-scoring indices from the pool
    // head_floor..rows.len() (tail + any non-guaranteed head).
    let remaining = output_len.saturating_sub(head_floor);
    if remaining > 0 {
        let mut pool: Vec<usize> = (head_floor..rows.len()).collect();
        pool.sort_by(|&a, &b| {
            blend_score(b)
                .partial_cmp(&blend_score(a))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.cmp(&b))
        });
        for index in pool.into_iter().take(remaining) {
            selected.push(index);
        }
    }

    // Order the selected set by blend_score descending (stable tiebreak: index
    // ascending) for the final output sequence.
    selected.sort_by(|&a, &b| {
        blend_score(b)
            .partial_cmp(&blend_score(a))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.cmp(&b))
    });

    selected
        .into_iter()
        .map(|index| {
            let rerank_score = rerank_by_index.get(&index).map(|(_, score)| *score);
            apply_blend_relevance(rows[index].clone(), blend_score(index), rerank_score)
        })
        .collect()
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rerank_merge_blends_rerank_order_with_hybrid_rank() {
        let rows = (0..6)
            .map(|idx| {
                json!({
                    "id": format!("hybrid-{idx}"),
                    "text": format!("candidate {idx}"),
                    "relevance": 1.0 - (idx as f64 * 0.01),
                    "score": { "final": 1.0 - (idx as f64 * 0.01) }
                })
            })
            .collect::<Vec<_>>();

        let merged = merge_rerank_order_with_hybrid_floor(
            &rows,
            &[(5, 0.99), (4, 0.98), (2, 0.97), (1, 0.96)],
            3,
        );
        let ids = merged
            .iter()
            .map(|row| row["id"].as_str().expect("row id"))
            .collect::<Vec<_>>();

        // head_floor for top_k=3 is 2 -> hybrid-0 and hybrid-1 are guaranteed by
        // the seatbelt; the remaining slot promotes the best tail item (hybrid-5,
        // rerank_rank 1, blend 1.167). Final order is blend_score descending.
        assert_eq!(ids, vec!["hybrid-5", "hybrid-0", "hybrid-1"]);
        // relevance now equals the blend sort key (reciprocal-rank-fusion), not the
        // raw provider signal, so [Score=X.XX] in agent context matches the rank.
        assert_eq!(merged[0]["relevance"], json!(1.167)); // 1/6 + 1/1
        assert_eq!(merged[1]["relevance"], json!(1.143)); // 1/1 + 1/7
        assert_eq!(merged[2]["relevance"], json!(0.75)); //  1/2 + 1/4
                                                         // rerank_score keeps the raw provider signal where the model scored the row.
        assert_eq!(merged[0]["rerank_score"], json!(0.99));
        assert!(merged[1].get("rerank_score").is_none());
        assert_eq!(merged[2]["rerank_score"], json!(0.96));
        assert!(
            ids.contains(&"hybrid-0"),
            "hybrid head should remain available as the no-evict seatbelt"
        );
    }

    #[test]
    fn rerank_merge_allows_tail_promotion_without_dropping_the_hybrid_head() {
        let rows = (0..6)
            .map(|idx| {
                json!({
                    "id": format!("hybrid-{idx}"),
                    "text": format!("candidate {idx}"),
                    "relevance": 1.0 - (idx as f64 * 0.01),
                    "score": { "final": 1.0 - (idx as f64 * 0.01) }
                })
            })
            .collect::<Vec<_>>();

        let merged = merge_rerank_order_with_hybrid_floor(
            &rows,
            &[(5, 0.99), (4, 0.98), (2, 0.97), (1, 0.96)],
            3,
        );
        let ids = merged
            .iter()
            .map(|row| row["id"].as_str().expect("row id"))
            .collect::<Vec<_>>();

        assert!(
            ids.contains(&"hybrid-5"),
            "expanded rerank candidates must be able to promote into top_k: {ids:?}"
        );
        assert!(
            ids.contains(&"hybrid-0"),
            "the hybrid head is the no-evict seatbelt for prior top-k hits: {ids:?}"
        );
    }

    #[test]
    fn rerank_merge_never_evicts_hybrid_head_beyond_head_floor() {
        let rows = (0..6)
            .map(|idx| {
                json!({
                    "id": format!("hybrid-{idx}"),
                    "text": format!("candidate {idx}"),
                    "relevance": 1.0 - (idx as f64 * 0.01),
                    "score": { "final": 1.0 - (idx as f64 * 0.01) }
                })
            })
            .collect::<Vec<_>>();
        // rerank strongly prefers the TAIL — model ranks 5,4,3,2,1,0
        let order = vec![
            (5, 0.99_f64),
            (4, 0.98),
            (3, 0.97),
            (2, 0.96),
            (1, 0.95),
            (0, 0.94),
        ];
        let merged = merge_rerank_order_with_hybrid_floor(&rows, &order, 3);
        let ids: Vec<&str> = merged.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(merged.len(), 3, "output must be exactly top_k: {ids:?}");
        // head_floor for top_k=3 is 2 → hybrid-0 and hybrid-1 MUST survive
        assert!(
            ids.contains(&"hybrid-0"),
            "hybrid rank-1 must survive (head floor): {ids:?}"
        );
        assert!(
            ids.contains(&"hybrid-1"),
            "hybrid rank-2 must survive (head floor): {ids:?}"
        );
    }

    #[test]
    fn rerank_merge_relevance_matches_blend_sort_order() {
        let rows = (0..4)
            .map(|idx| {
                json!({
                    "id": format!("hybrid-{idx}"),
                    "text": format!("candidate {idx}"),
                    "relevance": 1.0 - (idx as f64 * 0.1),
                    "score": { "final": 1.0 - (idx as f64 * 0.1) }
                })
            })
            .collect::<Vec<_>>();
        let order = vec![(3, 0.99_f64)]; // only tail item 3 gets a rerank score
        let merged = merge_rerank_order_with_hybrid_floor(&rows, &order, 2);
        assert_eq!(merged.len(), 2);
        // relevance must be non-increasing down the output (matches sort order)
        let rels: Vec<f64> = merged
            .iter()
            .map(|r| r["relevance"].as_f64().unwrap())
            .collect();
        for w in rels.windows(2) {
            assert!(
                w[0] >= w[1] - 1e-9,
                "relevance must be non-increasing down the output: {rels:?}"
            );
        }
        // promoted tail item hybrid-3 keeps its raw provider score in rerank_score
        let promo = merged.iter().find(|r| r["id"].as_str() == Some("hybrid-3"));
        assert!(
            promo.is_some(),
            "hybrid-3 should promote: {:?}",
            merged.iter().map(|r| r["id"].as_str()).collect::<Vec<_>>()
        );
        if let Some(p) = promo {
            assert_eq!(p["rerank_score"].as_f64().unwrap(), 0.99);
        }
    }

    #[test]
    fn apply_blend_relevance_stamps_score_fields() {
        let row = json!({"id": "x", "score": {"final": 0.1}});
        let scored = apply_blend_relevance(row, 0.9876, Some(0.8));
        assert_eq!(scored["relevance"], json!(0.988), "blend rounded to 3dp");
        assert_eq!(scored["score"]["final"], json!(0.988), "final = blend");
        assert_eq!(
            scored["rerank_score"],
            json!(0.8),
            "raw provider score kept"
        );
    }

    #[test]
    fn apply_blend_relevance_without_rerank_score_omits_field() {
        let row = json!({"id": "y"});
        let scored = apply_blend_relevance(row, 0.5, None);
        assert_eq!(scored["relevance"], json!(0.5));
        assert!(
            scored.get("rerank_score").is_none(),
            "no rerank_score when None"
        );
    }
}
