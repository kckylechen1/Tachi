//! Codex `app-server` dialect: the locally installed codex binary's own
//! JSON-RPC session protocol (no `jsonrpc` field, thread/turn model).
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout};

use super::super::dispatch_v2::append_trajectory_event;
use super::connection::sanitize_receipt_field;
use super::supervised::{append_raw_event, SessionDriver};
use super::{NativeAcpEventTarget, NativeAcpPromptOutcome, NativeAcpRunSpec, NativeAcpStagedEvent};

pub(super) struct CodexAppServerConnection {
    stdin: Option<ChildStdin>,
    stdout_lines: Lines<BufReader<ChildStdout>>,
    raw_messages: Vec<Value>,
    staged_events: Vec<NativeAcpStagedEvent>,
    mapped_events: usize,
    request_index: u64,
    turn_number: u64,
    thread_id: Option<String>,
    active_turn: Option<String>,
    /// Evidence for the current thread, keyed by turn id, so ordering between a
    /// turn/start response and its notifications cannot misattribute output.
    turns: HashMap<String, TurnRecord>,
    observed_model: Option<String>,
    observed_effort: Option<String>,
    identity_fault: Option<String>,
    events_path: Option<PathBuf>,
    permission_label: String,
    defer_worker_text: bool,
    dispatch_id: String,
    agent: String,
    trajectory_path: PathBuf,
}

#[derive(Default)]
struct TurnRecord {
    final_answer: Option<String>,
    unphased: Vec<String>,
    completed: Option<Value>,
}

