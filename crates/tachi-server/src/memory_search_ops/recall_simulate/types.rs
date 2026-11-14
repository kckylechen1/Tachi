use memcore::{HybridWeights, RecallConfig};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub(super) struct RecallSimCase {
    #[serde(default)]
    pub(super) name: Option<String>,
    pub(super) query: String,
    #[serde(default)]
    expected_id: Option<String>,
    #[serde(default)]
    expected_ids: Vec<String>,
    #[serde(default)]
    pub(super) top_k: Option<usize>,
    #[serde(default)]
    pub(super) scope: Option<String>,
    #[serde(default)]
    pub(super) project: Option<String>,
    #[serde(default)]
    pub(super) domain: Option<String>,
    #[serde(default)]
    pub(super) path_prefix: Option<String>,
    #[serde(default)]
    pub(super) include_archived: Option<bool>,
    #[serde(default)]
    pub(super) include_training: Option<bool>,
    #[serde(default)]
    pub(super) as_of: Option<String>,
}

impl RecallSimCase {
    pub(super) fn expected_ids(&self) -> Vec<String> {
        let mut expected = self.expected_ids.clone();
        if let Some(id) = self.expected_id.as_deref().map(str::trim) {
            if !id.is_empty() && !expected.iter().any(|candidate| candidate == id) {
                expected.push(id.to_string());
            }
        }
        expected
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RecallConfigOverrides {
    #[serde(default)]
    default_semantic: Option<f64>,
    #[serde(default)]
    default_fts: Option<f64>,
    #[serde(default)]
    default_symbolic: Option<f64>,
    #[serde(default)]
    default_decay: Option<f64>,
    #[serde(default)]
    default_use_rrf: Option<bool>,
    #[serde(default)]
    guide_semantic: Option<f64>,
    #[serde(default)]
    guide_fts: Option<f64>,
    #[serde(default)]
    guide_symbolic: Option<f64>,
    #[serde(default)]
    guide_decay: Option<f64>,
    #[serde(default)]
    guide_use_rrf: Option<bool>,
    #[serde(default)]
    wiki_semantic: Option<f64>,
    #[serde(default)]
    wiki_fts: Option<f64>,
    #[serde(default)]
    wiki_symbolic: Option<f64>,
    #[serde(default)]
    wiki_decay: Option<f64>,
    #[serde(default)]
    wiki_use_rrf: Option<bool>,
    #[serde(default)]
    events_notes_semantic: Option<f64>,
    #[serde(default)]
    events_notes_fts: Option<f64>,
    #[serde(default)]
    events_notes_symbolic: Option<f64>,
    #[serde(default)]
    events_notes_decay: Option<f64>,
    #[serde(default)]
    events_notes_use_rrf: Option<bool>,
    #[serde(default)]
    expanded_fts_score_factor: Option<f64>,
    #[serde(default)]
    max_expanded_fts_queries: Option<usize>,
    #[serde(default)]
    or_fallback_fts_score_factor: Option<f64>,
    #[serde(default)]
    or_fallback_fts_max_terms: Option<usize>,
    #[serde(default)]
    raw_half_life_days: Option<f64>,
    #[serde(default)]
    consolidated_half_life_days: Option<f64>,
    #[serde(default)]
    pattern_half_life_days: Option<f64>,
    #[serde(default)]
    id_like_exact_match_boost: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct RecallSimVariant {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    recall_config: RecallConfigOverrides,
    #[serde(flatten)]
    direct: RecallConfigOverrides,
}

impl RecallSimVariant {
    pub(super) fn configured_name(&self, idx: usize) -> String {
        self.name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("variant_{idx}"))
    }

    pub(super) fn to_recall_config(&self, base: &RecallConfig) -> RecallConfig {
        let mut config = base.clone();
        self.recall_config.apply(&mut config);
        self.direct.apply(&mut config);
        config.sanitized()
    }
}

impl RecallConfigOverrides {
    fn apply(&self, config: &mut RecallConfig) {
        apply_weight_overrides(
            &mut config.default_weights,
            self.default_semantic,
            self.default_fts,
            self.default_symbolic,
            self.default_decay,
            self.default_use_rrf,
        );
        apply_weight_overrides(
            &mut config.guide_weights,
            self.guide_semantic,
            self.guide_fts,
            self.guide_symbolic,
            self.guide_decay,
            self.guide_use_rrf,
        );
        apply_weight_overrides(
            &mut config.wiki_weights,
            self.wiki_semantic,
            self.wiki_fts,
            self.wiki_symbolic,
            self.wiki_decay,
            self.wiki_use_rrf,
        );
        apply_weight_overrides(
            &mut config.events_notes_weights,
            self.events_notes_semantic,
            self.events_notes_fts,
            self.events_notes_symbolic,
            self.events_notes_decay,
            self.events_notes_use_rrf,
        );
        if let Some(value) = self.expanded_fts_score_factor {
            config.expanded_fts_score_factor = value;
        }
        if let Some(value) = self.max_expanded_fts_queries {
            config.max_expanded_fts_queries = value;
        }
        if let Some(value) = self.or_fallback_fts_score_factor {
            config.or_fallback_fts_score_factor = value;
        }
        if let Some(value) = self.or_fallback_fts_max_terms {
            config.or_fallback_fts_max_terms = value;
        }
        if let Some(value) = self.raw_half_life_days {
            config.raw_half_life_days = value;
        }
        if let Some(value) = self.consolidated_half_life_days {
            config.consolidated_half_life_days = value;
        }
        if let Some(value) = self.pattern_half_life_days {
            config.pattern_half_life_days = value;
        }
        if let Some(value) = self.id_like_exact_match_boost {
            config.id_like_exact_match_boost = value;
        }
    }
}

fn apply_weight_overrides(
    weights: &mut HybridWeights,
    semantic: Option<f64>,
    fts: Option<f64>,
    symbolic: Option<f64>,
    decay: Option<f64>,
    use_rrf: Option<bool>,
) {
    if let Some(value) = semantic {
        weights.semantic = value;
    }
    if let Some(value) = fts {
        weights.fts = value;
    }
    if let Some(value) = symbolic {
        weights.symbolic = value;
    }
    if let Some(value) = decay {
        weights.decay = value;
    }
    if let Some(value) = use_rrf {
        weights.use_rrf = value;
    }
}
