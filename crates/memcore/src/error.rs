use thiserror::Error;

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
}
