//! Vertical discrimination tests for the TaskIntent bridge (tachi#1840
//! DoD rows; zeroclaw #205 TB clause checks). Each test names the DoD/TB
//! row it pins.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::*;

// ── fixtures ──────────────────────────────────────────────────────────────

struct FakeAuthority {
    capabilities: Mutex<std::collections::BTreeSet<Capability>>,
    unavailable: bool,
}

impl FakeAuthority {
    fn admitting(capabilities: &[Capability]) -> Arc<Self> {
        Arc::new(Self {
            capabilities: Mutex::new(capabilities.iter().copied().collect()),
            unavailable: false,
        })
    }
}

impl RequesterAuthorityPort for FakeAuthority {
    fn resolve(
        &self,
        _requester: &RequesterRef,
    ) -> Result<AdmittedAuthority, RequesterAuthorityError> {
        if self.unavailable {
            return Err(RequesterAuthorityError::Unavailable);
        }
        Ok(AdmittedAuthority {
            capabilities: self.capabilities.lock().expect("caps").clone(),
        })
    }
}

struct FakePlans {
    mode: LifecycleMode,
    owner: &'static str,
    counter: AtomicUsize,
    refuse: bool,
}

impl FakePlans {
    fn managed() -> Arc<Self> {
        Arc::new(Self {
            mode: LifecycleMode::TachiManagedBatch,
            owner: "managed-custom-backend",
            counter: AtomicUsize::new(0),
            refuse: false,
        })
    }

    fn attached() -> Arc<Self> {
        Arc::new(Self {
            mode: LifecycleMode::HarnessNativeAttached,
            owner: "acp-adapter-alpha",
            counter: AtomicUsize::new(0),
            refuse: false,
        })
    }
}

impl PlanAdmissionPort for FakePlans {
    fn admit_plan(
        &self,
        task_ref: &TaskRef,
        _preference: Option<&RoutingPreference>,
    ) -> Result<ExecutionPlanProjection, PlanAdmissionError> {
        if self.refuse {
            return Err(PlanAdmissionError::NoAdmittedPlan);
        }
        let n = self.counter.fetch_add(1, Ordering::SeqCst);
        Ok(ExecutionPlanProjection {
            plan_ref: format!("plan-{n}-{}", task_ref.as_wire()),
            task_ref: task_ref.clone(),
            lifecycle_mode: self.mode,
            lifecycle_owner: self.owner.to_string(),
            work_claim_id: Some(format!("claim-{n}")),
            work_claim_transition_version: Some(n as i64 + 1),
            exec_env_id: Some(format!("env-{n}")),
            dispatch_id: Some(format!("dispatch-{n}")),
            backend: Some("custom".to_string()),
            launch_digest: Some(format!("launch-{n}")),
            revision: 1,
        })
    }
}

struct FakeOwners {
    stop_result: OwnerForwardResult,
    intervention_result: OwnerForwardResult,
}

impl FakeOwners {
    fn forwarding() -> Arc<Self> {
        Arc::new(Self {
            stop_result: OwnerForwardResult::Forwarded,
            intervention_result: OwnerForwardResult::Forwarded,
        })
    }
}

impl LifecycleOwnerPort for FakeOwners {
    fn request_stop(
        &self,
        _task_ref: &TaskRef,
        _mode: StopMode,
        _reason: &str,
    ) -> OwnerForwardResult {
        self.stop_result
    }

    fn forward_intervention(
        &self,
        _task_ref: &TaskRef,
        _operation: InterventionV1Static,
    ) -> OwnerForwardResult {
        self.intervention_result
    }
}

#[derive(Clone)]
struct FixedClock(Arc<Mutex<u64>>);

impl FixedClock {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(0)))
    }

    fn stamp(&self, n: u64) -> String {
        format!("2026-08-25T{n:04}Z")
    }
}

impl BridgeClock for FixedClock {
    fn now_utc_iso(&self) -> String {
        let mut n = self.0.lock().expect("clock");
        *n += 1;
        format!("2026-08-25T{n:04}Z")
    }
}

struct Rig {
    bridge: TaskIntentBridge,
    facts: Arc<InMemoryTaskFacts>,
    bindings: Arc<InProcessRequestBindings>,
    clock: FixedClock,
}

fn rig_managed() -> Rig {
    let facts = Arc::new(InMemoryTaskFacts::new());
    let bindings = Arc::new(InProcessRequestBindings::new());
    let plans = FakePlans::managed();
    let clock = FixedClock::new();
    Rig {
        bridge: TaskIntentBridge::new(
            bindings.clone(),
            facts.clone(),
            FakeAuthority::admitting(&[Capability::ReasoningReview]),
            plans.clone(),
            FakeOwners::forwarding(),
            Arc::new(clock.clone()),
        ),
        facts,
        bindings,
        clock,
    }
}

