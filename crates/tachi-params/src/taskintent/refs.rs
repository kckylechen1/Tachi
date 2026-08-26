//! Typed identity vocabulary for the TaskIntent bridge (tachi#1840 / zeroclaw
//! #205 TB-14).
//!
//! The eight frozen refs — `ConversationSessionRef`, `ParentRunRef`,
//! `SubAgentRunRef`, `TaskRef`, `AttemptRef`, `HarnessSessionRef`,
//! `ProcedureRunRef`, `DeliveryIntentRef` — are non-interchangeable at the
//! type level AND at the serialization level: each serializes as a string in
//! its own wire namespace (`"conv:"`, `"parent:"`, …), and deserialization
//! validates the namespace, so a value serialized as one ref cannot be
//! consumed as another. There is deliberately **no `From<String>`** and no
//! `Deref` — the only construction paths are:
//!
//! * [`TaskRef::mint`] — `pub(crate)`, callable only from this crate's bridge
//!   (Tachi mints after admission; caller-provided task ids are not
//!   authority, TB-6). ZeroClaw has no construction path: the type cannot be
//!   built outside this crate, and wire decode requires the `"task:"`
//!   namespace plus a minted-shape body.
//! * the other refs mint through the same `pub(crate)` `mint` used by the
//!   bridge when projecting existing truth (e.g. an `AttemptRef` over a
//!   `dispatch_outcomes` row).
//!
//! Wire namespaces are part of the frozen golden (`task-intent.v1`): changing
//! a prefix breaks the golden decode test on this side and the encoder test
//! on the ZeroClaw side (V2b).

use std::fmt;

use serde::{de::Error as _, Deserialize, Deserializer, Serialize};

/// Byte cap for any single ref value on the wire. Refs are opaque bounded
/// identifiers, never content carriers (TB-4 forbids oversized payloads).
pub const REF_VALUE_MAX: usize = 256;

