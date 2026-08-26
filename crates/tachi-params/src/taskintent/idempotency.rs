//! Request-id idempotency for the bridge (tachi#1840, zeroclaw #205 TB-7).
//!
//! The durable binding is `(requester, request_id) → { canonical request
//! digest, bound ref }`. Rules implemented:
//!
//! 1. duplicate submit with known tuple + MATCHING digest → the SAME
//!    [`TaskRef`] (never a second worker);
//! 2. same tuple + DIFFERENT digest → typed [`RequestConflict`]
//!    (`RequestIdConflict`), zero new execution;
//! 3. ambiguous submit (binding exists, task fact not yet materialized) →
//!    `ReconciliationUnknown` — reconcile, never a second spawn;
//! 4. intervene/stop request ids obey the same tuple law (rule 6).
//!
//! ## DECISION (OPEN) TB-7/B — honest restart-recovery record
//!
//! DECISION TB-7/B (interim idempotency carrier) is OPEN at contract rev 3
//! and "default closed/deny until picked". This leaf therefore ships **no
//! interim journal** (option (a)'s journal + Tachi lookup endpoint is a
//! durable addition the owner has not picked; option (b) blocks V2a on
//! tachi#1623). What the shipped in-process carrier CAN and CANNOT do:
//!
//! * CAN: all four rules above, for the lifetime of the bridge process —
//!   duplicate submits, digest conflicts, ambiguous-submit reconciliation,
//!   and intervene/stop tuple law are enforced and tested.
//! * CANNOT: rule 5's Tachi-restart half. A Tachi daemon restart loses the
//!   in-process binding map; a requester that then replays the same
//!   `(requester, request_id)` gets a fresh admission (a new TaskRef), not
//!   the pre-restart one. The contract's own DECISION B note says the same
//!   thing about a client-side journal alone: without a Tachi-side durable
//!   binding, recovery across Tachi restart is NOT provided.
//! * The durable carrier belongs to tachi#1623 (durable TaskRef +
//!   submit-level idempotency) or an owner-picked option (a) endpoint —
//!   both are outside this leaf's no-new-DDL hard law. When it lands, the
//!   [`RequestBindingStore`] trait is the seam it plugs into; nothing else
//!   in the bridge changes.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::refs::{RequesterRef, TaskRef};

/// What a `(requester, request_id)` tuple is bound to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BoundRef {
    /// A submitted task (submit path).
    Task(TaskRef),
    /// An intervention receipt (intervene path).
    Intervention {
        /// The task the intervention targeted.
        task: TaskRef,
        /// The minted intervention id.
        intervention_id: String,
    },
    /// A stop receipt (request_stop path).
    Stop {
        /// The task the stop targeted.
        task: TaskRef,
        /// The minted stop operation id.
        stop_id: String,
    },
}

/// The durable-shape binding: digest + bound ref.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestBinding {
    /// Canonical request digest at bind time.
    pub digest: String,
    /// What the request created.
    pub bound: BoundRef,
}

/// Typed conflict when a known tuple arrives with a different digest.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RequestConflict {
    /// Same `(requester, request_id)`, different canonical digest (TB-7
    /// rule 3).
    #[error(
        "request id conflict: same (requester, request_id) already bound to a different digest"
    )]
    RequestIdConflict {
        /// Digest the tuple is already bound to.
        bound_digest: String,
        /// Digest of the incoming request.
        submitted_digest: String,
    },
}

impl RequestConflict {
    /// The digest the conflicting tuple is already bound to.
    pub fn bound_digest(&self) -> &str {
        match self {
            Self::RequestIdConflict { bound_digest, .. } => bound_digest,
        }
    }
}

/// Storage seam for the TB-7 binding. The in-process implementation ships
/// here; the durable carrier (tachi#1623 or an owner-picked DECISION B(a)
/// endpoint) implements the same trait — see the module's honest-record
/// docs.
pub trait RequestBindingStore: Send + Sync {
    /// Look up the binding for a tuple, if any.
    fn lookup(&self, requester: &RequesterRef, request_id: &str) -> Option<RequestBinding>;

    /// Bind a tuple. If the tuple is already bound with the SAME digest,
    /// return the existing binding (idempotent replay); a DIFFERENT digest
    /// returns [`RequestConflict::RequestIdConflict`].
    fn bind(
        &self,
        requester: &RequesterRef,
        request_id: &str,
        digest: &str,
        bound: BoundRef,
    ) -> Result<RequestBinding, RequestConflict>;
}

/// In-process binding store (see module docs for the honest restart-gap
/// record: this carrier does not survive a Tachi restart).
#[derive(Debug, Default)]
pub struct InProcessRequestBindings {
    entries: Mutex<BTreeMap<(String, String), RequestBinding>>,
}