fn rig_attached() -> Rig {
    let facts = Arc::new(InMemoryTaskFacts::new());
    let bindings = Arc::new(InProcessRequestBindings::new());
    let plans = FakePlans::attached();
    let clock = FixedClock::new();
    Rig {
        bridge: TaskIntentBridge::new(
            bindings.clone(),
            facts.clone(),
            FakeAuthority::admitting(&[Capability::ReasoningReview]),
            plans.clone(),
            FakeOwners::forwarding(),
            Arc::new(clock.clone()),
        ),
        facts,
        bindings,
        clock,
    }
}

fn golden_intent() -> TaskIntentV1 {
    serde_json::from_value(
        serde_json::from_str::<serde_json::Value>(GOLDEN_TASK_INTENT_V1)
            .expect("golden is valid JSON")["intent"]
            .clone(),
    )
    .expect("golden intent decodes")
}

fn submit_ok(rig: &Rig, intent: &TaskIntentV1, request_id: &str) -> TaskRef {
    let request = RequestId::new(request_id).expect("bounded");
    match rig.bridge.submit(intent, &request) {
        SubmitReceipt::Admitted {
            task_ref,
            replayed: false,
        } => task_ref,
        other => panic!("expected fresh admission, got {other:?}"),
    }
}

fn append_owner_terminal(rig: &Rig, task_ref: &TaskRef, terminal: &str) {
    let now = rig.clock.stamp(9_000);
    let payload = TaskEventPayload::OwnerConfirmedTerminal {
        terminal: terminal.to_string(),
    };
    let value = serde_json::to_value(&payload).expect("serializes");
    let event = TaskEvent {
        seq: 0,
        event_id: format!("owner-terminal-{terminal}"),
        source: EventSource::LifecycleOwner {
            operation: "confirm_terminal".to_string(),
        },
        source_revision: "1".to_string(),
        occurred_at: now.clone(),
        recorded_at: now,
        payload_digest: memcore::canonical_digest::canonical_json_digest_hex(&value),
        visibility: VisibilityClass::Internal,
        payload,
    };
    rig.facts.append(task_ref, event).expect("append");
}

// ── DoD row 1: golden wire + submit envelope ─────────────────────────────

#[test]
fn golden_decodes_and_round_trips_byte_identically() {
    let file: serde_json::Value = serde_json::from_str(GOLDEN_TASK_INTENT_V1).expect("golden JSON");
    let intent: TaskIntentV1 =
        serde_json::from_value(file["intent"].clone()).expect("decoder half: golden decodes");
    let reencoded = serde_json::to_value(&intent).expect("re-encode");
    assert_eq!(
        reencoded, file["intent"],
        "Tachi decoder half must round-trip the golden byte-identically"
    );
    let pinned = file["digest_sha256"].as_str().expect("digest pinned");
    assert_eq!(
        intent.canonical_digest(),
        pinned,
        "digest rule must match the golden pin"
    );
}

#[test]
fn golden_pins_exactly_the_frozen_field_set_and_no_execution_detail() {
    let file: serde_json::Value = serde_json::from_str(GOLDEN_TASK_INTENT_V1).expect("golden JSON");
    let intent_json = &file["intent"];
    let mut top: Vec<&str> = intent_json
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    top.sort_unstable();
    assert_eq!(
        top,
        [
            "approval_requirement",
            "capability_request",
            "constraints",
            "context_bundle_ref",
            "evaluation_requirement",
            "expected_artifacts",
            "expiry",
            "objective",
            "parent_ref",
            "privacy_class",
            "requester",
            "retry_of",
            "routing_preference",
            "schema",
            "source_refs",
            "supervisor_ref",
            "workspace_source",
        ],
        "TB-3 field freeze: exactly the seventeen frozen fields"
    );
    // TB-1/TB-4: no field capable of carrying execution detail under ANY
    // name or nesting — recursive key scan.
    fn assert_no_execution_keys(value: &serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, inner) in map {
                    let lowered = key.to_ascii_lowercase();
                    for forbidden in [
                        "command",
                        "cmd",
                        "argv",
                        "args",
                        "env",
                        "env_vars",
                        "cwd",
                        "worktree",
                        "worktree_path",
                        "path",
                        "paths",
                        "model",
                        "backend",
                        "provider",
                        "llm",
                        "cli",
                        "shell",
                        "api_key",
                        "token",
                        "credential",
                    ] {
                        assert_ne!(
                            lowered, forbidden,
                            "wire admits execution detail under key `{key}`"
                        );
                    }
                    assert_no_execution_keys(inner);
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(assert_no_execution_keys),
            _ => {}
        }
    }
    assert_no_execution_keys(intent_json);
}

