//! Discriminating tests for the build broker (#894 S2c).
//!
//! Each of these fails if the property it names is broken — they are not
//! smoke tests. In particular `diverged_source_never_bare_reuses_the_resident_target`
//! is THE poisoning defense: delete the divergence branch in `plan_target` and
//! it goes red.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use memcore::MemoryStore;

use super::runner::{BuildOutcome, BuildRun, BuildRunner};
use super::target::{
    plan_target, LineageOracle, TargetGeneration, TargetSlotKind, TargetSlotState,
};
use super::ticket::{submit_ticket, BuildCommand, BuildTicket, SourceIdentity};
use super::{
    abandon_stale_slot, execute_ticket, load_receipt, pending_tickets, run_next, slot, ExecutorSeat,
};

const MAIN_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DESCENDANT_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const FORK_SHA: &str = "cccccccccccccccccccccccccccccccccccccccc";

// ─── fakes ──────────────────────────────────────────────────────────────────

/// Lineage answers from an explicit list of same-line pairs — so one test can
/// have "resident diverged, scratch on-lineage" without a git repo.
struct FakeLineage {
    same_pairs: Vec<(String, String)>,
}

impl FakeLineage {
    fn none() -> Self {
        FakeLineage {
            same_pairs: Vec::new(),
        }
    }
    fn with(pairs: &[(&str, &str)]) -> Self {
        FakeLineage {
            same_pairs: pairs
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
        }
    }
}

impl LineageOracle for FakeLineage {
    fn same_lineage(&self, _repo_root: &str, a: &str, b: &str) -> Result<bool, String> {
        Ok(self
            .same_pairs
            .iter()
            .any(|(x, y)| (x == a && y == b) || (x == b && y == a)))
    }
}

/// A runner that never touches git or cargo, but records what the broker asked
/// it to do — and, crucially, measures how many builds are in flight at once.
struct FakeRunner {
    outcome: BuildOutcome,
    /// Held for this long inside `run`, so an overlap would actually be observed.
    hold: std::time::Duration,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    cleared: Mutex<Vec<String>>,
    ran_on: Mutex<Vec<(String, String)>>,
}

impl FakeRunner {
    fn new(outcome: BuildOutcome) -> Self {
        FakeRunner {
            outcome,
            hold: std::time::Duration::from_millis(0),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            cleared: Mutex::new(Vec::new()),
            ran_on: Mutex::new(Vec::new()),
        }
    }
    fn holding(outcome: BuildOutcome, hold: std::time::Duration) -> Self {
        let mut r = FakeRunner::new(outcome);
        r.hold = hold;
        r
    }
    fn cleared_targets(&self) -> Vec<String> {
        self.cleared.lock().unwrap().clone()
    }
    fn targets_built_on(&self) -> Vec<String> {
        self.ran_on
            .lock()
            .unwrap()
            .iter()
            .map(|(_, t)| t.clone())
            .collect()
    }
}

impl BuildRunner for FakeRunner {
    fn prepare_checkout(&self, _checkout: &Path, _source: &SourceIdentity) -> Result<(), String> {
        Ok(())
    }

    fn run(
        &self,
        ticket: &BuildTicket,
        _checkout: &Path,
        target_dir: &Path,
    ) -> Result<BuildRun, String> {
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(now, Ordering::SeqCst);
        self.ran_on
            .lock()
            .unwrap()
            .push((ticket.ticket_id.clone(), target_dir.display().to_string()));
        if !self.hold.is_zero() {
            std::thread::sleep(self.hold);
        }
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        Ok(BuildRun {
            outcome: self.outcome,
            exit_code: match self.outcome {
                BuildOutcome::Success => Some(0),
                BuildOutcome::Failed => Some(101),
                BuildOutcome::Interrupted => None,
            },
            stdout_tail: String::new(),
            stderr_tail: String::new(),
        })
    }

    fn clear_target(&self, target_dir: &Path) -> Result<i64, String> {
        self.cleared
            .lock()
            .unwrap()
            .push(target_dir.display().to_string());
        Ok(1_024)
    }
}

// ─── fixtures ───────────────────────────────────────────────────────────────

fn seat() -> ExecutorSeat {
    ExecutorSeat {
        checkout: PathBuf::from("/seat/checkout"),
        resident_target: PathBuf::from("/seat/target-resident"),
        scratch_target: PathBuf::from("/seat/target-scratch"),
    }
}

fn source(head: &str) -> SourceIdentity {
    SourceIdentity {
        repo_root: "/repo".to_string(),
        base_sha: MAIN_SHA.to_string(),
        head_sha: head.to_string(),
    }
}

