use crate::scorer::HybridWeights;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

const DEFAULT_EXPANDED_FTS_SCORE_FACTOR: f64 = 0.78;
const DEFAULT_MAX_EXPANDED_FTS_QUERIES: usize = 6;
const DEFAULT_RAW_HALF_LIFE_DAYS: f64 = 30.0;
const DEFAULT_CONSOLIDATED_HALF_LIFE_DAYS: f64 = 60.0;
const DEFAULT_PATTERN_HALF_LIFE_DAYS: f64 = 30_000.0;
const DEFAULT_ID_LIKE_EXACT_MATCH_BOOST: f64 = 12.0;
const DEFAULT_OR_FALLBACK_FTS_SCORE_FACTOR: f64 = 0.0;
const DEFAULT_OR_FALLBACK_FTS_MAX_TERMS: usize = 8;

#[derive(Debug, Clone, PartialEq)]
pub struct RecallConfig {
    pub default_weights: HybridWeights,
    pub guide_weights: HybridWeights,
    pub wiki_weights: HybridWeights,
    pub events_notes_weights: HybridWeights,
    pub expanded_fts_score_factor: f64,
    pub max_expanded_fts_queries: usize,
    pub raw_half_life_days: f64,
    pub consolidated_half_life_days: f64,
    pub pattern_half_life_days: f64,
    pub id_like_exact_match_boost: f64,
    pub or_fallback_fts_score_factor: f64,
    pub or_fallback_fts_max_terms: usize,
}

impl Default for RecallConfig {
    fn default() -> Self {
        Self {
            default_weights: HybridWeights::default(),
            guide_weights: HybridWeights {
                decay: 0.02,
                semantic: 0.25,
                fts: 0.45,
                symbolic: 0.28,
                use_rrf: true,
            },
            wiki_weights: HybridWeights {
                decay: 0.02,
                semantic: 0.48,
                fts: 0.30,
                symbolic: 0.20,
                use_rrf: true,
            },
            events_notes_weights: HybridWeights {
                decay: 0.25,
                semantic: 0.35,
                fts: 0.25,
                symbolic: 0.15,
                use_rrf: true,
            },
            expanded_fts_score_factor: DEFAULT_EXPANDED_FTS_SCORE_FACTOR,
            max_expanded_fts_queries: DEFAULT_MAX_EXPANDED_FTS_QUERIES,
            raw_half_life_days: DEFAULT_RAW_HALF_LIFE_DAYS,
            consolidated_half_life_days: DEFAULT_CONSOLIDATED_HALF_LIFE_DAYS,
            pattern_half_life_days: DEFAULT_PATTERN_HALF_LIFE_DAYS,
            id_like_exact_match_boost: DEFAULT_ID_LIKE_EXACT_MATCH_BOOST,
            or_fallback_fts_score_factor: DEFAULT_OR_FALLBACK_FTS_SCORE_FACTOR,
            or_fallback_fts_max_terms: DEFAULT_OR_FALLBACK_FTS_MAX_TERMS,
        }
    }
}