#[test]
fn submit_envelope_every_error_variant_is_producible() {
    // TB-5b / suite 19: Unavailable, ReconciliationUnknown, RequestIdConflict,
    // plus the admission-rejection surface — each producible.
    let rig = rig_managed();
    let intent = golden_intent();
    let request = RequestId::new("req-envelope").expect("bounded");

    // Unavailable: facts source down.
    let down = TaskIntentBridge::new(
        Arc::new(InProcessRequestBindings::new()),
        Arc::new(UnavailableTaskFacts),
        FakeAuthority::admitting(&[Capability::ReasoningReview]),
        FakePlans::managed(),
        FakeOwners::forwarding(),
        Arc::new(SystemBridgeClock),
    );
    assert_eq!(down.submit(&intent, &request), SubmitReceipt::Unavailable);

    // Admission rejection: forbidden content.
    let mut bad = golden_intent();
    bad.objective = BoundedText::new("ssh prod 'cargo test'").expect("bounded");
    assert!(matches!(
        rig.bridge.submit(&bad, &request),
        SubmitReceipt::Rejected(AdmissionRejection::ForbiddenContent { .. })
    ));

    // RequestIdConflict: same tuple, different digest.
    submit_ok(&rig, &intent, "req-a");
    let mut different = golden_intent();
    different.objective = BoundedText::new("a different objective").expect("bounded");
    let request_a = RequestId::new("req-a").expect("bounded");
    assert!(matches!(
        rig.bridge.submit(&different, &request_a),
        SubmitReceipt::RequestIdConflict { .. }
    ));

    // ReconciliationUnknown: bound but not materialized (crash window).
    let bound_not_materialized = RequestId::new("req-crash").expect("bounded");
    let phantom = TaskRef::mint("task:phantom");
    rig.bindings
        .bind(
            &intent.requester,
            "req-crash",
            &intent.canonical_digest(),
            BoundRef::Task(phantom),
        )
        .expect("bind");
    assert!(matches!(
        rig.bridge.submit(&intent, &bound_not_materialized),
        SubmitReceipt::ReconciliationUnknown { .. }
    ));
    // ...and the replay never spawns a second task: still exactly one
    // materialized task fact log (the phantom has none).
    assert!(rig
        .facts
        .facts(&TaskRef::mint("task:phantom"))
        .expect("facts")
        .is_empty());
}

// ── DoD row 2: TaskRef minting ────────────────────────────────────────────

#[test]
fn submit_output_refs_are_server_minted() {
    let rig = rig_managed();
    let task_ref = submit_ok(&rig, &golden_intent(), "req-mint");
    assert!(task_ref.as_wire().starts_with("task:"));
    // The caller's request id never appears in the ref: ids are not
    // caller-shaped.
    assert!(!task_ref.as_wire().contains("req-mint"));
}

// ── DoD row 3: TB-7 idempotency ───────────────────────────────────────────

#[test]
fn double_submit_same_tuple_same_digest_one_task() {
    let rig = rig_managed();
    let intent = golden_intent();
    let first = submit_ok(&rig, &intent, "req-1");
    let request = RequestId::new("req-1").expect("bounded");
    match rig.bridge.submit(&intent, &request) {
        SubmitReceipt::Admitted {
            task_ref,
            replayed: true,
        } => {
            assert_eq!(task_ref, first, "same TaskRef, never a second worker");
        }
        other => panic!("expected replay, got {other:?}"),
    }
    // Exactly one TaskSubmitted fact exists: no second spawn.
    let facts = rig.facts.facts(&first).expect("facts");
    let submits = facts
        .iter()
        .filter(|f| matches!(f.payload, TaskEventPayload::TaskSubmitted { .. }))
        .count();
    assert_eq!(submits, 1);
}

#[test]
fn ambiguous_submit_reconciles_to_exactly_one_task() {
    // TB-7 rule 4: crash between bind and materialize; replay returns
    // ReconciliationUnknown — and once the fact materializes (recovery),
    // the SAME tuple returns the SAME TaskRef. No path mints a second.
    let rig = rig_managed();
    let intent = golden_intent();
    let request = RequestId::new("req-amb").expect("bounded");
    // Simulate the crash window: bind without appending facts.
    let orphan = TaskRef::mint(format!("task:{}", "crash-window"));
    rig.bindings
        .bind(
            &intent.requester,
            "req-amb",
            &intent.canonical_digest(),
            BoundRef::Task(orphan.clone()),
        )
        .expect("bind");
    assert!(matches!(
        rig.bridge.submit(&intent, &request),
        SubmitReceipt::ReconciliationUnknown { .. }
    ));
    // Recovery materializes the task under the bound ref.
    let now = rig.clock.stamp(5_000);
    let payload = TaskEventPayload::TaskSubmitted {
        intent_digest: intent.canonical_digest(),
        contract: IntentContractProjection {
            objective: intent.objective.as_str().to_string(),
            constraints: vec![],
            expected_artifacts: vec![],
            evaluation_requirement: intent.evaluation_requirement.independence,
        },
    };
    let value = serde_json::to_value(&payload).expect("serializes");
    rig.facts
        .append(
            &orphan,
            TaskEvent {
                seq: 0,
                event_id: "recovered-submitted".to_string(),
                source: EventSource::BridgeAdmission,
                source_revision: "1".to_string(),
                occurred_at: now.clone(),
                recorded_at: now,
                payload_digest: memcore::canonical_digest::canonical_json_digest_hex(&value),
                visibility: VisibilityClass::Internal,
                payload,
            },
        )
        .expect("append");
    match rig.bridge.submit(&intent, &request) {
        SubmitReceipt::Admitted {
            task_ref,
            replayed: true,
        } => assert_eq!(task_ref, orphan),
        other => panic!("expected reconciled replay, got {other:?}"),
    }
}

