use crate::scorer::HybridWeights;
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::OnceLock;

pub const MAX_RECALL_CONFIG_ENV_BYTES: usize = 1024 * 1024;

const DEFAULT_EXPANDED_FTS_SCORE_FACTOR: f64 = 0.78;
const DEFAULT_MAX_EXPANDED_FTS_QUERIES: usize = 6;
const DEFAULT_RAW_HALF_LIFE_DAYS: f64 = 30.0;
const DEFAULT_CONSOLIDATED_HALF_LIFE_DAYS: f64 = 60.0;
const DEFAULT_PATTERN_HALF_LIFE_DAYS: f64 = 30_000.0;
const DEFAULT_ID_LIKE_EXACT_MATCH_BOOST: f64 = 12.0;
// Provisional tachi#708 Phase B calibration. Gate 1 mechanical readout on the
// adversarial corpus: 0 hit→miss, 1 miss→hit, 30 unchanged.
const DEFAULT_OR_FALLBACK_FTS_SCORE_FACTOR: f64 = 0.55;
const DEFAULT_OR_FALLBACK_FTS_MAX_TERMS: usize = 8;
// Phase C lever (#708): lower k sharpens RRF so single-channel precision wins more often.
const DEFAULT_RRF_K: f64 = 20.0;
// Provisional #1242: raw-tier vector hits below this cosine-similarity floor are
// dropped from the vector channel only (FTS/symbolic unaffected). Calibrated
// below typical good-hit band (~0.41–0.48) to cut clearly-weak raw matches.
const DEFAULT_RAW_VECTOR_SIMILARITY_FLOOR: f64 = 0.35;

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
    /// Reciprocal Rank Fusion k (classic is 60). Lower values amplify top ranks.
    pub rrf_k: f64,
    /// Minimum vector similarity for raw-tier rows in the vector channel (provisional).
    pub raw_vector_similarity_floor: f64,
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
            rrf_k: DEFAULT_RRF_K,
            raw_vector_similarity_floor: DEFAULT_RAW_VECTOR_SIMILARITY_FLOOR,
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
            if let Ok(body) = read_config_env_bounded(&path) {
                config = Self::from_config_env_source(&body);
            }
        }
        config.apply_config_env(&process_recall_env());
        config.sanitized()
    }

    /// Parse the same config.env source consumed by production `load` without
    /// consulting process-global paths or environment variables. Later
    /// declarations win because `parse_config_env` collects into a HashMap.
    pub fn from_config_env_source(body: &str) -> RecallConfig {
        let mut config = Self::default();
        config.apply_config_env(&parse_config_env(body));
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
        apply_f64(values, "TACHI_RECALL_RRF_K", &mut self.rrf_k);
        apply_f64(
            values,
            "TACHI_RECALL_RAW_VECTOR_SIMILARITY_FLOOR",
            &mut self.raw_vector_similarity_floor,
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
        self.max_expanded_fts_queries = self.max_expanded_fts_queries.clamp(1, 64);
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
        self.or_fallback_fts_max_terms = self.or_fallback_fts_max_terms.clamp(1, 32);
        if !self.rrf_k.is_finite() || self.rrf_k < 1.0 {
            self.rrf_k = DEFAULT_RRF_K;
        }
        self.rrf_k = self.rrf_k.clamp(1.0, 200.0);
        self.raw_vector_similarity_floor = finite_or_default(
            self.raw_vector_similarity_floor,
            DEFAULT_RAW_VECTOR_SIMILARITY_FLOOR,
        )
        .clamp(0.0, 1.0);
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

fn read_config_env_bounded(path: &std::path::Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if metadata.len() > MAX_RECALL_CONFIG_ENV_BYTES as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "recall config.env exceeds maximum size",
        ));
    }
    let mut body = String::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take((MAX_RECALL_CONFIG_ENV_BYTES + 1) as u64)
        .read_to_string(&mut body)?;
    if body.len() > MAX_RECALL_CONFIG_ENV_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "recall config.env exceeds maximum size",
        ));
    }
    Ok(body)
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