impl RecallConfig {
    pub fn get() -> &'static RecallConfig {
        static CONFIG: OnceLock<RecallConfig> = OnceLock::new();
        CONFIG.get_or_init(Self::load)
    }

    pub fn load() -> RecallConfig {
        if env_truthy("TACHI_TEST_DISABLE_RECALL_CONFIG") || cfg!(test) {
            return Self::default();
        }
        let mut config = Self::default();
        if let Some(path) = config_env_path() {
            if let Ok(body) = std::fs::read_to_string(&path) {
                config.apply_config_env(&parse_config_env(&body));
            }
        }
        config.apply_config_env(&process_recall_env());
        config.sanitized()
    }

    pub fn weights_for_path(&self, path: &str) -> HybridWeights {
        if path.starts_with("/guide") {
            self.guide_weights.clone()
        } else if path.starts_with("/wiki")
            || path.starts_with("/behavior")
            || path.starts_with("/rules")
        {
            self.wiki_weights.clone()
        } else if path.starts_with("/events") || path.starts_with("/notes") {
            self.events_notes_weights.clone()
        } else {
            self.default_weights.clone()
        }
    }

    pub fn half_life_days_for_tier(&self, tier: &str) -> f64 {
        match tier {
            "pattern" => self.pattern_half_life_days,
            "consolidated" => self.consolidated_half_life_days,
            _ => self.raw_half_life_days,
        }
    }

    fn apply_config_env(&mut self, values: &HashMap<String, String>) {
        apply_weight_env(values, "TACHI_RECALL_DEFAULT", &mut self.default_weights);
        apply_weight_env(values, "TACHI_RECALL_GUIDE", &mut self.guide_weights);
        apply_weight_env(values, "TACHI_RECALL_WIKI", &mut self.wiki_weights);
        apply_weight_env(
            values,
            "TACHI_RECALL_EVENTS_NOTES",
            &mut self.events_notes_weights,
        );
        apply_f64(
            values,
            "TACHI_RECALL_EXPANDED_FTS_SCORE_FACTOR",
            &mut self.expanded_fts_score_factor,
        );
        apply_usize(
            values,
            "TACHI_RECALL_MAX_EXPANDED_FTS_QUERIES",
            &mut self.max_expanded_fts_queries,
        );
        apply_f64(
            values,
            "TACHI_RECALL_RAW_HALF_LIFE_DAYS",
            &mut self.raw_half_life_days,
        );
        apply_f64(
            values,
            "TACHI_RECALL_CONSOLIDATED_HALF_LIFE_DAYS",
            &mut self.consolidated_half_life_days,
        );
        apply_f64(
            values,
            "TACHI_RECALL_PATTERN_HALF_LIFE_DAYS",
            &mut self.pattern_half_life_days,
        );
        apply_f64(
            values,
            "TACHI_RECALL_ID_LIKE_EXACT_MATCH_BOOST",
            &mut self.id_like_exact_match_boost,
        );
        apply_f64(
            values,
            "TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR",
            &mut self.or_fallback_fts_score_factor,
        );
        apply_usize(
            values,
            "TACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS",
            &mut self.or_fallback_fts_max_terms,
        );
    }

    pub fn sanitized(mut self) -> Self {
        let defaults = Self::default();
        sanitize_weights(&mut self.default_weights, &defaults.default_weights);
        sanitize_weights(&mut self.guide_weights, &defaults.guide_weights);
        sanitize_weights(&mut self.wiki_weights, &defaults.wiki_weights);
        sanitize_weights(
            &mut self.events_notes_weights,
            &defaults.events_notes_weights,
        );
        self.expanded_fts_score_factor = finite_or_default(
            self.expanded_fts_score_factor,
            DEFAULT_EXPANDED_FTS_SCORE_FACTOR,
        )
        .clamp(0.0, 1.0);
        self.max_expanded_fts_queries = self.max_expanded_fts_queries.max(1).min(64);
        self.raw_half_life_days =
            positive_or_default(self.raw_half_life_days, DEFAULT_RAW_HALF_LIFE_DAYS);
        self.consolidated_half_life_days = positive_or_default(
            self.consolidated_half_life_days,
            DEFAULT_CONSOLIDATED_HALF_LIFE_DAYS,
        );
        self.pattern_half_life_days =
            positive_or_default(self.pattern_half_life_days, DEFAULT_PATTERN_HALF_LIFE_DAYS);
        self.id_like_exact_match_boost = positive_or_default(
            self.id_like_exact_match_boost,
            DEFAULT_ID_LIKE_EXACT_MATCH_BOOST,
        );
        self.or_fallback_fts_score_factor = finite_or_default(
            self.or_fallback_fts_score_factor,
            DEFAULT_OR_FALLBACK_FTS_SCORE_FACTOR,
        )
        .clamp(0.0, 1.0);
        self.or_fallback_fts_max_terms = self.or_fallback_fts_max_terms.max(1).min(32);
        self
    }
}

fn apply_weight_env(values: &HashMap<String, String>, prefix: &str, weights: &mut HybridWeights) {
    apply_f64(values, &format!("{prefix}_SEMANTIC"), &mut weights.semantic);
    apply_f64(values, &format!("{prefix}_FTS"), &mut weights.fts);
    apply_f64(values, &format!("{prefix}_SYMBOLIC"), &mut weights.symbolic);
    apply_f64(values, &format!("{prefix}_DECAY"), &mut weights.decay);
    apply_bool(values, &format!("{prefix}_USE_RRF"), &mut weights.use_rrf);
}

fn apply_f64(values: &HashMap<String, String>, key: &str, target: &mut f64) {
    if let Some(value) = values.get(key).and_then(|raw| raw.parse::<f64>().ok()) {
        *target = value;
    }
}

fn apply_usize(values: &HashMap<String, String>, key: &str, target: &mut usize) {
    if let Some(value) = values.get(key).and_then(|raw| raw.parse::<usize>().ok()) {
        *target = value;
    }
}

fn apply_bool(values: &HashMap<String, String>, key: &str, target: &mut bool) {
    if let Some(value) = values.get(key) {
        match value.trim() {
            "1" | "true" | "TRUE" | "True" | "yes" | "YES" | "on" | "ON" => *target = true,
            "0" | "false" | "FALSE" | "False" | "no" | "NO" | "off" | "OFF" => *target = false,
            _ => {}
        }
    }
}

fn sanitize_weights(weights: &mut HybridWeights, defaults: &HybridWeights) {
    weights.semantic = finite_or_default(weights.semantic, defaults.semantic).clamp(0.0, 1.0);
    weights.fts = finite_or_default(weights.fts, defaults.fts).clamp(0.0, 1.0);
    weights.symbolic = finite_or_default(weights.symbolic, defaults.symbolic).clamp(0.0, 1.0);
    weights.decay = finite_or_default(weights.decay, defaults.decay).clamp(0.0, 1.0);
}

fn positive_or_default(value: f64, default: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        default
    }
}