fn ticket(id: &str, head: &str) -> BuildTicket {
    BuildTicket::new(
        id,
        source(head),
        BuildCommand {
            program: "cargo".to_string(),
            args: vec!["build".to_string()],
            features: vec![],
        },
        Some("env-1".to_string()),
        None,
    )
    .expect("valid ticket")
}

fn generation(head: &str, ticket_id: &str) -> TargetGeneration {
    TargetGeneration {
        repo_root: "/repo".to_string(),
        head_sha: head.to_string(),
        ticket_id: ticket_id.to_string(),
        stamped_at: "2026-07-13T00:00:00Z".to_string(),
    }
}

fn slot_state(path: &str, gen: Option<TargetGeneration>, quarantined: bool) -> TargetSlotState {
    TargetSlotState {
        path: path.to_string(),
        generation: gen,
        quarantined,
    }
}

fn store() -> MemoryStore {
    MemoryStore::open_in_memory().expect("in-memory store")
}

// ─── ③ the poisoning defense (the load-bearing one) ─────────────────────────

#[test]
fn diverged_source_never_bare_reuses_the_resident_target() {
    // The resident target was last driven by MAIN. This ticket builds FORK,
    // which has diverged from MAIN (the oracle says: not on one line).
    let resident = slot_state(
        "/seat/target-resident",
        Some(generation(MAIN_SHA, "t-main")),
        false,
    );
    let scratch = slot_state("/seat/target-scratch", None, false);

    let plan = plan_target(&source(FORK_SHA), &resident, &scratch, &FakeLineage::none())
        .expect("plan resolves");

    // THE invariant: a diverged source must not land on the resident target.
    assert_eq!(
        plan.slot,
        TargetSlotKind::Scratch,
        "a forked source must go to the scratch target, never reuse the resident one — that \
         reuse is exactly the phantom-compile-error bug (#894 S2c). Plan was: {plan:?}"
    );
    assert_eq!(plan.path, "/seat/target-scratch");
    // Virgin scratch dir: nothing to wipe.
    assert!(!plan.clear_first);
}

#[test]
fn diverged_source_clears_a_scratch_target_that_holds_another_generation() {
    // Resident on MAIN; scratch still holds a THIRD tree's artifacts.
    let resident = slot_state(
        "/seat/target-resident",
        Some(generation(MAIN_SHA, "t-main")),
        false,
    );
    let scratch = slot_state(
        "/seat/target-scratch",
        Some(generation(DESCENDANT_SHA, "t-other")),
        false,
    );

    let plan = plan_target(&source(FORK_SHA), &resident, &scratch, &FakeLineage::none()).unwrap();

    assert_eq!(plan.slot, TargetSlotKind::Scratch);
    assert!(
        plan.clear_first,
        "a scratch target holding a foreign generation must be wiped before reuse, not built \
         into on top of someone else's fingerprints"
    );
}

#[test]
fn same_lineage_source_reuses_the_resident_target_without_clearing() {
    // The positive control — without this, "always use scratch, always clear"
    // would pass the test above and the broker would be useless (it would never
    // get an incremental build).
    let resident = slot_state(
        "/seat/target-resident",
        Some(generation(MAIN_SHA, "t-main")),
        false,
    );
    let scratch = slot_state("/seat/target-scratch", None, false);
    let lineage = FakeLineage::with(&[(MAIN_SHA, DESCENDANT_SHA)]);

    let plan = plan_target(&source(DESCENDANT_SHA), &resident, &scratch, &lineage).unwrap();

    assert_eq!(plan.slot, TargetSlotKind::Resident);
    assert!(
        !plan.clear_first,
        "a fast-forward on the same lineage must reuse the resident target as-is"
    );
}

#[test]
fn a_target_driven_by_another_repo_is_never_compatible() {
    let mut foreign = generation(MAIN_SHA, "t-other-repo");
    foreign.repo_root = "/some/other/repo".to_string();
    let resident = slot_state("/seat/target-resident", Some(foreign), false);
    let scratch = slot_state("/seat/target-scratch", None, false);

    // Even with an oracle that would call these shas one lineage, a different
    // repo is a different repo.
    let plan = plan_target(
        &source(MAIN_SHA),
        &resident,
        &scratch,
        &FakeLineage::with(&[(MAIN_SHA, MAIN_SHA)]),
    )
    .unwrap();
    assert_eq!(plan.slot, TargetSlotKind::Scratch);
}

