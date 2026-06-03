//! Per-lane backend model tier selection (#151).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackendModelTier {
    Fast,
    Balanced,
    Quality,
}

impl BackendModelTier {
    pub(crate) fn from_env(key: &str) -> Option<Self> {
        let raw = std::env::var(key).ok()?;
        match raw.trim().to_ascii_lowercase().as_str() {
            "fast" | "cheap" => Some(Self::Fast),
            "balanced" | "mid" | "medium" => Some(Self::Balanced),
            "quality" | "premium" => Some(Self::Quality),
            _ => None,
        }
    }

    pub(crate) fn model_for_lane(self, lane: &str) -> Option<String> {
        let lane_key = lane.trim().to_ascii_uppercase();
        let tier_key = match self {
            Self::Fast => "FAST",
            Self::Balanced => "BALANCED",
            Self::Quality => "QUALITY",
        };
        let specific = format!("TACHI_BACKEND_{lane_key}_TIER_{tier_key}_MODEL");
        if let Ok(model) = std::env::var(&specific) {
            let model = model.trim().to_string();
            if !model.is_empty() {
                return Some(model);
            }
        }
        let global = format!("TACHI_BACKEND_{tier_key}_MODEL");
        std::env::var(&global)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .or_else(|| match self {
                Self::Fast => Some("Qwen/Qwen3.5-27B".to_string()),
                Self::Balanced => Some("Qwen/Qwen2.5-72B-Instruct".to_string()),
                Self::Quality => None,
            })
    }
}

pub(crate) fn resolve_lane_model(
    lane: &str,
    tier_env: &str,
    explicit_model: Option<String>,
    default_model: &str,
) -> String {
    if let Some(model) = explicit_model.filter(|m| !m.trim().is_empty()) {
        return model;
    }
    if let Some(tier) = BackendModelTier::from_env(tier_env) {
        if let Some(model) = tier.model_for_lane(lane) {
            return model;
        }
    }
    default_model.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_parse_and_fast_default() {
        std::env::set_var("TACHI_BACKEND_EXTRACT_TIER", "fast");
        assert_eq!(
            BackendModelTier::from_env("TACHI_BACKEND_EXTRACT_TIER"),
            Some(BackendModelTier::Fast)
        );
        assert_eq!(
            BackendModelTier::Fast.model_for_lane("extract").as_deref(),
            Some("Qwen/Qwen3.5-27B")
        );
        std::env::remove_var("TACHI_BACKEND_EXTRACT_TIER");
    }

    #[test]
    fn explicit_model_wins_over_tier() {
        std::env::set_var("TACHI_BACKEND_EXTRACT_TIER", "balanced");
        let model = resolve_lane_model(
            "extract",
            "TACHI_BACKEND_EXTRACT_TIER",
            Some("custom/model".to_string()),
            "default",
        );
        assert_eq!(model, "custom/model");
        std::env::remove_var("TACHI_BACKEND_EXTRACT_TIER");
    }
}