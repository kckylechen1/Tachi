//! Verification ledger for background quality gates.
//!
//! Heavy checks such as `gitleaks detect`, `cargo check`, `cargo clippy`, or
//! full test suites are run by an external harness. This module only records
//! and reads their results under `.tachi/runs/<flow_id>/verification.json` so
//! leaders, briefing, and safe-merge gates consume the same evidence.

use crate::task_lifecycle::{flow_runs_root, run_dir_for_flow_id};
use crate::{MemoryServer, TachiVerifyParams};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

const LEDGER_FILE: &str = "verification.json";
const DEFAULT_STATUS_LIMIT: usize = 6;
const RECENT_SCAN_MAX: usize = 128;

/// Server-forced provenance for items written through the caller-facing
/// record/start path. Caller-authored prose (`head_sha`/`status`/`summary`)
/// can never mint merge authority: the gate requires `source` to start with
/// [`SERVER_RUN_SOURCE_PREFIX`] (written by the server-run executor, slice 2).
pub(crate) const CALLER_ASSERTED_SOURCE: &str = "caller_asserted";

/// Authority class prefix for ledger items produced by the server-side run
/// executor (`server_run:<kind>`). Since #1454 F1 the gate no longer reads
/// this string for authority — it is display-only metadata on the ledger
/// item; authority comes exclusively from the server-owned receipt store
/// ([`receipt_store`]). Kept so run receipts, ledger echoes, and tests can
/// still name the provenance class.
pub(crate) const SERVER_RUN_SOURCE_PREFIX: &str = "server_run:";

/// Canonical merge-evidence kinds — the full ci.yml rust-job `run:` surface.
/// // provisional (dispatch clause 9): mirrors ci.yml rust job; policy-tunable later.
///
/// The gate (gate.rs) requires a valid server-run receipt for EVERY kind in
/// this set before `overall` may be `passed`; missing kinds keep the gate
/// `pending` with `verification:<kind>:missing` (#1454 F4).
pub(crate) const MERGE_REQUIRED_RUN_KINDS: &[&str] = &[
    "version-sync",
    "clippy",
    "fmt",
    "audit",
    "nextest",
    "portable-contract",
    "doc",
];

mod gate;
mod handler;
mod ledger;
mod receipt;
mod receipt_store;
mod recent;
mod render;
mod run;
mod storage;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod receipt_golden_tests;

pub(crate) use self::gate::evaluate_verification_gate;
pub(crate) use self::handler::handle_tachi_verify;
pub(crate) use self::ledger::{read_verification_ledger, record_items, record_server_run_item};
pub(crate) use self::receipt_store::best_receipt_head;
pub(crate) use self::recent::recent_verification_summaries;
pub(crate) use self::run::run_verification_check;
pub(crate) use self::storage::markup_status;

#[cfg(test)]
pub(crate) use self::receipt_store::seed_run_receipt_for_test;
#[cfg(test)]
use self::storage::{ledger_path_for_flow, write_json};
