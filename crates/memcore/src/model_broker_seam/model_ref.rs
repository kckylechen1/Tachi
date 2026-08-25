use super::*;

// ---------------------------------------------------------------------------
// 1. ModelRef
// ---------------------------------------------------------------------------

/// An opaque reference to a model, resolved *against a specific alias-set policy
/// revision*.
///
/// It may name an alias (`memory.chat`) or a stable model reference — the
/// newtype is deliberately opaque, so callers cannot branch on "is this an
/// alias" at the type level; that classification is the resolver's job. The
/// paired `policy_revision` is the alias-set policy revision (a canonical-JSON
/// content digest per #1681 D2) the reference is meaningful under: a `ModelRef`
/// carries the revision it was minted against so a resolution can assert
/// `stamped == recomputed` rather than resolving against a drifted alias set
/// (a mismatch abstains with [`AbstainReason::PolicyRevisionMismatch`]).
///
/// Fields are private and `Deserialize` is routed through [`ModelRef::new`], so
/// neither a struct literal nor a JSON payload can produce a blank reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ModelRefWire")]
pub struct ModelRef {
    reference: String,
    policy_revision: String,
}

/// Deserialization shadow for [`ModelRef`] — same wire shape, no invariant;
/// the `TryFrom` below is the only way out of it.
#[derive(Deserialize)]
struct ModelRefWire {
    reference: String,
    policy_revision: String,
}

impl TryFrom<ModelRefWire> for ModelRef {
    type Error = SeamError;

    fn try_from(wire: ModelRefWire) -> Result<Self, Self::Error> {
        Self::new(wire.reference, wire.policy_revision)
    }
}

impl ModelRef {
    /// Construct a validated `ModelRef`.
    ///
    /// # Errors
    ///
    /// [`SeamError::EmptyField`] if `reference` or `policy_revision` is empty or
    /// whitespace-only.
    pub fn new(
        reference: impl Into<String>,
        policy_revision: impl Into<String>,
    ) -> Result<Self, SeamError> {
        let reference = reference.into();
        let policy_revision = policy_revision.into();
        require_non_empty(&reference, "reference")?;
        require_non_empty(&policy_revision, "policy_revision")?;
        Ok(Self {
            reference,
            policy_revision,
        })
    }

    /// The opaque reference string (alias name or stable model reference).
    pub fn reference(&self) -> &str {
        &self.reference
    }

    /// The alias-set policy revision this reference was minted against.
    pub fn policy_revision(&self) -> &str {
        &self.policy_revision
    }
}