/// #1096 leaf-2a: three-key precedence (`TACHI_HOME` → `SIGIL_HOME` →
/// `TACHI_APP_HOME`), matching the order of the two `tachi_home()` funnels in
/// `tachi-server`/`tachi-llm`. This used to check ONLY `TACHI_HOME` — a
/// process launched with just `SIGIL_HOME` or `TACHI_APP_HOME` set (as those
/// two crates' own funnels honor) silently read recall config from the
/// wrong default (`$HOME/.tachi/config.env`) instead of the home the rest of
/// the app actually resolved to. See `config_env_path_uses_three_key_home_precedence`
/// below for the RED-before-fix regression this closes.
///
/// #1096 leaf-2a round-2 (codex C3-recall_config): the skip test on each key
/// must match the canonical funnel's skip semantics
/// (`path_utils::home::tachi_home` in `tachi-server`), which reads via
/// `std::env::var` (UTF-8 only) and skips when the TRIMMED value is empty.
/// This used to read via `var_os` and only skip on a byte-empty `OsString`,
/// so a whitespace-only value like `SIGIL_HOME="   "` was NOT skipped here
/// (unlike the canonical funnel, which moves on to the next key) — the app's
/// resolved home and this crate's recall-config home silently disagreed.
fn config_env_path() -> Option<PathBuf> {
    for key in ["TACHI_HOME", "SIGIL_HOME", "TACHI_APP_HOME"] {
        let Ok(value) = std::env::var(key) else {
            continue;
        };
        if value.trim().is_empty() {
            continue;
        }
        let raw = PathBuf::from(value);
        let app_home = if raw.starts_with("~") {
            let home = std::env::var_os("HOME").map(PathBuf::from)?;
            home.join(raw.strip_prefix("~").ok()?)
        } else {
            raw
        };
        return Some(app_home.join("config.env"));
    }
    let app_home = std::env::var_os("HOME").map(PathBuf::from)?.join(".tachi");
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

    /// RAII env-var restore for the `config_env_path` precedence tests below.
    ///
    /// #1096 leaf-2a round-2 (codex C5): the prior version of
    /// `config_env_path_uses_three_key_home_precedence` saved/cleared env at
    /// the top and restored it via a plain closure call at the bottom — if
    /// any assertion in between panicked, the restore call never ran and the
    /// temp `TACHI_HOME`/`SIGIL_HOME`/`TACHI_APP_HOME`/`HOME` overrides leaked
    /// into every test that runs after it in the same process (`memcore`'s
    /// tests run in parallel by default, so this can poison an unrelated
    /// sibling test, not just a rerun). Restoring via `Drop` survives a panic
    /// during unwinding, same guarantee `tachi-server`'s `EnvRestore` gives.
    struct EnvVarSnapshotRestore {
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl EnvVarSnapshotRestore {
        /// Snapshot each key's current value, clear it, and return a guard
        /// that restores the snapshot on drop (including panic unwinding).
        fn capture_and_clear(keys: &[&'static str]) -> Self {
            let saved: Vec<(&'static str, Option<std::ffi::OsString>)> = keys
                .iter()
                .map(|key| (*key, std::env::var_os(key)))
                .collect();
            for key in keys {
                std::env::remove_var(key);
            }
            Self { saved }
        }
    }

    impl Drop for EnvVarSnapshotRestore {
        fn drop(&mut self) {
            for (key, value) in &self.saved {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

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
        // tachi#708 Gate 1 intentionally changed the factory default from
        // off to the provisional 0.55 coverage channel.
        assert_eq!(
            config.or_fallback_fts_score_factor,
            DEFAULT_OR_FALLBACK_FTS_SCORE_FACTOR
        );
        assert_eq!(
            config.or_fallback_fts_max_terms,
            DEFAULT_OR_FALLBACK_FTS_MAX_TERMS
        );
        assert_eq!(
            config.raw_vector_similarity_floor,
            DEFAULT_RAW_VECTOR_SIMILARITY_FLOOR
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
            TACHI_RECALL_RAW_VECTOR_SIMILARITY_FLOOR=0.28
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
        assert_eq!(config.raw_vector_similarity_floor, 0.28);
    }

    #[test]
    fn production_config_parser_uses_last_duplicate_recall_declaration() {
        let config = RecallConfig::from_config_env_source(
            "TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\n\
             TACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=2\n\
             TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.6\n\
             TACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=4\n",
        );

        assert_eq!(config.or_fallback_fts_score_factor, 0.6);
        assert_eq!(config.or_fallback_fts_max_terms, 4);
    }

    #[test]
    fn production_config_reader_accepts_size_boundary_and_refuses_one_byte_over() {
        let temp = tempfile::tempdir().expect("config tempdir");
        let path = temp.path().join("config.env");
        let boundary = "x".repeat(MAX_RECALL_CONFIG_ENV_BYTES);
        std::fs::write(&path, &boundary).expect("write boundary config");
        assert_eq!(
            read_config_env_bounded(&path)
                .expect("read exact boundary")
                .len(),
            MAX_RECALL_CONFIG_ENV_BYTES
        );

        std::fs::write(&path, format!("{boundary}x")).expect("write over-limit config");
        let err = read_config_env_bounded(&path).expect_err("over-limit config must refuse");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
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
            TACHI_RECALL_RAW_VECTOR_SIMILARITY_FLOOR=NaN
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
        assert_eq!(
            config.raw_vector_similarity_floor,
            DEFAULT_RAW_VECTOR_SIMILARITY_FLOOR
        );
    }

    /// #1096 leaf-2a. Against the pre-fix `config_env_path()` (which only
    /// ever read `TACHI_HOME`, unconditionally falling back to
    /// `$HOME/.tachi` otherwise) this test is RED at the `TACHI_APP_HOME`-only
    /// step below: that step's expected path is under `app_home_dir`, but the
    /// old implementation — seeing no `TACHI_HOME` — would have resolved
    /// `home_dir.join(".tachi")` instead, so the assertion would fail.
    /// Single test function (not split across several `#[test]`s) so the env
    /// mutations below are strictly sequential and never race another test
    /// thread touching the same process-global vars — this crate has no
    /// existing env-guard/lock convention to reuse for this. Restore is via
    /// `EnvVarSnapshotRestore`'s `Drop` (round-2 C5), not a postlude call, so
    /// a failing assertion below still restores env instead of leaking the
    /// temp overrides into whichever sibling test runs next.
    #[test]
    fn config_env_path_uses_three_key_home_precedence() {
        let _restore = EnvVarSnapshotRestore::capture_and_clear(&[
            "TACHI_HOME",
            "SIGIL_HOME",
            "TACHI_APP_HOME",
            "HOME",
        ]);

        let home_dir = tempfile::tempdir().expect("home tempdir");
        let sigil_home_dir = tempfile::tempdir().expect("sigil home tempdir");
        let app_home_dir = tempfile::tempdir().expect("app home tempdir");
        let tachi_home_dir = tempfile::tempdir().expect("tachi home tempdir");

        // No TACHI_HOME/SIGIL_HOME/TACHI_APP_HOME set: falls back to
        // $HOME/.tachi, same as before this fix.
        std::env::set_var("HOME", home_dir.path());
        assert_eq!(
            config_env_path(),
            Some(home_dir.path().join(".tachi").join("config.env"))
        );

        // TACHI_APP_HOME alone: the old single-key implementation never read
        // this var, so it would still have resolved $HOME/.tachi here. This
        // assertion is the RED case referenced above.
        std::env::set_var("TACHI_APP_HOME", app_home_dir.path());
        assert_eq!(
            config_env_path(),
            Some(app_home_dir.path().join("config.env"))
        );

        // SIGIL_HOME set alongside TACHI_APP_HOME: SIGIL_HOME wins (matches
        // the funnel's TACHI_HOME > SIGIL_HOME > TACHI_APP_HOME order).
        std::env::set_var("SIGIL_HOME", sigil_home_dir.path());
        assert_eq!(
            config_env_path(),
            Some(sigil_home_dir.path().join("config.env"))
        );

        // TACHI_HOME set alongside both: TACHI_HOME wins over everything.
        std::env::set_var("TACHI_HOME", tachi_home_dir.path());
        assert_eq!(
            config_env_path(),
            Some(tachi_home_dir.path().join("config.env"))
        );
    }

    /// #1096 leaf-2a round-2 (codex C3-recall_config): RED against the
    /// pre-fix `config_env_path()`, which read via `var_os` and only skipped
    /// a key on byte-empty `OsString` — a whitespace-only value like
    /// `SIGIL_HOME="   "` was NOT byte-empty, so the old code would have used
    /// `PathBuf::from("   ")` as `app_home` (a bogus non-empty path) instead
    /// of falling through to the next key/default, silently disagreeing with
    /// the canonical funnel (`path_utils::home::tachi_home` in
    /// `tachi-server`), which reads via `env::var` (UTF-8) and skips on
    /// TRIMMED-empty. Restore via `EnvVarSnapshotRestore`'s `Drop`, same as
    /// the sibling precedence test above.
    #[test]
    fn config_env_path_skips_whitespace_only_home_value() {
        let _restore = EnvVarSnapshotRestore::capture_and_clear(&[
            "TACHI_HOME",
            "SIGIL_HOME",
            "TACHI_APP_HOME",
            "HOME",
        ]);

        let home_dir = tempfile::tempdir().expect("home tempdir");
        std::env::set_var("HOME", home_dir.path());
        std::env::set_var("SIGIL_HOME", "   ");

        assert_eq!(
            config_env_path(),
            Some(home_dir.path().join(".tachi").join("config.env")),
            "whitespace-only SIGIL_HOME must be treated as unset, matching the \
             canonical funnel's trim-then-empty-check skip semantics"
        );
    }
}
