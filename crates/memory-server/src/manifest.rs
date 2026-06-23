//! Manifest v1 — single source of truth for which memory.db files Tachi owns.
//!
//! Stored at `~/.tachi/manifest.json`. JSON (not TOML) to avoid adding a new
//! workspace dependency; comments simulated via `_comment` keys where useful.
//!
//! Schema:
//! {
//!   "schema_version": 1,
//!   "generated_at": "<ISO-8601>",
//!   "_comment": "Tachi-owned memory DBs. Do not hand-edit while server runs.",
//!   "dbs": [
//!     {
//!       "path": "/Users/.../.tachi/global/memory.db",
//!       "role": "global",            // global | project | agent | foundry | unknown
//!       "owner": "tachi",            // tachi | openclaw-agent:<name> | antigravity | external
//!       "schema_kind": "tachi",      // tachi | openclaw_legacy | unknown
//!       "vec_enabled": true,
//!       "allow_write": true,
//!       "last_doctor_at": "<ISO-8601>",
//!       "last_classification": "healthy",
//!       "scope_hint": "global",
//!       "notes": ""
//!     }, ...
//!   ]
//! }
//!
//! Branch #2 deliverable: load/save manifest, populate from a doctor::DoctorReport,
//! lookup by role/scope_hint, and a CLI subcommand `tachi manifest` (show | init |
//! refresh). Branch #3 will route runtime save/recall through manifest lookups.

pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

mod gc;
mod model;
mod render;
mod schema;
mod sweep;

#[allow(unused_imports)]
pub use gc::{gc_manifest, GcReport};
#[allow(unused_imports)]
pub use model::{DbEntry, DbRole, Manifest, ManifestGuardError};
pub use render::render_manifest;
pub use schema::{
    canonicalize_db_path, classify_db_schema, is_archival_db_path, should_skip_path, SchemaKind,
};
#[allow(unused_imports)]
pub use sweep::{apply_sweep, plan_sweep, SweepAction, SweepReport};

#[cfg(test)]
mod tests;