impl CodexAppServerConnection {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        stdin: ChildStdin,
        stdout: ChildStdout,
        spec: &NativeAcpRunSpec,
        dispatch_id: &str,
        agent: &str,
        run_dir: &Path,
        trajectory_path: &Path,
        defer_worker_text: bool,
    ) -> Self {
        Self {
            stdin: Some(stdin),
            stdout_lines: BufReader::new(stdout).lines(),
            raw_messages: Vec::new(),
            staged_events: Vec::new(),
            mapped_events: 0,
            request_index: 0,
            turn_number: 0,
            thread_id: None,
            active_turn: None,
            turns: HashMap::new(),
            observed_model: None,
            observed_effort: None,
            identity_fault: None,
            events_path: spec.managed.then(|| run_dir.join("events.jsonl")),
            permission_label: spec.permission_label.clone(),
            defer_worker_text,
            dispatch_id: dispatch_id.to_string(),
            agent: agent.to_string(),
            trajectory_path: trajectory_path.to_path_buf(),
        }
    }

    async fn send(&mut self, message: Value) -> Result<(), String> {
        let line = serde_json::to_string(&message)
            .map_err(|err| format!("serialize codex app-server message: {err}"))?;
        self.record(&message, "client")?;
        let stdin = self
            .stdin
            .as_mut()
            .ok_or("codex app-server stdin is closed")?;
        let write = |err: std::io::Error| format!("write codex app-server request: {err}");
        stdin.write_all(line.as_bytes()).await.map_err(write)?;
        stdin.write_all(b"\n").await.map_err(write)?;
        stdin.flush().await.map_err(write)
    }

    async fn read(&mut self) -> Result<Value, String> {
        loop {
            let line = self
                .stdout_lines
                .next_line()
                .await
                .map_err(|err| format!("read codex app-server message: {err}"))?
                .ok_or("native codex app-server exited before responding")?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let message = serde_json::from_str::<Value>(trimmed)
                .map_err(|err| format!("parse codex app-server message: {err}"))?;
            self.record(&message, "agent")?;
            return Ok(message);
        }
    }

    fn record(&mut self, message: &Value, direction: &str) -> Result<(), String> {
        if let Some(path) = &self.events_path {
            append_raw_event(path, direction, message)?;
        }
        self.raw_messages.push(message.clone());
        Ok(())
    }

    /// Sends a request and processes every interleaved message until its response.
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.request_index += 1;
        let id = format!("tachi-codex-{}", self.request_index);
        self.send(json!({"id": id, "method": method, "params": params}))
            .await?;
        loop {
            let message = self.read().await?;
            if message.get("method").is_none() && message["id"] == id.as_str() {
                if let Some(error) = message.get("error") {
                    return Err(format!(
                        "codex app-server request '{method}' failed: {}",
                        error["message"].as_str().unwrap_or("unknown error")
                    ));
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
            self.process(message).await?;
        }
    }

    /// Maps notifications, refuses server requests fail-closed, and records
    /// identity drift without failing mid-message.
    async fn process(&mut self, message: Value) -> Result<(), String> {
        let method = message["method"].as_str().unwrap_or("").to_string();
        if method.is_empty() {
            return Ok(());
        }
        if message.get("id").is_some() {
            return self.refuse_server_request(&message, &method).await;
        }
        let params = &message["params"];
        let own_thread =
            self.thread_id.is_some() && params["threadId"].as_str() == self.thread_id.as_deref();
        match method.as_str() {
            "model/rerouted" => {
                self.observed_model = params["toModel"].as_str().map(str::to_string);
                self.identity_fault = Some(format!(
                    "native model identity mismatch: adapter rerouted model {} to {}; no automatic retry",
                    params["fromModel"].as_str().unwrap_or("?"),
                    params["toModel"].as_str().unwrap_or("?"),
                ));
            }
            "item/agentMessage/delta" => {
                if let Some(delta) = params["delta"].as_str() {
                    self.map_event("acp_native_message", &method, Some(delta.to_string()));
                }
            }
            "item/completed" => {
                let item = &params["item"];
                if item["type"] == "agentMessage" {
                    if let (true, Some(turn), Some(text)) =
                        (own_thread, params["turnId"].as_str(), item["text"].as_str())
                    {
                        let record = self.turns.entry(turn.to_string()).or_default();
                        match item["phase"].as_str() {
                            Some("final_answer") => record.final_answer = Some(text.to_string()),
                            None => record.unphased.push(text.to_string()),
                            Some(_) => {}
                        }
                    }
                } else {
                    let kind = item["type"].as_str().unwrap_or("item");
                    self.map_event("acp_native_tool_event", kind, None);
                }
            }
            "turn/completed" => {
                if let (true, Some(turn)) = (own_thread, params["turn"]["id"].as_str()) {
                    self.turns.entry(turn.to_string()).or_default().completed =
                        Some(params["turn"].clone());
                }
            }
            "error" => {
                let text = params["error"]["message"].as_str().map(str::to_string);
                self.map_event("acp_native_lifecycle_event", &method, text);
            }
            _ => {}
        }
        Ok(())
    }

    async fn refuse_server_request(&mut self, message: &Value, method: &str) -> Result<(), String> {
        let decision = match method {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                Some("decline")
            }
            "execCommandApproval" | "applyPatchApproval" => Some("denied"),
            _ => None,
        };
        let response = match decision {
            Some(decision) => json!({"id": message["id"], "result": {"decision": decision}}),
            None => json!({"id": message["id"], "error": {
                "code": -32601,
                "message": format!("Tachi native codex client does not implement '{method}'"),
            }}),
        };
        let raw_kind = if self.defer_worker_text {
            "[withheld pending exec_env_postflight]".to_string()
        } else {
            sanitize_receipt_field(method)
        };
        let receipt = json!({
            "event": "acp_native_permission_receipt",
            "dispatch_id": self.dispatch_id,
            "agent": self.agent,
            "permission_profile": self.permission_label,
            "request_kind": "codex_server_request",
            "raw_tool_kind": raw_kind,
            "verdict": "deny",
            "authorizer": "codex_app_server_fail_closed",
            "timestamp": Utc::now().to_rfc3339(),
        });
        if self.defer_worker_text {
            self.staged_events.push(NativeAcpStagedEvent {
                target: NativeAcpEventTarget::PostflightReceipt,
                payload: receipt,
            });
        } else {
            append_trajectory_event(&self.trajectory_path, receipt);
        }
        tracing::warn!(
            target: "tachi::acp::permission",
            dispatch_id = %self.dispatch_id,
            request = %raw_kind,
            "codex app-server request DENIED under profile '{}'",
            self.permission_label,
        );
        self.send(response).await
    }

    fn map_event(&mut self, event: &str, kind: &str, text: Option<String>) {
        let target = if event == "acp_native_message" {
            NativeAcpEventTarget::Progress
        } else {
            NativeAcpEventTarget::Trajectory
        };
        self.staged_events.push(NativeAcpStagedEvent {
            target,
            payload: json!({
                "event": event,
                "dispatch_id": self.dispatch_id,
                "agent": self.agent,
                "acp_event": kind,
                "text": text,
                "timestamp": Utc::now().to_rfc3339(),
            }),
        });
        self.mapped_events += 1;
    }

    async fn start_thread(&mut self, spec: &NativeAcpRunSpec) -> Result<(), String> {
        let contract = &spec.metadata["profile_contract"];
        let model = contract["model"]
            .as_str()
            .ok_or("native codex profile has no pinned model")?;
        self.request(
            "initialize",
            json!({"clientInfo": {"name": "tachi", "version": env!("CARGO_PKG_VERSION")}}),
        )
        .await?;
        self.send(json!({"method": "initialized"})).await?;
        let roster = self
            .request("model/list", json!({"includeHidden": true}))
            .await?;
        let offered = roster["data"]
            .as_array()
            .is_some_and(|models| models.iter().any(|entry| entry["id"] == model));
        if !offered {
            return Err(format!(
                "native adapter does not offer requested model={model}; no fallback"
            ));
        }
        let mut config = json!({"mcp_servers": codex_mcp_servers(&spec.mcp_servers)?});
        if let Some(effort) = contract["effort"].as_str() {
            config["model_reasoning_effort"] = json!(effort);
        }
        let started = self
            .request(
                "thread/start",
                json!({
                    "model": model,
                    "cwd": spec.cwd.to_string_lossy(),
                    // The server filesystem boundary is the sole enforcer; codex's
                    // own sandbox cannot nest inside it and would add approvals.
                    "sandbox": "danger-full-access",
                    "approvalPolicy": "never",
                    "ephemeral": true,
                    "config": config,
                }),
            )
            .await?;
        self.observed_model = started["model"].as_str().map(str::to_string);
        self.observed_effort = started["reasoningEffort"].as_str().map(str::to_string);
        for (field, observed) in [
            ("model", &self.observed_model),
            ("effort", &self.observed_effort),
        ] {
            if let Some(requested) = contract[field].as_str() {
                if observed.as_deref() != Some(requested) {
                    return Err(format!(
                        "native adapter did not acknowledge {field}={requested} (observed {observed:?}); no prompt sent"
                    ));
                }
            }
        }
        let thread = started["thread"]["id"]
            .as_str()
            .ok_or("codex app-server thread/start returned no thread id")?;
        self.thread_id = Some(thread.to_string());
        crate::staffing_ops::presence::worker_event("ready", "acp_session_initialized");
        Ok(())
    }
}