impl InProcessRequestBindings {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl RequestBindingStore for InProcessRequestBindings {
    fn lookup(&self, requester: &RequesterRef, request_id: &str) -> Option<RequestBinding> {
        self.entries
            .lock()
            .expect("request binding map poisoned")
            .get(&(requester.to_string(), request_id.to_string()))
            .cloned()
    }

    fn bind(
        &self,
        requester: &RequesterRef,
        request_id: &str,
        digest: &str,
        bound: BoundRef,
    ) -> Result<RequestBinding, RequestConflict> {
        let key = (requester.to_string(), request_id.to_string());
        let mut entries = self.entries.lock().expect("request binding map poisoned");
        if let Some(existing) = entries.get(&key) {
            if existing.digest == digest {
                return Ok(existing.clone());
            }
            return Err(RequestConflict::RequestIdConflict {
                bound_digest: existing.digest.clone(),
                submitted_digest: digest.to_string(),
            });
        }
        let binding = RequestBinding {
            digest: digest.to_string(),
            bound,
        };
        entries.insert(key, binding.clone());
        Ok(binding)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> InProcessRequestBindings {
        InProcessRequestBindings::new()
    }

    fn requester() -> RequesterRef {
        RequesterRef::claim("zeroclaw-host-alpha").expect("bounded")
    }

    fn task(n: &str) -> TaskRef {
        TaskRef::mint(format!("task:{n}"))
    }

    #[test]
    fn same_tuple_same_digest_returns_the_same_task() {
        // TB-7 rule 2 + owner test 1.
        let s = store();
        let first = s
            .bind(
                &requester(),
                "req-1",
                "digest-a",
                BoundRef::Task(task("01")),
            )
            .expect("bind");
        let replay = s
            .bind(
                &requester(),
                "req-1",
                "digest-a",
                BoundRef::Task(task("02")),
            )
            .expect("replay is not a conflict");
        assert_eq!(first, replay);
        assert_eq!(replay.bound, BoundRef::Task(task("01")));
    }

    #[test]
    fn same_tuple_different_digest_is_a_typed_conflict() {
        // TB-7 rule 3 + owner test 2: zero spawns — bind refuses.
        let s = store();
        s.bind(
            &requester(),
            "req-1",
            "digest-a",
            BoundRef::Task(task("01")),
        )
        .expect("bind");
        let conflict = s
            .bind(
                &requester(),
                "req-1",
                "digest-b",
                BoundRef::Task(task("02")),
            )
            .unwrap_err();
        assert_eq!(
            conflict,
            RequestConflict::RequestIdConflict {
                bound_digest: "digest-a".to_string(),
                submitted_digest: "digest-b".to_string(),
            }
        );
        // The bound entry is unchanged.
        assert_eq!(
            s.lookup(&requester(), "req-1").map(|b| b.bound),
            Some(BoundRef::Task(task("01")))
        );
    }

    #[test]
    fn different_requesters_do_not_alias() {
        // The scope is the (requester, request_id) TUPLE, not the id alone.
        let s = store();
        let other = RequesterRef::claim("another-host").expect("bounded");
        s.bind(
            &requester(),
            "req-1",
            "digest-a",
            BoundRef::Task(task("01")),
        )
        .expect("bind");
        s.bind(&other, "req-1", "digest-a", BoundRef::Task(task("02")))
            .expect("different requester ⇒ different tuple");
        assert_ne!(s.lookup(&requester(), "req-1"), s.lookup(&other, "req-1"));
    }

    #[test]
    fn intervention_and_stop_bindings_obey_the_same_law() {
        // TB-7 rule 6.
        let s = store();
        s.bind(
            &requester(),
            "iv-1",
            "iv-digest-a",
            BoundRef::Intervention {
                task: task("01"),
                intervention_id: "iv-01".to_string(),
            },
        )
        .expect("bind intervention");
        let replay = s
            .bind(
                &requester(),
                "iv-1",
                "iv-digest-a",
                BoundRef::Intervention {
                    task: task("01"),
                    intervention_id: "iv-02".to_string(),
                },
            )
            .expect("same digest replays");
        assert_eq!(
            replay.bound,
            BoundRef::Intervention {
                task: task("01"),
                intervention_id: "iv-01".to_string(),
            }
        );
        assert!(s
            .bind(
                &requester(),
                "iv-1",
                "iv-digest-b",
                BoundRef::Intervention {
                    task: task("01"),
                    intervention_id: "iv-03".to_string(),
                },
            )
            .is_err());
    }
}
