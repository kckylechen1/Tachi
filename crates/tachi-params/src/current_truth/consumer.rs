//! The #1693 consumer read surface (#1696 discrimination 11).
//!
//! Downstream projections (the Unified Work Read Model, handoff consumers)
//! read **this module only**: typed view rows carrying evidence heads and
//! source revisions, with no raw [`crate::current_truth::types::AssertionV1`]
//! in the surface. The boundary is real, not documentation: a consumer that
//! imports this module never needs the assertion type, the store row shape,
//! or the reducer internals.
//!
//! Visibility is enforced here (#1696 discrimination 12): an unauthorized
//! caller receives no private subject tokens, refs, or counts — private
//! subjects are filtered before aggregation, so even the health numbers
//! cannot be used to infer their existence.

use serde::Serialize;

use super::projection::{projection_health, OpenActionV1, ProjectionHealthV1};
use super::reducer::ReductionV1;
use super::store::{CurrentTruthSqliteStore, CurrentTruthStoreError, RefreshPostureRowV1};
use super::types::{GithubObjectRefV1, PredicateV1, ReductionStatusV1, VisibilityClassV1};

/// Caller authorization for consumer reads. `sees_private` is granted by the
/// owning server surface, not self-declared by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallerAuthorizationV1 {
    pub sees_private: bool,
}

/// One reduced predicate as a consumer sees it: status, value token, and
/// evidence heads — enough to cite revisions, never enough to re-reduce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PredicateViewV1 {
    pub predicate: PredicateV1,
    pub status: ReductionStatusV1,
    /// Predicate-specific value, rendered as a stable display token
    /// (`unit`, the object token, the SHA, or the action token).
    pub value_token: String,
    pub evidence_heads: Vec<EvidenceHeadViewV1>,
}

/// One evidence head as a consumer sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceHeadViewV1 {
    pub assertion_id: String,
    pub source: String,
    pub source_revision: String,
    pub observed_at: String,
}

/// One subject's current-truth row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubjectTruthViewV1 {
    /// The subject's stable token (`owner/repo#issue:N`).
    pub subject_token: String,
    pub predicates: Vec<PredicateViewV1>,
    pub open_action: Option<OpenActionV1>,
}

/// The whole repository view: posture, subjects, health.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CurrentTruthViewV1 {
    pub repo: String,
    pub posture: RefreshPostureRowV1,
    pub subjects: Vec<SubjectTruthViewV1>,
    pub health: ProjectionHealthV1,
}

/// Error reading a consumer view.
#[derive(Debug, thiserror::Error)]
pub enum ConsumerViewError {
    #[error("store error: {0}")]
    Store(#[from] CurrentTruthStoreError),
    #[error("no refresh posture recorded for `{0}` — state is unknown, not fresh")]
    NoPosture(String),
}

/// Read the consumer view for one repository. The posture comes from the
/// store's recorded refresh metadata; a repository with no recorded posture
/// is **unknown, never fresh** (#1696: GitHub unavailable ⇒ unknown/stale,
/// never guessed current).
pub fn read_view(
    store: &CurrentTruthSqliteStore,
    repo: &str,
    authorization: CallerAuthorizationV1,
) -> Result<CurrentTruthViewV1, ConsumerViewError> {
    let posture = store
        .refresh_posture_row(repo)?
        .ok_or_else(|| ConsumerViewError::NoPosture(repo.to_string()))?;
    let assertions = store.assertions_for_repo(repo)?;
    let visible: Vec<_> = assertions
        .into_iter()
        .filter(|assertion| {
            authorization.sees_private || assertion.visibility == VisibilityClassV1::Public
        })
        .collect();
    let reduction = super::reducer::reduce(&visible);
    let refresh_debt = usize::from(!posture.fresh);
    let health = projection_health(&reduction, 0, refresh_debt);
    let posture_for_actions = if posture.fresh {
        super::projection::RefreshPostureV1::fresh(
            posture.last_fresh_revision.clone().unwrap_or_default(),
            posture.last_fresh_at.clone().unwrap_or_default(),
            posture.last_attempt_at.clone(),
        )
    } else {
        super::projection::RefreshPostureV1::unavailable(
            posture
                .unavailable_reason
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            posture.last_fresh_revision.clone(),
            posture.last_fresh_at.clone(),
            posture.last_attempt_at.clone(),
        )
    };

    let mut subjects: Vec<SubjectTruthViewV1> = reduction
        .subjects()
        .into_iter()
        .map(|subject| {
            let predicates = super::projection::ALL_SOURCE_PREDICATES
                .iter()
                .map(|predicate| predicate_view(&reduction, &subject, *predicate))
                .collect();
            let open_action = if matches!(subject.object, GithubObjectRefV1::Issue(_)) {
                Some(super::projection::open_action_for(
                    &reduction,
                    &posture_for_actions,
                    &subject,
                ))
            } else {
                None
            };
            SubjectTruthViewV1 {
                subject_token: subject.as_token(),
                predicates,
                open_action,
            }
        })
        .collect();
    subjects.sort_by(|a, b| a.subject_token.cmp(&b.subject_token));

    Ok(CurrentTruthViewV1 {
        repo: repo.to_string(),
        posture,
        subjects,
        health,
    })
}

fn predicate_view(
    reduction: &ReductionV1,
    subject: &super::types::SubjectRefV1,
    predicate: PredicateV1,
) -> PredicateViewV1 {
    let reduced = reduction.get(subject, predicate);
    PredicateViewV1 {
        predicate,
        status: reduced.status,
        value_token: reduced
            .values
            .first()
            .map(value_token)
            .unwrap_or_else(|| "unknown".to_string()),
        evidence_heads: reduced
            .current_heads
            .iter()
            .map(|head| EvidenceHeadViewV1 {
                assertion_id: head.assertion_id.clone(),
                source: head.source.clone(),
                source_revision: head.source_revision.clone(),
                observed_at: head.observed_at.clone(),
            })
            .collect(),
    }
}

fn value_token(value: &super::types::AssertionValueV1) -> String {
    use super::types::AssertionValueV1;
    match value {
        AssertionValueV1::Unit => "unit".to_string(),
        AssertionValueV1::ObjectRef(object) => object.as_token(),
        AssertionValueV1::ObjectRefs(objects) => objects
            .iter()
            .map(|object| object.as_token())
            .collect::<Vec<_>>()
            .join(","),
        AssertionValueV1::CommitSha(sha) => sha.clone(),
        AssertionValueV1::HandoffId(id) => id.clone(),
        AssertionValueV1::Action(action) => action.as_str().to_string(),
    }
}
