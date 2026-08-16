use super::*;

pub(super) fn default_card_priority() -> String {
    "medium".to_string()
}

pub(super) fn default_card_type() -> String {
    "request".to_string()
}

fn default_include_broadcast() -> bool {
    true
}

fn default_inbox_limit() -> usize {
    100
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct PostCardParams {
    pub from_agent: String,
    pub to_agent: String,
    pub title: String,
    pub body: String,
    #[serde(default = "default_card_priority")]
    pub priority: String,
    #[serde(default = "default_card_type")]
    pub card_type: String,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub agent_session_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct CheckInboxParams {
    pub agent_id: String,
    #[serde(default)]
    pub status_filter: Option<String>,
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default = "default_include_broadcast")]
    pub include_broadcast: bool,
    #[serde(default = "default_inbox_limit")]
    pub limit: usize,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub conversation_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct UpdateCardParams {
    pub card_id: String,
    pub new_status: String,
    #[serde(default)]
    pub response_text: Option<String>,
}
