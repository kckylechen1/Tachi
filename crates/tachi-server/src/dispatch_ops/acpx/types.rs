use serde_json::Value;
use std::path::PathBuf;

pub(super) const ACPX_EVENTS_FILE: &str = "acpx_events.jsonl";
pub(super) const ACPX_NODE_REQUIREMENT: &str = ">=22.13.0";
pub(super) const ACPX_NODE_MIN_VERSION: (u64, u64, u64) = (22, 13, 0);

#[derive(Debug, Clone)]
pub(in crate::dispatch_ops) struct AcpxCommandSpec {
    pub command: String,
    pub args: Vec<String>,
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum AcpxRunMode {
    Exec,
    Session,
}

#[derive(Debug, Clone)]
pub(super) struct AcpxSession {
    pub(super) name: String,
    pub(super) source: &'static str,
}

#[derive(Debug, Clone)]
pub(in crate::dispatch_ops) struct AcpxEventSummary {
    pub events_file: PathBuf,
    pub mapped_events: usize,
    pub final_response: Option<String>,
}
