//! Typed schema-migration authorization threaded through the DB-open call
//! chain (#1119).
//!
//! ## Why this exists (the incident, and why the fix is a type, not an env var)
//!
//! A dev-built binary opened a live project DB and silently upgraded its
//! `PRAGMA user_version` from schema 17 to 18. The deployed daemon — which
//! only understood schema 17 — then refused every poll of that DB (correctly;
//! see [`crate::db::migrations::check_schema_version_gate`]) and WARN-spammed
//! for hours before anyone noticed.
//!
//! The first fix attempt made "may this process migrate?" a process
//! environment variable (`TACHI_ALLOW_SCHEMA_MIGRATION`), opt-in by default,
//! with a per-spawn `env_remove` at every subprocess boundary. sol judged that
//! **wrong-layer**: there are 40+ spawn surfaces, enumerating them is
//! forever-incomplete (ACPx/stdio were already missed), and there is no
//! compile-time enforcement that a new spawn site scrubs the var. The root
//! mistake was letting a *capability* become ambient process state at all.
//!
//! The second wrong-layer piece inferred "fresh DB vs existing DB" from DB
//! **content** (a `sqlite_master` table count). That is unsound: `init_schema`
//! builds a full-table DB that still reads back `user_version == 0`, so a
//! freshly-created DB is indistinguishable from a data-bearing legacy one by
//! content alone.
//!
//! ## The fix
//!
//! Authorization is a **typed value threaded down the DB-open call chain**,
//! never read from the environment and never inferred from DB content. The
//! caller that opens a DB already knows *why* it is opening it (provisioning a
//! new DB vs opening an operational one) and *whether* it is the intended
//! deploy-time migrator — so it says so, in the type system. This mirrors the
//! #894 exec-env authorization pattern (`PrivateTargetApproval { approved_by,
//! .. }`, `env_id`/`unmanaged_cwd`) already established in this codebase.

/// Legacy environment variable the first (reverted) fix attempt used to carry
/// migration opt-in. **Never read** by this design — authorization is the
/// typed [`DbOpenContext`] threaded through the call chain. The deploy
/// bootstrap clears this var once at startup (defensively, in case a stale
/// value survives in some launch environment) so that even code paths outside
/// this crate cannot resurrect the ambient-capability antipattern.
pub const SCHEMA_MIGRATION_LEGACY_ENV: &str = "TACHI_ALLOW_SCHEMA_MIGRATION";

/// Why a DB is being opened. Carries the caller's *intent* so the gate never
/// has to infer "fresh vs existing" from DB content (the second wrong-layer
/// piece this redesign removes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenIntent {
    /// Provisioning a brand-new DB at this path. Building schema on a fresh
    /// (unstamped, `user_version == 0`) file needs no migration authority.
    /// A real, *older*-stamped operational DB is not a create target and is
    /// refused rather than silently migrated under a "create" label.
    CreateFresh,
    /// Opening a DB that is expected to already exist and be operational. A
    /// stored version below `EXPECTED_SCHEMA_VERSION` is a *migration*
    /// decision gated by [`MigrationAuthority`] — never silently inferred to
    /// be "fresh".
    OpenExisting,
}

/// Whether this process is authorized to migrate an existing older-schema DB
/// forward in place. Fail-closed: [`MigrationAuthority::Deny`] by default;
/// [`MigrationAuthority::Allow`] only when an operator explicitly opted in
/// (the deploy ritual's `--allow-schema-migration` flag), carrying provenance
/// for the audit log line the incident report asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationAuthority {
    /// Refuse to auto-migrate an existing older-schema DB; return a typed
    /// [`crate::error::MemoryError::SchemaMigrationOptInRequired`] instead.
    /// This is the fail-closed posture every dev/test/agent binary carries.
    Deny,
    /// Migrate an existing older-schema DB forward in place. `approved_by`
    /// records who signed off (e.g. `"cli:--allow-schema-migration"`), so an
    /// operator grepping logs after a migration can attribute it.
    Allow { approved_by: String },
}

/// Typed context threaded down the DB-open call chain (#1119). Never read from
/// process env, never inferred from DB content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbOpenContext {
    pub intent: OpenIntent,
    pub migration: MigrationAuthority,
}

impl DbOpenContext {
    /// Provision a brand-new DB. Migration authority is neither needed nor
    /// consulted — building schema on a fresh file harms no deployed daemon.
    pub fn create_fresh() -> Self {
        Self {
            intent: OpenIntent::CreateFresh,
            migration: MigrationAuthority::Deny,
        }
    }

    /// Open an existing operational DB, refusing any in-place schema migration.
    /// This is the fail-closed default for dev/test/agent binaries — exactly
    /// the #1119 accident path, now blocked by construction.
    pub fn open_existing_deny() -> Self {
        Self {
            intent: OpenIntent::OpenExisting,
            migration: MigrationAuthority::Deny,
        }
    }

    /// Open an existing operational DB with explicit authority to migrate it
    /// forward in place. Only the deploy ritual's opted-in daemon constructs
    /// this; `approved_by` is recorded in the migration log.
    pub fn open_existing_allow(approved_by: impl Into<String>) -> Self {
        Self {
            intent: OpenIntent::OpenExisting,
            migration: MigrationAuthority::Allow {
                approved_by: approved_by.into(),
            },
        }
    }

    /// True iff this context authorizes migrating an existing older-schema DB.
    pub fn migration_allowed(&self) -> bool {
        matches!(self.migration, MigrationAuthority::Allow { .. })
    }

    /// The provenance string when migration is authorized, else `None`.
    pub fn approved_by(&self) -> Option<&str> {
        match &self.migration {
            MigrationAuthority::Allow { approved_by } => Some(approved_by.as_str()),
            MigrationAuthority::Deny => None,
        }
    }
}

impl Default for DbOpenContext {
    /// Fail-closed default: open an existing DB, refuse migration. Every bare
    /// `MemoryStore::open` / `open_with_label` resolves to this until a caller
    /// threads an explicit context — so an accidentally-unthreaded open can
    /// only ever *refuse* to migrate, never silently perform one.
    fn default() -> Self {
        Self::open_existing_deny()
    }
}
