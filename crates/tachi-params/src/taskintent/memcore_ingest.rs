//! Ingestion adapter: projects existing `dispatch_outcomes` truth onto the
//! bridge fact log (tachi#1840). This is the concrete "projection over
//! canonical truth" seam — it READS the canonical row (via the existing
//! `memcore::db::dispatch_outcomes` API) and appends a derived
//! [`TaskEvent`]; it writes no canonical state and owns no storage.
//!
//! Idempotent by construction: the derived event id is
//! `outcome-{outcome_id}-{updated_at}`, so re-ingesting the same row (e.g.
//! after a reconnect) produces the SAME `(seq, event_id)` and is
//! deterministically suppressed by the derivation (TB-9).

use memcore::db::dispatch_outcomes as outcomes_db;
use rusqlite::Connection;

use super::bridge::TaskFactSource;
use super::events::{
    EventSource, OutcomeObservation, TaskEvent, TaskEventPayload, VisibilityClass,
};
use super::mapping::execution::CanonicalExecutionFact;
use super::refs::TaskRef;

/// Typed ingest failure.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    /// The canonical store read failed.
    #[error("canonical dispatch_outcomes read failed: {0}")]
    Store(#[from] memcore::error::MemoryError),
    /// The bridge fact log is unavailable (TB-20).
    #[error("bridge fact source unavailable")]
    Unavailable,
}

/// Map the canonical `dispatch_outcomes.execution_outcome` vocabulary
/// (`completed`/`failed`/`aborted`/`partial` — the machine-resolved verdict
/// AFTER the #878-A completion predicate) onto the bridge's canonical
/// execution facts. Unknown values map to `None` (the caller decides how to
/// surface an unrecognized canonical state; the bridge never guesses).
pub fn map_execution_outcome(raw: &str) -> Option<CanonicalExecutionFact> {
    match raw {
        "completed" => Some(CanonicalExecutionFact::Completed),
        "failed" => Some(CanonicalExecutionFact::Failed),
        "aborted" => Some(CanonicalExecutionFact::AbortedOrTimedOut),
        "partial" => Some(CanonicalExecutionFact::Partial),
        _ => None,
    }
}

