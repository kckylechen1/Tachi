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
    /// #1454 slice 2: server-executed verification run (closed kind → argv
    /// table, server-observed head_sha). Side-effecting; NOT replay-safe.
    Run,
}

impl TachiVerifyAction {
    pub const ALL: &'static [Self] = &[
        Self::Start,
        Self::Record,
        Self::Status,
        Self::Board,
        Self::Run,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Record => "record",
            Self::Status => "status",
            Self::Board => "board",
            Self::Run => "run",
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
            "run" => Ok(Self::Run),
            other => Err(format!(
                "Invalid tachi_verify action '{other}'. Use 'start', 'record', 'status', 'board', or 'run'."
            )),
        }
    }
}

// ─── tachi_task ──────────────────────────────────────────────────────────────

/// Actions accepted by `tachi_task`.
///
/// GitHub PR lifecycle (`link_pr` / `pr_status` / `pr_handoff` / `release_note`)
/// is **not** accepted here — use `tachi_gh` (#757). Worker launch/wait left
/// Task in #1319-C2; cancellation uses `tachi_staff(action='cancel')`.
/// Route tuning left Task in #1426; use `tachi_tune` instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
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
    Brief,
}

/// Deserialize the wire action through the exact schema-advertised token set.
/// Exact retired tokens then use the canonical typed parser so their migration
/// guidance remains reachable at the MCP boundary; normalized aliases never
/// become accepted wire actions.
impl<'de> Deserialize<'de> for TachiTaskAction {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = String::deserialize(deserializer)?;
        if let Some(&action) = Self::PRIMARY.iter().find(|action| action.as_str() == wire) {
            return Ok(action);
        }

        if Self::is_explicit_retired_wire(&wire) {
            if let Err(error) = wire.parse::<Self>() {
                return Err(serde::de::Error::custom(error));
            }
        }

        Err(serde::de::Error::custom(Self::invalid_exact_wire_error(
            &wire,
        )))
    }
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
        Self::Brief,
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
            Self::Brief => "brief",
        }
    }

    /// Wire strings derived from [`PRIMARY`](Self::PRIMARY) — the single
    /// positive inventory for `tachi_task` (no separate `&[&str]` mirror).
    pub fn primary_wire_strings() -> Vec<&'static str> {
        Self::PRIMARY.iter().map(|action| action.as_str()).collect()
    }

    fn is_explicit_retired_wire(wire: &str) -> bool {
        matches!(
            wire,
            "dispatch"
                | "wait"
                | "cancel"
                | "plan"
                | "cycle_plan"
                | "recommend"
                | "refine_issues"
                | "merge"
                | "ux_matrix"
                | "briefing"
                | "doc_index"
                | "cycle_status"
                | "profiles"
                | "profile"
                | "card"
                | "build_references"
                | "close_loop"
                | "route_simulate"
                | "proposals"
                | "review_proposal"
                | "apply_proposals"
                | "link_pr"
                | "pr_status"
                | "pr_handoff"
                | "release_note"
        )
    }

    fn invalid_exact_wire_error(wire: &str) -> String {
        format!(
            "Invalid tachi_task action '{wire}'. Wire actions must exactly match one of: {}.",
            Self::primary_wire_strings().join(", ")
        )
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
            "brief" => Ok(Self::Brief),
            "briefing" | "doc_index" | "cycle_status" => Err(format!(
                "Invalid tachi_task action '{s}'. This Task action was retired by #1712 C1b; use tachi_task(action='brief') for the feature briefing or tachi_task(action='status', flow_id=..., issue_ref=..., or pr_ref=...) for the lifecycle read model."
            )),
            "profiles" | "profile" | "card" => Err(format!(
                "Invalid tachi_task action '{s}'. Model-facing profile/card inspection was retired by #1687 C1c; operators use `tachi card list` or `tachi card show <profile-id>`, evaluators use the append-only eval surface, and static profile admission remains a dispatch concern."
            )),
            "build_references" | "close_loop" => Err(format!(
                "Invalid tachi_task action '{s}'. Closure moved to tachi_gh(action='close_loop') by #1713; use dry_run=true there for the reference/promotion preview."
            )),
            "plan" | "cycle_plan" | "recommend" | "refine_issues" | "merge" | "ux_matrix" => {
                Err(format!(
                    "Invalid tachi_task action '{s}'. This Task action was retired by #1683 C1a; use the surviving tachi_task primary actions or the owning facade for that workflow."
                ))
            }
            // #1319-C2: dispatch/wait left Task (worker launch moved to
            // tachi_staff). Point callers at the canonical worker surface.
            "dispatch" | "wait" => Err(format!(
                "Invalid tachi_task action '{s}'. Worker launch/wait left Task in #1319-C2; use tachi_staff(action='start') to launch a worker and tachi_task(action='status') to read its state."
            )),
            "cancel" => Err(format!(
                "Invalid tachi_task action '{s}'. Cancellation uses tachi_staff(action='cancel'); provide dispatch_id and expected_status_revision."
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
            "brief",
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
    fn f1687_c1c_task_rejects_retired_profile_card_actions() {
        for retired in ["profiles", "profile", "card"] {
            let err = retired
                .parse::<TachiTaskAction>()
                .expect_err("retired profile/card action must not parse as tachi_task action");
            assert!(
                err.contains("#1687 C1c"),
                "error for {retired} should name #1687 C1c, got: {err}"
            );
            assert!(
                err.contains("tachi card"),
                "error for {retired} should point at the operator surface, got: {err}"
            );
        }
    }

    #[test]
    fn f1713_task_rejects_retired_closure_action_tokens() {
        for retired in ["build_references", "close_loop"] {
            let err = retired
                .parse::<TachiTaskAction>()
                .expect_err("retired closure action must not parse as tachi_task action");
            assert!(
                err.contains("#1713"),
                "error for {retired} should name #1713, got: {err}"
            );
        }
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
    fn f1712_c1b1_task_rejects_folded_action_tokens() {
        for retired in ["briefing", "doc_index", "cycle_status"] {
            let err = retired
                .parse::<TachiTaskAction>()
                .expect_err("folded action token must not parse as tachi_task action");
            assert!(
                err.contains("#1712 C1b"),
                "error for {retired} should name #1712 C1b, got: {err}"
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

    #[test]
    fn f1833_task_cancel_rejection_text_is_pinned_to_staff_cancel() {
        let error = "cancel"
            .parse::<TachiTaskAction>()
            .expect_err("retired cancel action must not parse as tachi_task action");
        assert_eq!(
            error,
            "Invalid tachi_task action 'cancel'. Cancellation uses tachi_staff(action='cancel'); provide dispatch_id and expected_status_revision."
        );
    }
}
