//! The TaskIntent bridge surface (tachi#1840): `submit` / `get` / `watch` /
//! `intervene` / `request_stop` / `collect`, assembled from EXISTING
//! primitives — WorkClaim/ExecEnv/dispatch truth via the fact source,
//! `dispatch_outcomes` idempotency precedent via the TB-7 binding store,
//! the adjudication spine via the adjudication mapping, the attached-session
//! vocabulary via the lifecycle-owner port.
//!
//! **No new ledger (TB-1/DoD row 13):** this module owns no DDL, opens no
//! database, creates no table. It binds typed refs to existing truth
//! through the port traits below; the only state it touches is the
//! [`super::idempotency::RequestBindingStore`] seam and the fact log kept
//! by the [`TaskFactSource`] implementation. See [`super::idempotency`]
//! for the DECISION TB-7/B honest restart-gap record.

use std::sync::Arc;

use super::admission::{AdmissionRejection, AdmittedAuthority};
use super::events::{
    EventSource, ExpectedArtifactProjection, IntentContractProjection, OutcomeObservation,
    TaskEvent, TaskEventPage, TaskEventPayload, VisibilityClass,
};
use super::idempotency::{BoundRef, RequestBindingStore, RequestConflict};
use super::intervention::{
    InterventionError, InterventionReceipt, InterventionV1, InterventionV1Static, StopMode,
    StopReceipt, StopStage,
};
use super::mapping::execution::ExecutionState;
use super::plan::ExecutionPlanProjection;
use super::refs::{AttemptRef, RequestId, RequesterRef, TaskRef};
use super::result::{CollectError, ProvenanceProjection, ResultProjectionV1, VerificationSummary};
use super::snapshot::{GetError, TaskSnapshot};
use super::wire::{RoutingPreference, TaskIntentV1, SCHEMA_TAG};

/// Typed unavailability (TB-20: Tachi outage fails closed for durable work;
/// the bridge never falls back to direct execution — it has no such path).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("bridge truth source unavailable")]
pub struct UnavailableError;

/// Read/append seam over per-task fact logs. Production implementations
/// bind to existing truth surfaces (see [`super::memcore_ingest`]); the
/// in-memory implementation ships for process-lifetime use and tests.
pub trait TaskFactSource: Send + Sync {
    /// Append one fact (bridge-minted or ingested).
    fn append(&self, task_ref: &TaskRef, event: TaskEvent) -> Result<(), UnavailableError>;

    /// Read the raw fact log for a task (empty ⇒ unknown task).
    fn facts(&self, task_ref: &TaskRef) -> Result<Vec<TaskEvent>, UnavailableError>;
}

/// In-memory fact log (process-lifetime carrier; see the honest restart
/// record in [`super::idempotency`] — bridge-minted facts do not survive a
/// restart, while INGESTED facts re-derive from the canonical rows they
/// project).
#[derive(Debug, Default)]
pub struct InMemoryTaskFacts {
    tasks: std::sync::Mutex<std::collections::BTreeMap<String, Vec<TaskEvent>>>,
}

impl InMemoryTaskFacts {
    /// An empty fact log.
    pub fn new() -> Self {
        Self::default()
    }
}

impl TaskFactSource for InMemoryTaskFacts {
    fn append(&self, task_ref: &TaskRef, event: TaskEvent) -> Result<(), UnavailableError> {
        self.tasks
            .lock()
            .expect("task fact map poisoned")
            .entry(task_ref.as_wire().to_string())
            .or_default()
            .push(event);
        Ok(())
    }

    fn facts(&self, task_ref: &TaskRef) -> Result<Vec<TaskEvent>, UnavailableError> {
        Ok(self
            .tasks
            .lock()
            .expect("task fact map poisoned")
            .get(task_ref.as_wire())
            .cloned()
            .unwrap_or_default())
    }
}

/// A fact source that always fails (TB-20 outage tests).
#[derive(Debug, Default)]
pub struct UnavailableTaskFacts;

impl TaskFactSource for UnavailableTaskFacts {
    fn append(&self, _task_ref: &TaskRef, _event: TaskEvent) -> Result<(), UnavailableError> {
        Err(UnavailableError)
    }

    fn facts(&self, _task_ref: &TaskRef) -> Result<Vec<TaskEvent>, UnavailableError> {
        Err(UnavailableError)
    }
}

/// Resolves a requester's own admitted authority (TB-5 intersection law).
pub trait RequesterAuthorityPort: Send + Sync {
    /// Resolve; `Err(NotAdmitted)` is a typed admission rejection,
    /// `Err(Unavailable)` maps to TB-20 `Unavailable`.
    fn resolve(
        &self,
        requester: &RequesterRef,
    ) -> Result<AdmittedAuthority, RequesterAuthorityError>;
}

