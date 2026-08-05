use super::string_enum_schema;
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::str::FromStr;

fn tachi_tune_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &TachiTuneAction::all_wire_strings(),
        "Required Tachi tuning action. Admin/operator-only surface for route and recall tuning lifecycle actions.",
        generator,
    )
}

fn default_tune_top_k() -> usize {
    6
}

/// Actions accepted by `tachi_tune`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TachiTuneAction {
    RouteSimulate,
    RouteProposals,
    RouteReview,
    RouteApply,
    RecallSimulate,
    RecallProposals,
    RecallReview,
    RecallApply,
}

impl TachiTuneAction {
    pub const ALL: &'static [Self] = &[
        Self::RouteSimulate,
        Self::RouteProposals,
        Self::RouteReview,
        Self::RouteApply,
        Self::RecallSimulate,
        Self::RecallProposals,
        Self::RecallReview,
        Self::RecallApply,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::RouteSimulate => "route_simulate",
            Self::RouteProposals => "route_proposals",
            Self::RouteReview => "route_review",
            Self::RouteApply => "route_apply",
            Self::RecallSimulate => "recall_simulate",
            Self::RecallProposals => "recall_proposals",
            Self::RecallReview => "recall_review",
            Self::RecallApply => "recall_apply",
        }
    }

    pub fn all_wire_strings() -> Vec<&'static str> {
        Self::ALL.iter().map(|action| action.as_str()).collect()
    }
}

impl fmt::Display for TachiTuneAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TachiTuneAction {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "route_simulate" => Ok(Self::RouteSimulate),
            "route_proposals" => Ok(Self::RouteProposals),
            "route_review" => Ok(Self::RouteReview),
            "route_apply" => Ok(Self::RouteApply),
            "recall_simulate" => Ok(Self::RecallSimulate),
            "recall_proposals" => Ok(Self::RecallProposals),
            "recall_review" => Ok(Self::RecallReview),
            "recall_apply" => Ok(Self::RecallApply),
            other => Err(format!(
                "Invalid tachi_tune action '{other}'. Use 'route_simulate', 'route_proposals', 'route_review', 'route_apply', 'recall_simulate', 'recall_proposals', 'recall_review', or 'recall_apply'."
            )),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TachiTuneParams {
    #[schemars(schema_with = "tachi_tune_action_schema")]
    pub action: TachiTuneAction,
    #[serde(default, alias = "output_format")]
    pub format: Option<String>,

    // Route tuning fields.
    #[serde(default)]
    #[schemars(description = "[action=route_simulate] Optional task/prompt focus.")]
    pub task: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=route_simulate] Declared side-effect level L0-L3.")]
    pub execution_level: Option<super::ExecutionLevel>,
    #[serde(default)]
    #[schemars(description = "[action=route_simulate] Documentation paths used as routing evidence.")]
    pub doc_paths: Vec<String>,
    #[serde(default)]
    #[schemars(description = "[action=route_simulate] Spec paths used as routing evidence.")]
    pub spec_paths: Vec<String>,
    #[serde(default)]
    #[schemars(description = "[action=route_simulate] Risk override: low, medium, high, or critical.")]
    pub risk: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=route_simulate|route_proposals] Maximum rows/proposals.")]
    pub limit: Option<usize>,
    #[serde(default)]
    #[schemars(description = "[action=route_proposals|recall_proposals] Optional proposal status filter.")]
    pub state_filter: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=route_review|route_apply|recall_review|recall_apply] Proposal id.")]
    pub proposal_id: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=route_review|recall_review] Review status: approved or rejected.")]
    pub review_status: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=route_review|recall_review] Optional review note.")]
    pub notes: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=route_apply|recall_apply] Required true to apply an approved proposal.")]
    pub confirm: bool,

    // Recall tuning fields.
    #[serde(default = "default_tune_top_k")]
    #[schemars(description = "[action=recall_simulate|recall_proposals] Default top_k for labeled cases.")]
    pub top_k: usize,
    #[serde(default)]
    #[schemars(description = "[action=recall_simulate|recall_proposals] Cases/eval_cases and optional variants.")]
    pub metadata: Option<Value>,
    #[serde(default)]
    #[schemars(description = "[action=recall_simulate|recall_proposals] JSON cases/variants text fallback.")]
    pub text: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=recall_simulate|recall_proposals] Enable adaptive reranking during replay.")]
    pub enable_rerank: bool,
    #[serde(default)]
    #[schemars(schema_with = "super::memory_scope_schema")]
    pub scope: Option<String>,
    #[serde(default)]
    pub path_prefix: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub file_context: Option<String>,
    #[serde(default)]
    pub error_context: Option<String>,
    #[serde(default)]
    pub include_archived: bool,
    #[serde(default)]
    pub include_training: bool,
    #[serde(default)]
    #[schemars(description = "[action=recall_proposals] Persist variants even when they do not improve metrics.")]
    pub force: bool,
    #[serde(default)]
    pub as_of: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tune_action_roundtrip_is_exhaustive() {
        for &action in TachiTuneAction::ALL {
            let s = action.as_str();
            let parsed: TachiTuneAction = s.parse().expect("tune action");
            assert_eq!(parsed, action);
            let wire = serde_json::to_string(&parsed).unwrap();
            assert_eq!(wire, format!("\"{s}\""));
            let back: TachiTuneAction = serde_json::from_str(&wire).unwrap();
            assert_eq!(back, action);
        }
        assert_eq!(TachiTuneAction::ALL.len(), 8);
    }

    #[test]
    fn tune_action_rejects_old_route_and_recall_review_aliases() {
        for retired in [
            "proposals",
            "review_proposal",
            "apply_proposals",
            "review_recall_proposal",
            "apply_recall_proposals",
        ] {
            let err = retired
                .parse::<TachiTuneAction>()
                .expect_err("retired action spelling must not parse as tachi_tune");
            assert!(
                err.contains("Invalid tachi_tune action"),
                "retired spelling {retired} should be rejected, got: {err}"
            );
        }
    }
}