#[test]
fn a_quarantined_resident_target_is_never_reused_without_clearing() {
    // On-lineage, but quarantined: reuse is allowed only WITH a wipe.
    let resident = slot_state(
        "/seat/target-resident",
        Some(generation(MAIN_SHA, "t-main")),
        true,
    );
    let scratch = slot_state("/seat/target-scratch", None, false);

    let plan = plan_target(&source(MAIN_SHA), &resident, &scratch, &FakeLineage::none()).unwrap();

    assert_eq!(plan.slot, TargetSlotKind::Resident);
    assert!(
        plan.clear_first,
        "an interrupted (quarantined) target must be cleared before a retry touches it"
    );
}

// ─── ② strict serialization of the executor slot ────────────────────────────

#[test]
fn two_tickets_cannot_hold_the_executor_slot_at_once() {
    let store = store();
    assert_eq!(
        slot::acquire_slot(&store, "t-1").unwrap(),
        slot::SlotOutcome::Acquired
    );
    match slot::acquire_slot(&store, "t-2").unwrap() {
        slot::SlotOutcome::Busy { holder } => assert_eq!(holder.ticket_id, "t-1"),
        slot::SlotOutcome::Acquired => {
            panic!("two tickets must never both hold the machine's single executor slot")
        }
    }
    // A non-holder cannot release the slot out from under the holder.
    assert!(slot::release_slot(&store, "t-2").is_err());
    assert!(slot::release_slot(&store, "t-1").unwrap());
    // Now it is free again.
    assert_eq!(
        slot::acquire_slot(&store, "t-2").unwrap(),
        slot::SlotOutcome::Acquired
    );
}