/// Typed requester-authority failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RequesterAuthorityError {
    /// The requester is not admitted.
    #[error("requester not admitted")]
    NotAdmitted,
    /// The authority source is unreachable (TB-20).
    #[error("authority source unavailable")]
    Unavailable,
}

/// Staffing-plane plan admission (TB-1/TB-2): Tachi — not the caller —
/// chooses the ExecutionPlan; the projection binds existing WorkClaim/
/// ExecEnv/dispatch identities.
pub trait PlanAdmissionPort: Send + Sync {
    /// Admit a plan for the task under the requester's typed preference
    /// (preference never grants placement — TB-5).
    fn admit_plan(
        &self,
        task_ref: &TaskRef,
        preference: Option<&RoutingPreference>,
    ) -> Result<ExecutionPlanProjection, PlanAdmissionError>;
}

/// Typed plan-admission failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PlanAdmissionError {
    /// No execution lane admitted the intent.
    #[error("no admitted execution plan")]
    NoAdmittedPlan,
    /// The staffing plane is unreachable (TB-20).
    #[error("plan admission unavailable")]
    Unavailable,
}

/// Forwarding seam to the lifecycle owner (tachi#1678 vocabulary): the
/// bridge never claims control the owner lacks; unsupported is typed;
/// disappearance after possible side effects is a fact, not a guess.
pub trait LifecycleOwnerPort: Send + Sync {
    /// Forward a stop request.
    fn request_stop(&self, task_ref: &TaskRef, mode: StopMode, reason: &str) -> OwnerForwardResult;

    /// Forward a non-stop intervention.
    fn forward_intervention(
        &self,
        task_ref: &TaskRef,
        operation: InterventionV1Static,
    ) -> OwnerForwardResult;
}

/// Owner forwarding outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerForwardResult {
    /// The owner accepted the request (still not the resulting lifecycle
    /// state — TB-11).
    Forwarded,
    /// The owner does not support the operation.
    Unsupported,
    /// The owner disappeared (possibly after side effects).
    Disappeared,
}

/// Clock seam for deterministic tests.
pub trait BridgeClock: Send + Sync {
    /// Current UTC time as ISO-8601/RFC3339.
    fn now_utc_iso(&self) -> String;
}

/// Wall-clock implementation.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemBridgeClock;

impl BridgeClock for SystemBridgeClock {
    fn now_utc_iso(&self) -> String {
        chrono::Utc::now().to_rfc3339()
    }
}

/// The `submit` typed envelope (TB-5b): `Ok(TaskRef)` on admitted
/// submission, or typed `Unavailable` / `ReconciliationUnknown` /
/// `RequestIdConflict` — plus the typed admission-rejection surface TB-4
/// requires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitReceipt {
    /// Admitted (or replayed): the task ref. `replayed` marks a TB-7 rule-2
    /// duplicate-submit replay that returned the SAME TaskRef and started
    /// no second worker.
    Admitted {
        /// The Tachi-minted task identity.
        task_ref: TaskRef,
        /// Whether this was an idempotent replay.
        replayed: bool,
    },
    /// Typed admission rejection (TB-4/TB-5 and submit-local refusals).
    Rejected(AdmissionRejection),
    /// The bridge truth source is unavailable (TB-20).
    Unavailable,
    /// An ambiguous submit is pending reconciliation (TB-7 rule 4): the
    /// tuple is bound but the task fact has not materialized; resubmitting
    /// the same tuple reconciles to exactly one task, never a second spawn.
    ReconciliationUnknown {
        /// The submitted canonical digest.
        digest: String,
    },
    /// Same `(requester, request_id)` bound to a different digest (TB-7
    /// rule 3). Zero new execution.
    RequestIdConflict {
        /// Digest the tuple is already bound to.
        bound_digest: String,
        /// Digest of the incoming request.
        submitted_digest: String,
    },
}

/// The bridge. Clone-safe over `Arc` ports.
#[derive(Clone)]
pub struct TaskIntentBridge {
    bindings: Arc<dyn RequestBindingStore>,
    facts: Arc<dyn TaskFactSource>,
    authority: Arc<dyn RequesterAuthorityPort>,
    plans: Arc<dyn PlanAdmissionPort>,
    owners: Arc<dyn LifecycleOwnerPort>,
    clock: Arc<dyn BridgeClock>,
}

impl TaskIntentBridge {
    /// Assemble the bridge over its ports.
    pub fn new(
        bindings: Arc<dyn RequestBindingStore>,
        facts: Arc<dyn TaskFactSource>,
        authority: Arc<dyn RequesterAuthorityPort>,
        plans: Arc<dyn PlanAdmissionPort>,
        owners: Arc<dyn LifecycleOwnerPort>,
        clock: Arc<dyn BridgeClock>,
    ) -> Self {
        Self {
            bindings,
            facts,
            authority,
            plans,
            owners,
            clock,
        }
    }