fn finite_or_default(value: f64, default: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        default
    }
}

fn process_recall_env() -> HashMap<String, String> {
    std::env::vars()
        .filter(|(key, _)| key.starts_with("TACHI_RECALL_"))
        .collect()
}

fn parse_config_env(body: &str) -> HashMap<String, String> {
    body.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            let (key, value) = trimmed.split_once('=')?;
            let key = key.trim();
            if !key.starts_with("TACHI_RECALL_") {
                return None;
            }
            Some((key.to_string(), unquote_env_value(value.trim()).to_string()))
        })
        .collect()
}

fn unquote_env_value(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
        .unwrap_or(value)
}

fn config_env_path() -> Option<PathBuf> {
    let app_home = match std::env::var_os("TACHI_HOME") {
        Some(raw) if !raw.is_empty() => {
            let raw = PathBuf::from(raw);
            if raw.starts_with("~") {
                let home = std::env::var_os("HOME").map(PathBuf::from)?;
                home.join(raw.strip_prefix("~").ok()?)
            } else {
                raw
            }
        }
        _ => std::env::var_os("HOME").map(PathBuf::from)?.join(".tachi"),
    };
    Some(app_home.join("config.env"))
}

fn env_truthy(key: &str) -> bool {
    matches!(
        std::env::var(key).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("on")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_preserve_prior_hardcoded_values() {
        let config = RecallConfig::default();
        assert_eq!(config.default_weights, HybridWeights::default());
        assert_eq!(config.guide_weights.fts, 0.45);
        assert_eq!(config.wiki_weights.semantic, 0.48);
        assert_eq!(config.events_notes_weights.decay, 0.25);
        assert_eq!(
            config.expanded_fts_score_factor,
            DEFAULT_EXPANDED_FTS_SCORE_FACTOR
        );
        assert_eq!(
            config.max_expanded_fts_queries,
            DEFAULT_MAX_EXPANDED_FTS_QUERIES
        );
        assert_eq!(config.raw_half_life_days, DEFAULT_RAW_HALF_LIFE_DAYS);
        assert_eq!(
            config.id_like_exact_match_boost,
            DEFAULT_ID_LIKE_EXACT_MATCH_BOOST
        );
        assert_eq!(
            config.or_fallback_fts_score_factor,
            DEFAULT_OR_FALLBACK_FTS_SCORE_FACTOR
        );
        assert_eq!(
            config.or_fallback_fts_max_terms,
            DEFAULT_OR_FALLBACK_FTS_MAX_TERMS
        );
    }

    #[test]
    fn config_env_overrides_recall_surface() {
        let values = parse_config_env(
            r#"
            TACHI_RECALL_DEFAULT_SEMANTIC=0.41
            TACHI_RECALL_GUIDE_FTS=0.51
            TACHI_RECALL_WIKI_USE_RRF=false
            TACHI_RECALL_EXPANDED_FTS_SCORE_FACTOR=0.66
            TACHI_RECALL_MAX_EXPANDED_FTS_QUERIES=9
            TACHI_RECALL_ID_LIKE_EXACT_MATCH_BOOST=15
            TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.22
            TACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=4
            "#,
        );
        let mut config = RecallConfig::default();
        config.apply_config_env(&values);
        let config = config.sanitized();

        assert_eq!(config.default_weights.semantic, 0.41);
        assert_eq!(config.guide_weights.fts, 0.51);
        assert!(!config.wiki_weights.use_rrf);
        assert_eq!(config.expanded_fts_score_factor, 0.66);
        assert_eq!(config.max_expanded_fts_queries, 9);
        assert_eq!(config.id_like_exact_match_boost, 15.0);
        assert_eq!(config.or_fallback_fts_score_factor, 0.22);
        assert_eq!(config.or_fallback_fts_max_terms, 4);
    }

    #[test]
    fn invalid_config_values_are_sanitized() {
        let values = parse_config_env(
            r#"
            TACHI_RECALL_DEFAULT_FTS=2.5
            TACHI_RECALL_DEFAULT_DECAY=NaN
            TACHI_RECALL_WIKI_USE_RRF=maybe
            TACHI_RECALL_EXPANDED_FTS_SCORE_FACTOR=-1
            TACHI_RECALL_MAX_EXPANDED_FTS_QUERIES=0
            TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=NaN
            TACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=0
            "#,
        );
        let mut config = RecallConfig::default();
        config.apply_config_env(&values);
        let config = config.sanitized();

        assert_eq!(config.default_weights.fts, 1.0);
        assert_eq!(
            config.default_weights.decay,
            RecallConfig::default().default_weights.decay
        );
        assert!(config.wiki_weights.use_rrf);
        assert_eq!(config.expanded_fts_score_factor, 0.0);
        assert_eq!(config.max_expanded_fts_queries, 1);
        assert_eq!(
            config.or_fallback_fts_score_factor,
            RecallConfig::default().or_fallback_fts_score_factor
        );
        assert_eq!(config.or_fallback_fts_max_terms, 1);
    }
}
