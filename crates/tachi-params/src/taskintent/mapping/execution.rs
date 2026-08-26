//! EXECUTION dimension mapping table (TB-16; tachi#1636 law).
//!
//! Maps canonical Tachi execution truth onto the bridge-visible execution
//! vocabulary. Canonical inputs and their existing sources:
//!
//! | Canonical input | Existing truth surface |
//! |---|---|
//! | `queued` / `admitted` / `preparing` / `running` / `waiting_input` / `submitted` | `StaffRunReceipt.state` run-lifecycle vocabulary (`crates/tachi-params/src/facade/dispatch.rs`) and the #1679 frozen execution model |
//! | `failed` / `timed_out` | `dispatch_outcomes.execution_outcome` (`failed`/`aborted` — the watchdog/timeout terminal classes) |
//! | `completed` | `dispatch_outcomes.execution_outcome = completed` — the MACHINE-resolved verdict, not the agent self-report (`reported_outcome`) |
//! | `cancellation_requested` | the bridge's own stop-request fact (TB-12: a request, never a terminal) |
//! | `cancelled` | only an authoritative lifecycle-owner terminal confirmation fact (TB-12) |
//! | `orphaned` | `session_claims.ClaimState = orphaned` / harness disappearance |
//! | `inconsistent` | #1678 conflicting-terminal-facts law |
//! | `unknown` / `outcome_unknown` | disappearance after possible side effects (TB-12/TB-18) |
//!
//! The output enum [`ExecutionState`] is the tachi#1679 task-level
//! (~13-state) execution vocabulary expressed as an ALIAS surface — it is
//! DEFINED here and nowhere else (TB-16 grep check: no independent
//! task/attempt state enum exists outside the mapping tables).
//!
//! Cross-dimension law: functions in this module never look at adjudication
//! or delivery state, and no function in `adjudication`/`delivery` looks at
//! execution state.

use std::borrow::Borrow;

use serde::{Deserialize, Serialize};

/// Canonical execution fact as observed on an existing Tachi truth surface.
/// Constructed by the fact-source adapter from real rows — the bridge never
/// mints these from caller input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalExecutionFact {
    /// Run accepted but not started (admission recorded, no start receipt).
    Queued,
    /// Run started, in progress (`StaffRunReceipt.state = working`).
    Running,
    /// Run is waiting for input (harness-reported `waiting_input`).
    WaitingInput,
    /// Run submitted its result but no terminal verdict exists yet
    /// (`submitted` — self-report is NOT semantic acceptance, tachi#1678).
    Submitted,
    /// Machine-resolved terminal success
    /// (`dispatch_outcomes.execution_outcome = completed`).
    Completed,
    /// Machine-resolved terminal failure (`execution_outcome = failed`).
    Failed,
    /// Machine-resolved partial terminal (`execution_outcome = partial`):
    /// terminal, but contract satisfaction is decided by evidence, not by
    /// this verdict.
    Partial,
    /// Terminal abort (`execution_outcome = aborted`, incl. watchdog
    /// timeouts / preflight failure).
    AbortedOrTimedOut,
    /// A stop was REQUESTED (bridge stop fact; TB-12 stage: requested).
    CancellationRequested,
    /// The lifecycle OWNER authoritatively confirmed cancellation (TB-12:
    /// only this input can produce `Cancelled`).
    OwnerConfirmedCancelled,
    /// Claim went orphaned (`session_claims.ClaimState = orphaned`) or the
    /// harness/controller disappeared before any side effect was possible.
    Orphaned,
    /// Conflicting terminal facts (#1678 `inconsistent/reconciling`).
    InconsistentTerminals,
    /// Disappearance AFTER possible side effects — outcome unknown (TB-12/
    /// TB-18; never optimistic success/failure/cancel).
    OutcomeUnknown,
}

/// Bridge-visible task-level execution state — the tachi#1679 ~13-state
/// vocabulary. Alias over canonical truth; defined only in this mapping
/// table (TB-16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    /// Admitted, not yet started.
    Queued,
    /// In progress.
    Running,
    /// Waiting for requester/harness input.
    WaitingInput,
    /// Result submitted, terminal verdict pending.
    Submitted,
    /// Machine-resolved terminal success.
    Completed,
    /// Machine-resolved terminal failure.
    Failed,
    /// Machine-resolved partial terminal: terminal, with contract
    /// satisfaction decided by evidence (TB-13).
    Partial,
    /// Timed out or aborted.
    TimedOut,
    /// Stop requested, not confirmed (TB-12: request ≠ confirmation).
    CancellationRequested,
    /// Owner-confirmed cancellation.
    Cancelled,
    /// Holder/session disappeared pre-side-effect.
    Orphaned,
    /// Conflicting terminal facts under reconciliation.
    Inconsistent,
    /// Outcome unknown after possible side effects.
    OutcomeUnknown,
}