/// ACP-shaped MCP descriptors become a run-private codex `mcp_servers` override.
fn codex_mcp_servers(servers: &[Value]) -> Result<Value, String> {
    let mut mapped = serde_json::Map::new();
    for server in servers {
        let name = server["name"]
            .as_str()
            .ok_or("native MCP descriptor has no name")?;
        if server["type"] != "http" {
            return Err(format!(
                "native codex profile cannot inject non-HTTP MCP server '{name}'"
            ));
        }
        let headers = server["headers"]
            .as_array()
            .map(|headers| {
                headers
                    .iter()
                    .filter_map(|header| {
                        Some((
                            header["name"].as_str()?.to_string(),
                            header["value"].clone(),
                        ))
                    })
                    .collect::<serde_json::Map<_, _>>()
            })
            .unwrap_or_default();
        mapped.insert(
            name.to_string(),
            json!({"url": server["url"], "http_headers": headers}),
        );
    }
    Ok(Value::Object(mapped))
}

impl SessionDriver for CodexAppServerConnection {
    type IdleMessage = Value;

    fn dispatch_id(&self) -> &str {
        &self.dispatch_id
    }

    fn session_id(&self) -> Option<&str> {
        self.thread_id.as_deref()
    }

    fn observed(&self) -> Value {
        json!({"model": self.observed_model, "effort": self.observed_effort})
    }

