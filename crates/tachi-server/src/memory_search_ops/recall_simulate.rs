//! Labeled recall replay behind `tachi eval recall`.

mod config;
mod input;
mod runner;
mod types;

use serde_json::Value;

pub(crate) use runner::build_recall_simulation_report;

/// Replay inputs. Per-case fields in `metadata`/`text` override these defaults.
#[derive(Debug, Clone, Default)]
pub(crate) struct RecallSimulationParams {
    pub(crate) top_k: usize,
    /// Cases (`cases`/`eval_cases`/`case`) and optional `variants`.
    pub(crate) metadata: Option<Value>,
    /// JSON text fallback for cases/variants when `metadata` carries none.
    pub(crate) text: Option<String>,
    pub(crate) enable_rerank: bool,
    pub(crate) scope: Option<String>,
    pub(crate) path_prefix: Option<String>,
    pub(crate) project: Option<String>,
    pub(crate) domain: Option<String>,
    pub(crate) file_context: Option<String>,
    pub(crate) error_context: Option<String>,
    pub(crate) include_archived: bool,
    pub(crate) include_training: bool,
    pub(crate) as_of: Option<String>,
}