/// The execution mapping table: canonical fact → bridge execution state.
///
/// This is the ONLY place an [`ExecutionState`] value is produced from
/// state truth. Deterministic, total, and side-effect free.
pub fn map_execution(fact: &CanonicalExecutionFact) -> ExecutionState {
    match fact {
        CanonicalExecutionFact::Queued => ExecutionState::Queued,
        CanonicalExecutionFact::Running => ExecutionState::Running,
        CanonicalExecutionFact::WaitingInput => ExecutionState::WaitingInput,
        CanonicalExecutionFact::Submitted => ExecutionState::Submitted,
        CanonicalExecutionFact::Completed => ExecutionState::Completed,
        CanonicalExecutionFact::Failed => ExecutionState::Failed,
        CanonicalExecutionFact::Partial => ExecutionState::Partial,
        CanonicalExecutionFact::AbortedOrTimedOut => ExecutionState::TimedOut,
        CanonicalExecutionFact::CancellationRequested => ExecutionState::CancellationRequested,
        CanonicalExecutionFact::OwnerConfirmedCancelled => ExecutionState::Cancelled,
        CanonicalExecutionFact::Orphaned => ExecutionState::Orphaned,
        CanonicalExecutionFact::InconsistentTerminals => ExecutionState::Inconsistent,
        CanonicalExecutionFact::OutcomeUnknown => ExecutionState::OutcomeUnknown,
    }
}

/// Whether a canonical execution fact is terminal for the execution
/// dimension (used by the projection to freeze the latest state).
pub fn is_execution_terminal(fact: &CanonicalExecutionFact) -> bool {
    matches!(
        fact,
        CanonicalExecutionFact::Completed
            | CanonicalExecutionFact::Failed
            | CanonicalExecutionFact::Partial
            | CanonicalExecutionFact::AbortedOrTimedOut
            | CanonicalExecutionFact::OwnerConfirmedCancelled
            | CanonicalExecutionFact::InconsistentTerminals
            | CanonicalExecutionFact::OutcomeUnknown
    )
}

/// Fold canonical facts (in observation order) into the current execution
/// state. Stale/non-terminal facts never regress a terminal state (TB-10:
/// replay of older events cannot rewrite canonical state): once a terminal
/// fact is observed, later non-terminal facts are ignored; a SECOND
/// differing terminal fact yields [`ExecutionState::Inconsistent`] (tachi
/// #1678 conflicting-terminal law).
pub fn project_execution<I>(facts: I) -> Option<ExecutionState>
where
    I: IntoIterator,
    I::Item: std::borrow::Borrow<CanonicalExecutionFact>,
{
    let mut terminal: Option<ExecutionState> = None;
    let mut latest: Option<ExecutionState> = None;
    for fact in facts {
        let fact = fact.borrow();
        let state = map_execution(fact);
        if is_execution_terminal(fact) {
            terminal = match terminal {
                None => Some(state),
                Some(existing) if existing == state => Some(existing),
                Some(_) => Some(ExecutionState::Inconsistent),
            };
        } else if terminal.is_none() {
            // Non-terminal facts advance the live state but cannot regress
            // a terminal one.
            latest = Some(state);
        }
    }
    terminal.or(latest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_facts_map_deterministically() {
        assert_eq!(
            map_execution(&CanonicalExecutionFact::Completed),
            ExecutionState::Completed
        );
        assert_eq!(
            map_execution(&CanonicalExecutionFact::CancellationRequested),
            ExecutionState::CancellationRequested
        );
    }

    #[test]
    fn stale_events_never_regress_terminal_state() {
        // TB-10: replaying an older (non-terminal) page after a newer
        // terminal leaves the projection unchanged.
        let facts = [
            CanonicalExecutionFact::Completed,
            CanonicalExecutionFact::Running,
            CanonicalExecutionFact::Submitted,
        ];
        assert_eq!(
            project_execution(facts.iter()),
            Some(ExecutionState::Completed)
        );
    }

    #[test]
    fn conflicting_terminals_project_inconsistent() {
        let facts = [
            CanonicalExecutionFact::Completed,
            CanonicalExecutionFact::Failed,
        ];
        assert_eq!(
            project_execution(facts.iter()),
            Some(ExecutionState::Inconsistent)
        );
    }

    #[test]
    fn stop_request_alone_never_mints_cancelled() {
        // TB-12: requested → at most cancellation_requested until an OWNER
        // confirmation arrives.
        let facts = [
            CanonicalExecutionFact::Running,
            CanonicalExecutionFact::CancellationRequested,
        ];
        assert_eq!(
            project_execution(facts.iter()),
            Some(ExecutionState::CancellationRequested)
        );
        let confirmed = [
            CanonicalExecutionFact::Running,
            CanonicalExecutionFact::CancellationRequested,
            CanonicalExecutionFact::OwnerConfirmedCancelled,
        ];
        assert_eq!(
            project_execution(confirmed.iter()),
            Some(ExecutionState::Cancelled)
        );
    }

    #[test]
    fn disappearance_after_side_effects_is_outcome_unknown() {
        let facts = [
            CanonicalExecutionFact::Running,
            CanonicalExecutionFact::OutcomeUnknown,
        ];
        assert_eq!(
            project_execution(facts.iter()),
            Some(ExecutionState::OutcomeUnknown)
        );
    }
}