/// Generates one namespaced ref newtype.
///
/// `$mint_vis` is always `pub(crate)`: minting is Tachi-internal authority.
macro_rules! namespaced_ref {
    ($(#[$meta:meta])* $name:ident, $prefix:expr) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        pub struct $name(String);

        impl $name {
            /// Wire namespace prefix (frozen by the `task-intent.v1` golden).
            pub const WIRE_PREFIX: &'static str = $prefix;

            /// The namespaced wire form, e.g. `"task:…"`.
            pub fn as_wire(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(d)?;
                let prefix = Self::WIRE_PREFIX;
                let body = raw
                    .strip_prefix(prefix)
                    .ok_or_else(|| {
                        D::Error::custom(concat!(
                            stringify!($name),
                            " wire value must use its own namespace prefix",
                        ))
                    })?;
                if body.is_empty() || body.len() > REF_VALUE_MAX {
                    return Err(D::Error::custom(concat!(
                        stringify!($name),
                        " wire body must be 1..=256 bytes",
                    )));
                }
                Ok(Self(raw))
            }
        }
    };
}

/// Same as [`namespaced_ref!`] plus a `pub(crate)` mint — ONLY for refs
/// this crate itself mints today (`TaskRef` after admission, `AttemptRef`
/// over dispatch truth). Refs without a current minter stay wire-only
/// (constructed by deserialization) rather than carrying dead mint paths.
macro_rules! mintable_ref {
    ($(#[$meta:meta])* $name:ident, $prefix:expr) => {
        namespaced_ref!($(#[$meta])* $name, $prefix);
        impl $name {
            /// Tachi-internal mint. Not callable outside this crate, so no
            /// host can construct its own ref values (TB-6).
            pub(crate) fn mint(value: impl Into<String>) -> Self {
                Self(value.into())
            }
        }
    };
}

namespaced_ref!(
    /// A durable conversational session identity (TB-14). Not a Tachi
    /// `acp_sessions.session_uuid`, not a `TaskRef`.
    ConversationSessionRef,
    "conv:"
);
namespaced_ref!(
    /// A parent run the submitting requester belongs to (TB-14).
    ParentRunRef,
    "parent:"
);
namespaced_ref!(
    /// A supervising sub-agent run (TB-14, zeroclaw #202 SubAgent spine).
    SubAgentRunRef,
    "subrun:"
);
mintable_ref!(
    /// A Tachi-minted durable task identity (TB-6: minted by Tachi only,
    /// after admission; caller-provided task ids are not authority).
    ///
    /// In V2a this is a typed bridge identity over existing work/dispatch
    /// truth (TB-1); the durable TaskRef store itself is tachi#1623 (NOT-YET).
    TaskRef,
    "task:"
);
mintable_ref!(
    /// One execution try of a task (TB-18: Task ≠ Attempt). In V2a attempt
    /// facts ride existing `dispatch_outcomes` rows; this ref is the typed
    /// projection over that truth (TB-14 mapping note).
    AttemptRef,
    "attempt:"
);
namespaced_ref!(
    /// A host/harness-owned session attached through the #1678/#1733
    /// attachment spine (never an ACP `session_uuid`).
    HarnessSessionRef,
    "harness:"
);
namespaced_ref!(
    /// A procedure run identity (TB-14).
    ProcedureRunRef,
    "proc:"
);
namespaced_ref!(
    /// A durable delivery intent identity (tachi#1679 surface; V2a is
    /// pull-only so this ref is only projected, never minted here).
    DeliveryIntentRef,
    "deliver:"
);

/// The identity of the admitted requester submitting an intent (TB-3 wire
/// field; not one of the TB-14 eight, and never interchangeable with them).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RequesterRef(String);

impl RequesterRef {
    /// Minimum/maximum length for a requester identity value.
    pub const LEN_RANGE: std::ops::RangeInclusive<usize> = 1..=REF_VALUE_MAX;

    /// Admitted requester identity. Unlike the eight canonical refs this is
    /// caller-stated identity that admission must verify against its own
    /// authority source — construction is therefore a *claim*, and the
    /// bridge only trusts the value after the
    /// [`crate::taskintent::RequesterAuthorityPort`] resolves it.
    pub fn claim(value: impl Into<String>) -> Result<Self, RefError> {
        let value = value.into();
        if !Self::LEN_RANGE.contains(&value.len()) {
            return Err(RefError::InvalidLength);
        }
        Ok(Self(value))
    }
}

impl fmt::Display for RequesterRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<RequesterRef> for String {
    fn from(value: RequesterRef) -> Self {
        value.0
    }
}

/// A caller RequestId for TB-7 idempotency: the `(requester, request_id)`
/// tuple is the idempotency scope for submit, intervene, and stop alike.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RequestId(String);

impl RequestId {
    /// Minimum/maximum length for a request id value.
    pub const LEN_RANGE: std::ops::RangeInclusive<usize> = 1..=128;

    /// Caller-chosen request id. Ruling-205 §2: a caller that loses a submit
    /// response must REPLAY the same request id, never invent a new one.
    pub fn new(value: impl Into<String>) -> Result<Self, RefError> {
        let value = value.into();
        if !Self::LEN_RANGE.contains(&value.len()) {
            return Err(RefError::InvalidLength);
        }
        Ok(Self(value))
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<RequestId> for String {
    fn from(value: RequestId) -> Self {
        value.0
    }
}

impl TryFrom<String> for RequesterRef {
    type Error = RefError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::claim(value)
    }
}

impl TryFrom<String> for RequestId {
    type Error = RefError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// Typed ref construction failure.
#[derive(Debug, thiserror::Error)]
#[error("ref value length outside admitted bounds")]
pub enum RefError {
    /// Value length outside the admitted bounds for the ref type.
    InvalidLength,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eight_refs_have_distinct_wire_namespaces() {
        // TB-14: distinct serialization namespaces. A collision here would
        // make two refs interchangeable at the wire level.
        let prefixes = [
            ConversationSessionRef::WIRE_PREFIX,
            ParentRunRef::WIRE_PREFIX,
            SubAgentRunRef::WIRE_PREFIX,
            TaskRef::WIRE_PREFIX,
            AttemptRef::WIRE_PREFIX,
            HarnessSessionRef::WIRE_PREFIX,
            ProcedureRunRef::WIRE_PREFIX,
            DeliveryIntentRef::WIRE_PREFIX,
        ];
        let mut sorted = prefixes;
        sorted.sort_unstable();
        for pair in sorted.windows(2) {
            assert_ne!(pair[0], pair[1], "namespace prefix collision");
        }
    }

    #[test]
    fn refs_are_not_interchangeable_at_the_wire_level() {
        // TB-14 compile/serialization check: a `task:` value cannot decode as
        // an AttemptRef, and vice versa.
        let task = serde_json::to_string(&TaskRef::mint("task:abc")).expect("serialize");
        assert!(serde_json::from_str::<AttemptRef>(&task).is_err());
        let attempt = serde_json::to_string(&AttemptRef::mint("attempt:abc")).expect("serialize");
        assert!(serde_json::from_str::<TaskRef>(&attempt).is_err());
        assert!(serde_json::from_str::<TaskRef>("\"nonsense\"").is_err());
        // Namespace-only values are rejected (empty body).
        assert!(serde_json::from_str::<TaskRef>("\"task:\"").is_err());
    }

    #[test]
    fn task_ref_cannot_be_built_from_a_bare_string_by_hosts() {
        // TB-6 structural half: the only constructor is `pub(crate) mint`;
        // deserialization demands the minted wire namespace. There is no
        // From<String>, no Deref, and no other conversion (grep-verified in
        // `task_ref_has_no_string_construction_path`).
        let json = serde_json::json!("task:01J8ZER0CLAW0000000000000");
        let decoded: TaskRef = serde_json::from_value(json).expect("namespaced decode");
        assert!(decoded.as_wire().starts_with("task:"));
    }

    #[test]
    fn request_ids_and_requester_refs_are_bounded() {
        assert!(RequestId::new("").is_err());
        assert!(RequestId::new("x".repeat(129)).is_err());
        assert!(RequestId::new("retry-2026-08-25-001").is_ok());
        assert!(RequesterRef::claim("").is_err());
        assert!(RequesterRef::claim("zeroclaw-host-alpha").is_ok());
    }
}