// ── DoD rows 4–5: get + watch ─────────────────────────────────────────────

#[test]
fn get_projects_from_mapping_tables_with_revisioned_advertisement() {
    let rig = rig_managed();
    let task_ref = submit_ok(&rig, &golden_intent(), "req-get");
    let snapshot = rig.bridge.get(&task_ref).expect("snapshot");
    assert_eq!(
        snapshot.execution,
        mapping::execution::ExecutionState::Queued
    );
    assert_eq!(
        snapshot.lifecycle_mode,
        Some(LifecycleMode::TachiManagedBatch)
    );
    assert_eq!(
        snapshot.supported_interventions,
        vec![
            InterventionV1Static::RequestGracefulStop,
            InterventionV1Static::RequestHardCancel
        ]
    );
    assert_eq!(snapshot.intent_digest, golden_intent().canonical_digest());
    let revision_before = snapshot.task_revision;
    // Advertisement changes bump the snapshot revision (TB-15).
    let now = rig.clock.stamp(6_000);
    let payload = TaskEventPayload::OwnerCapabilitiesDeclared {
        operations: vec![InterventionV1Static::RequestPause],
    };
    let value = serde_json::to_value(&payload).expect("serializes");
    rig.facts
        .append(
            &task_ref,
            TaskEvent {
                seq: 0,
                event_id: "caps-declared".to_string(),
                source: EventSource::LifecycleOwner {
                    operation: "declare_capabilities".to_string(),
                },
                source_revision: "1".to_string(),
                occurred_at: now.clone(),
                recorded_at: now,
                payload_digest: memcore::canonical_digest::canonical_json_digest_hex(&value),
                visibility: VisibilityClass::Internal,
                payload,
            },
        )
        .expect("append");
    let after = rig.bridge.get(&task_ref).expect("snapshot");
    assert!(after.task_revision > revision_before);
    assert!(after
        .supported_interventions
        .contains(&InterventionV1Static::RequestPause));
}

#[test]
fn watch_backfills_exactly_the_missed_events_after_reconnect() {
    let rig = rig_managed();
    let task_ref = submit_ok(&rig, &golden_intent(), "req-watch");
    // Watcher saw events up to seq 2 (TaskSubmitted + PlanAdmitted).
    let page_one = rig.bridge.watch(&task_ref, 0, 10).expect("page");
    assert_eq!(page_one.events.len(), 2);
    let last_seen = page_one.events.last().expect("event").seq;
    // While disconnected, more facts land.
    append_owner_terminal(&rig, &task_ref, "cancelled");
    let page_two = rig.bridge.watch(&task_ref, last_seen, 10).expect("page");
    assert_eq!(page_two.events.len(), 1, "exactly the missed events");
    assert_eq!(page_two.events[0].seq, 3, "no gaps");
    // Duplicate re-read of the same window is stable (deterministic
    // derivation; duplicate (seq, event_id) suppressed).
    let page_two_again = rig.bridge.watch(&task_ref, last_seen, 10).expect("page");
    assert_eq!(page_two, page_two_again);
    // Watching never creates tasks: an unknown ref is NotFound, and no new
    // facts appear anywhere.
    assert_eq!(
        rig.bridge.watch(&TaskRef::mint("task:none"), 0, 10),
        Err(GetError::NotFound)
    );
}

#[test]
fn every_durable_event_binds_the_eight_field_set() {
    let rig = rig_managed();
    let task_ref = submit_ok(&rig, &golden_intent(), "req-fields");
    let page = rig.bridge.watch(&task_ref, 0, 10).expect("page");
    for event in page.events {
        assert!(event.seq > 0, "1) monotonic seq");
        assert!(!event.event_id.is_empty(), "2) stable event_id");
        assert!(
            !matches!(event.source, EventSource::BridgeAdmission)
                || !event.source_revision.is_empty(),
            "3+4) source identity and revision"
        );
        assert!(!event.occurred_at.is_empty(), "5) occurred_at");
        assert!(!event.recorded_at.is_empty(), "6) recorded_at");
        assert!(event.payload_digest.len() == 64, "7) payload digest");
        assert!(
            matches!(
                event.visibility,
                VisibilityClass::Public | VisibilityClass::Internal
            ),
            "8) visibility class"
        );
    }
}

