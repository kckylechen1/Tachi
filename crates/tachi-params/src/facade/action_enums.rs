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
/// is **not** accepted here — use `tachi_gh` (#757). Worker launch/wait/cancel
/// left Task in #1319-C2; use `tachi_staff(action='start'|'status')` instead.
/// Route tuning left Task in #1426; use `tachi_tune` instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TachiTaskAction {
    Intake,
    Claim,
    Heartbeat,
    Handoff,
    Release,
    Board,
    Status,
    Complete,
    Adjudicate,
    Briefing,
    DocIndex,
    CycleStatus,
    Profiles,
    Profile,
    Card,
    BuildReferences,
    CloseLoop,
}

impl TachiTaskAction {
    /// Schema-advertised primary actions.
    pub const PRIMARY: &'static [Self] = &[
        Self::Intake,
        Self::Claim,
        Self::Heartbeat,
        Self::Handoff,
        Self::Release,
        Self::Board,
        Self::Status,
        Self::Complete,
        Self::Adjudicate,
        Self::Briefing,
        Self::DocIndex,
        Self::CycleStatus,
        Self::Profiles,
        Self::Profile,
        Self::Card,
        Self::BuildReferences,
        Self::CloseLoop,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Intake => "intake",
            Self::Claim => "claim",
            Self::Heartbeat => "heartbeat",
            Self::Handoff => "handoff",
            Self::Release => "release",
            Self::Board => "board",
            Self::Status => "status",
            Self::Complete => "complete",
            Self::Adjudicate => "adjudicate",
            Self::Briefing => "briefing",
            Self::DocIndex => "doc_index",
            Self::CycleStatus => "cycle_status",
            Self::Profiles => "profiles",
            Self::Profile => "profile",
            Self::Card => "card",
            Self::BuildReferences => "build_references",
            Self::CloseLoop => "close_loop",
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
            "intake" => Ok(Self::Intake),
            "claim" => Ok(Self::Claim),
            "heartbeat" => Ok(Self::Heartbeat),
            "handoff" => Ok(Self::Handoff),
            "release" => Ok(Self::Release),
            "board" => Ok(Self::Board),
            "status" => Ok(Self::Status),
            "complete" => Ok(Self::Complete),
            "adjudicate" => Ok(Self::Adjudicate),
            "briefing" => Ok(Self::Briefing),
            "doc_index" => Ok(Self::DocIndex),
            "cycle_status" => Ok(Self::CycleStatus),
            "profiles" => Ok(Self::Profiles),
            "profile" => Ok(Self::Profile),
            "card" => Ok(Self::Card),
            "build_references" => Ok(Self::BuildReferences),
            "close_loop" => Ok(Self::CloseLoop),
            "plan" | "cycle_plan" | "recommend" | "refine_issues" | "merge" | "ux_matrix" => {
                Err(format!(
                    "Invalid tachi_task action '{s}'. This Task action was retired by #1683 C1a; use the surviving tachi_task primary actions or the owning facade for that workflow."
                ))
            }
            // #1319-C2: dispatch/wait/cancel left Task (worker launch moved to
            // tachi_staff). Point callers at the canonical worker surface.
            "dispatch" | "wait" | "cancel" => Err(format!(
                "Invalid tachi_task action '{s}'. Worker launch/wait/cancel left Task in #1319-C2; use tachi_staff(action='start') to launch a worker and tachi_task(action='status') to read its state."
            )),
            // #1426: route tuning moved behind the admin/operator tachi_tune
            // surface. Keep typed rejects explicit so old callers get the new
            // destination instead of a generic unknown-action error.
            "route_simulate" => Err(format!(
                "Invalid tachi_task action '{s}'. Route tuning left Task in #1426; use tachi_tune(action='route_simulate')."
            )),
            "proposals" => Err(format!(
                "Invalid tachi_task action '{s}'. Route tuning left Task in #1426; use tachi_tune(action='route_proposals')."
            )),
            "review_proposal" => Err(format!(
                "Invalid tachi_task action '{s}'. Route tuning left Task in #1426; use tachi_tune(action='route_review')."
            )),
            "apply_proposals" => Err(format!(
                "Invalid tachi_task action '{s}'. Route tuning left Task in #1426; use tachi_tune(action='route_apply')."
            )),
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
        let expected = [
            "intake",
            "claim",
            "heartbeat",
            "handoff",
            "release",
            "board",
            "status",
            "complete",
            "adjudicate",
            "briefing",
            "doc_index",
            "cycle_status",
            "profiles",
            "profile",
            "card",
            "build_references",
            "close_loop",
        ];
        assert_eq!(TachiTaskAction::primary_wire_strings(), expected);
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
    fn f1683_c1a_task_rejects_retired_primary_actions() {
        for retired in [
            "plan",
            "cycle_plan",
            "recommend",
            "refine_issues",
            "merge",
            "ux_matrix",
        ] {
            let err = retired
                .parse::<TachiTaskAction>()
                .expect_err("retired C1a action must not parse as tachi_task action");
            assert!(
                err.contains("#1683 C1a"),
                "retired action {retired} error should name #1683 C1a, got: {err}"
            );
        }
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

    #[test]
    fn f1426_task_rejects_removed_route_tuning_actions() {
        for (retired, target) in [
            ("route_simulate", "route_simulate"),
            ("proposals", "route_proposals"),
            ("review_proposal", "route_review"),
            ("apply_proposals", "route_apply"),
        ] {
            let err = retired
                .parse::<TachiTaskAction>()
                .expect_err("removed route tuning action must not parse as tachi_task action");
            assert!(
                err.contains(&format!("tachi_tune(action='{target}')")),
                "error for {retired} should point at tachi_tune {target}, got: {err}"
            );
        }
    }
}