#[test]
fn concurrent_builds_are_strictly_serialized_never_overlapping() {
    // Four threads, four tickets, one machine. The runner measures how many
    // builds are inside `run` simultaneously; if the slot leaked, max would be
    // >1 and this reds.
    let tmp = crate::test_support::non_skipped_fixture_tempdir("build-broker-");
    let db = tmp.path().join("global").join("memory.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let db_str = db.to_str().unwrap().to_string();

    let setup = MemoryStore::open(&db_str).expect("open store");
    let tickets: Vec<BuildTicket> = (0..4)
        .map(|i| {
            let t = ticket(&format!("t-{i}"), MAIN_SHA);
            submit_ticket(&setup, &t).expect("submit");
            t
        })
        .collect();
    drop(setup);

    let runner = Arc::new(FakeRunner::holding(
        BuildOutcome::Success,
        std::time::Duration::from_millis(25),
    ));
    let seat = seat();

    let handles: Vec<_> = tickets
        .into_iter()
        .map(|t| {
            let db_str = db_str.clone();
            let runner = Arc::clone(&runner);
            let seat = seat.clone();
            std::thread::spawn(move || {
                let mut store = MemoryStore::open(&db_str).expect("open store");
                let lineage = FakeLineage::with(&[(MAIN_SHA, MAIN_SHA)]);
                // Retry while the slot is busy — that is the queue.
                for _ in 0..400 {
                    match execute_ticket(&mut store, &seat, &t, runner.as_ref(), &lineage) {
                        Ok(receipt) => return receipt,
                        Err(err) if err.starts_with("executor slot busy") => {
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(err) => panic!("unexpected broker error: {err}"),
                    }
                }
                panic!("ticket {} never got the executor slot", t.ticket_id);
            })
        })
        .collect();

    let receipts: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    assert_eq!(receipts.len(), 4);
    assert_eq!(
        runner.max_in_flight.load(Ordering::SeqCst),
        1,
        "two cargos ran at once: the machine's executor slot is not mutually exclusive (#894 S2c)"
    );
    for receipt in &receipts {
        assert_eq!(receipt.outcome, BuildOutcome::Success);
    }
    // And the slot is free afterwards — no build wedged the machine.
    let store = MemoryStore::open(&db_str).unwrap();
    assert!(slot::current_holder(&store).unwrap().is_none());
}

// ─── ④ interruption → quarantine → no bare reuse on retry ───────────────────

#[test]
fn an_interrupted_build_quarantines_its_target_and_the_retry_must_clear_it() {
    let mut store = store();
    let seat = seat();
    let lineage = FakeLineage::with(&[(MAIN_SHA, MAIN_SHA)]);

    let t1 = ticket("t-interrupted", MAIN_SHA);
    submit_ticket(&store, &t1).unwrap();
    let killed = FakeRunner::new(BuildOutcome::Interrupted);
    let receipt = execute_ticket(&mut store, &seat, &t1, &killed, &lineage).unwrap();

    assert_eq!(receipt.outcome, BuildOutcome::Interrupted);
    assert_eq!(
        receipt.quarantined_target.as_deref(),
        Some("/seat/target-resident"),
        "an interrupted cargo must quarantine the target dir it was writing into"
    );

    // The ledger agrees: that target is fenced off.
    let res = memcore::find_resource_by_path(
        store.connection(),
        "/seat/target-resident",
        memcore::ResourceKind::BuildTarget,
    )
    .unwrap()
    .expect("target registered");
    assert_eq!(res.state, memcore::ResourceState::Quarantined);

    // No generation was stamped: we do not know what state the dir reached, and
    // claiming it is "at" MAIN would be a lie the next compatibility check would
    // believe.
    assert!(
        super::target::read_generation(&store, "/seat/target-resident")
            .unwrap()
            .is_none(),
        "an interrupted build must not stamp a generation"
    );

    // Retry the same source. The target is on-lineage, so the plan may reuse it
    // — but ONLY after a wipe.
    let t2 = ticket("t-retry", MAIN_SHA);
    submit_ticket(&store, &t2).unwrap();
    let retry = FakeRunner::new(BuildOutcome::Success);
    let receipt2 = execute_ticket(&mut store, &seat, &t2, &retry, &lineage).unwrap();

    assert!(
        receipt2.cleared_target,
        "the retry reused a quarantined target WITHOUT clearing it (#894 S2c item 4)"
    );
    assert_eq!(
        retry.cleared_targets(),
        vec!["/seat/target-resident".to_string()],
        "the retry must actually wipe the poisoned dir, not just flip a DB row"
    );
    assert_eq!(receipt2.outcome, BuildOutcome::Success);

    // And the quarantine is lifted only now that the bytes are gone.
    let res = memcore::get_resource(store.connection(), &res.resource_id)
        .unwrap()
        .unwrap();
    assert_eq!(res.state, memcore::ResourceState::Active);
}

#[test]
fn abandoning_a_stale_slot_quarantines_the_dead_builds_target_before_releasing() {
    let mut store = store();

    // Simulate a daemon that died mid-build: slot held, target recorded, no
    // receipt, no release.
    slot::acquire_slot(&store, "t-dead").unwrap();
    slot::record_slot_target(&store, "t-dead", "/seat/target-resident").unwrap();

    assert!(abandon_stale_slot(&mut store, "daemon killed").unwrap());

    let res = memcore::find_resource_by_path(
        store.connection(),
        "/seat/target-resident",
        memcore::ResourceKind::BuildTarget,
    )
    .unwrap()
    .expect("target registered by the recovery path");
    assert_eq!(
        res.state,
        memcore::ResourceState::Quarantined,
        "crash recovery must fence off the dead build's target BEFORE freeing the slot"
    );
    assert!(slot::current_holder(&store).unwrap().is_none());
}

// ─── ticket immutability + queue ────────────────────────────────────────────

#[test]
fn a_submitted_ticket_cannot_be_rewritten() {
    let store = store();
    let original = ticket("t-1", MAIN_SHA);
    submit_ticket(&store, &original).unwrap();

    // Re-submitting the identical ticket is an idempotent no-op (retries are fine).
    submit_ticket(&store, &original).expect("identical re-submit is idempotent");

    // Re-pointing the same ticket id at a different source is refused: the
    // executor may already have chosen a target dir for the OLD identity.
    let mut tampered = original.clone();
    tampered.source.head_sha = FORK_SHA.to_string();
    let err = submit_ticket(&store, &tampered).unwrap_err();
    assert!(err.contains("immutable"), "got: {err}");

    let stored = super::ticket::load_ticket(&store, "t-1").unwrap().unwrap();
    assert_eq!(
        stored.source.head_sha, MAIN_SHA,
        "the stored ticket must be untouched by the rejected rewrite"
    );
}

#[test]
fn a_ticket_source_must_be_an_object_id_not_a_ref_or_a_flag() {
    // The seat feeds head_sha to `git checkout --detach <sha>`; a ref name or a
    // flag there is an injection surface.
    for bad in ["main", "--upload-pack=touch /tmp/pwn", "HEAD", "../etc", ""] {
        let err = BuildTicket::new(
            "t-bad",
            SourceIdentity {
                repo_root: "/repo".to_string(),
                base_sha: MAIN_SHA.to_string(),
                head_sha: bad.to_string(),
            },
            BuildCommand {
                program: "cargo".to_string(),
                args: vec![],
                features: vec![],
            },
            None,
            None,
        )
        .unwrap_err();
        assert!(
            err.contains("not a hex object id"),
            "head_sha '{bad}' must be refused; got: {err}"
        );
    }
}

#[test]
fn the_queue_drains_oldest_first_and_a_built_ticket_leaves_it() {
    let mut store = store();
    let seat = seat();
    let lineage = FakeLineage::with(&[(MAIN_SHA, MAIN_SHA)]);
    let runner = FakeRunner::new(BuildOutcome::Success);

    let first = ticket("t-first", MAIN_SHA);
    submit_ticket(&store, &first).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let second = ticket("t-second", MAIN_SHA);
    submit_ticket(&store, &second).unwrap();

    assert_eq!(pending_tickets(&store).unwrap().len(), 2);

    let receipt = run_next(&mut store, &seat, &runner, &lineage)
        .unwrap()
        .expect("a ticket ran");
    assert_eq!(
        receipt.ticket_id, "t-first",
        "the queue is FIFO by submission time"
    );
    assert_eq!(pending_tickets(&store).unwrap().len(), 1);

    run_next(&mut store, &seat, &runner, &lineage)
        .unwrap()
        .expect("second ticket ran");
    assert!(pending_tickets(&store).unwrap().is_empty());
    assert!(run_next(&mut store, &seat, &runner, &lineage)
        .unwrap()
        .is_none());

    // Both built on the resident target (same lineage), and the generation now
    // points at the last one.
    assert_eq!(
        runner.targets_built_on(),
        vec![
            "/seat/target-resident".to_string(),
            "/seat/target-resident".to_string()
        ]
    );
    let gen = super::target::read_generation(&store, "/seat/target-resident")
        .unwrap()
        .expect("generation stamped");
    assert_eq!(gen.head_sha, MAIN_SHA);
    assert_eq!(gen.ticket_id, "t-second");
}

#[test]
fn a_failed_build_still_stamps_the_generation_and_keeps_its_receipt() {
    let mut store = store();
    let seat = seat();
    let lineage = FakeLineage::with(&[(MAIN_SHA, MAIN_SHA)]);

    let t = ticket("t-fail", MAIN_SHA);
    submit_ticket(&store, &t).unwrap();
    let runner = FakeRunner::new(BuildOutcome::Failed);
    let receipt = execute_ticket(&mut store, &seat, &t, &runner, &lineage).unwrap();

    assert_eq!(receipt.outcome, BuildOutcome::Failed);
    assert_eq!(receipt.exit_code, Some(101));
    // A failed cargo still wrote artifacts into the dir — the generation is real.
    let gen = super::target::read_generation(&store, "/seat/target-resident")
        .unwrap()
        .expect("a failed build still defines the target's generation");
    assert_eq!(gen.head_sha, MAIN_SHA);
    // The result is bound to the ticket id.
    let stored = load_receipt(&store, "t-fail").unwrap().unwrap();
    assert_eq!(stored, receipt);
    assert_eq!(stored.env_id.as_deref(), Some("env-1"));
    // And the slot went back.
    assert!(slot::current_holder(&store).unwrap().is_none());
}

#[test]
fn a_forked_ticket_builds_on_scratch_leaving_the_resident_target_intact() {
    // End-to-end version of the poisoning defense: after a MAIN build and a FORK
    // build, the resident target still belongs to MAIN.
    let mut store = store();
    let seat = seat();
    let runner = FakeRunner::new(BuildOutcome::Success);

    let main_ticket = ticket("t-main", MAIN_SHA);
    submit_ticket(&store, &main_ticket).unwrap();
    execute_ticket(
        &mut store,
        &seat,
        &main_ticket,
        &runner,
        &FakeLineage::with(&[(MAIN_SHA, MAIN_SHA)]),
    )
    .unwrap();

    let fork_ticket = ticket("t-fork", FORK_SHA);
    submit_ticket(&store, &fork_ticket).unwrap();
    let fork_receipt = execute_ticket(
        &mut store,
        &seat,
        &fork_ticket,
        &runner,
        // FORK is on nobody's lineage.
        &FakeLineage::none(),
    )
    .unwrap();

    assert_eq!(fork_receipt.target_slot, "scratch");
    assert_eq!(fork_receipt.target_path, "/seat/target-scratch");

    let resident_gen = super::target::read_generation(&store, "/seat/target-resident")
        .unwrap()
        .expect("resident generation");
    assert_eq!(
        resident_gen.head_sha, MAIN_SHA,
        "the fork build must not have touched the resident target's generation"
    );
    let scratch_gen = super::target::read_generation(&store, "/seat/target-scratch")
        .unwrap()
        .expect("scratch generation");
    assert_eq!(scratch_gen.head_sha, FORK_SHA);
}