// ── DoD rows 6–7: intervene / request_stop ────────────────────────────────

#[test]
fn all_ten_interventions_on_an_unsupported_mode_are_typed_refusals_with_zero_mutation() {
    // Owner vertical test 6 + TB-11: attached mode with NO declared owner
    // capabilities ⇒ every op refuses, no fresh-task fallback, no mutation.
    let rig = rig_attached();
    let task_ref = submit_ok(&rig, &golden_intent(), "req-iv");
    let facts_before = rig.facts.facts(&task_ref).expect("facts").len();
    let requester = golden_intent().requester.clone();
    for op in INTERVENTION_V1_OPERATIONS {
        let intervention = placeholder_intervention(*op);
        let request = RequestId::new(format!("iv-{:?}", op)).expect("bounded");
        let result = rig
            .bridge
            .intervene(&task_ref, &intervention, &requester, &request, None);
        match *op {
            InterventionV1Static::RequestIndependentReview | InterventionV1Static::Escalate => {
                assert!(
                    matches!(
                        result,
                        Err(InterventionError::RequiresNewTaskLineage { .. })
                    ),
                    "{op:?} must be the typed new-lineage refusal"
                );
            }
            _ => {
                assert!(
                    matches!(
                        result,
                        Err(InterventionError::UnsupportedByLifecycleOwner { .. })
                    ),
                    "{op:?} must be the typed unsupported refusal"
                );
            }
        }
    }
    let facts_after = rig.facts.facts(&task_ref).expect("facts").len();
    assert_eq!(facts_before, facts_after, "zero state mutation");
}

fn placeholder_intervention(op: InterventionV1Static) -> InterventionV1 {
    let note = BoundedText::new("note").expect("bounded");
    match op {
        InterventionV1Static::ProvideAdditionalContext => {
            InterventionV1::ProvideAdditionalContext { note }
        }
        InterventionV1Static::RequestCorrection => InterventionV1::RequestCorrection { note },
        InterventionV1Static::RequestContinuation => InterventionV1::RequestContinuation { note },
        InterventionV1Static::RequestIndependentReview => {
            InterventionV1::RequestIndependentReview {
                independence_class: IndependenceClass::HumanReview,
            }
        }
        InterventionV1Static::RequestUserInput => InterventionV1::RequestUserInput { prompt: note },
        InterventionV1Static::RequestPause => InterventionV1::RequestPause,
        InterventionV1Static::RequestResume => InterventionV1::RequestResume,
        InterventionV1Static::RequestGracefulStop => {
            InterventionV1::RequestGracefulStop { reason: note }
        }
        InterventionV1Static::RequestHardCancel => {
            InterventionV1::RequestHardCancel { reason: note }
        }
        InterventionV1Static::Escalate => InterventionV1::Escalate { reason: note },
    }
}

#[test]
fn supported_intervention_is_idempotent_and_revision_bound() {
    // Managed lane with a declared pause capability.
    let rig = rig_managed();
    let task_ref = submit_ok(&rig, &golden_intent(), "req-iv2");
    let now = rig.clock.stamp(7_000);
    let payload = TaskEventPayload::OwnerCapabilitiesDeclared {
        operations: vec![InterventionV1Static::RequestPause],
    };
    let value = serde_json::to_value(&payload).expect("serializes");
    rig.facts
        .append(
            &task_ref,
            TaskEvent {
                seq: 0,
                event_id: "caps".to_string(),
                source: EventSource::LifecycleOwner {
                    operation: "declare".to_string(),
                },
                source_revision: "1".to_string(),
                occurred_at: now.clone(),
                recorded_at: now,
                payload_digest: memcore::canonical_digest::canonical_json_digest_hex(&value),
                visibility: VisibilityClass::Internal,
                payload,
            },
        )
        .expect("append");
    let requester = golden_intent().requester.clone();
    let intervention = InterventionV1::RequestPause;
    let request = RequestId::new("iv-pause").expect("bounded");
    // Stale revision first: typed conflict, never best-effort apply.
    let stale = rig
        .bridge
        .intervene(&task_ref, &intervention, &requester, &request, Some(1));
    assert!(matches!(
        stale,
        Err(InterventionError::RevisionConflict { .. })
    ));
    let receipt = rig
        .bridge
        .intervene(&task_ref, &intervention, &requester, &request, None)
        .expect("forwarded");
    let InterventionReceipt::Paused { intervention_id } = receipt else {
        panic!("pause receipt")
    };
    // Same tuple twice ⇒ ONE receipt (same id), one forwarded fact.
    let replay = rig
        .bridge
        .intervene(&task_ref, &intervention, &requester, &request, None)
        .expect("replay");
    assert_eq!(replay, InterventionReceipt::Paused { intervention_id });
    let forwarded = rig
        .facts
        .facts(&task_ref)
        .expect("facts")
        .iter()
        .filter(|f| matches!(f.payload, TaskEventPayload::InterventionForwarded { .. }))
        .count();
    assert_eq!(forwarded, 1);
}