    fn turn_number(&self) -> u64 {
        self.turn_number
    }

    fn set_turn_number(&mut self, turn: u64) {
        self.turn_number = turn;
    }

    fn check_profile(&self, spec: &NativeAcpRunSpec) -> Result<(), String> {
        if let Some(fault) = &self.identity_fault {
            return Err(fault.clone());
        }
        let contract = &spec.metadata["profile_contract"];
        for (field, observed) in [
            ("model", &self.observed_model),
            ("effort", &self.observed_effort),
        ] {
            if let Some(requested) = contract[field].as_str() {
                if observed.as_deref() != Some(requested) {
                    return Err(format!("native model identity mismatch: requested {field}={requested}, observed={observed:?}; no automatic retry"));
                }
            }
        }
        Ok(())
    }

    fn record_raw(&self, direction: &str, message: &Value) -> Result<(), String> {
        match &self.events_path {
            Some(path) => append_raw_event(path, direction, message),
            None => Ok(()),
        }
    }

    async fn run_turn(
        &mut self,
        spec: &NativeAcpRunSpec,
    ) -> Result<NativeAcpPromptOutcome, String> {
        if self.thread_id.is_none() {
            self.start_thread(spec).await?;
        }
        self.check_profile(spec)?;
        let thread = self.thread_id.clone().expect("thread started above");
        self.turns.clear();
        crate::staffing_ops::presence::worker_event("busy", "acp_prompt_sent");
        let started = self
            .request(
                "turn/start",
                json!({"threadId": thread, "input": [{"type": "text", "text": spec.prompt}]}),
            )
            .await?;
        let turn_id = started["turn"]["id"]
            .as_str()
            .ok_or("codex app-server turn/start returned no turn id")?
            .to_string();
        self.active_turn = Some(turn_id.clone());
        let record = loop {
            // Identity evidence gates every exit, including a completion that
            // arrived before the turn/start response.
            self.check_profile(spec)?;
            if self
                .turns
                .get(&turn_id)
                .is_some_and(|record| record.completed.is_some())
            {
                break self.turns.remove(&turn_id).expect("completed turn present");
            }
            let message = self.read().await?;
            self.process(message).await?;
        };
        self.active_turn = None;
        let turn = record.completed.clone().expect("completed turn");
        let status = turn["status"].as_str().unwrap_or("unknown");
        if status != "completed" {
            return Err(format!(
                "native turn stopped with {status}; no automatic retry"
            ));
        }
        // Commentary is progress, never the result; unphased text is the legacy shape.
        let output = record
            .final_answer
            .unwrap_or_else(|| record.unphased.join("\n"));
        if output.trim().is_empty() {
            return Err(
                "native terminal turn has no result text; execution is not accepted".into(),
            );
        }
        Ok(NativeAcpPromptOutcome {
            output,
            session_id: thread,
            agent_session_id: None,
            raw_messages: self.raw_messages.clone(),
            mapped_events: self.mapped_events,
            prompt_result: json!({"stopReason": status, "turnId": turn["id"]}),
            used_existing_session: false,
            observed_model: self.observed_model.clone(),
            staged_events: self.staged_events.clone(),
        })
    }

    async fn next_idle_message(&mut self) -> Result<Value, String> {
        self.read().await
    }

    async fn handle_idle_message(
        &mut self,
        message: Value,
        spec: &NativeAcpRunSpec,
    ) -> Result<(), String> {
        self.process(message).await?;
        self.check_profile(spec)
    }

    async fn cancel_session(&mut self) -> Result<(), String> {
        if let (Some(thread), Some(turn)) = (self.thread_id.clone(), self.active_turn.clone()) {
            self.request_index += 1;
            let id = format!("tachi-codex-{}", self.request_index);
            self.send(json!({"id": id, "method": "turn/interrupt", "params": {"threadId": thread, "turnId": turn}}))
                .await?;
        }
        Ok(())
    }

    async fn close_stdin(&mut self) -> Result<(), String> {
        if let Some(mut stdin) = self.stdin.take() {
            stdin
                .shutdown()
                .await
                .map_err(|err| format!("shutdown codex app-server stdin: {err}"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