    /// `submit(TaskIntentV1, RequestId)` (TB-5b/TB-6/TB-7).
    pub fn submit(&self, intent: &TaskIntentV1, request_id: &RequestId) -> SubmitReceipt {
        // TB-20: fail closed BEFORE any mutation — if the truth source is
        // down, no binding is consumed and the receipt is typed
        // `Unavailable`, never an ambiguous submit this bridge itself
        // caused.
        if self
            .facts
            .facts(&TaskRef::mint("task:_health_probe"))
            .is_err()
        {
            return SubmitReceipt::Unavailable;
        }
        if intent.schema != SCHEMA_TAG {
            return SubmitReceipt::Rejected(AdmissionRejection::SchemaTag {
                expected: SCHEMA_TAG,
                actual: intent.schema.clone(),
            });
        }
        let digest = intent.canonical_digest();

        // TB-18: a deliberate retry references a real prior task.
        if let Some(prior) = &intent.retry_of {
            match self.facts.facts(prior) {
                Err(_) => return SubmitReceipt::Unavailable,
                Ok(prior_facts) if prior_facts.is_empty() => {
                    return SubmitReceipt::Rejected(AdmissionRejection::UnknownRetryLineage)
                }
                Ok(_) => {}
            }
        }

        // TB-5: requester-bounded capability; TB-20 on authority outage.
        let authority = match self.authority.resolve(&intent.requester) {
            Err(RequesterAuthorityError::Unavailable) => return SubmitReceipt::Unavailable,
            Err(RequesterAuthorityError::NotAdmitted) => {
                return SubmitReceipt::Rejected(AdmissionRejection::RequesterNotAdmitted)
            }
            Ok(authority) => authority,
        };
        if let Err(rejection) = super::admission::admit(intent, &authority) {
            return SubmitReceipt::Rejected(rejection);
        }

        // TB-7 tuple law, checked before any mutation.
        if let Some(binding) = self
            .bindings
            .lookup(&intent.requester, &request_id.to_string())
        {
            if binding.digest != digest {
                return SubmitReceipt::RequestIdConflict {
                    bound_digest: binding.digest,
                    submitted_digest: digest,
                };
            }
            return match binding.bound {
                // Rule 2: same digest ⇒ same TaskRef, never a second worker.
                BoundRef::Task(task_ref) => match self.facts.facts(&task_ref) {
                    Err(_) => SubmitReceipt::Unavailable,
                    Ok(facts) if facts.is_empty() => {
                        SubmitReceipt::ReconciliationUnknown { digest }
                    }
                    Ok(_) => SubmitReceipt::Admitted {
                        task_ref,
                        replayed: true,
                    },
                },
                // Same tuple bound to a non-submit operation with a matching
                // digest: still a cross-operation conflict.
                _ => SubmitReceipt::RequestIdConflict {
                    bound_digest: binding.digest,
                    submitted_digest: digest,
                },
            };
        }

        // TB-6: Tachi mints the TaskRef, after admission.
        let task_ref = TaskRef::mint(format!("task:{}", uuid::Uuid::new_v4().simple()));

        // TB-2: the staffing plane chooses the plan.
        let plan = match self
            .plans
            .admit_plan(&task_ref, intent.routing_preference.as_ref())
        {
            Err(PlanAdmissionError::Unavailable) => return SubmitReceipt::Unavailable,
            Err(PlanAdmissionError::NoAdmittedPlan) => {
                return SubmitReceipt::Rejected(AdmissionRejection::NoAdmittedExecutionPlan)
            }
            Ok(plan) => plan,
        };

        // Bind FIRST, then materialize: the window between them is exactly
        // the TB-7 rule-4 ambiguity a replay reconciles (never a second
        // spawn: the lookup above short-circuits before minting).
        if let Err(conflict) = self.bindings.bind(
            &intent.requester,
            &request_id.to_string(),
            &digest,
            BoundRef::Task(task_ref.clone()),
        ) {
            return SubmitReceipt::RequestIdConflict {
                bound_digest: conflict.bound_digest().to_string(),
                submitted_digest: digest,
            };
        }

        let contract = contract_projection(intent);
        let now = self.clock.now_utc_iso();
        let submitted = self.fact(
            EventSource::BridgeAdmission,
            format!("evt-{}", uuid::Uuid::new_v4().simple()),
            &now,
            VisibilityClass::Internal,
            TaskEventPayload::TaskSubmitted {
                intent_digest: digest.clone(),
                contract: contract.clone(),
            },
        );
        if self.facts.append(&task_ref, submitted).is_err() {
            return SubmitReceipt::ReconciliationUnknown { digest };
        }
        let admitted = self.fact(
            EventSource::BridgeAdmission,
            format!("evt-{}", uuid::Uuid::new_v4().simple()),
            &now,
            VisibilityClass::Internal,
            TaskEventPayload::PlanAdmitted { plan },
        );
        if self.facts.append(&task_ref, admitted).is_err() {
            return SubmitReceipt::ReconciliationUnknown { digest };
        }
        SubmitReceipt::Admitted {
            task_ref,
            replayed: false,
        }
    }