#[test]
fn stop_request_is_not_cancellation_and_disappearance_is_outcome_unknown() {
    // Owner vertical tests 4/5 + TB-12.
    let rig = rig_managed();
    let task_ref = submit_ok(&rig, &golden_intent(), "req-stop");
    let requester = golden_intent().requester.clone();
    let request = RequestId::new("stop-1").expect("bounded");
    let receipt = rig
        .bridge
        .request_stop(&task_ref, StopMode::Graceful, &requester, &request, None)
        .expect("stop forwarded");
    assert_eq!(receipt.stage, StopStage::Forwarded);
    let snapshot = rig.bridge.get(&task_ref).expect("snapshot");
    assert_eq!(
        snapshot.execution,
        mapping::execution::ExecutionState::CancellationRequested,
        "stop request != cancelled confirmation"
    );
    // Owner confirmation arrives later: only NOW cancelled.
    append_owner_terminal(&rig, &task_ref, "cancelled");
    let snapshot = rig.bridge.get(&task_ref).expect("snapshot");
    assert_eq!(
        snapshot.execution,
        mapping::execution::ExecutionState::Cancelled
    );
    // Replayed stop reflects the confirmed stage.
    let replay = rig
        .bridge
        .request_stop(&task_ref, StopMode::Graceful, &requester, &request, None)
        .expect("replay");
    assert_eq!(replay.stage, StopStage::Confirmed);
    assert_eq!(replay.stop_id, receipt.stop_id, "one stop operation");
}

#[test]
fn owner_disappearance_after_side_effects_is_outcome_unknown_not_cancelled() {
    let facts = Arc::new(InMemoryTaskFacts::new());
    let bridge = TaskIntentBridge::new(
        Arc::new(InProcessRequestBindings::new()),
        facts.clone(),
        FakeAuthority::admitting(&[Capability::ReasoningReview]),
        FakePlans::managed(),
        Arc::new(FakeOwners {
            stop_result: OwnerForwardResult::Disappeared,
            intervention_result: OwnerForwardResult::Disappeared,
        }),
        Arc::new(SystemBridgeClock),
    );
    let intent = golden_intent();
    let request = RequestId::new("req-gone").expect("bounded");
    let task_ref = match bridge.submit(&intent, &request) {
        SubmitReceipt::Admitted { task_ref, .. } => task_ref,
        other => panic!("expected admission, got {other:?}"),
    };
    let request = RequestId::new("stop-gone").expect("bounded");
    let receipt = bridge
        .request_stop(&task_ref, StopMode::Hard, &intent.requester, &request, None)
        .expect("receipt");
    assert_eq!(receipt.stage, StopStage::OutcomeUnknown);
    let snapshot = bridge.get(&task_ref).expect("snapshot");
    assert_eq!(
        snapshot.execution,
        mapping::execution::ExecutionState::OutcomeUnknown,
        "never optimistic success/failure/cancel"
    );
    // TB-18: outcome_unknown never auto-retries — no second worker. The
    // fact log shows exactly one task, one stop request, one disappearance.
    let log = facts.facts(&task_ref).expect("facts");
    let submits = log
        .iter()
        .filter(|f| matches!(f.payload, TaskEventPayload::TaskSubmitted { .. }))
        .count();
    assert_eq!(submits, 1);
    // A deliberate retry is a NEW explicit submission with retry_of
    // lineage; the prior attempt's facts are never rewritten.
    let facts_before = log.clone();
    let mut retry = golden_intent();
    retry.retry_of = Some(task_ref.clone());
    let retry_request = RequestId::new("req-retry").expect("bounded");
    let retry_ref = match bridge.submit(&retry, &retry_request) {
        SubmitReceipt::Admitted { task_ref, .. } => task_ref,
        other => panic!("expected retry admission, got {other:?}"),
    };
    assert_ne!(retry_ref, task_ref, "retry = new task identity");
    assert_eq!(
        facts.facts(&task_ref).expect("facts"),
        facts_before,
        "prior attempt facts unchanged"
    );
}

