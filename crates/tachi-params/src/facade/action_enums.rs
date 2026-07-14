//! F4 (#913): typed facade action enums (stringly `action: String` → enum).
//!
//! Wire format remains snake_case strings for MCP compatibility. JsonSchema for
//! `tachi_task` advertises **primary** actions only. GitHub PR lifecycle actions
//! live exclusively on `tachi_gh` (#757: removed from tachi_task).

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

// ─── tachi_verify ────────────────────────────────────────────────────────────

/// Actions accepted by `tachi_verify`.
///
/// MCP schema is still produced via `string_enum_schema` on the params field
/// (keeps description + inventory lists as the single source of truth).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TachiVerifyAction {
    Start,
    Record,
    Status,
    Board,
}

impl TachiVerifyAction {
    pub const ALL: &'static [Self] = &[Self::Start, Self::Record, Self::Status, Self::Board];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Record => "record",
            Self::Status => "status",
            Self::Board => "board",
        }
    }

    /// Wire strings derived from [`ALL`](Self::ALL) — the single positive
    /// inventory for `tachi_verify` (no separate `&[&str]` mirror).
    pub fn all_wire_strings() -> Vec<&'static str> {
        Self::ALL.iter().map(|action| action.as_str()).collect()
    }
}

impl fmt::Display for TachiVerifyAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TachiVerifyAction {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "start" => Ok(Self::Start),
            "record" => Ok(Self::Record),
            "status" => Ok(Self::Status),
            "board" => Ok(Self::Board),
            other => Err(format!(
                "Invalid tachi_verify action '{other}'. Use 'start', 'record', 'status', or 'board'."
            )),
        }
    }
}

// ─── tachi_task ──────────────────────────────────────────────────────────────

/// Actions accepted by `tachi_task`.
///
/// GitHub PR lifecycle (`link_pr` / `pr_status` / `pr_handoff` / `release_note`)
/// is **not** accepted here — use `tachi_gh` (#757).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TachiTaskAction {
    Plan,
    Briefing,
    DocIndex,
    Recommend,
    Dispatch,
    Complete,
    Profiles,
    Profile,
    Card,
    RouteSimulate,
    Proposals,
    ReviewProposal,
    ApplyProposals,
    Status,
    Cancel,
    Board,
    Wait,
    Merge,
    Intake,
    CycleStatus,
    CyclePlan,
    UxMatrix,
    BuildReferences,
    CloseLoop,
    RefineIssues,
    Adjudicate,
}

impl TachiTaskAction {
    /// Schema-advertised primary actions.
    pub const PRIMARY: &'static [Self] = &[
        Self::Plan,
        Self::Briefing,
        Self::DocIndex,
        Self::Recommend,
        Self::Dispatch,
        Self::Complete,
        Self::Profiles,
        Self::Profile,
        Self::Card,
        Self::RouteSimulate,
        Self::Proposals,
        Self::ReviewProposal,
        Self::ApplyProposals,
        Self::Status,
        Self::Cancel,
        Self::Board,
        Self::Wait,
        Self::Merge,
        Self::Intake,
        Self::CycleStatus,
        Self::CyclePlan,
        Self::UxMatrix,
        Self::BuildReferences,
        Self::CloseLoop,
        Self::RefineIssues,
        Self::Adjudicate,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Briefing => "briefing",
            Self::DocIndex => "doc_index",
            Self::Recommend => "recommend",
            Self::Dispatch => "dispatch",
            Self::Complete => "complete",
            Self::Profiles => "profiles",
            Self::Profile => "profile",
            Self::Card => "card",
            Self::RouteSimulate => "route_simulate",
            Self::Proposals => "proposals",
            Self::ReviewProposal => "review_proposal",
            Self::ApplyProposals => "apply_proposals",
            Self::Status => "status",
            Self::Cancel => "cancel",
            Self::Board => "board",
            Self::Wait => "wait",
            Self::Merge => "merge",
            Self::Intake => "intake",
            Self::CycleStatus => "cycle_status",
            Self::CyclePlan => "cycle_plan",
            Self::UxMatrix => "ux_matrix",
            Self::BuildReferences => "build_references",
            Self::CloseLoop => "close_loop",
            Self::RefineIssues => "refine_issues",
            Self::Adjudicate => "adjudicate",
        }
    }

    /// Wire strings derived from [`PRIMARY`](Self::PRIMARY) — the single
    /// positive inventory for `tachi_task` (no separate `&[&str]` mirror).
    pub fn primary_wire_strings() -> Vec<&'static str> {
        Self::PRIMARY.iter().map(|action| action.as_str()).collect()
    }
}

