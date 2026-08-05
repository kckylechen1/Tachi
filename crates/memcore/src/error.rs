use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkClaimTransitionReason {
    HolderMismatch,
}

impl std::fmt::Display for WorkClaimTransitionReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HolderMismatch => formatter.write_str("holder_mismatch"),
        }
    }
}

/// Why the outbox reconciliation protocol refused a reported outcome or a
/// conflict resolution (tachi#1644, #1630 workstream A leaf A2).
///
/// These are **protocol violations**, not illegal transitions: the caller
/// reported something about an event that is not in a position to receive it.
/// They are typed and separate from [`MemoryError::OutboxIllegalTransition`]
/// because a reconciliation loop must be able to tell "my message arrived out
/// of order / for the wrong event" (fix the loop) from "this edge does not
/// exist in the state machine" (fix the code), without parsing prose.
///
/// Carrying a reason enum rather than four error variants follows
/// [`WorkClaimTransitionReason`]: the refusal shape (which event, in which
/// state) is identical across all four, and only the *why* differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxOutcomeRefusal {
    /// An outcome was reported for an event no consumer was ever handed
    /// (`pending`). Nobody could have observed it, so nobody can report on it.
    NeverClaimed,
    /// An outcome was reported for an event that was withdrawn from the
    /// pipeline (`quarantined`). Un-withdrawing is an operator decision, not
    /// something a late acknowledgement may do implicitly.
    Withdrawn,
    /// The event already carries a terminal outcome, and the reported one is
    /// not the same outcome with the same error class. The recorded outcome
    /// stands: a second, different report is never allowed to overwrite the
    /// first — that would be exactly the last-write-wins behaviour #1630
    /// forbids.
    OutcomeAlreadyDiffers,
    /// A conflict resolution was requested for an event that is not
    /// `conflicted` — including one whose conflict was already resolved, since
    /// a resolution consumes the event it resolves.
    NotConflicted,
}

impl std::fmt::Display for OutboxOutcomeRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self {
            Self::NeverClaimed => "never_claimed",
            Self::Withdrawn => "withdrawn",
            Self::OutcomeAlreadyDiffers => "outcome_already_differs",
            Self::NotConflicted => "not_conflicted",
        };
        formatter.write_str(reason)
    }
}

/// Why a stored recall-impression group cannot be replayed by this binary.
///
/// This deliberately carries no query, memory, path, or score material: replay
/// compatibility is metadata-only and callers must be able to report a refusal
/// without turning the error path into a content surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecallReplayCompatibilityReason {
    /// v25 groups predate policy persistence, so current math is not evidence
    /// of their historical fusion result.
    LegacyUnversioned,
    /// A group has only part of the required policy tuple.
    IncompletePolicy,
    /// A versioned group lacks the canonical 64-character lowercase SHA-256
    /// cohort identity, or carries a malformed value.
    InvalidQueryFingerprint,
    UnsupportedFusionPolicy,
    UnsupportedPreBoostAdjustmentPolicy,
    UnsupportedTieBreakPolicy,
    UnsupportedCandidatePolicy,
    UnsupportedSchemaIdentity,
}