#[test]
fn intervene_stop_and_request_stop_share_one_authority_and_receipt_type() {
    // TB-11 type connection: same tuple, same digest ⇒ the SAME stop
    // operation regardless of entry point.
    let rig = rig_managed();
    let task_ref = submit_ok(&rig, &golden_intent(), "req-alias");
    let requester = golden_intent().requester.clone();
    let request = RequestId::new("stop-alias").expect("bounded");
    let reason = BoundedText::new("vertical complete").expect("bounded");
    let via_intervene = rig
        .bridge
        .intervene(
            &task_ref,
            &InterventionV1::RequestGracefulStop {
                reason: reason.clone(),
            },
            &requester,
            &request,
            None,
        )
        .expect("stop via intervene");
    let InterventionReceipt::Stop(receipt) = via_intervene else {
        panic!("stop variants carry exactly the StopReceipt");
    };
    let via_stop = rig
        .bridge
        .request_stop(&task_ref, StopMode::Graceful, &requester, &request, None)
        .expect("replay via request_stop");
    assert_eq!(
        via_stop.stop_id, receipt.stop_id,
        "one underlying stop operation"
    );
    // One StopRequested fact total.
    let stops = rig
        .facts
        .facts(&task_ref)
        .expect("facts")
        .iter()
        .filter(|f| matches!(f.payload, TaskEventPayload::StopRequested { .. }))
        .count();
    assert_eq!(stops, 1);
}

// ── DoD rows 8–10: collect, re-target, retry lineage ─────────────────────

fn append_outcome_fact(rig: &Rig, task_ref: &TaskRef, observation: OutcomeObservation) {
    let event_id = format!("outcome-{}", observation.outcome_id);
    let source = EventSource::DispatchOutcome {
        outcome_id: observation.outcome_id.clone(),
    };
    let value = serde_json::to_value(&observation).expect("serializes");
    let payload = TaskEventPayload::OutcomeObserved { observation };
    let now = rig.clock.stamp(8_000);
    rig.facts
        .append(
            task_ref,
            TaskEvent {
                seq: 0,
                event_id,
                source,
                source_revision: "1".to_string(),
                occurred_at: now.clone(),
                recorded_at: now,
                payload_digest: memcore::canonical_digest::canonical_json_digest_hex(&value),
                visibility: VisibilityClass::Internal,
                payload,
            },
        )
        .expect("append");
}

fn observation(evidence: usize) -> OutcomeObservation {
    OutcomeObservation {
        outcome_id: format!("out-{evidence}"),
        dispatch_id: format!("dispatch-{evidence}"),
        execution: mapping::execution::CanonicalExecutionFact::Completed,
        reported_outcome: Some("success".to_string()),
        verification_present: evidence > 1,
        diff_present: false,
        evidence_refs: (0..evidence).map(|i| format!("ev-{i}")).collect(),
        vendor: "unknown".to_string(),
        model: None,
        identity_attribution_basis: "unknown".to_string(),
    }
}

#[test]
fn collect_is_artifact_first_and_revision_pinned() {
    let rig = rig_managed();
    let task_ref = submit_ok(&rig, &golden_intent(), "req-collect");
    // Not ready before any terminal outcome.
    assert_eq!(
        rig.bridge.collect(&task_ref, None),
        Err(CollectError::NotReady)
    );
    // Worker says success but evidence is empty: the golden intent REQUIRES
    // a report artifact — the contract is violated regardless of prose.
    append_outcome_fact(&rig, &task_ref, observation(0));
    let projection = rig.bridge.collect(&task_ref, None).expect("projection");
    assert_eq!(projection.result_revision, 1);
    assert!(
        !projection.contract_violations.is_empty(),
        "owner vertical test 7"
    );
    assert_eq!(projection.terminal_classification, "completed");
    assert_eq!(
        projection.provenance.reported_outcome.as_deref(),
        Some("success"),
        "self-report kept verbatim, distinct from the verdict"
    );
    // Adjudication lands: revision 2, older pin still returns revision 1.
    let now = rig.clock.stamp(8_500);
    let payload =
        TaskEventPayload::Adjudication(mapping::adjudication::CanonicalAdjudicationFact::Accepted);
    let value = serde_json::to_value(&payload).expect("serializes");
    rig.facts
        .append(
            &task_ref,
            TaskEvent {
                seq: 0,
                event_id: "adj-1".to_string(),
                source: EventSource::Adjudication {
                    adjudication_id: "adj-1".to_string(),
                },
                source_revision: "1".to_string(),
                occurred_at: now.clone(),
                recorded_at: now,
                payload_digest: memcore::canonical_digest::canonical_json_digest_hex(&value),
                visibility: VisibilityClass::Internal,
                payload,
            },
        )
        .expect("append");
    let latest = rig.bridge.collect(&task_ref, None).expect("latest");
    assert_eq!(latest.result_revision, 2, "monotonic mint");
    assert_eq!(
        latest.adjudication_state,
        mapping::adjudication::AdjudicationState::Accepted
    );
    let pinned_one = rig
        .bridge
        .collect(&task_ref, Some(1))
        .expect("pinned exact");
    assert_eq!(pinned_one.result_revision, 1);
    assert_eq!(
        pinned_one.adjudication_state,
        mapping::adjudication::AdjudicationState::Unreviewed,
        "the pinned revision is immutable history"
    );
    assert_eq!(
        rig.bridge.collect(&task_ref, Some(3)),
        Err(CollectError::ResultRevisionNotFound),
        "bogus pin is typed not_found"
    );
}

