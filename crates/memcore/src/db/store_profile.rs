//! Which *shape* of database a store is (#1585 D2).
//!
//! ## Why a stamped profile, not a feature flag
//!
//! `memcore` compiles two ways: with `admin` (the full Tachi product surface)
//! and without it (`portable-kernel`). Until #1585 the DDL did not care — a
//! portable build still *created every product table* so it could open a DB
//! written by full Tachi. That made the boundary a compile-time fiction: the
//! file on disk was identical either way, so "portable kernel" never actually
//! described anything a downstream embedder received.
//!
//! [`StoreProfile`] makes the boundary a property of the DATABASE, written
//! into it at creation and read back at every subsequent open:
//!
//! * [`StoreProfile::PortableKernel`] — the memory kernel only. Product
//!   tables (Hub/Vault/Foundry/ExecEnv/dispatch/mirror-eval/session-claims/
//!   identity, plus `audit_log`, `agent_known_state`, `llm_usage`, and the
//!   `sandbox_*` trio) are never created.
//! * [`StoreProfile::TachiFull`] — everything, i.e. every database this
//!   codebase has ever produced before #1585.
//!
//! ## The load-bearing rule (frozen, #1585 D2)
//!
//! **The effective profile that drives DDL and the migration walk is the
//! STORED profile, never the required one.** A caller's
//! [`crate::db::DbOpenContext::required_profile`] is an *admission check*:
//! "may I use this store at all?". It never re-shapes a store. Getting this
//! backwards is the single most damaging failure mode in this design — a
//! portable opener holding `MigrationAuthority::Allow` would migrate a real
//! `TachiFull` database *as if* it were portable, skipping every product
//! migration while still marking their sentinels, and hand back a database
//! that every gate calls complete and that is silently missing columns the
//! deployed daemon writes. The pin test
//! `portable_opens_full_store_and_migrates_as_full` exists for exactly this.
//!
//! `Default` is [`StoreProfile::TachiFull`], so an un-threaded open can only
//! ever *demand more* than it needs (refusing a portable store loudly), never
//! silently accept less.
//!
//! ## Exact admission (W1-2)
//!
//! The requirement is a [`ProfileRequirement`]. `AtLeast(p)` is the lattice
//! rule above: a `TachiFull` store admits a `PortableKernel` caller. `Exact(p)`
//! admits only a store stamped `p`. It exists for embedders whose product
//! boundary is stricter than the kernel's: Hypermem must never serve, or stamp a
//! role into, a Tachi product database. Exactness only narrows *admission*.
//! The effective-profile rule is unchanged: whatever is admitted is driven by
//! its stored profile.

use crate::error::MemoryError;

/// `hard_state` namespace holding a store's write-once identity rows (#1579).
/// Shared by the role stamp and the profile stamp; guarded against
/// `set_state`/`delete_state` in [`crate::db::state`].
pub const STORE_IDENTITY_NAMESPACE: &str = "store_identity";

/// `hard_state` key (in [`STORE_IDENTITY_NAMESPACE`]) holding the profile
/// stamp.
pub const STORE_PROFILE_KEY: &str = "profile";

/// `hard_state` key (in [`STORE_IDENTITY_NAMESPACE`]) holding the manifest
/// role stamp (`global` / `wiki` / a validated project name).
pub const STORE_ROLE_KEY: &str = "role";

/// The schema shape of a database. See the module docs for the lattice and
/// for the frozen effective-profile rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StoreProfile {
    /// Memory kernel only — no product tables.
    PortableKernel,
    /// Full Tachi product schema. Every pre-#1585 database is this.
    TachiFull,
}

impl StoreProfile {
    /// Stable on-disk token. Written into the profile stamp's JSON, so it is
    /// part of the persisted contract — do not rename without a migration.
    pub fn as_str(self) -> &'static str {
        match self {
            StoreProfile::PortableKernel => "portable_kernel",
            StoreProfile::TachiFull => "tachi_full",
        }
    }

    /// Parse a stamped token. Unknown tokens are `None` — a store stamped by
    /// a NEWER kernel with a profile this build does not understand must be
    /// refused, never silently downgraded to a shape we happen to know.
    pub fn from_stamp_token(token: &str) -> Option<Self> {
        match token {
            "portable_kernel" => Some(StoreProfile::PortableKernel),
            "tachi_full" => Some(StoreProfile::TachiFull),
            _ => None,
        }
    }

    /// Lattice: `TachiFull ⊇ PortableKernel`. `self` is the profile a store
    /// actually has; `required` is what a caller demands of it.
    ///
    /// A full store satisfies a portable requirement (it is a superset); a
    /// portable store does NOT satisfy a full requirement (the product tables
    /// the caller will reach for do not exist).
    pub fn satisfies(self, required: StoreProfile) -> bool {
        match (self, required) {
            (StoreProfile::TachiFull, _) => true,
            (StoreProfile::PortableKernel, StoreProfile::PortableKernel) => true,
            (StoreProfile::PortableKernel, StoreProfile::TachiFull) => false,
        }
    }

    /// Whether product-scoped DDL and product-scoped migrations run for this
    /// profile. The single predicate every gating site asks — deliberately
    /// NOT a `table_exists` sniff, which would silently "adapt" to a
    /// half-built database instead of refusing it.
    pub fn includes_product(self) -> bool {
        matches!(self, StoreProfile::TachiFull)
    }
}