impl std::fmt::Display for RecallReplayCompatibilityReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self {
            Self::LegacyUnversioned => "legacy_unversioned",
            Self::IncompletePolicy => "incomplete_policy",
            Self::InvalidQueryFingerprint => "invalid_query_fingerprint",
            Self::UnsupportedFusionPolicy => "unsupported_fusion_policy",
            Self::UnsupportedPreBoostAdjustmentPolicy => "unsupported_pre_boost_adjustment_policy",
            Self::UnsupportedTieBreakPolicy => "unsupported_tie_break_policy",
            Self::UnsupportedCandidatePolicy => "unsupported_candidate_policy",
            Self::UnsupportedSchemaIdentity => "unsupported_schema_identity",
        };
        formatter.write_str(reason)
    }
}

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid argument: {0}")]
    InvalidArg(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Vault error: {0}")]
    Vault(String),

    #[error("Vault locked")]
    VaultLocked,

    #[error("Vault not initialized")]
    VaultNotInitialized,

    #[error("Duplicate entry: {0}")]
    Duplicate(String),

    #[error("Internal error: {0}")]
    Internal(String),

    /// A versioned WorkClaim transition lost its compare-and-swap race.
    #[error("WorkClaim conflict: {0}")]
    WorkClaimConflict(String),

    /// A requested WorkClaim operation is not valid for the persisted state.
    #[error("WorkClaim incompatible state: {0}")]
    WorkClaimIncompatibleState(String),

    /// tachi#1643: a durable-outbox transition the frozen #1630 state machine
    /// does not permit. Typed rather than a generic `InvalidArg` string so a
    /// reconciliation loop can branch on "this outcome no longer applies to
    /// this event" without string-sniffing, and so both endpoints survive the
    /// trip to the caller. The fields carry the canonical state tokens
    /// (`OutboxState::as_str`) as `String` rather than the enum itself: this
    /// module deliberately depends on nothing in `crate::db`, the same reason
    /// [`WorkClaimTransitionReason`] is declared here instead of imported.
    #[error("outbox event '{event_id}' cannot transition from '{from}' to '{to}'")]
    OutboxIllegalTransition {
        event_id: String,
        from: String,
        to: String,
    },

    /// tachi#1644: the reconciliation protocol refused a reported outcome or a
    /// conflict resolution because the event is not in a position to receive
    /// it. See [`OutboxOutcomeRefusal`] for the four reasons and why they are
    /// distinct from [`Self::OutboxIllegalTransition`]. `state` carries the
    /// canonical state token as a `String` for the same reason the illegal
    /// transition's endpoints do: this module depends on nothing in
    /// `crate::db`.
    ///
    /// Nothing is written on any of these refusals — the recorded outcome is
    /// exactly what it was before the call.
    #[error("outbox outcome for event '{event_id}' refused ({reason}): event is '{state}'")]
    OutboxOutcomeRefused {
        reason: OutboxOutcomeRefusal,
        event_id: String,
        state: String,
    },

    /// An admitted caller attempted a transition that only the persisted
    /// holder may perform. The stable reason is typed so API boundaries can
    /// distinguish authorization refusal from state/version conflicts.
    #[error(
        "WorkClaim transition refused ({reason}): claim {claim_id} is held by {holder_identity_id}, caller is {caller_identity_id}"
    )]
    WorkClaimTransitionRefused {
        reason: WorkClaimTransitionReason,
        claim_id: String,
        holder_identity_id: String,
        caller_identity_id: String,
    },

    /// #1119: a process carrying [`crate::db::MigrationAuthority::Deny`] tried
    /// to open an EXISTING DB stamped below this kernel's
    /// `EXPECTED_SCHEMA_VERSION` (or a `CreateFresh` provisioning call landed
    /// on a real, older-stamped DB). Refused instead of silently migrating in
    /// place — which would strand any deployed daemon still depending on
    /// schema `stored`. A typed variant (not a generic `InvalidArg` string) so
    /// callers can match this refusal programmatically instead of
    /// string-sniffing it. Authorization is the typed
    /// [`crate::db::DbOpenContext`] threaded through the DB-open call chain —
    /// there is no longer any process env var to set (#1119 redesign; the old
    /// `TACHI_ALLOW_SCHEMA_MIGRATION` opt-in was reverted as wrong-layer).
    #[error(
        "refusing to migrate db schema {stored} -> {expected} at {db_path} without explicit \
         authority: this looks like a dev/test/agent binary — or a fresh-provisioning open that \
         landed on a real older DB — opening a live database a deployed daemon may still depend \
         on schema {stored} for (see kckylechen1/Sigil#1119). Only the deploy ritual should \
         migrate in place: pass --allow-schema-migration to tachi-server, which becomes a typed \
         MigrationAuthority::Allow threaded to every DB open (never a process env var). A \
         completed migration would leave a trail beside this DB: {backup_hint} (pre-migration \
         backup) and {marker_hint} (fingerprint of the last migration run)."
    )]
    SchemaMigrationOptInRequired {
        stored: u32,
        expected: u32,
        db_path: String,
        backup_hint: String,
        marker_hint: String,
    },

    /// #1119: an `OpenIntent::CreateFresh` open landed on a path that already
    /// holds a *stamped* DB (`PRAGMA user_version == stored`, `stored >= 1`).
    /// A "create" is not a "migrate" and not an "open" — refusing here stops a
    /// provisioning call from silently operating on (or clobbering) a live
    /// operational DB. Use `OpenIntent::OpenExisting` (with authority, to
    /// migrate an older one) to open an existing DB. A brand-new file
    /// (`stored == 0`, even if `init_schema` already built its tables) is
    /// treated as fresh and is NOT rejected — the discriminator is the version
    /// stamp, never DB content.
    #[error(
        "refusing to CreateFresh at {db_path}: a database is already stamped there at schema \
         {stored}. A fresh-provisioning open must not operate on a pre-existing operational DB \
         (kckylechen1/Sigil#1119). Open it with OpenExisting instead (add migration authority \
         to bring an older schema forward)."
    )]
    DbCreateTargetExists { stored: u32, db_path: String },

    /// A recall-impression group does not carry a replay policy that this
    /// binary can execute. This is intentionally a typed, content-free
    /// refusal: replay must not silently substitute current math for unknown
    /// historical math.
    #[error("recall impression replay refused for group {group_id}: {reason}")]
    RecallReplayIncompatible {
        group_id: String,
        reason: RecallReplayCompatibilityReason,
    },

    /// #1579: a caller declared a manifest role that disagrees with the role
    /// stamped inside the database.
    ///
    /// Deliberately NOT resolved by preferring either side. A conflict means
    /// either the routing that produced the claim is wrong, or the file has
    /// been moved/replaced — and both are worse to guess at than to refuse.
    /// The pre-#1579 behavior (first writer's inferred label wins, silently)
    /// is exactly the bug this replaces.
    #[error(
        "store role conflict at {db_path}: caller claims role {claimed:?} but the database is \
         stamped {stored:?}. Store identity is written once inside the file and is not \
         overridable by the caller or by the directory the file sits in \
         (kckylechen1/Sigil#1579). Either route this open to the store that really holds \
         {claimed:?}, or clear the `store_identity` namespace on a stopped daemon if this \
         database was genuinely re-purposed."
    )]
    StoreRoleConflict {
        claimed: String,
        stored: String,
        db_path: String,
    },

    /// #1585: this caller requires a schema profile the store does not
    /// provide. In practice: full Tachi opening a `portable_kernel` database,
    /// whose product tables (Vault/Hub/Foundry/ExecEnv/dispatch/…) were never
    /// created. Refused at open rather than surfacing as `no such table` at
    /// the first product call.
    #[error(
        "store profile mismatch at {db_path}: caller requires profile {required:?} but the \
         database is stamped {stored:?}. A portable-kernel database does not carry the Tachi \
         product tables and cannot be grown into one by opening it \
         (kckylechen1/Sigil#1585). Point this process at a {required:?} database."
    )]
    StoreProfileMismatch {
        required: String,
        stored: String,
        db_path: String,
    },

    /// #1585: a caller that requires only the portable kernel opened an
    /// EXISTING database carrying no profile stamp.
    ///
    /// Adoption is asymmetric on purpose. An unstamped existing database is
    /// pre-#1585, i.e. certainly a full Tachi store, so a `TachiFull` opener
    /// adopts it silently and correctly. A portable opener must NOT: adopting
    /// would let a portable binary claim authority over a product database and
    /// then walk its migrations as if the product tables were absent.
    #[error(
        "store profile unstamped at {db_path}: this caller requires the portable-kernel \
         profile, but the database carries no profile stamp, which means it predates \
         kckylechen1/Sigil#1585 and is a full Tachi store. Open it once with a full-profile \
         binary (which adopts and stamps it as tachi_full), or point this process at a \
         database created by a portable-kernel build."
    )]
    StoreProfileUnstamped { db_path: String },
}
