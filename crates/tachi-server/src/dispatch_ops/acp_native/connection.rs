use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};

use super::super::dispatch_v2::append_trajectory_event;
use super::permission::{native_permission_decision, AcpPermissionDecision};
use super::protocol::{
    compact_json, ensure_session_id, extract_agent_session_id, extract_session_id,
    extract_text_recursive, extract_update_text, format_json_rpc_error, is_json_rpc_notification,
    is_json_rpc_request, is_session_update, response_id_matches,
};
use super::session::read_stored_acp_session_id;
use super::{
    NativeAcpConnection, NativeAcpPromptOutcome, NativeAcpRunMode, NativeAcpRunSpec,
    ACP_PROTOCOL_VERSION, ACP_STREAM_FILE,
};

impl NativeAcpConnection {
    pub(super) fn new(
        stdin: ChildStdin,
        stdout: ChildStdout,
        spec: &NativeAcpRunSpec,
        dispatch_id: &str,
        agent: &str,
        run_dir: &Path,
        trajectory_path: &Path,
    ) -> Self {
        Self {
            stdin: Some(stdin),
            stdout_lines: BufReader::new(stdout).lines(),
            raw_messages: Vec::new(),
            final_text_parts: Vec::new(),
            mapped_events: 0,
            request_index: 0,
            permission_label: spec.permission_label.clone(),
            dispatch_id: dispatch_id.to_string(),
            agent: agent.to_string(),
            run_dir: run_dir.to_path_buf(),
            trajectory_path: trajectory_path.to_path_buf(),
        }
    }