/// Ingest the outcome row for `dispatch_id` onto `task_ref`'s fact log.
/// Returns whether a new fact was appended (`false` = row absent).
pub fn ingest_dispatch_outcome(
    facts: &dyn TaskFactSource,
    conn: &Connection,
    task_ref: &TaskRef,
    dispatch_id: &str,
) -> Result<bool, IngestError> {
    let Some(row) = outcomes_db::find_outcome_by_dispatch_id(conn, dispatch_id)? else {
        return Ok(false);
    };
    let Some(execution) = map_execution_outcome(&row.execution_outcome) else {
        // An unrecognized canonical vocabulary value is surfaced, never
        // guessed into a terminal state.
        return Ok(false);
    };
    let evidence_refs: Vec<String> = row
        .evidence_refs
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let observation = OutcomeObservation {
        outcome_id: row.outcome_id.clone(),
        dispatch_id: row.dispatch_id.clone(),
        execution,
        reported_outcome: row.reported_outcome.clone(),
        verification_present: row.verification_present,
        diff_present: row.diff_present,
        evidence_refs,
        vendor: row.vendor.clone(),
        model: row.model.clone(),
        identity_attribution_basis: row.identity_attribution_basis.clone(),
    };
    let event_id = format!("outcome-{}-{}", row.outcome_id, row.updated_at);
    let payload = TaskEventPayload::OutcomeObserved { observation };
    let payload_digest = {
        let value = serde_json::to_value(&payload).expect("payload serializes");
        memcore::canonical_digest::canonical_json_digest_hex(&value)
    };
    let event = TaskEvent {
        seq: 0,
        event_id,
        source: EventSource::DispatchOutcome {
            outcome_id: row.outcome_id,
        },
        source_revision: row.updated_at.clone(),
        occurred_at: row.updated_at,
        recorded_at: row.created_at,
        payload_digest,
        visibility: VisibilityClass::Internal,
        payload,
    };
    facts
        .append(task_ref, event)
        .map_err(|_| IngestError::Unavailable)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::taskintent::bridge::{InMemoryTaskFacts, RequesterAuthorityPort, TaskIntentBridge};
    use crate::taskintent::mapping::execution::ExecutionState;
    use crate::taskintent::result::CollectError;
    use crate::taskintent::wire::Capability;
    use crate::taskintent::{
        AdmittedAuthority, PlanAdmissionError, PlanAdmissionPort, RequestId,
        RequesterAuthorityError, RequesterRef, SubmitReceipt,
    };
    use memcore::db::dispatch_outcomes::{
        get_outcome, upsert_outcome, DispatchOutcomeRow, NewDispatchOutcome,
    };
    use std::sync::Arc;

    struct AllowAll;
    impl RequesterAuthorityPort for AllowAll {
        fn resolve(
            &self,
            _requester: &RequesterRef,
        ) -> Result<AdmittedAuthority, RequesterAuthorityError> {
            Ok(AdmittedAuthority::of([
                Capability::ReasoningReview,
                Capability::ReadOnlyInvestigation,
            ]))
        }
    }

    struct ForwardingOwners;
    impl crate::taskintent::bridge::LifecycleOwnerPort for ForwardingOwners {
        fn request_stop(
            &self,
            _task_ref: &TaskRef,
            _mode: crate::taskintent::StopMode,
            _reason: &str,
        ) -> crate::taskintent::bridge::OwnerForwardResult {
            crate::taskintent::bridge::OwnerForwardResult::Forwarded
        }

        fn forward_intervention(
            &self,
            _task_ref: &TaskRef,
            _intervention: &crate::taskintent::InterventionV1,
            _intervention_id: &str,
        ) -> crate::taskintent::bridge::OwnerForwardResult {
            crate::taskintent::bridge::OwnerForwardResult::Forwarded
        }
    }

    struct ManagedPlan;
    impl PlanAdmissionPort for ManagedPlan {
        fn admit_plan(
            &self,
            task_ref: &TaskRef,
            _preference: Option<&crate::taskintent::wire::RoutingPreference>,
        ) -> Result<crate::taskintent::ExecutionPlanProjection, PlanAdmissionError> {
            Ok(crate::taskintent::ExecutionPlanProjection {
                plan_ref: format!("plan-{task_ref}"),
                task_ref: task_ref.clone(),
                lifecycle_mode: crate::taskintent::LifecycleMode::TachiManagedBatch,
                lifecycle_owner: "managed-custom-backend".to_string(),
                work_claim_id: Some("claim-1".to_string()),
                work_claim_transition_version: Some(1),
                exec_env_id: Some("env-1".to_string()),
                dispatch_id: Some("dispatch-e2e".to_string()),
                backend: Some("custom".to_string()),
                launch_digest: Some("launch-1".to_string()),
                revision: 1,
            })
        }
    }

    #[test]
    fn ingest_projects_real_dispatch_outcomes_truth_end_to_end() {
        // The vertical: a REAL canonical dispatch_outcomes row (written
        // through the existing memcore API) is the terminal truth the
        // bridge projects — get/collect derive from it, re-ingest is
        // suppressed, and nothing outside existing tables is stored.
        let store = memcore::MemoryStore::open_in_memory().expect("open in-memory store");
        let conn = store.connection();
        let row: DispatchOutcomeRow = upsert_outcome(
            conn,
            &NewDispatchOutcome {
                outcome_id: "out-e2e-1".to_string(),
                dispatch_id: "dispatch-e2e".to_string(),
                vendor: "unknown".to_string(),
                execution_outcome: "completed".to_string(),
                reported_outcome: Some("success".to_string()),
                verification_present: true,
                diff_present: false,
                evidence_refs: serde_json::json!(["ev-report-1"]),
                ..NewDispatchOutcome::default()
            },
        )
        .expect("canonical row written");
        assert_eq!(row.execution_outcome, "completed");

        let facts = Arc::new(InMemoryTaskFacts::new());
        let bridge = TaskIntentBridge::new(
            Arc::new(crate::taskintent::InProcessRequestBindings::new()),
            facts.clone(),
            Arc::new(AllowAll),
            Arc::new(ManagedPlan),
            Arc::new(ForwardingOwners),
            Arc::new(crate::taskintent::bridge::SystemBridgeClock),
        );

        let request = RequestId::new("req-e2e").expect("bounded");
        let task_ref = match bridge.submit(&golden(), &request) {
            SubmitReceipt::Admitted { task_ref, .. } => task_ref,
            other => panic!("expected admission, got {other:?}"),
        };

        // Before ingestion: no result yet.
        assert_eq!(bridge.collect(&task_ref, None), Err(CollectError::NotReady));
        // Ingest the real row.
        assert!(
            ingest_dispatch_outcome(facts.as_ref(), conn, &task_ref, "dispatch-e2e")
                .expect("ingest")
        );
        let snapshot = bridge.get(&task_ref).expect("snapshot");
        assert_eq!(snapshot.execution, ExecutionState::Completed);
        let projection = bridge.collect(&task_ref, None).expect("projection");
        assert_eq!(projection.result_revision, 1);
        assert!(projection
            .artifact_evidence_refs
            .contains(&"ev-report-1".to_string()));
        assert!(
            projection.contract_violations.is_empty(),
            "required report satisfied by evidence"
        );
        // Re-ingest is deterministically suppressed: the raw log carries
        // the fact twice, but the DERIVED event stream (what watch/get/
        // collect consume) dedups by stable event id.
        assert!(
            ingest_dispatch_outcome(facts.as_ref(), conn, &task_ref, "dispatch-e2e")
                .expect("re-ingest")
        );
        let page = bridge.watch(&task_ref, 0, 100).expect("page");
        let outcomes = page
            .events
            .iter()
            .filter(|e| matches!(e.payload, TaskEventPayload::OutcomeObserved { .. }))
            .count();
        assert_eq!(outcomes, 1, "duplicate (seq, event_id) suppressed");
        // The canonical row is untouched by the bridge (read-only adapter).
        let still = get_outcome(conn, "out-e2e-1")
            .expect("read")
            .expect("row present");
        assert_eq!(still.updated_at, row.updated_at);
    }

    fn golden() -> crate::taskintent::wire::TaskIntentV1 {
        let file: serde_json::Value =
            serde_json::from_str(crate::taskintent::GOLDEN_TASK_INTENT_V1).expect("golden JSON");
        serde_json::from_value(file["intent"].clone()).expect("golden decodes")
    }

    #[test]
    fn canonical_outcome_vocabulary_maps_totally() {
        assert_eq!(
            map_execution_outcome("completed"),
            Some(CanonicalExecutionFact::Completed)
        );
        assert_eq!(
            map_execution_outcome("failed"),
            Some(CanonicalExecutionFact::Failed)
        );
        assert_eq!(
            map_execution_outcome("aborted"),
            Some(CanonicalExecutionFact::AbortedOrTimedOut)
        );
        assert_eq!(
            map_execution_outcome("partial"),
            Some(CanonicalExecutionFact::Partial)
        );
        assert_eq!(map_execution_outcome("nonsense"), None);
    }
}