    /// `get(TaskRef)` (TB-8/TB-15): read projection over canonical truth.
    pub fn get(&self, task_ref: &TaskRef) -> Result<TaskSnapshot, GetError> {
        let events = self.derived_events(task_ref)?;
        if events.is_empty() {
            return Err(GetError::NotFound);
        }
        Ok(self.snapshot_from(task_ref, &events))
    }

    /// `watch(TaskRef, after_seq)` (TB-9): durable backfill; reconnect with
    /// `after_seq = last_seen` replays exactly the missed events. Read-only
    /// — watching never creates tasks.
    pub fn watch(
        &self,
        task_ref: &TaskRef,
        after_seq: u64,
        limit: usize,
    ) -> Result<TaskEventPage, GetError> {
        let events = self.derived_events(task_ref)?;
        if events.is_empty() {
            return Err(GetError::NotFound);
        }
        let limit = limit.max(1);
        let missed: Vec<TaskEvent> = events
            .into_iter()
            .filter(|event| event.seq > after_seq)
            .take(limit)
            .collect();
        let has_more = missed.len() == limit;
        Ok(TaskEventPage {
            task_ref: task_ref.clone(),
            events: missed,
            has_more,
        })
    }

    /// `intervene(TaskRef, InterventionV1, RequestId,
    /// expected_task_revision?)` (TB-11).
    pub fn intervene(
        &self,
        task_ref: &TaskRef,
        intervention: &InterventionV1,
        requester: &RequesterRef,
        request_id: &RequestId,
        expected_task_revision: Option<u64>,
    ) -> Result<InterventionReceipt, InterventionError> {
        let op = intervention.discriminant();
        // TB-11: these are new task/adjudication lineage (tachi#1623/#1675),
        // NOT session interventions — typed refusal, zero mutation, no
        // fresh-task fallback.
        if matches!(
            op,
            InterventionV1Static::RequestIndependentReview | InterventionV1Static::Escalate
        ) {
            return Err(InterventionError::RequiresNewTaskLineage { operation: op });
        }
        // Single-pathed stop authority: the stop variants ARE request_stop.
        if matches!(
            op,
            InterventionV1Static::RequestGracefulStop | InterventionV1Static::RequestHardCancel
        ) {
            let (mode, reason) = match intervention {
                InterventionV1::RequestGracefulStop { reason } => {
                    (StopMode::Graceful, reason.as_str().to_string())
                }
                InterventionV1::RequestHardCancel { reason } => {
                    (StopMode::Hard, reason.as_str().to_string())
                }
                _ => unreachable!("discriminant checked above"),
            };
            let receipt = self.request_stop_inner(
                task_ref,
                mode,
                &reason,
                requester,
                request_id,
                expected_task_revision,
                op,
            )?;
            return Ok(InterventionReceipt::Stop(receipt));
        }

        let events = self.derived_events(task_ref)?;
        if events.is_empty() {
            return Err(InterventionError::NotFound);
        }
        let snapshot = self.snapshot_from(task_ref, &events);
        // Advertisement check FIRST: typed refusal, zero mutation (TB-11).
        if !snapshot.supported_interventions.contains(&op) {
            return Err(InterventionError::UnsupportedByLifecycleOwner { operation: op });
        }
        // Revision-bound, never best-effort (TB-11).
        if let Some(expected) = expected_task_revision {
            if expected != snapshot.task_revision {
                return Err(InterventionError::RevisionConflict {
                    expected,
                    actual: snapshot.task_revision,
                });
            }
        }
        // TB-7 rule 6 tuple law.
        let digest = composite_digest(&serde_json::json!({
            "task": task_ref.as_wire(),
            "intervention": intervention.canonical_digest(),
        }));
        if let Some(binding) = self.bindings.lookup(requester, &request_id.to_string()) {
            let conflict = || {
                InterventionError::RequestIdConflict(RequestConflict::RequestIdConflict {
                    bound_digest: binding.digest.clone(),
                    submitted_digest: digest.clone(),
                })
            };
            if binding.digest != digest {
                return Err(conflict());
            }
            if let BoundRef::Intervention {
                task,
                intervention_id,
            } = binding.bound
            {
                if task == *task_ref {
                    return Ok(replay_receipt(op, intervention_id));
                }
            }
            return Err(conflict());
        }
        // Forward to the lifecycle owner (tachi#1678 typed request).
        match self.owners.forward_intervention(task_ref, op) {
            OwnerForwardResult::Unsupported => {
                // The advertisement and the owner can disagree; the owner is
                // authority: typed refusal, zero mutation.
                Err(InterventionError::UnsupportedByLifecycleOwner { operation: op })
            }
            OwnerForwardResult::Forwarded => {
                let intervention_id = format!("iv:{}", uuid::Uuid::new_v4().simple());
                if let Err(conflict) = self.bindings.bind(
                    requester,
                    &request_id.to_string(),
                    &digest,
                    BoundRef::Intervention {
                        task: task_ref.clone(),
                        intervention_id: intervention_id.clone(),
                    },
                ) {
                    return Err(InterventionError::RequestIdConflict(conflict));
                }
                let now = self.clock.now_utc_iso();
                let event = self.fact(
                    EventSource::LifecycleOwner {
                        operation: op_token(op).to_string(),
                    },
                    format!("evt-{}", uuid::Uuid::new_v4().simple()),
                    &now,
                    VisibilityClass::Internal,
                    TaskEventPayload::InterventionForwarded {
                        operation: op_token(op).to_string(),
                        intervention_id: intervention_id.clone(),
                    },
                );
                if self.facts.append(task_ref, event).is_err() {
                    return Err(InterventionError::Unavailable);
                }
                Ok(replay_receipt(op, intervention_id))
            }
            OwnerForwardResult::Disappeared => Err(InterventionError::OwnerDisappeared),
        }
    }

