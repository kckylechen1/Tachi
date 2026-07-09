//! Discrimination: SearchOptions.decay_policy reaches hybrid_search ranking.

use super::*;
use crate::scorer::{DecayPolicy, HybridWeights};
use crate::types::MemoryEntry;
use std::sync::Arc;

/// Prefer ids ending in `-hot` over otherwise equal lexical hits.
struct PreferHotIdDecay;

impl DecayPolicy for PreferHotIdDecay {
    fn score_decay(
        &self,
        entry: &MemoryEntry,
        _recall_config: &crate::RecallConfig,
        _access_ages: Option<&[f64]>,
    ) -> f64 {
        if entry.id.ends_with("-hot") {
            1.0
        } else {
            0.01
        }
    }
}

#[test]
fn hybrid_search_honors_injected_decay_policy() {
    let mut conn = setup();
    // Distinct text so FTS returns both; shared tokens keep lexical scores comparable.
    insert(
        &mut conn,
        "signal-cold",
        "alpha signal needle cold variant for decay policy ranking",
        &["alpha", "signal", "needle", "cold"],
    );
    insert(
        &mut conn,
        "signal-hot",
        "alpha signal needle hot variant for decay policy ranking",
        &["alpha", "signal", "needle", "hot"],
    );

    let weights = HybridWeights {
        semantic: 0.0,
        fts: 0.15,
        symbolic: 0.15,
        decay: 1.0,
        use_rrf: false,
    };

    let injected_opts = SearchOptions {
        top_k: 2,
        candidates_per_channel: 10,
        record_access: false,
        weights,
        mmr_threshold: None,
        decay_policy: Some(Arc::new(PreferHotIdDecay)),
        ..Default::default()
    };
    let injected = hybrid_search(&conn, "alpha signal needle", &injected_opts).unwrap();
    assert!(
        injected.len() >= 2,
        "expected both candidates, got {:?}",
        injected.iter().map(|r| &r.entry.id).collect::<Vec<_>>()
    );
    assert_eq!(
        injected[0].entry.id,
        "signal-hot",
        "injected decay policy must promote *-hot when decay weight dominates; got {:?}",
        injected
            .iter()
            .map(|r| (&r.entry.id, r.score.decay, r.score.final_score))
            .collect::<Vec<_>>()
    );
    assert!(
        injected[0].score.decay > injected[1].score.decay,
        "hot decay {} should exceed cold {}",
        injected[0].score.decay,
        injected[1].score.decay
    );
}
