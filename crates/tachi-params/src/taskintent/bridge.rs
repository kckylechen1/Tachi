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

    /// Forward a non-stop intervention. The FULL typed intervention
    /// travels — notes, prompts, and independence classes are the payload
    /// the owner acts on — together with the STABLE intervention id from
    /// the TB-7 binding, so the owner side can dedup replays of the same
    /// request.
    fn forward_intervention(
        &self,
        task_ref: &TaskRef,
        intervention: &InterventionV1,
        intervention_id: &str,
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
    /// Serializes this bridge instance's mutating operations (submit /
    /// intervene / request_stop) so concurrent duplicate requests
    /// linearize: the second duplicate finds the first one's binding and
    /// replays instead of double-executing (TB-7). Shared across clones;
    /// cross-instance concurrency collapses at the binding store's atomic
    /// `bind` (the [`RequestBindingStore`] contract).
    op_lock: Arc<std::sync::Mutex<()>>,
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
            op_lock: Arc::new(std::sync::Mutex::new(())),
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

        // TB-18: a deliberate retry references a real prior task OWNED BY
        // THE SAME REQUESTER (foreign retry lineage is refused).
        if let Some(prior) = &intent.retry_of {
            match self.facts.facts(prior) {
                Err(_) => return SubmitReceipt::Unavailable,
                Ok(prior_facts) if prior_facts.is_empty() => {
                    return SubmitReceipt::Rejected(AdmissionRejection::UnknownRetryLineage)
                }
                Ok(prior_facts) => {
                    let owned = prior_facts.iter().any(|f| match &f.payload {
                        TaskEventPayload::TaskSubmitted { requester, .. } => {
                            requester == &intent.requester.to_string()
                        }
                        _ => false,
                    });
                    if !owned {
                        return SubmitReceipt::Rejected(AdmissionRejection::UnknownRetryLineage);
                    }
                }
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

        // Mutation region: linearized per-instance by the op lock, and
        // cross-instance by the binding store's atomic bind.
        let _guard = self.op_lock.lock().expect("bridge op lock poisoned");

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
                BoundRef::Task(task_ref) => self.replay_task(&task_ref, intent),
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

        // BIND FIRST — the atomic tuple reservation precedes every
        // side-effecting call (plan admission can launch work): a
        // concurrent duplicate collapses HERE, before any lane is touched.
        // The window between bind and materialization is exactly the TB-7
        // rule-4 ambiguity a replay reconciles.
        match self.bindings.bind(
            &intent.requester,
            &request_id.to_string(),
            &digest,
            BoundRef::Task(task_ref.clone()),
        ) {
            Ok(returned) => {
                // Lost the race against a concurrent duplicate? The store
                // returns the WINNER's binding — reconcile onto it, never
                // admit a second plan or materialize a second task.
                match returned.bound {
                    BoundRef::Task(bound_task) if bound_task != task_ref => {
                        return self.replay_task(&bound_task, intent);
                    }
                    BoundRef::Task(_) => {}
                    _ => {
                        return SubmitReceipt::RequestIdConflict {
                            bound_digest: returned.digest,
                            submitted_digest: digest,
                        }
                    }
                }
            }
            Err(conflict) => {
                return SubmitReceipt::RequestIdConflict {
                    bound_digest: conflict.bound_digest().to_string(),
                    submitted_digest: digest,
                }
            }
        }

        // TB-2: the staffing plane chooses the plan — AFTER the tuple is
        // reserved, so at most one caller ever reaches this call per
        // tuple.
        let plan = match self
            .plans
            .admit_plan(&task_ref, intent.routing_preference.as_ref())
        {
            Err(PlanAdmissionError::Unavailable) => return SubmitReceipt::Unavailable,
            Err(PlanAdmissionError::NoAdmittedPlan) => {
                // Record the definitive refusal on the bound task's log so
                // the same tuple replays to the same typed rejection
                // instead of an eternal ambiguity.
                let now = self.clock.now_utc_iso();
                let event = self.fact(
                    EventSource::BridgeAdmission,
                    format!("evt-{}", uuid::Uuid::new_v4().simple()),
                    &now,
                    VisibilityClass::Internal,
                    TaskEventPayload::SubmitRejected {
                        reason: "no_admitted_execution_plan".to_string(),
                    },
                );
                if self.facts.append(&task_ref, event).is_err() {
                    // The refusal could not be recorded: claiming a
                    // definitive rejection the log cannot replay would be
                    // dishonest — return the ambiguous window instead.
                    return SubmitReceipt::ReconciliationUnknown { digest };
                }
                return SubmitReceipt::Rejected(AdmissionRejection::NoAdmittedExecutionPlan);
            }
            Ok(plan) => plan,
        };

        let contract = contract_projection(intent);
        let now = self.clock.now_utc_iso();
        let submitted = self.fact(
            EventSource::BridgeAdmission,
            format!("evt-{}", uuid::Uuid::new_v4().simple()),
            &now,
            VisibilityClass::Internal,
            TaskEventPayload::TaskSubmitted {
                intent_digest: digest.clone(),
                requester: intent.requester.to_string(),
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
            // TaskSubmitted landed, PlanAdmitted did not: the replay path
            // self-heals the plan (see `replay_task`). This call cannot
            // claim success it did not observe.
            return SubmitReceipt::ReconciliationUnknown { digest };
        }
        SubmitReceipt::Admitted {
            task_ref,
            replayed: false,
        }
    }

    /// Replay/reconcile a bound submit onto its task (TB-7 rules 2 and 4).
    /// `Admitted` requires the `TaskSubmitted` fact to have MATERIALIZED —
    /// a non-empty log alone is not admission — and the fact must name the
    /// SAME requester and digest as the bound tuple (defensive: a sound
    /// binding store guarantees this; the check keeps reconciliation
    /// honest against a corrupted or adversarial log). A missing
    /// `PlanAdmitted` fact is self-healed by re-running plan admission
    /// (TB-2 allows re-targeting; the task identity never changes); a heal
    /// that finds NO admissible plan is the same definitive rejection the
    /// initial submit returns.
    fn replay_task(&self, task_ref: &TaskRef, intent: &TaskIntentV1) -> SubmitReceipt {
        let facts = match self.facts.facts(task_ref) {
            Err(_) => return SubmitReceipt::Unavailable,
            Ok(facts) => facts,
        };
        // Verify the tuple FIRST: a materialized TaskSubmitted must name
        // the SAME requester and digest as the bound tuple. This check
        // precedes the SubmitRejected shortcut so a corrupted/mixed log
        // (SubmitRejected + a mismatched TaskSubmitted) resolves to a typed
        // conflict, never to a wrong definitive answer.
        let submitted = facts.iter().find_map(|f| match &f.payload {
            TaskEventPayload::TaskSubmitted {
                intent_digest,
                requester,
                ..
            } => Some((intent_digest.clone(), requester.clone())),
            _ => None,
        });
        if let Some((bound_digest_log, bound_requester)) = submitted {
            if bound_digest_log != intent.canonical_digest()
                || bound_requester != intent.requester.to_string()
            {
                // The log under this bound ref does not match the tuple: a
                // conflict, never a silent admission onto foreign truth.
                return SubmitReceipt::RequestIdConflict {
                    bound_digest: bound_digest_log,
                    submitted_digest: intent.canonical_digest(),
                };
            }
        } else if facts
            .iter()
            .any(|f| matches!(f.payload, TaskEventPayload::SubmitRejected { .. }))
        {
            // No TaskSubmitted, but a recorded definitive refusal: the
            // tuple replays to the same typed rejection.
            return SubmitReceipt::Rejected(AdmissionRejection::NoAdmittedExecutionPlan);
        } else {
            return SubmitReceipt::ReconciliationUnknown {
                digest: intent.canonical_digest(),
            };
        }
        // A recorded definitive refusal is TERMINAL for the tuple: TB-7
        // replay determinism — the same (requester, request_id) + digest
        // must always yield the same answer. Without this, a rejection
        // recorded during heal could flip to Admitted when a lane becomes
        // available on a later replay.
        if facts
            .iter()
            .any(|f| matches!(f.payload, TaskEventPayload::SubmitRejected { .. }))
        {
            return SubmitReceipt::Rejected(AdmissionRejection::NoAdmittedExecutionPlan);
        }
        let has_plan = facts
            .iter()
            .any(|f| matches!(f.payload, TaskEventPayload::PlanAdmitted { .. }));
        if !has_plan {
            match self
                .plans
                .admit_plan(task_ref, intent.routing_preference.as_ref())
            {
                Ok(plan) => {
                    let now = self.clock.now_utc_iso();
                    let event = self.fact(
                        EventSource::BridgeAdmission,
                        format!("evt-{}", uuid::Uuid::new_v4().simple()),
                        &now,
                        VisibilityClass::Internal,
                        TaskEventPayload::PlanAdmitted { plan },
                    );
                    if self.facts.append(task_ref, event).is_err() {
                        return SubmitReceipt::ReconciliationUnknown {
                            digest: intent.canonical_digest(),
                        };
                    }
                }
                Err(PlanAdmissionError::Unavailable) => {
                    return SubmitReceipt::ReconciliationUnknown {
                        digest: intent.canonical_digest(),
                    }
                }
                Err(PlanAdmissionError::NoAdmittedPlan) => {
                    // Consistent with the initial-submit refusal: without
                    // an admissible plan this submit is not admitted — and
                    // the definitive refusal is RECORDED so later replays
                    // of the same tuple stay deterministically Rejected.
                    let now = self.clock.now_utc_iso();
                    let event = self.fact(
                        EventSource::BridgeAdmission,
                        format!("evt-{}", uuid::Uuid::new_v4().simple()),
                        &now,
                        VisibilityClass::Internal,
                        TaskEventPayload::SubmitRejected {
                            reason: "no_admitted_execution_plan".to_string(),
                        },
                    );
                    if self.facts.append(task_ref, event).is_err() {
                        return SubmitReceipt::ReconciliationUnknown {
                            digest: intent.canonical_digest(),
                        };
                    }
                    return SubmitReceipt::Rejected(AdmissionRejection::NoAdmittedExecutionPlan);
                }
            }
        }
        SubmitReceipt::Admitted {
            task_ref: task_ref.clone(),
            replayed: true,
        }
    }

    /// `get(TaskRef)` (TB-8/TB-15): read projection over canonical truth.
    pub fn get(&self, task_ref: &TaskRef) -> Result<TaskSnapshot, GetError> {
        let events = self.derived_events(task_ref)?;
        if !has_task_submitted(&events) {
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
        if !has_task_submitted(&events) {
            return Err(GetError::NotFound);
        }
        let limit = limit.max(1);
        let total_missed = events.iter().filter(|event| event.seq > after_seq).count();
        let missed: Vec<TaskEvent> = events
            .into_iter()
            .filter(|event| event.seq > after_seq)
            .take(limit)
            .collect();
        let has_more = total_missed > missed.len();
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
        // TB-4 first and uniformly: every text-bearing intervention field
        // is scanned before any disposition (including the lineage
        // refusals below), so no refusal path becomes a content bypass.
        if let Err(rejection) = scan_intervention_texts(intervention) {
            let AdmissionRejection::ForbiddenContent { category, field } = rejection else {
                return Err(InterventionError::Unavailable);
            };
            return Err(InterventionError::ForbiddenContent { category, field });
        }
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

        // Requester admission (same authority surface as submit) and
        // requester-owns-task: a requester may only intervene on the task
        // it submitted. Both refusals collapse to NotFound so existence is
        // not leaked to non-owners.
        let authority = match self.authority.resolve(requester) {
            Err(RequesterAuthorityError::Unavailable) => {
                return Err(InterventionError::Unavailable)
            }
            Err(RequesterAuthorityError::NotAdmitted) => {
                return Err(InterventionError::RequesterNotAdmitted)
            }
            Ok(authority) => authority,
        };
        let _ = authority; // capability scope is submit-time; interventions need admission only.
                           // Linearize BEFORE reading state: ownership, advertisement, and
                           // the `expected_task_revision` compare-and-apply must all observe
                           // POST-LOCK truth (two requesters validating revision N
                           // concurrently must not both mutate — the second observes N+1 and
                           // conflicts).
        let _guard = self.op_lock.lock().expect("bridge op lock poisoned");
        let events = self.derived_events(task_ref)?;
        if !has_task_submitted(&events) || !requester_owns(&events, requester) {
            return Err(InterventionError::NotFound);
        }
        let snapshot = self.snapshot_from(task_ref, &events);
        // Advertisement check: typed refusal, zero mutation (TB-11).
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

        // TB-7 rule 6 tuple law: bind FIRST (atomic reserve), forward
        // SECOND — a concurrent duplicate collapses at the bind and can
        // never double-forward.
        let digest = composite_digest(&serde_json::json!({
            "task": task_ref.as_wire(),
            "intervention": intervention.canonical_digest(),
        }));
        let minted_id = format!("iv:{}", uuid::Uuid::new_v4().simple());
        let (bound_id, fresh) = match self.bindings.bind(
            requester,
            &request_id.to_string(),
            &digest,
            BoundRef::Intervention {
                task: task_ref.clone(),
                intervention_id: minted_id.clone(),
            },
        ) {
            Ok(returned) => match returned.bound {
                BoundRef::Intervention {
                    task,
                    intervention_id: bound,
                } => {
                    if task != *task_ref {
                        return Err(InterventionError::RequestIdConflict(
                            RequestConflict::RequestIdConflict {
                                bound_digest: returned.digest,
                                submitted_digest: digest,
                            },
                        ));
                    }
                    // Fresh bind returns OUR mint; a same-digest replay
                    // returns the winner's binding instead — only the
                    // FRESH binder may forward.
                    (bound.clone(), bound == minted_id)
                }
                // Cross-operation use of the same tuple.
                _ => {
                    return Err(InterventionError::RequestIdConflict(
                        RequestConflict::RequestIdConflict {
                            bound_digest: returned.digest,
                            submitted_digest: digest,
                        },
                    ))
                }
            },
            Err(conflict) => return Err(InterventionError::RequestIdConflict(conflict)),
        };
        if !fresh {
            // Replayed tuple already materialized? Return the one receipt.
            if let Ok(events_now) = self.derived_events(task_ref) {
                let materialized = events_now.iter().any(|e| match &e.payload {
                    TaskEventPayload::InterventionForwarded {
                        intervention_id, ..
                    } => intervention_id == &bound_id,
                    _ => false,
                });
                if materialized {
                    return Ok(replay_receipt(op, bound_id));
                }
            }
            // Bound but not materialized: the winner is mid-flight (or
            // died after forwarding). NEVER re-forward — the parallel of
            // submit's ambiguous window.
            return Err(InterventionError::ReconciliationUnknown);
        }
        // Forward the FULL typed intervention to the lifecycle owner
        // (tachi#1678 request path); the payload travels, not just the op.
        match self
            .owners
            .forward_intervention(task_ref, intervention, &bound_id)
        {
            OwnerForwardResult::Unsupported => {
                // The advertisement and the owner can disagree; the owner is
                // authority: typed refusal. The binding remains so a replay
                // of the same tuple re-attempts (and is refused again)
                // rather than silently binding to nothing.
                Err(InterventionError::UnsupportedByLifecycleOwner { operation: op })
            }
            OwnerForwardResult::Forwarded => {
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
                        intervention_id: bound_id.clone(),
                    },
                );
                if self.facts.append(task_ref, event).is_err() {
                    // Bound but not materialized: the parallel of submit's
                    // ambiguous window. The next same-tuple call re-attempts
                    // the forward (owner-side dedup is the production
                    // carrier's concern).
                    return Err(InterventionError::ReconciliationUnknown);
                }
                Ok(replay_receipt(op, bound_id))
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
        // Requester admission + requester-owns-task (collapsed to NotFound
        // so existence is not leaked).
        match self.authority.resolve(requester) {
            Err(RequesterAuthorityError::Unavailable) => {
                return Err(InterventionError::Unavailable)
            }
            Err(RequesterAuthorityError::NotAdmitted) => {
                return Err(InterventionError::RequesterNotAdmitted)
            }
            Ok(_) => {}
        }
        // TB-4: the stop reason is scanned like every text-bearing value.
        if let Err(rejection) = super::admission::scan_intervention_text(
            "stop.reason",
            &super::wire::BoundedText::new(reason).map_err(|_| InterventionError::Unavailable)?,
        ) {
            let AdmissionRejection::ForbiddenContent { category, field } = rejection else {
                return Err(InterventionError::Unavailable);
            };
            return Err(InterventionError::ForbiddenContent { category, field });
        }

        // Linearize BEFORE reading state (same law as intervene:
        // ownership, advertisement, and the revision compare-and-apply all
        // observe post-lock truth).
        let _guard = self.op_lock.lock().expect("bridge op lock poisoned");
        let events = self.derived_events(task_ref)?;
        if !has_task_submitted(&events) || !requester_owns(&events, requester) {
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

        // Stop identity digest = {task, mode} ONLY: the stop-alias
        // interventions (TB-11) and `request_stop` must share ONE
        // idempotency binding for the same stop operation regardless of
        // entry point; the reason rides the fact, not the identity (the
        // frozen `request_stop` signature has no reason parameter).
        let digest = composite_digest(&serde_json::json!({
            "task": task_ref.as_wire(),
            "mode": mode.as_str(),
        }));
        // Bind FIRST (atomic reserve), forward SECOND — concurrent
        // duplicates collapse at the bind; only the winner forwards.
        let stop_id = format!("stop:{}", uuid::Uuid::new_v4().simple());
        let (bound_id, was_fresh) = match self.bindings.bind(
            requester,
            &request_id.to_string(),
            &digest,
            BoundRef::Stop {
                task: task_ref.clone(),
                stop_id: stop_id.clone(),
            },
        ) {
            Ok(returned) => match returned.bound {
                BoundRef::Stop {
                    task,
                    stop_id: bound,
                } => {
                    // Fresh bind returns OUR mint; a same-digest replay
                    // returns the winner's binding instead.
                    let fresh = bound == stop_id;
                    if task != *task_ref {
                        return Err(InterventionError::RequestIdConflict(
                            RequestConflict::RequestIdConflict {
                                bound_digest: returned.digest,
                                submitted_digest: digest,
                            },
                        ));
                    }
                    (bound, fresh)
                }
                _ => {
                    return Err(InterventionError::RequestIdConflict(
                        RequestConflict::RequestIdConflict {
                            bound_digest: returned.digest,
                            submitted_digest: digest,
                        },
                    ))
                }
            },
            Err(conflict) => return Err(InterventionError::RequestIdConflict(conflict)),
        };
        // A replayed tuple that already materialized reports its CURRENT
        // stage and is NOT re-forwarded (a stop re-forwarded on replay
        // could duplicate the owner side effect).
        if !was_fresh {
            return Ok(current_stop_receipt(
                &events,
                task_ref,
                &bound_id,
                mode,
                &request_id.to_string(),
            ));
        }

        match self.owners.request_stop(task_ref, mode, reason) {
            OwnerForwardResult::Unsupported => {
                Err(InterventionError::UnsupportedByLifecycleOwner { operation: op })
            }
            forward => {
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
                        stop_id: bound_id.clone(),
                        mode: mode.as_str().to_string(),
                        reason: reason.to_string(),
                    },
                );
                if self.facts.append(task_ref, requested).is_err() {
                    return Err(InterventionError::ReconciliationUnknown);
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
                    stop_id: bound_id,
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
        if !has_task_submitted(&events) {
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
                    requester: _,
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

/// A task EXISTS on this bridge only once its `TaskSubmitted` fact has
/// materialized: bound-but-unmaterialized and bound-then-rejected refs are
/// NOT tasks (get/watch/collect all say NotFound).
fn has_task_submitted(events: &[TaskEvent]) -> bool {
    events
        .iter()
        .any(|e| matches!(e.payload, TaskEventPayload::TaskSubmitted { .. }))
}

/// Requester-owns-task: the task's `TaskSubmitted` fact names this
/// requester. Non-owners get the same NotFound as strangers (existence is
/// not leaked).
fn requester_owns(events: &[TaskEvent], requester: &RequesterRef) -> bool {
    let owner = requester.to_string();
    events.iter().any(|e| match &e.payload {
        TaskEventPayload::TaskSubmitted { requester, .. } => *requester == owner,
        _ => false,
    })
}

/// TB-4 scan over every text-bearing intervention field.
fn scan_intervention_texts(
    intervention: &InterventionV1,
) -> Result<(), super::admission::AdmissionRejection> {
    use super::admission::scan_intervention_text as scan;
    match intervention {
        InterventionV1::ProvideAdditionalContext { note } => scan("intervention.note", note),
        InterventionV1::RequestCorrection { note } => scan("intervention.note", note),
        InterventionV1::RequestContinuation { note } => scan("intervention.note", note),
        InterventionV1::RequestIndependentReview { .. } => Ok(()),
        InterventionV1::RequestUserInput { prompt } => scan("intervention.prompt", prompt),
        InterventionV1::RequestPause | InterventionV1::RequestResume => Ok(()),
        InterventionV1::RequestGracefulStop { reason }
        | InterventionV1::RequestHardCancel { reason } => scan("intervention.reason", reason),
        InterventionV1::Escalate { reason } => scan("intervention.reason", reason),
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
    bound_stop_id: &str,
    mode: StopMode,
    request_id: &str,
) -> StopReceipt {
    // Replay reflects the CURRENT stage, derived from the fact log.
    // Bound-but-unmaterialized ⇒ Requested (the forward state is unknown,
    // never claimed): only THIS stop's materialized StopRequested fact
    // states a forward happened; only an owner-confirmed CANCELLATION
    // confirms; disappearance pins OutcomeUnknown.
    let mut stage = StopStage::Requested;
    for event in events {
        if let TaskEventPayload::StopRequested { stop_id, .. } = &event.payload {
            if stop_id == bound_stop_id {
                stage = StopStage::Forwarded;
            }
        }
    }
    // Terminal promotions apply only to a stop that MATERIALIZED (stage
    // Forwarded): a bound-but-unmaterialized stop cannot skip straight to
    // Confirmed/OutcomeUnknown off task-level facts from an older stop.
    if stage == StopStage::Forwarded {
        for event in events {
            match &event.payload {
                // ONLY an owner-confirmed CANCELLATION confirms the stop —
                // a confirmed "completed" (the work finished on its own) is
                // not a stop confirmation (TB-12).
                TaskEventPayload::OwnerConfirmedTerminal { terminal }
                    if terminal == "cancelled" =>
                {
                    stage = StopStage::Confirmed;
                }
                TaskEventPayload::OwnerDisappeared => stage = StopStage::OutcomeUnknown,
                _ => {}
            }
        }
    }
    StopReceipt {
        task_ref: task_ref.clone(),
        stop_id: bound_stop_id.to_string(),
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