    pub(super) async fn run_prompt_turn(
        &mut self,
        spec: &NativeAcpRunSpec,
    ) -> Result<NativeAcpPromptOutcome, String> {
        let _initialize = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": ACP_PROTOCOL_VERSION,
                    "clientCapabilities": {
                        "fs": {
                            "readTextFile": false,
                            "writeTextFile": false,
                        },
                        "terminal": false,
                    },
                    "clientInfo": {
                        "name": "tachi",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                }),
            )
            .await?;

        let stored_session_id = if spec.mode == NativeAcpRunMode::Session {
            spec.session_record_path
                .as_ref()
                .and_then(|path| read_stored_acp_session_id(path))
        } else {
            None
        };

        let mut used_existing_session = false;
        let session_result = if let Some(session_id) = stored_session_id.as_deref() {
            match self.resume_or_load_session(session_id, &spec.cwd).await {
                Ok(result) => {
                    used_existing_session = true;
                    result
                }
                Err(err) => {
                    append_trajectory_event(
                        &self.trajectory_path,
                        json!({
                            "event": "acp_native_session_reconnect_failed",
                            "dispatch_id": self.dispatch_id,
                            "agent": self.agent,
                            "stored_session_id": session_id,
                            "error": err,
                            "fallback": "session/new",
                            "timestamp": Utc::now().to_rfc3339(),
                        }),
                    );
                    self.new_session(&spec.cwd).await?
                }
            }
        } else {
            self.new_session(&spec.cwd).await?
        };

        let session_id = extract_session_id(&session_result).ok_or_else(|| {
            format!(
                "Native ACP session response did not include sessionId: {}",
                compact_json(&session_result)
            )
        })?;
        let agent_session_id = extract_agent_session_id(&session_result);
        let prompt_result = self
            .request(
                "session/prompt",
                json!({
                    "sessionId": session_id,
                    "prompt": [
                        {
                            "type": "text",
                            "text": spec.prompt,
                        }
                    ],
                }),
            )
            .await?;

        let output = if self.final_text_parts.is_empty() {
            extract_text_recursive(&prompt_result).unwrap_or_default()
        } else {
            self.final_text_parts.join("")
        };

        Ok(NativeAcpPromptOutcome {
            output,
            session_id,
            agent_session_id,
            raw_messages: self.raw_messages.clone(),
            mapped_events: self.mapped_events,
            prompt_result,
            used_existing_session,
        })
    }

    async fn resume_or_load_session(
        &mut self,
        session_id: &str,
        cwd: &Path,
    ) -> Result<Value, String> {
        let params = json!({
            "sessionId": session_id,
            "cwd": cwd.to_string_lossy(),
            "mcpServers": [],
        });
        match self.request("session/resume", params.clone()).await {
            Ok(result) => Ok(ensure_session_id(result, session_id)),
            Err(resume_err) => match self.request("session/load", params).await {
                Ok(result) => Ok(ensure_session_id(result, session_id)),
                Err(load_err) => Err(format!(
                    "session/resume failed: {resume_err}; session/load failed: {load_err}"
                )),
            },
        }
    }

    async fn new_session(&mut self, cwd: &Path) -> Result<Value, String> {
        self.request(
            "session/new",
            json!({
                "cwd": cwd.to_string_lossy(),
                "mcpServers": [],
            }),
        )
        .await
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.request_index += 1;
        let id = format!("tachi-acp-{}", self.request_index);
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.send_message(request).await?;
        loop {
            let message = self.read_message().await?;
            if response_id_matches(&message, &id) {
                if let Some(error) = message.get("error") {
                    return Err(format!(
                        "ACP request '{method}' failed: {}",
                        format_json_rpc_error(error)
                    ));
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
            if is_json_rpc_request(&message) {
                self.handle_agent_request(message).await?;
            } else if is_session_update(&message) {
                self.map_session_update(&message);
            } else if is_json_rpc_notification(&message) {
                self.map_generic_notification(&message);
            }
        }
    }

    async fn send_message(&mut self, message: Value) -> Result<(), String> {
        let line = serde_json::to_string(&message)
            .map_err(|err| format!("serialize ACP message: {err}"))?;
        self.raw_messages.push(message);
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| "Native ACP adapter stdin is closed".to_string())?;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|err| format!("write ACP request: {err}"))?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(|err| format!("write ACP request newline: {err}"))?;
        stdin
            .flush()
            .await
            .map_err(|err| format!("flush ACP request: {err}"))
    }

    async fn read_message(&mut self) -> Result<Value, String> {
        loop {
            let line = self
                .stdout_lines
                .next_line()
                .await
                .map_err(|err| format!("read ACP response: {err}"))?
                .ok_or_else(|| "Native ACP adapter exited before responding".to_string())?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let message = serde_json::from_str::<Value>(trimmed)
                .map_err(|err| format!("parse ACP JSON-RPC line '{trimmed}': {err}"))?;
            self.raw_messages.push(message.clone());
            return Ok(message);
        }
    }

    async fn handle_agent_request(&mut self, message: Value) -> Result<(), String> {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let response = if method.contains("permission") {
            let decision = native_permission_decision(&self.permission_label, &params);
            self.append_permission_receipt(&decision);
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": decision.response,
            })
        } else {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("Tachi native ACP client does not implement '{method}'"),
                },
            })
        };
        self.send_message(response).await
    }

    /// Emit a fail-closed receipt for every ACP permission decision, naming the
    /// typed request kind and the verdict (#894 S0). A DENY additionally logs
    /// loudly so an unexpected auto-deny is visible in the daemon log.
    fn append_permission_receipt(&self, decision: &AcpPermissionDecision) {
        let verdict = if decision.allowed { "allow" } else { "deny" };
        let authorizer = if decision.heuristic {
            "legacy_heuristic"
        } else {
            "typed_taxonomy"
        };
        append_trajectory_event(
            &self.trajectory_path,
            json!({
                "event": "acp_native_permission_receipt",
                "dispatch_id": self.dispatch_id,
                "agent": self.agent,
                "permission_profile": self.permission_label,
                "request_kind": decision.kind.as_str(),
                "raw_tool_kind": decision.raw_kind,
                "verdict": verdict,
                "authorizer": authorizer,
                "timestamp": Utc::now().to_rfc3339(),
            }),
        );
        if !decision.allowed {
            tracing::warn!(
                target: "tachi::acp::permission",
                dispatch_id = %self.dispatch_id,
                agent = %self.agent,
                request_kind = decision.kind.as_str(),
                raw_tool_kind = %decision.raw_kind,
                authorizer,
                "ACP permission request DENIED under profile '{}' (request kind '{}')",
                self.permission_label,
                decision.kind.as_str(),
            );
        }
    }

    fn map_session_update(&mut self, message: &Value) {
        let Some(params) = message.get("params") else {
            return;
        };
        let update = params.get("update").unwrap_or(params);
        let kind = update
            .get("sessionUpdate")
            .or_else(|| update.get("type"))
            .or_else(|| update.get("kind"))
            .and_then(Value::as_str)
            .unwrap_or("session/update");
        let text = extract_update_text(update).or_else(|| extract_update_text(params));
        let lower = kind.to_ascii_lowercase();
        let event = if lower.contains("message") || text.is_some() {
            "acp_native_message"
        } else if lower.contains("tool") {
            "acp_native_tool_event"
        } else if lower.contains("permission") || lower.contains("approval") {
            "acp_native_permission_event"
        } else if lower.contains("diff") || lower.contains("edit") {
            "acp_native_diff_event"
        } else if lower.contains("error") || lower.contains("cancel") || lower.contains("status") {
            "acp_native_lifecycle_event"
        } else {
            "acp_native_event"
        };
        if let Some(text) = text.as_deref().filter(|text| !text.trim().is_empty()) {
            self.final_text_parts.push(text.to_string());
        }
        self.append_mapped_event(event, kind, text);
    }

    fn map_generic_notification(&mut self, message: &Value) {
        let kind = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("notification");
        let text = extract_text_recursive(message);
        self.append_mapped_event("acp_native_notification", kind, text);
    }

    fn append_mapped_event(&mut self, event: &str, kind: &str, text: Option<String>) {
        let mapped = json!({
            "event": event,
            "dispatch_id": self.dispatch_id,
            "agent": self.agent,
            "acp_event": kind,
            "text": text,
            "timestamp": Utc::now().to_rfc3339(),
        });
        let target = if event == "acp_native_message" {
            self.run_dir.join("progress.jsonl")
        } else {
            self.trajectory_path.clone()
        };
        append_trajectory_event(&target, mapped);
        self.mapped_events += 1;
    }

    pub(super) async fn close_stdin(&mut self) -> Result<(), String> {
        if let Some(mut stdin) = self.stdin.take() {
            stdin
                .shutdown()
                .await
                .map_err(|err| format!("shutdown ACP stdin: {err}"))?;
        }
        Ok(())
    }

    pub(super) fn persist_raw_stream(&self, run_dir: &Path) -> Result<PathBuf, String> {
        let stream_path = run_dir.join(ACP_STREAM_FILE);
        let mut lines = Vec::with_capacity(self.raw_messages.len());
        for message in &self.raw_messages {
            lines.push(
                serde_json::to_string(message)
                    .map_err(|err| format!("serialize ACP stream message: {err}"))?,
            );
        }
        let payload = if lines.is_empty() {
            String::new()
        } else {
            format!("{}\n", lines.join("\n"))
        };
        crate::utils::write_owner_only_file_atomic(&stream_path, payload.as_bytes())
            .map_err(|err| format!("write {ACP_STREAM_FILE}: {err}"))?;
        append_trajectory_event(
            &self.trajectory_path,
            json!({
                "event": "acp_native_stream_persisted",
                "dispatch_id": self.dispatch_id,
                "agent": self.agent,
                "raw_stream": stream_path.to_string_lossy(),
                "raw_message_count": self.raw_messages.len(),
                "mapped_events": self.mapped_events,
                "timestamp": Utc::now().to_rfc3339(),
            }),
        );
        Ok(stream_path)
    }
}
