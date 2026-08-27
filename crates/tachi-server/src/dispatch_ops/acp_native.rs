use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::io::{BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout};

mod connection;
mod permission;
mod protocol;
mod runner;
mod session;
mod spec;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(super) use runner::run_native_acp_dispatch;
pub(super) use runner::{publish_native_acp_artifacts, run_native_acp_dispatch_with_liveness};
pub(super) use spec::{build_native_acp_run_spec, is_native_acp_transport};

const ACP_STREAM_FILE: &str = "acp.stream.ndjson";
const ACP_SESSION_SCHEMA: &str = "tachi.acp_session.v1";
const ACP_PROTOCOL_VERSION: u64 = 1;

#[derive(Debug, Clone)]
pub(super) struct NativeAcpRunSpec {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub prompt: String,
    pub mode: NativeAcpRunMode,
    pub permission_label: String,
    pub session: Option<NativeAcpSession>,
    pub session_record_path: Option<PathBuf>,
    pub session_distill_path: Option<PathBuf>,
    pub metadata: Value,
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum NativeAcpRunMode {
    OneShot,
    Session,
}

#[derive(Debug, Clone)]
pub(super) struct NativeAcpSession {
    pub name: String,
    pub source: &'static str,
}

#[derive(Debug, Clone)]
pub(in crate::dispatch_ops) struct NativeAcpPromptOutcome {
    output: String,
    session_id: String,
    agent_session_id: Option<String>,
    raw_messages: Vec<Value>,
    mapped_events: usize,
    prompt_result: Value,
    used_existing_session: bool,
    observed_model: Option<String>,
    staged_events: Vec<NativeAcpStagedEvent>,
}

#[derive(Debug, Clone)]
pub(in crate::dispatch_ops) struct NativeAcpStagedEvent {
    pub target: NativeAcpEventTarget,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy)]
pub(in crate::dispatch_ops) enum NativeAcpEventTarget {
    Progress,
    Trajectory,
}

#[derive(Debug)]
pub(in crate::dispatch_ops) struct NativeAcpDeferredArtifacts {
    pub spec: NativeAcpRunSpec,
    pub outcome: NativeAcpPromptOutcome,
    pub process_exit_code: Option<i32>,
}

struct NativeAcpConnection {
    stdin: Option<ChildStdin>,
    stdout_lines: Lines<BufReader<ChildStdout>>,
    raw_messages: Vec<Value>,
    final_text_parts: Vec<String>,
    mapped_events: usize,
    staged_events: Vec<NativeAcpStagedEvent>,
    observed_model: Option<String>,
    request_index: u64,
    permission_label: String,
    dispatch_id: String,
    agent: String,
    trajectory_path: PathBuf,
}