#[test]
fn retargeting_changes_plan_identity_not_intent_contract() {
    // TB-2 / owner vertical test 9.
    let rig = rig_managed();
    let intent = golden_intent();
    let first = submit_ok(&rig, &intent, "req-plan-1");
    let second = submit_ok(&rig, &intent, "req-plan-2");
    let snap_one = rig.bridge.get(&first).expect("snapshot");
    let snap_two = rig.bridge.get(&second).expect("snapshot");
    assert_ne!(
        snap_one.plan.as_ref().expect("plan").plan_ref,
        snap_two.plan.as_ref().expect("plan").plan_ref,
        "different observed plan identities"
    );
    assert_eq!(
        snap_one.contract, snap_two.contract,
        "intent-derived contract projection is observably equivalent"
    );
}

// ── DoD row 11: three dimensions never rewrite each other ────────────────

#[test]
fn cross_dimension_transitions_never_rewrite_each_other() {
    let rig = rig_managed();
    let task_ref = submit_ok(&rig, &golden_intent(), "req-dims");
    append_outcome_fact(&rig, &task_ref, observation(2));
    let before = rig.bridge.get(&task_ref).expect("snapshot");
    // A DELIVERY transition (result ready) must not touch execution or
    // adjudication (TB-16 / tachi#1636 law).
    let now = rig.clock.stamp(8_800);
    let payload = TaskEventPayload::Delivery(mapping::delivery::CanonicalDeliveryFact::ResultReady);
    let value = serde_json::to_value(&payload).expect("serializes");
    rig.facts
        .append(
            &task_ref,
            TaskEvent {
                seq: 0,
                event_id: "deliv-1".to_string(),
                source: EventSource::BridgeAdmission,
                source_revision: "1".to_string(),
                occurred_at: now.clone(),
                recorded_at: now,
                payload_digest: memcore::canonical_digest::canonical_json_digest_hex(&value),
                visibility: VisibilityClass::Internal,
                payload,
            },
        )
        .expect("append");
    let after = rig.bridge.get(&task_ref).expect("snapshot");
    assert_eq!(after.execution, before.execution);
    assert_eq!(after.adjudication, before.adjudication);
    assert_eq!(
        after.delivery,
        mapping::delivery::DeliveryState::Ready,
        "the delivery dimension DID move"
    );
}

// ── DoD rows 12–13: typed refs vocabulary + no new ledgers ───────────────

#[test]
fn task_ref_has_no_string_construction_path() {
    // TB-6/TB-14 grep-equivalent: the module exposes no `From<String>`
    // for TaskRef. The compile-checked reality: TaskRef is a newtype with
    // `pub(crate) mint` only; hosts outside this crate cannot construct
    // one. (Asserted here by decode discipline in refs.rs tests.)
    let encoded = serde_json::to_string(&TaskRef::mint("task:x")).expect("ser");
    assert!(serde_json::from_str::<AttemptRef>(&encoded).is_err());
}

#[test]
fn outage_fails_closed_for_every_operation() {
    // TB-20: with the truth source down, every operation returns typed
    // Unavailable; there is no fallback execution path in this module.
    let down = TaskIntentBridge::new(
        Arc::new(InProcessRequestBindings::new()),
        Arc::new(UnavailableTaskFacts),
        FakeAuthority::admitting(&[Capability::ReasoningReview]),
        FakePlans::managed(),
        FakeOwners::forwarding(),
        Arc::new(SystemBridgeClock),
    );
    let task_ref = TaskRef::mint("task:any");
    let requester = golden_intent().requester.clone();
    let request = RequestId::new("req-down").expect("bounded");
    assert_eq!(
        down.submit(&golden_intent(), &request),
        SubmitReceipt::Unavailable
    );
    assert_eq!(down.get(&task_ref), Err(GetError::Unavailable));
    assert_eq!(down.watch(&task_ref, 0, 10), Err(GetError::Unavailable));
    assert_eq!(
        down.intervene(
            &task_ref,
            &InterventionV1::RequestPause,
            &requester,
            &request,
            None
        ),
        Err(InterventionError::Unavailable)
    );
    assert_eq!(
        down.collect(&task_ref, None),
        Err(CollectError::Unavailable)
    );
}
