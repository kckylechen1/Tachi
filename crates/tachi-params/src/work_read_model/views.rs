//! Bounded consumer views over the unified projection (#1693 consumer
//! contracts). Every view is a **pure function** of one
//! [`WorkReadModelV1`]: board rows, status rows, and read-only briefs all
//! carry the SAME work token, revision fingerprint, and derived state —
//! there is no independent board/status/brief store behind them.
//!
//! `brief` composes already-projected data only: it cannot plan, mutate,
//! or fetch. It renders section summaries; it never invents state.

use crate::current_truth::projection::ActionOwnerClassV1;

use super::types::{SectionState, WorkReadModelV1};

/// One board row: the work token, a deterministic column token, blocker
/// count, and the top pending action (highest frozen priority). Derived
/// entirely from the model — the board is a view, not a lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkBoardRowV1 {
    pub work_token: String,
    pub revision: String,
    pub column: String,
    pub blocker_count: usize,
    pub top_action: Option<String>,
}

/// One status row: per-section state tokens plus staleness, for status
/// surfaces that need one line per work item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkStatusRowV1 {
    pub work_token: String,
    pub revision: String,
    pub read_at: String,
    pub github: String,
    pub claim: String,
    pub run: String,
    pub adjudication: String,
    pub delivery: String,
    pub success_shaped: bool,
}

/// One read-only brief: the same identity/revision plus bounded section
/// summaries composed from projected data only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkBriefV1 {
    pub work_token: String,
    pub revision: String,
    pub read_at: String,
    pub sections: Vec<(String, String)>,
}

/// Render the board row for one work item.
pub fn board_view(model: &WorkReadModelV1) -> WorkBoardRowV1 {
    WorkBoardRowV1 {
        work_token: model.work_token(),
        revision: model.revision.clone(),
        column: column_token(model),
        blocker_count: model.blockers.len(),
        top_action: model
            .next_actions
            .first()
            .map(|action| action.kind.as_str().to_string()),
    }
}

/// Render the status row for one work item.
pub fn status_view(model: &WorkReadModelV1) -> WorkStatusRowV1 {
    WorkStatusRowV1 {
        work_token: model.work_token(),
        revision: model.revision.clone(),
        read_at: model.read_at.clone(),
        github: github_status_token(model),
        claim: section_token(&model.claim, |section| {
            section
                .claims
                .iter()
                .map(|row| {
                    format!(
                        "{}:{}",
                        row.fact.claim_id,
                        match row.effectively_expired {
                            Some(true) => "expired".to_string(),
                            _ => row.fact.state.as_str().to_string(),
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join(",")
        }),
        run: section_token(&model.run, |section| {
            section
                .runs
                .iter()
                .map(|row| format!("{}:{}", row.fact.dispatch_id, row.execution_state.as_str()))
                .collect::<Vec<_>>()
                .join(",")
        }),
        adjudication: section_token(&model.adjudication, |section| {
            adjudication_token(&section.state)
        }),
        delivery: model.delivery.observation.as_str().to_string(),
        success_shaped: model.success_shaped,
    }
}

/// Render the read-only brief for one work item.
pub fn brief_view(model: &WorkReadModelV1) -> WorkBriefV1 {
    let sections = vec![
        ("github".to_string(), github_status_token(model)),
        (
            "blockers".to_string(),
            model
                .blockers
                .iter()
                .map(|blocker| blocker.kind.as_str().to_string())
                .collect::<Vec<_>>()
                .join(","),
        ),
        (
            "next_actions".to_string(),
            model
                .next_actions
                .iter()
                .map(|action| action.kind.as_str().to_string())
                .collect::<Vec<_>>()
                .join(","),
        ),
        (
            "delivery".to_string(),
            model.delivery.observation.as_str().to_string(),
        ),
    ];
    WorkBriefV1 {
        work_token: model.work_token(),
        revision: model.revision.clone(),
        read_at: model.read_at.clone(),
        sections,
    }
}

/// The frozen board-column vocabulary. Columns are derived from established
/// state only — a worker submit never lands in a completion column.
fn column_token(model: &WorkReadModelV1) -> String {
    if model.success_shaped {
        return "complete".to_string();
    }
    if model
        .blockers
        .iter()
        .any(|blocker| blocker.kind == super::types::BlockerKindV1::GithubConflict)
    {
        return "conflicted".to_string();
    }
    match &model.github {
        SectionState::Available(section) => {
            if !section.posture_fresh {
                "stale".to_string()
            } else {
                match section.implementation_status {
                    super::types::ImplementationStatusV1::UnderReview => "under_review".to_string(),
                    super::types::ImplementationStatusV1::Present => "landed".to_string(),
                    // An outstanding revert debt is repair-blocked, never
                    // presented as landed work (R6-2 owner ruling).
                    super::types::ImplementationStatusV1::Reverted => "reverted".to_string(),
                    _ => "in_flight".to_string(),
                }
            }
        }
        SectionState::Unavailable { .. } => "unknown".to_string(),
        SectionState::NotApplicable => {
            if model
                .next_actions
                .iter()
                .any(|action| action.owner_class == ActionOwnerClassV1::Worker)
            {
                "in_flight".to_string()
            } else {
                "unknown".to_string()
            }
        }
    }
}

fn github_status_token(model: &WorkReadModelV1) -> String {
    match &model.github {
        SectionState::Available(section) => format!(
            "{}:{}",
            section.repo,
            section.implementation_status.as_str()
        ),
        SectionState::Unavailable { .. } => "unavailable".to_string(),
        SectionState::NotApplicable => "not_applicable".to_string(),
    }
}

fn section_token<T>(section: &SectionState<T>, render: impl Fn(&T) -> String) -> String {
    match section {
        SectionState::Available(value) => render(value),
        SectionState::Unavailable { .. } => "unavailable".to_string(),
        SectionState::NotApplicable => "not_applicable".to_string(),
    }
}

fn adjudication_token(
    state: &crate::taskintent::mapping::adjudication::AdjudicationState,
) -> String {
    use crate::taskintent::mapping::adjudication::AdjudicationState as S;
    match state {
        S::Unreviewed => "unreviewed".to_string(),
        S::Accepted => "accepted".to_string(),
        S::Rejected => "rejected".to_string(),
        S::NotRequired { .. } => "not_required".to_string(),
        S::NeedsFollowUp => "needs_follow_up".to_string(),
        S::Inconsistent => "inconsistent".to_string(),
    }
}
