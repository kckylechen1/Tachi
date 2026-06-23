use serde_json::Value;

#[derive(Clone, Debug)]
pub(crate) struct PromptAssembly {
    pub prompt: String,
    pub capability_bundle: Value,
    pub feedback_rules: Value,
}
