use memcore::{HybridWeights, RecallConfig};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(super) fn recall_config_env_diff(
    base: &RecallConfig,
    candidate: &RecallConfig,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    insert_weight_env_diff(
        &mut out,
        "TACHI_RECALL_DEFAULT",
        &base.default_weights,
        &candidate.default_weights,
    );
    insert_weight_env_diff(
        &mut out,
        "TACHI_RECALL_GUIDE",
        &base.guide_weights,
        &candidate.guide_weights,
    );
    insert_weight_env_diff(
        &mut out,
        "TACHI_RECALL_WIKI",
        &base.wiki_weights,
        &candidate.wiki_weights,
    );
    insert_weight_env_diff(
        &mut out,
        "TACHI_RECALL_EVENTS_NOTES",
        &base.events_notes_weights,
        &candidate.events_notes_weights,
    );
    insert_f64_env_diff(
        &mut out,
        "TACHI_RECALL_EXPANDED_FTS_SCORE_FACTOR",
        base.expanded_fts_score_factor,
        candidate.expanded_fts_score_factor,
    );
    insert_usize_env_diff(
        &mut out,
        "TACHI_RECALL_MAX_EXPANDED_FTS_QUERIES",
        base.max_expanded_fts_queries,
        candidate.max_expanded_fts_queries,
    );
    insert_f64_env_diff(
        &mut out,
        "TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR",
        base.or_fallback_fts_score_factor,
        candidate.or_fallback_fts_score_factor,
    );
    insert_usize_env_diff(
        &mut out,
        "TACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS",
        base.or_fallback_fts_max_terms,
        candidate.or_fallback_fts_max_terms,
    );
    insert_f64_env_diff(
        &mut out,
        "TACHI_RECALL_RAW_HALF_LIFE_DAYS",
        base.raw_half_life_days,
        candidate.raw_half_life_days,
    );
    insert_f64_env_diff(
        &mut out,
        "TACHI_RECALL_CONSOLIDATED_HALF_LIFE_DAYS",
        base.consolidated_half_life_days,
        candidate.consolidated_half_life_days,
    );
    insert_f64_env_diff(
        &mut out,
        "TACHI_RECALL_PATTERN_HALF_LIFE_DAYS",
        base.pattern_half_life_days,
        candidate.pattern_half_life_days,
    );
    insert_f64_env_diff(
        &mut out,
        "TACHI_RECALL_ID_LIKE_EXACT_MATCH_BOOST",
        base.id_like_exact_match_boost,
        candidate.id_like_exact_match_boost,
    );
    out
}

fn insert_weight_env_diff(
    out: &mut BTreeMap<String, String>,
    prefix: &str,
    base: &HybridWeights,
    candidate: &HybridWeights,
) {
    insert_f64_env_diff(
        out,
        &format!("{prefix}_SEMANTIC"),
        base.semantic,
        candidate.semantic,
    );
    insert_f64_env_diff(out, &format!("{prefix}_FTS"), base.fts, candidate.fts);
    insert_f64_env_diff(
        out,
        &format!("{prefix}_SYMBOLIC"),
        base.symbolic,
        candidate.symbolic,
    );
    insert_f64_env_diff(out, &format!("{prefix}_DECAY"), base.decay, candidate.decay);
    if base.use_rrf != candidate.use_rrf {
        out.insert(format!("{prefix}_USE_RRF"), candidate.use_rrf.to_string());
    }
}

fn insert_f64_env_diff(out: &mut BTreeMap<String, String>, key: &str, base: f64, candidate: f64) {
    if (base - candidate).abs() > f64::EPSILON {
        out.insert(key.to_string(), format_env_f64(candidate));
    }
}

fn insert_usize_env_diff(
    out: &mut BTreeMap<String, String>,
    key: &str,
    base: usize,
    candidate: usize,
) {
    if base != candidate {
        out.insert(key.to_string(), candidate.to_string());
    }
}

fn format_env_f64(value: f64) -> String {
    let mut out = format!("{value:.6}");
    while out.contains('.') && out.ends_with('0') {
        out.pop();
    }
    if out.ends_with('.') {
        out.pop();
    }
    out
}

pub(super) fn recall_config_summary(config: &RecallConfig) -> Value {
    json!({
        "default_weights": {
            "semantic": config.default_weights.semantic,
            "fts": config.default_weights.fts,
            "symbolic": config.default_weights.symbolic,
            "decay": config.default_weights.decay,
            "use_rrf": config.default_weights.use_rrf,
        },
        "guide_weights": {
            "semantic": config.guide_weights.semantic,
            "fts": config.guide_weights.fts,
            "symbolic": config.guide_weights.symbolic,
            "decay": config.guide_weights.decay,
            "use_rrf": config.guide_weights.use_rrf,
        },
        "wiki_weights": {
            "semantic": config.wiki_weights.semantic,
            "fts": config.wiki_weights.fts,
            "symbolic": config.wiki_weights.symbolic,
            "decay": config.wiki_weights.decay,
            "use_rrf": config.wiki_weights.use_rrf,
        },
        "events_notes_weights": {
            "semantic": config.events_notes_weights.semantic,
            "fts": config.events_notes_weights.fts,
            "symbolic": config.events_notes_weights.symbolic,
            "decay": config.events_notes_weights.decay,
            "use_rrf": config.events_notes_weights.use_rrf,
        },
        "expanded_fts_score_factor": config.expanded_fts_score_factor,
        "max_expanded_fts_queries": config.max_expanded_fts_queries,
        "or_fallback_fts_score_factor": config.or_fallback_fts_score_factor,
        "or_fallback_fts_max_terms": config.or_fallback_fts_max_terms,
        "raw_half_life_days": config.raw_half_life_days,
        "consolidated_half_life_days": config.consolidated_half_life_days,
        "pattern_half_life_days": config.pattern_half_life_days,
        "id_like_exact_match_boost": config.id_like_exact_match_boost,
    })
}