impl fmt::Display for TachiTaskAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TachiTaskAction {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "plan" => Ok(Self::Plan),
            "briefing" => Ok(Self::Briefing),
            "doc_index" => Ok(Self::DocIndex),
            "recommend" => Ok(Self::Recommend),
            "dispatch" => Ok(Self::Dispatch),
            "complete" => Ok(Self::Complete),
            "profiles" => Ok(Self::Profiles),
            "profile" => Ok(Self::Profile),
            "card" => Ok(Self::Card),
            "route_simulate" => Ok(Self::RouteSimulate),
            "proposals" => Ok(Self::Proposals),
            "review_proposal" => Ok(Self::ReviewProposal),
            "apply_proposals" => Ok(Self::ApplyProposals),
            "status" => Ok(Self::Status),
            "cancel" => Ok(Self::Cancel),
            "board" => Ok(Self::Board),
            "wait" => Ok(Self::Wait),
            "merge" => Ok(Self::Merge),
            "intake" => Ok(Self::Intake),
            "cycle_status" => Ok(Self::CycleStatus),
            "cycle_plan" => Ok(Self::CyclePlan),
            "ux_matrix" => Ok(Self::UxMatrix),
            "build_references" => Ok(Self::BuildReferences),
            "close_loop" => Ok(Self::CloseLoop),
            "refine_issues" => Ok(Self::RefineIssues),
            "adjudicate" => Ok(Self::Adjudicate),
            // #757: these were removed from tachi_task; point callers at tachi_gh.
            "link_pr" | "pr_status" | "pr_handoff" | "release_note" => Err(format!(
                "Invalid tachi_task action '{s}'. GitHub PR lifecycle actions live on tachi_gh(action='{s}')."
            )),
            other => Err(format!(
                "Invalid tachi_task action '{other}'. See primary task actions or use tachi_gh for GitHub PR lifecycle."
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::action_inventory::TACHI_TASK_REMOVED_GH_LIFECYCLE_ACTIONS;
    use super::*;

    #[test]
    fn f4_verify_action_roundtrip() {
        for &action in TachiVerifyAction::ALL {
            let s = action.as_str();
            let parsed: TachiVerifyAction = s.parse().expect("verify action");
            assert_eq!(parsed, action);
            let wire = serde_json::to_string(&parsed).unwrap();
            assert_eq!(wire, format!("\"{s}\""));
            let back: TachiVerifyAction = serde_json::from_str(&wire).unwrap();
            assert_eq!(back, action);
        }
        assert!("nope".parse::<TachiVerifyAction>().is_err());
    }

    #[test]
    fn f4_task_action_primary_roundtrip() {
        for &action in TachiTaskAction::PRIMARY {
            let s = action.as_str();
            let parsed: TachiTaskAction = s.parse().expect("task primary");
            assert_eq!(parsed, action);
            let wire = serde_json::to_string(&parsed).unwrap();
            assert_eq!(wire, format!("\"{s}\""));
        }
        assert!("nope".parse::<TachiTaskAction>().is_err());
    }

    #[test]
    fn f757_task_rejects_removed_gh_lifecycle_actions() {
        for &s in TACHI_TASK_REMOVED_GH_LIFECYCLE_ACTIONS {
            let err = s
                .parse::<TachiTaskAction>()
                .expect_err("removed lifecycle must not parse as tachi_task action");
            assert!(
                err.contains("tachi_gh"),
                "error for {s} should point at tachi_gh, got: {err}"
            );
        }
    }
}
