//! Verification ledger for background quality gates.
//!
//! Heavy checks such as `gitleaks detect`, `cargo check`, `cargo clippy`, or
//! full test suites are run by an external harness. This module only records
//! and reads their results under `.tachi/runs/<flow_id>/verification.json` so
//! leaders, briefing, and safe-merge gates consume the same evidence.

use crate::task_lifecycle::{run_dir_for_flow_id, shell_runs_root};
use crate::{MemoryServer, TachiVerifyParams};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

const LEDGER_FILE: &str = "verification.json";
const DEFAULT_STATUS_LIMIT: usize = 6;
const RECENT_SCAN_MAX: usize = 128;

mod gate;
mod handler;
mod ledger;
mod receipt;
mod recent;
mod render;
mod storage;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod receipt_golden_tests;

pub(crate) use self::gate::evaluate_verification_gate;
pub(crate) use self::handler::handle_tachi_verify;
pub(crate) use self::ledger::{read_verification_ledger, record_items};
pub(crate) use self::recent::recent_verification_summaries;

#[cfg(test)]
use self::storage::{ledger_path_for_flow, write_json};
