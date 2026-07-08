use serde::Serialize;

pub const GLM_CODING_MODEL_ALIAS: &str = "glm_coding";
pub const GLM_CODING_DEFAULT_MODEL: &str = "zhipuai-coding-plan/glm-5.2";
pub const GLM_CODING_ENV_OVERRIDE: &str = "TACHI_DISPATCH_GLM_CODING_MODEL";

#[derive(Debug, Clone, Copy, Serialize)]
pub struct DispatchModelCard {
    pub alias: &'static str,
    pub vendor: &'static str,
    pub role: &'static str,
    pub default_model: &'static str,
    pub env_override: &'static str,
}

pub const GLM_CODING_MODEL_CARD: DispatchModelCard = DispatchModelCard {
    alias: GLM_CODING_MODEL_ALIAS,
    vendor: "glm",
    role: "executor",
    default_model: GLM_CODING_DEFAULT_MODEL,
    env_override: GLM_CODING_ENV_OVERRIDE,
};

pub const DISPATCH_MODEL_CARDS: &[DispatchModelCard] = &[GLM_CODING_MODEL_CARD];

pub fn dispatch_model_card(alias: &str) -> Option<&'static DispatchModelCard> {
    let norm = alias.trim().to_ascii_lowercase();
    DISPATCH_MODEL_CARDS.iter().find(|card| card.alias == norm)
}

pub fn resolve_dispatch_model(alias: &str) -> Option<String> {
    let card = dispatch_model_card(alias)?;
    Some(resolve_dispatch_model_card(card, None))
}

pub fn resolve_dispatch_model_with_config(
    alias: &str,
    config_override: Option<&str>,
) -> Option<String> {
    let card = dispatch_model_card(alias)?;
    Some(resolve_dispatch_model_card(card, config_override))
}

pub fn resolve_dispatch_model_card(
    card: &DispatchModelCard,
    config_override: Option<&str>,
) -> String {
    config_override
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var(card.env_override)
                .ok()
                .map(|model| model.trim().to_string())
                .filter(|model| !model.is_empty())
        })
        .unwrap_or_else(|| card.default_model.to_string())
}
