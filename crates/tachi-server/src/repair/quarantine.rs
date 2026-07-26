//! R3 — Quarantine resolution.
//!
//! PR-3 v4 migration moved cross-DB-polluted rows to /_quarantine/cross-db/...
//! and stamped `metadata.quarantine = { reason, original_path, detected_at,
//! expected_db, actual_db }`. This module exposes:
//!   - sweep mode (used by `tachi repair`): just reports a count of
//!     quarantined rows per DB.
//!   - `tachi repair quarantine list`           — full inventory
//!   - `tachi repair quarantine restore --id …` — same-DB restore to original_path
//!   - `tachi repair quarantine restore-all --to-db <label>`
//!         — bulk cross-DB physical move (INSERT into dest → verify → DELETE from src)
//!   - `tachi repair quarantine purge --older-than <days>`

use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, params_from_iter, Connection};
use serde_json::{json, Value};

use crate::manifest::Manifest;

use super::{
    inventory::{label_for, resolve_one, select_dbs},
    open_repair_connection, DbContext, Finding, RepairError, RepairExit, RepairRule, RuleReport,
};

mod legacy;
mod list;
mod purge;
mod restore;
mod rows;
mod sweep;

#[cfg(test)]
pub(crate) use self::legacy::rewrite_legacy_expected_db;
pub use self::list::cmd_list;
pub use self::purge::cmd_purge;
pub use self::restore::{cmd_restore, cmd_restore_all};
pub use self::sweep::QuarantineSweep;