impl Default for StoreProfile {
    /// Fail-*loud* default: demand the full product schema. An open that
    /// forgot to declare a profile can only over-demand (and get a typed
    /// [`MemoryError::StoreProfileMismatch`]), never silently accept a
    /// portable store and then hit `no such table` at runtime.
    fn default() -> Self {
        StoreProfile::TachiFull
    }
}

/// What a caller demands of a store's profile (#1585 D2, W1-2). Carried by
/// [`crate::db::DbOpenContext::required_profile`].
///
/// On a **fresh** file both variants build [`Self::profile`]. On an **existing**
/// store they differ only in which stamped profiles they admit:
///
/// | stored \ required | `AtLeast(Portable)` | `AtLeast(Full)` | `Exact(Portable)` | `Exact(Full)` |
/// |------------------|---------------------|-----------------|-------------------|---------------|
/// | `PortableKernel` | admit               | refuse          | admit             | refuse        |
/// | `TachiFull`      | admit               | admit           | **refuse**        | admit         |
///
/// A refusal happens in the open funnel's read-only identity preflight, before
/// the migration backup, the connection PRAGMAs, any DDL and any stamp. The
/// same resolver re-runs inside the schema transaction as the authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProfileRequirement {
    /// The stored profile must [`StoreProfile::satisfies`] this one. This is
    /// the historical #1585 D2 rule.
    AtLeast(StoreProfile),
    /// The stored profile must be exactly this one. A superset store is
    /// refused with [`MemoryError::StoreProfileNotExact`].
    Exact(StoreProfile),
}

impl ProfileRequirement {
    /// The profile named by the requirement, which is also the shape a fresh
    /// file is built with.
    pub fn profile(self) -> StoreProfile {
        match self {
            ProfileRequirement::AtLeast(profile) | ProfileRequirement::Exact(profile) => profile,
        }
    }

    /// Whether an existing store stamped `stored` is admitted.
    pub fn admits(self, stored: StoreProfile) -> bool {
        match self {
            ProfileRequirement::AtLeast(required) => stored.satisfies(required),
            ProfileRequirement::Exact(required) => stored == required,
        }
    }

    pub fn is_exact(self) -> bool {
        matches!(self, ProfileRequirement::Exact(_))
    }
}

impl Default for ProfileRequirement {
    /// `AtLeast(TachiFull)`: the same over-demanding default as
    /// [`StoreProfile::default`].
    fn default() -> Self {
        ProfileRequirement::AtLeast(StoreProfile::default())
    }
}

impl From<StoreProfile> for ProfileRequirement {
    /// A bare profile keeps its historical meaning: `AtLeast`.
    fn from(profile: StoreProfile) -> Self {
        ProfileRequirement::AtLeast(profile)
    }
}

impl std::fmt::Display for StoreProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Decode a stamped profile token, refusing an unknown one instead of
/// guessing. `db_path` only decorates the error.
pub(crate) fn parse_stored_profile(
    token: &str,
    db_path: &std::path::Path,
) -> Result<StoreProfile, MemoryError> {
    StoreProfile::from_stamp_token(token).ok_or_else(|| {
        MemoryError::InvalidArg(format!(
            "database at {} carries an unrecognized store profile stamp {token:?}; \
             this kernel understands {:?} and {:?} — refusing to guess its shape",
            db_path.display(),
            StoreProfile::PortableKernel.as_str(),
            StoreProfile::TachiFull.as_str(),
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lattice_is_a_superset_relation_not_equality() {
        assert!(StoreProfile::TachiFull.satisfies(StoreProfile::TachiFull));
        assert!(StoreProfile::TachiFull.satisfies(StoreProfile::PortableKernel));
        assert!(StoreProfile::PortableKernel.satisfies(StoreProfile::PortableKernel));
        assert!(!StoreProfile::PortableKernel.satisfies(StoreProfile::TachiFull));
    }

    #[test]
    fn product_inclusion_matches_the_lattice_top() {
        assert!(StoreProfile::TachiFull.includes_product());
        assert!(!StoreProfile::PortableKernel.includes_product());
    }

    #[test]
    fn default_is_the_over_demanding_end() {
        assert_eq!(StoreProfile::default(), StoreProfile::TachiFull);
        assert_eq!(
            ProfileRequirement::default(),
            ProfileRequirement::AtLeast(StoreProfile::TachiFull)
        );
    }

    #[test]
    fn requirement_admission_table() {
        use ProfileRequirement::{AtLeast, Exact};
        use StoreProfile::{PortableKernel as P, TachiFull as F};
        // (required, stored, admitted)
        let table = [
            (AtLeast(P), P, true),
            (AtLeast(P), F, true),
            (AtLeast(F), P, false),
            (AtLeast(F), F, true),
            (Exact(P), P, true),
            (Exact(P), F, false),
            (Exact(F), P, false),
            (Exact(F), F, true),
        ];
        for (required, stored, admitted) in table {
            assert_eq!(
                required.admits(stored),
                admitted,
                "{required:?} admitting {stored:?}"
            );
        }
        assert_eq!(ProfileRequirement::from(P), AtLeast(P));
        assert_eq!(Exact(P).profile(), P);
    }

    #[test]
    fn stamp_tokens_round_trip_and_unknown_tokens_are_refused() {
        for profile in [StoreProfile::PortableKernel, StoreProfile::TachiFull] {
            assert_eq!(
                StoreProfile::from_stamp_token(profile.as_str()),
                Some(profile)
            );
        }
        assert_eq!(StoreProfile::from_stamp_token("hypermem_v2"), None);
        assert!(parse_stored_profile("hypermem_v2", std::path::Path::new("/tmp/x.db")).is_err());
    }
}