    /// `request_stop(TaskRef, mode, RequestId, expected_task_revision?)`
    /// (TB-12): the multi-stage stop fact. The projection NEVER mints
    /// `cancelled` without authoritative owner confirmation.
    pub fn request_stop(
        &self,
        task_ref: &TaskRef,
        mode: StopMode,
        requester: &RequesterRef,
        request_id: &RequestId,
        expected_task_revision: Option<u64>,
    ) -> Result<StopReceipt, InterventionError> {
        let op = match mode {
            StopMode::Graceful => InterventionV1Static::RequestGracefulStop,
            StopMode::Hard => InterventionV1Static::RequestHardCancel,
        };
        self.request_stop_inner(
            task_ref,
            mode,
            "",
            requester,
            request_id,
            expected_task_revision,
            op,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn request_stop_inner(
        &self,
        task_ref: &TaskRef,
        mode: StopMode,
        reason: &str,
        requester: &RequesterRef,
        request_id: &RequestId,
        expected_task_revision: Option<u64>,
        op: InterventionV1Static,
    ) -> Result<StopReceipt, InterventionError> {
        let events = self.derived_events(task_ref)?;
        if events.is_empty() {
            return Err(InterventionError::NotFound);
        }
        let snapshot = self.snapshot_from(task_ref, &events);
        if !snapshot.supported_interventions.contains(&op) {
            return Err(InterventionError::UnsupportedByLifecycleOwner { operation: op });
        }
        if let Some(expected) = expected_task_revision {
            if expected != snapshot.task_revision {
                return Err(InterventionError::RevisionConflict {
                    expected,
                    actual: snapshot.task_revision,
                });
            }
        }
        // The stop identity digest is {task, mode} ONLY: the stop-alias
        // interventions (TB-11) and `request_stop` must share ONE
        // idempotency binding for the same stop operation regardless of
        // entry point; a reason is recorded on the fact, not part of the
        // request identity.
        let digest = composite_digest(&serde_json::json!({
            "task": task_ref.as_wire(),
            "mode": mode.as_str(),
        }));
        if let Some(binding) = self.bindings.lookup(requester, &request_id.to_string()) {
            if binding.digest != digest {
                return Err(InterventionError::RequestIdConflict(
                    RequestConflict::RequestIdConflict {
                        bound_digest: binding.digest,
                        submitted_digest: digest,
                    },
                ));
            }
            if let BoundRef::Stop { task, stop_id } = binding.bound {
                if task == *task_ref {
                    return Ok(current_stop_receipt(
                        &events,
                        task_ref,
                        &stop_id,
                        mode,
                        &request_id.to_string(),
                    ));
                }
            }
            return Err(InterventionError::RequestIdConflict(
                RequestConflict::RequestIdConflict {
                    bound_digest: binding.digest,
                    submitted_digest: digest,
                },
            ));
        }

        match self.owners.request_stop(task_ref, mode, reason) {
            OwnerForwardResult::Unsupported => {
                Err(InterventionError::UnsupportedByLifecycleOwner { operation: op })
            }
            forward => {
                let stop_id = format!("stop:{}", uuid::Uuid::new_v4().simple());
                if let Err(conflict) = self.bindings.bind(
                    requester,
                    &request_id.to_string(),
                    &digest,
                    BoundRef::Stop {
                        task: task_ref.clone(),
                        stop_id: stop_id.clone(),
                    },
                ) {
                    return Err(InterventionError::RequestIdConflict(conflict));
                }
                let stage = match forward {
                    OwnerForwardResult::Forwarded => StopStage::Forwarded,
                    // TB-12: disappearance after possible side effects is
                    // outcome_unknown — never optimistic success/failure/
                    // cancel.
                    OwnerForwardResult::Unsupported | OwnerForwardResult::Disappeared => {
                        StopStage::OutcomeUnknown
                    }
                };
                let now = self.clock.now_utc_iso();
                let requested = self.fact(
                    EventSource::LifecycleOwner {
                        operation: "request_cancel".to_string(),
                    },
                    format!("evt-{}", uuid::Uuid::new_v4().simple()),
                    &now,
                    VisibilityClass::Internal,
                    TaskEventPayload::StopRequested {
                        stop_id: stop_id.clone(),
                        mode: mode.as_str().to_string(),
                    },
                );
                if self.facts.append(task_ref, requested).is_err() {
                    return Err(InterventionError::Unavailable);
                }
                if forward == OwnerForwardResult::Disappeared {
                    let event = self.fact(
                        EventSource::LifecycleOwner {
                            operation: "request_cancel".to_string(),
                        },
                        format!("evt-{}", uuid::Uuid::new_v4().simple()),
                        &now,
                        VisibilityClass::Internal,
                        TaskEventPayload::OwnerDisappeared,
                    );
                    if self.facts.append(task_ref, event).is_err() {
                        return Err(InterventionError::Unavailable);
                    }
                }
                Ok(StopReceipt {
                    task_ref: task_ref.clone(),
                    stop_id,
                    mode,
                    stage,
                    request_id: request_id.to_string(),
                })
            }
        }
    }

    /// `collect(TaskRef, result_revision?)` (TB-13): artifact/evidence-first
    /// result projection; pull-only in V2. Result revisions are
    /// Tachi-minted, monotonic, and immutable: a pinned revision returns
    /// exactly that revision's projection; a revision that never existed is
    /// typed `not_found`; the latest is returned when unpinned. A stale
    /// older revision can never overwrite a newer projection because
    /// projections are DERIVED per revision, never stored.
    pub fn collect(
        &self,
        task_ref: &TaskRef,
        result_revision: Option<u64>,
    ) -> Result<ResultProjectionV1, CollectError> {
        let events = self.derived_events(task_ref)?;
        if events.is_empty() {
            return Err(CollectError::NotFound);
        }
        let expected: Vec<ExpectedArtifactProjection> = events
            .iter()
            .find_map(|e| match &e.payload {
                TaskEventPayload::TaskSubmitted { contract, .. } => {
                    Some(contract.expected_artifacts.clone())
                }
                _ => None,
            })
            .unwrap_or_default();
        // Fold the fact log into the immutable per-revision projection
        // history: every OutcomeObserved / Adjudication / OwnerConfirmed
        // terminal fact mints the next revision.
        let mut history: Vec<ResultProjectionV1> = Vec::new();
        for (index, event) in events.iter().enumerate() {
            let seen = &events[..=index];
            match &event.payload {
                TaskEventPayload::OutcomeObserved { observation } => {
                    let revision = history.len() as u64 + 1;
                    history.push(outcome_projection(
                        task_ref,
                        seen,
                        observation,
                        &expected,
                        revision,
                    ));
                }
                TaskEventPayload::Adjudication(_) => {
                    if let Some(prior) = history.last() {
                        let mut next = prior.clone();
                        next.adjudication_state =
                            super::mapping::adjudication::project_adjudication(
                                seen.iter().filter_map(|e| match &e.payload {
                                    TaskEventPayload::Adjudication(fact) => Some(fact),
                                    _ => None,
                                }),
                            );
                        next.result_revision = history.len() as u64 + 1;
                        history.push(next);
                    }
                }
                TaskEventPayload::OwnerConfirmedTerminal { .. } => {
                    if let Some(prior) = history.last() {
                        let mut next = prior.clone();
                        next.terminal_classification = terminal_classification(seen);
                        next.result_revision = history.len() as u64 + 1;
                        history.push(next);
                    }
                }
                _ => {}
            }
        }
        let Some(latest) = history.last() else {
            return Err(CollectError::NotReady);
        };
        match result_revision {
            None => Ok(latest.clone()),
            Some(pinned) => history
                .iter()
                .find(|projection| projection.result_revision == pinned)
                .cloned()
                .ok_or(CollectError::ResultRevisionNotFound),
        }
    }

    // ── internals ────────────────────────────────────────────────────────

    fn derived_events(&self, task_ref: &TaskRef) -> Result<Vec<TaskEvent>, GetError> {
        let facts = self
            .facts
            .facts(task_ref)
            .map_err(|_| GetError::Unavailable)?;
        Ok(TaskEvent::derive(facts))
    }

    fn snapshot_from(&self, task_ref: &TaskRef, events: &[TaskEvent]) -> TaskSnapshot {
        let execution_facts: Vec<_> = events.iter().filter_map(execution_fact_of).collect();
        let adjudication =
            super::mapping::adjudication::project_adjudication(events.iter().filter_map(|e| {
                match &e.payload {
                    TaskEventPayload::Adjudication(fact) => Some(fact),
                    _ => None,
                }
            }));
        let delivery =
            super::mapping::delivery::project_delivery(events.iter().filter_map(
                |e| match &e.payload {
                    TaskEventPayload::Delivery(fact) => Some(fact),
                    _ => None,
                },
            ));
        let plan = events.iter().find_map(|e| match &e.payload {
            TaskEventPayload::PlanAdmitted { plan } => Some(plan.clone()),
            _ => None,
        });
        let (contract, intent_digest) = events
            .iter()
            .find_map(|e| match &e.payload {
                TaskEventPayload::TaskSubmitted {
                    intent_digest,
                    contract,
                } => Some((Some(contract.clone()), intent_digest.clone())),
                _ => None,
            })
            .unwrap_or((None, String::new()));
        let mut supported: Vec<InterventionV1Static> = plan
            .as_ref()
            .map(|p| p.lifecycle_mode.baseline_supported_interventions().to_vec())
            .unwrap_or_default();
        for op in events.iter().flat_map(|e| match &e.payload {
            TaskEventPayload::OwnerCapabilitiesDeclared { operations } => operations.clone(),
            _ => Vec::new(),
        }) {
            if !supported.contains(&op) {
                supported.push(op);
            }
        }
        TaskSnapshot {
            task_ref: task_ref.clone(),
            task_revision: events.len() as u64,
            execution: super::mapping::execution::project_execution(execution_facts.iter())
                .unwrap_or(ExecutionState::Queued),
            adjudication,
            delivery,
            plan: plan.clone(),
            lifecycle_mode: plan.as_ref().map(|p| p.lifecycle_mode),
            lifecycle_owner: plan.as_ref().map(|p| p.lifecycle_owner.clone()),
            supported_interventions: supported,
            contract,
            intent_digest,
        }
    }

    fn fact(
        &self,
        source: EventSource,
        event_id: String,
        now: &str,
        visibility: VisibilityClass,
        payload: TaskEventPayload,
    ) -> TaskEvent {
        let payload_value = serde_json::to_value(&payload).expect("payload serializes");
        TaskEvent {
            seq: 0,
            event_id,
            source,
            source_revision: "1".to_string(),
            occurred_at: now.to_string(),
            recorded_at: now.to_string(),
            payload_digest: memcore::canonical_digest::canonical_json_digest_hex(&payload_value),
            visibility,
            payload,
        }
    }
}

impl From<GetError> for CollectError {
    fn from(error: GetError) -> Self {
        match error {
            GetError::NotFound => Self::NotFound,
            GetError::Unavailable => Self::Unavailable,
        }
    }
}

impl From<GetError> for InterventionError {
    fn from(error: GetError) -> Self {
        match error {
            GetError::NotFound => Self::NotFound,
            GetError::Unavailable => Self::Unavailable,
        }
    }
}

fn op_token(op: InterventionV1Static) -> &'static str {
    match op {
        InterventionV1Static::ProvideAdditionalContext => "provide_context",
        InterventionV1Static::RequestCorrection => "prompt_or_correct",
        InterventionV1Static::RequestContinuation => "request_continuation",
        InterventionV1Static::RequestIndependentReview => "request_independent_review",
        InterventionV1Static::RequestUserInput => "request_user_input",
        InterventionV1Static::RequestPause => "request_pause",
        InterventionV1Static::RequestResume => "request_resume",
        InterventionV1Static::RequestGracefulStop => "request_cancel",
        InterventionV1Static::RequestHardCancel => "request_cancel",
        InterventionV1Static::Escalate => "escalate",
    }
}

fn replay_receipt(op: InterventionV1Static, intervention_id: String) -> InterventionReceipt {
    match op {
        InterventionV1Static::ProvideAdditionalContext => {
            InterventionReceipt::ContextProvided { intervention_id }
        }
        InterventionV1Static::RequestCorrection => {
            InterventionReceipt::CorrectionRequested { intervention_id }
        }
        InterventionV1Static::RequestContinuation => {
            InterventionReceipt::ContinuationRequested { intervention_id }
        }
        InterventionV1Static::RequestUserInput => {
            InterventionReceipt::UserInputRequested { intervention_id }
        }
        InterventionV1Static::RequestPause => InterventionReceipt::Paused { intervention_id },
        InterventionV1Static::RequestResume => InterventionReceipt::Resumed { intervention_id },
        InterventionV1Static::RequestGracefulStop
        | InterventionV1Static::RequestHardCancel
        | InterventionV1Static::RequestIndependentReview
        | InterventionV1Static::Escalate => unreachable!("stop/new-lineage ops do not replay here"),
    }
}

fn composite_digest(value: &serde_json::Value) -> String {
    memcore::canonical_digest::canonical_json_digest_hex(value)
}

fn current_stop_receipt(
    events: &[TaskEvent],
    task_ref: &TaskRef,
    stop_id: &str,
    mode: StopMode,
    request_id: &str,
) -> StopReceipt {
    // Replay reflects the CURRENT stage, derived from the fact log: a later
    // OwnerConfirmedTerminal advances Confirmed; OwnerDisappeared pins
    // OutcomeUnknown.
    let mut stage = StopStage::Forwarded;
    for event in events {
        match &event.payload {
            TaskEventPayload::OwnerConfirmedTerminal { .. } => stage = StopStage::Confirmed,
            TaskEventPayload::OwnerDisappeared => stage = StopStage::OutcomeUnknown,
            _ => {}
        }
    }
    StopReceipt {
        task_ref: task_ref.clone(),
        stop_id: stop_id.to_string(),
        mode,
        stage,
        request_id: request_id.to_string(),
    }
}

fn execution_fact_of(
    event: &TaskEvent,
) -> Option<super::mapping::execution::CanonicalExecutionFact> {
    use super::mapping::execution::CanonicalExecutionFact;
    match &event.payload {
        TaskEventPayload::Execution(fact) => Some(fact.clone()),
        TaskEventPayload::OutcomeObserved { observation } => Some(observation.execution.clone()),
        // The stop machine's facts ARE execution-dimension facts: a stop
        // request projects `cancellation_requested` (never `cancelled`); an
        // owner-confirmed terminal projects the owner's confirmation;
        // disappearance projects `outcome_unknown` (TB-12).
        TaskEventPayload::StopRequested { .. } => {
            Some(CanonicalExecutionFact::CancellationRequested)
        }
        TaskEventPayload::OwnerConfirmedTerminal { terminal } => match terminal.as_str() {
            "cancelled" => Some(CanonicalExecutionFact::OwnerConfirmedCancelled),
            _ => None,
        },
        TaskEventPayload::OwnerDisappeared => Some(CanonicalExecutionFact::OutcomeUnknown),
        _ => None,
    }
}

fn terminal_classification(events: &[TaskEvent]) -> String {
    let state =
        super::mapping::execution::project_execution(events.iter().filter_map(execution_fact_of))
            .unwrap_or(ExecutionState::Queued);
    serde_json::to_value(state)
        .expect("ExecutionState serializes")
        .as_str()
        .expect("enum variants serialize as strings")
        .to_string()
}

fn contract_projection(intent: &TaskIntentV1) -> IntentContractProjection {
    IntentContractProjection {
        objective: intent.objective.as_str().to_string(),
        constraints: intent
            .constraints
            .iter()
            .map(|c| c.description.as_str().to_string())
            .collect(),
        expected_artifacts: intent
            .expected_artifacts
            .iter()
            .map(|a| ExpectedArtifactProjection {
                artifact_class: a.artifact_class,
                required: a.required,
            })
            .collect(),
        evaluation_requirement: intent.evaluation_requirement.independence,
    }
}

fn outcome_projection(
    task_ref: &TaskRef,
    events: &[TaskEvent],
    observation: &OutcomeObservation,
    expected: &[ExpectedArtifactProjection],
    revision: u64,
) -> ResultProjectionV1 {
    let evidence = VerificationSummary {
        verification_present: observation.verification_present,
        diff_present: observation.diff_present,
        evidence_ref_count: observation.evidence_refs.len(),
    };
    // TB-13: contract violations are computed against the SUBMITTED
    // intent's expected artifacts, never against the worker's self-report
    // (a `success` without the required artifact/evidence violates).
    let contract_violations = super::result::contract_violations(expected, &evidence);
    let adjudication_state =
        super::mapping::adjudication::project_adjudication(events.iter().filter_map(|e| {
            match &e.payload {
                TaskEventPayload::Adjudication(fact) => Some(fact),
                _ => None,
            }
        }));
    ResultProjectionV1 {
        task_ref: task_ref.clone(),
        attempt_ref: AttemptRef::mint(format!("attempt:{}", observation.dispatch_id)),
        terminal_classification: serde_json::to_value(&observation.execution)
            .expect("CanonicalExecutionFact serializes")
            .as_str()
            .expect("enum variants serialize as strings")
            .to_string(),
        canonical_artifact_ref: observation.evidence_refs.first().cloned(),
        artifact_evidence_refs: observation.evidence_refs.clone(),
        verification_summary: evidence,
        adjudication_state,
        contract_violations,
        provenance: ProvenanceProjection {
            vendor: observation.vendor.clone(),
            model: observation.model.clone(),
            identity_attribution_basis: observation.identity_attribution_basis.clone(),
            reported_outcome: observation.reported_outcome.clone(),
        },
        pending_user_action: None,
        result_revision: revision,
    }
}
