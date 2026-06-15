use super::*;

use super::dispatch::DispatchResult;
use super::dispatch_v2::append_trajectory_event;
use super::subprocess::resolve_permission_profile;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout};

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

#[derive(Debug)]
struct NativeAcpPromptOutcome {
    output: String,
    session_id: String,
    agent_session_id: Option<String>,
    raw_messages: Vec<Value>,
    mapped_events: usize,
    prompt_result: Value,
    used_existing_session: bool,
}

struct NativeAcpConnection {
    stdin: Option<ChildStdin>,
    stdout_lines: Lines<BufReader<ChildStdout>>,
    raw_messages: Vec<Value>,
    final_text_parts: Vec<String>,
    mapped_events: usize,
    request_index: u64,
    permission_label: String,
    dispatch_id: String,
    agent: String,
    run_dir: PathBuf,
    trajectory_path: PathBuf,
}

pub(super) fn is_native_acp_transport(transport: &str) -> bool {
    matches!(
        transport.trim().to_ascii_lowercase().as_str(),
        "acp-native" | "acp_native" | "native-acp" | "native_acp" | "acp-rs" | "acp_rs"
    )
}

pub(super) fn build_native_acp_run_spec(
    params: &TachiDispatchParams,
    agent: &str,
    prompt: &str,
) -> Result<NativeAcpRunSpec, String> {
    let (command, args, command_source) = resolve_native_acp_command(params, agent)?;
    if !crate::utils::is_trusted_command(&command) {
        return Err(format!(
            "native ACP command '{}' is not trusted. Use agent='custom' with a trusted command, or set TACHI_ACP_NATIVE_COMMAND to a trusted binary.",
            command
        ));
    }
    if !command_available(&command) {
        return Err(format!(
            "native ACP transport requested, but command '{}' was not found on PATH",
            command
        ));
    }

    let cwd = params
        .cwd
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let cwd = absolutize_cwd(&cwd);
    let mode = resolve_native_acp_run_mode()?;
    let permission_label = native_acp_permission_label(resolve_permission_profile(params)?)?;
    let session = if mode == NativeAcpRunMode::Session {
        Some(resolve_native_acp_session(params)?)
    } else {
        None
    };
    let session_key = native_acp_session_key(agent, &command, &args, &cwd, session.as_ref());
    let (session_record_path, session_distill_path) = if mode == NativeAcpRunMode::Session {
        let record_id = crate::utils::stable_hash(
            &serde_json::to_string(&session_key)
                .map_err(|err| format!("serialize native ACP session key: {err}"))?,
        );
        let base = crate::path_utils::tachi_home().join("sessions").join("acp");
        (
            Some(base.join(format!("{record_id}.json"))),
            Some(base.join(format!("{record_id}.md"))),
        )
    } else {
        (None, None)
    };

    let session_name = session.as_ref().map(|session| session.name.clone());
    let session_source = session.as_ref().map(|session| session.source);
    let metadata = json!({
        "execution_backend": "acp_native",
        "agent": agent,
        "command": command,
        "args": args,
        "command_source": command_source,
        "mode": mode.as_str(),
        "session": session_name,
        "session_source": session_source,
        "session_key": session_key,
        "session_record": session_record_path.as_ref().map(|path| path.to_string_lossy().to_string()),
        "session_distill": session_distill_path.as_ref().map(|path| path.to_string_lossy().to_string()),
        "cwd": cwd.to_string_lossy(),
        "permissions": permission_label,
        "client_capabilities": {
            "fs": {
                "readTextFile": false,
                "writeTextFile": false
            },
            "terminal": false
        },
        "raw_stream_file": ACP_STREAM_FILE,
    });

    Ok(NativeAcpRunSpec {
        command,
        args,
        cwd,
        prompt: prompt.to_string(),
        mode,
        permission_label: permission_label.to_string(),
        session,
        session_record_path,
        session_distill_path,
        metadata,
        env: HashMap::new(),
    })
}

pub(super) async fn run_native_acp_dispatch(
    spec: NativeAcpRunSpec,
    run_dir: &Path,
    trajectory_path: &Path,
    dispatch_id: &str,
    agent: &str,
    timeout: Duration,
) -> Result<DispatchResult, String> {
    match tokio::time::timeout(
        timeout,
        run_native_acp_dispatch_inner(spec, run_dir, trajectory_path, dispatch_id, agent),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(format!(
            "Native ACP dispatch timed out after {}s (adapter killed)",
            timeout.as_secs()
        )),
    }
}

async fn run_native_acp_dispatch_inner(
    spec: NativeAcpRunSpec,
    run_dir: &Path,
    trajectory_path: &Path,
    dispatch_id: &str,
    agent: &str,
) -> Result<DispatchResult, String> {
    let mut cmd = Command::new(&spec.command);
    cmd.args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for (name, value) in &spec.env {
        cmd.env(name, value);
    }

    let mut child = cmd
        .spawn()
        .map_err(|err| format!("Failed to spawn native ACP adapter: {err}"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "Native ACP adapter stdin was not piped".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Native ACP adapter stdout was not piped".to_string())?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Native ACP adapter stderr was not piped".to_string())?;
    let stderr_task = tokio::spawn(async move {
        let mut captured = String::new();
        let _ = stderr.read_to_string(&mut captured).await;
        captured
    });

    let mut connection = NativeAcpConnection::new(
        stdin,
        stdout,
        &spec,
        dispatch_id,
        agent,
        run_dir,
        trajectory_path,
    );
    let outcome = connection.run_prompt_turn(&spec).await;
    let close_result = connection.close_stdin().await;
    let stream_result = connection.persist_raw_stream(run_dir);

    let graceful_exit = tokio::time::timeout(Duration::from_millis(1500), child.wait()).await;
    let process_exit_code = match graceful_exit {
        Ok(Ok(status)) => status.code(),
        Ok(Err(err)) => {
            append_trajectory_event(
                trajectory_path,
                json!({
                    "event": "acp_native_process_wait_failed",
                    "dispatch_id": dispatch_id,
                    "agent": agent,
                    "error": err.to_string(),
                    "timestamp": Utc::now().to_rfc3339(),
                }),
            );
            None
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            append_trajectory_event(
                trajectory_path,
                json!({
                    "event": "acp_native_process_killed_after_turn",
                    "dispatch_id": dispatch_id,
                    "agent": agent,
                    "reason": "adapter did not exit after stdin close",
                    "timestamp": Utc::now().to_rfc3339(),
                }),
            );
            None
        }
    };
    let stderr_output = stderr_task.await.unwrap_or_default();

    if let Err(err) = close_result {
        append_trajectory_event(
            trajectory_path,
            json!({
                "event": "acp_native_stdin_close_failed",
                "dispatch_id": dispatch_id,
                "agent": agent,
                "error": err,
                "timestamp": Utc::now().to_rfc3339(),
            }),
        );
    }
    let stream_path = match stream_result {
        Ok(path) => path,
        Err(err) => {
            append_trajectory_event(
                trajectory_path,
                json!({
                    "event": "acp_native_stream_persist_failed",
                    "dispatch_id": dispatch_id,
                    "agent": agent,
                    "error": err,
                    "timestamp": Utc::now().to_rfc3339(),
                }),
            );
            run_dir.join(ACP_STREAM_FILE)
        }
    };

    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(err) => {
            let stderr_tail = super::subprocess::tail_chars(&stderr_output, 4000);
            let suffix = if stderr_tail.is_empty() {
                String::new()
            } else {
                format!("; adapter stderr: {stderr_tail}")
            };
            return Err(format!("{err}{suffix}"));
        }
    };

    if let Some(record_path) = spec.session_record_path.as_ref() {
        write_native_acp_session_record(
            &spec,
            record_path,
            spec.session_distill_path.as_ref(),
            &outcome,
            &stream_path,
            dispatch_id,
        )?;
    }

    append_trajectory_event(
        trajectory_path,
        json!({
            "event": "acp_native_turn_finished",
            "dispatch_id": dispatch_id,
            "agent": agent,
            "session_id": outcome.session_id,
            "agent_session_id": outcome.agent_session_id,
            "used_existing_session": outcome.used_existing_session,
            "mapped_events": outcome.mapped_events,
            "raw_stream": stream_path.to_string_lossy(),
            "process_exit_code": process_exit_code,
            "timestamp": Utc::now().to_rfc3339(),
        }),
    );

    Ok(DispatchResult {
        output: outcome.output,
        exit_code: Some(0),
    })
}

impl NativeAcpRunMode {
    fn as_str(self) -> &'static str {
        match self {
            NativeAcpRunMode::OneShot => "oneshot",
            NativeAcpRunMode::Session => "session",
        }
    }
}

impl NativeAcpConnection {
    fn new(
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

    async fn run_prompt_turn(
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
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": native_permission_response(&self.permission_label, &params),
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

    async fn close_stdin(&mut self) -> Result<(), String> {
        if let Some(mut stdin) = self.stdin.take() {
            stdin
                .shutdown()
                .await
                .map_err(|err| format!("shutdown ACP stdin: {err}"))?;
        }
        Ok(())
    }

    fn persist_raw_stream(&self, run_dir: &Path) -> Result<PathBuf, String> {
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

fn resolve_native_acp_command(
    params: &TachiDispatchParams,
    agent: &str,
) -> Result<(String, Vec<String>, &'static str), String> {
    if agent == "custom" && !params.command.is_empty() {
        let command = params.command[0].trim().to_string();
        let args = params.command[1..].to_vec();
        if command.is_empty() {
            return Err("agent='custom' native ACP command cannot be empty".to_string());
        }
        return Ok((command, args, "dispatch.command"));
    }

    let command = std::env::var("TACHI_ACP_NATIVE_COMMAND")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            "harness_transport='acp-native' requires TACHI_ACP_NATIVE_COMMAND, or agent='custom' with a command array naming an ACP stdio adapter"
                .to_string()
        })?;
    Ok((command, env_args("TACHI_ACP_NATIVE_ARGS"), "env"))
}

fn resolve_native_acp_run_mode() -> Result<NativeAcpRunMode, String> {
    let raw = std::env::var("TACHI_ACP_NATIVE_RUN_MODE")
        .ok()
        .or_else(|| std::env::var("TACHI_ACP_RUN_MODE").ok())
        .unwrap_or_else(|| "session".to_string());
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "session" | "persistent" | "named" => Ok(NativeAcpRunMode::Session),
        "exec" | "oneshot" | "one-shot" | "one_shot" => Ok(NativeAcpRunMode::OneShot),
        other => Err(format!(
            "unsupported native ACP run mode '{other}'. Use TACHI_ACP_NATIVE_RUN_MODE=session or oneshot."
        )),
    }
}

fn native_acp_permission_label(profile: &str) -> Result<&'static str, String> {
    match profile {
        "default" => Ok("approve-reads"),
        "allowlist" => Err(
            "permission_profile 'allowlist' is not supported by native ACP yet; use default read-approved posture."
                .to_string(),
        ),
        "full" => Err(
            "permission_profile 'full' is not supported by native ACP; Tachi does not map ACP to approve-all yet."
                .to_string(),
        ),
        other => Err(format!(
            "unsupported permission_profile '{other}' for native ACP backend"
        )),
    }
}

fn resolve_native_acp_session(params: &TachiDispatchParams) -> Result<NativeAcpSession, String> {
    if let Ok(explicit) = std::env::var("TACHI_ACP_NATIVE_SESSION") {
        let explicit = explicit.trim();
        if !explicit.is_empty() {
            return Ok(NativeAcpSession {
                name: validate_session_name(explicit)?,
                source: "env:TACHI_ACP_NATIVE_SESSION",
            });
        }
    }
    if let Ok(explicit) = std::env::var("TACHI_ACP_SESSION") {
        let explicit = explicit.trim();
        if !explicit.is_empty() {
            return Ok(NativeAcpSession {
                name: validate_session_name(explicit)?,
                source: "env:TACHI_ACP_SESSION",
            });
        }
    }

    for (value, source) in [
        (params.profile.as_deref(), "dispatch_profile"),
        (params.stage.as_deref(), "stage"),
    ] {
        if let Some(session) = value.and_then(derive_session_from_card_hint) {
            return Ok(NativeAcpSession {
                name: session,
                source,
            });
        }
    }

    Ok(NativeAcpSession {
        name: "scv".to_string(),
        source: "default_builder",
    })
}

fn derive_session_from_card_hint(value: &str) -> Option<String> {
    let lower = value.trim().to_ascii_lowercase();
    if lower.is_empty() {
        None
    } else if lower.contains("poke") || lower.contains("probe") || lower.contains("explore") {
        Some("poke".to_string())
    } else if lower.contains("raven")
        || lower.contains("review")
        || lower.contains("verify")
        || lower.contains("verifier")
    {
        Some("raven".to_string())
    } else if lower.contains("medic") || lower.contains("hotfix") {
        Some("scv-medic".to_string())
    } else if lower.contains("scv")
        || lower.contains("impl")
        || lower.contains("execute")
        || lower.contains("builder")
    {
        Some("scv".to_string())
    } else {
        None
    }
}

fn validate_session_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("native ACP session name cannot be empty".to_string());
    }
    if trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        Ok(trimmed.to_string())
    } else {
        Err(format!(
            "invalid native ACP session name '{trimmed}'. Use only ASCII letters, numbers, '-', '_', or '.'."
        ))
    }
}

fn native_acp_session_key(
    agent: &str,
    command: &str,
    args: &[String],
    cwd: &Path,
    session: Option<&NativeAcpSession>,
) -> Value {
    json!({
        "agent": agent,
        "command": command,
        "args": args,
        "cwd": cwd.to_string_lossy(),
        "name": session.map(|session| session.name.clone()),
    })
}

fn read_stored_acp_session_id(record_path: &Path) -> Option<String> {
    let record = crate::task_lifecycle::read_json_file(record_path)
        .ok()
        .flatten()?;
    if record
        .get("closed")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    record
        .get("acp_session_id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn write_native_acp_session_record(
    spec: &NativeAcpRunSpec,
    record_path: &Path,
    distill_path: Option<&PathBuf>,
    outcome: &NativeAcpPromptOutcome,
    stream_path: &Path,
    dispatch_id: &str,
) -> Result<(), String> {
    if let Some(parent) = record_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("create native ACP session dir: {err}"))?;
    }
    let existing = crate::task_lifecycle::read_json_file(record_path)
        .ok()
        .flatten();
    let now = Utc::now().to_rfc3339();
    let created_at = existing
        .as_ref()
        .and_then(|value| value.get("created_at"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| now.clone());
    let session_key = native_acp_session_key(
        spec.metadata
            .get("agent")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        &spec.command,
        &spec.args,
        &spec.cwd,
        spec.session.as_ref(),
    );
    let record = json!({
        "schema": ACP_SESSION_SCHEMA,
        "session_key": session_key,
        "acp_session_id": outcome.session_id,
        "agent_session_id": outcome.agent_session_id,
        "created_at": created_at,
        "last_used_at": now,
        "last_dispatch_id": dispatch_id,
        "closed": false,
        "event_log": {
            "latest_run_stream": stream_path.to_string_lossy(),
            "raw_message_count": outcome.raw_messages.len(),
        },
        "last_prompt_result": outcome.prompt_result,
        "last_output_preview": super::subprocess::tail_chars(&outcome.output, 2000),
        "distill_path": distill_path.map(|path| path.to_string_lossy().to_string()),
    });
    let body = serde_json::to_vec_pretty(&record)
        .map_err(|err| format!("serialize native ACP session record: {err}"))?;
    crate::utils::write_owner_only_file_atomic(record_path, &body)
        .map_err(|err| format!("write native ACP session record: {err}"))?;

    if let Some(distill_path) = distill_path {
        let distill = format!(
            "# Tachi ACP Session\n\n- schema: {ACP_SESSION_SCHEMA}\n- name: {}\n- acp_session_id: {}\n- agent_session_id: {}\n- command: {} {}\n- cwd: {}\n- last_dispatch_id: {}\n- latest_run_stream: {}\n\n## Last Output\n\n{}\n",
            spec.session
                .as_ref()
                .map(|session| session.name.as_str())
                .unwrap_or("oneshot"),
            outcome.session_id,
            outcome.agent_session_id.as_deref().unwrap_or("none"),
            spec.command,
            spec.args.join(" "),
            spec.cwd.to_string_lossy(),
            dispatch_id,
            stream_path.to_string_lossy(),
            outcome.output.trim(),
        );
        crate::utils::write_owner_only_file_atomic(distill_path, distill.as_bytes())
            .map_err(|err| format!("write native ACP session distill: {err}"))?;
    }
    Ok(())
}

fn native_permission_response(permission_label: &str, params: &Value) -> Value {
    if permission_label == "approve-reads" && permission_request_is_read_like(params) {
        if let Some(option_id) = select_permission_option(params, true) {
            return json!({
                "outcome": {
                    "outcome": "selected",
                    "optionId": option_id,
                }
            });
        }
    }
    if let Some(option_id) = select_permission_option(params, false) {
        return json!({
            "outcome": {
                "outcome": "selected",
                "optionId": option_id,
            }
        });
    }
    json!({
        "outcome": {
            "outcome": "cancelled",
        }
    })
}

fn permission_request_is_read_like(params: &Value) -> bool {
    let haystack = serde_json::to_string(params)
        .unwrap_or_default()
        .to_ascii_lowercase();
    ["read", "search", "grep", "list", "view", "find"]
        .iter()
        .any(|needle| haystack.contains(needle))
        && ![
            "write", "edit", "delete", "remove", "terminal", "shell", "exec", "create",
        ]
        .iter()
        .any(|needle| haystack.contains(needle))
}

fn select_permission_option(params: &Value, approve: bool) -> Option<String> {
    let options = params.get("options").and_then(Value::as_array)?;
    let mut fallback = None;
    for option in options {
        let id = option
            .get("optionId")
            .or_else(|| option.get("id"))
            .or_else(|| option.get("name"))
            .and_then(Value::as_str)?;
        let label = serde_json::to_string(option)
            .unwrap_or_default()
            .to_ascii_lowercase();
        if fallback.is_none() {
            fallback = Some(id.to_string());
        }
        let selected = if approve {
            ["approve", "allow", "accept", "yes", "read"]
                .iter()
                .any(|needle| label.contains(needle))
                && !["deny", "reject", "cancel", "no"]
                    .iter()
                    .any(|needle| label.contains(needle))
        } else {
            ["deny", "reject", "cancel", "no"]
                .iter()
                .any(|needle| label.contains(needle))
        };
        if selected {
            return Some(id.to_string());
        }
    }
    if approve {
        fallback
    } else {
        None
    }
}

fn is_json_rpc_request(message: &Value) -> bool {
    message.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
        && message.get("method").and_then(Value::as_str).is_some()
        && message.get("id").is_some()
}

fn is_json_rpc_notification(message: &Value) -> bool {
    message.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
        && message.get("method").and_then(Value::as_str).is_some()
        && message.get("id").is_none()
}

fn is_session_update(message: &Value) -> bool {
    message.get("method").and_then(Value::as_str) == Some("session/update")
}

fn response_id_matches(message: &Value, expected: &str) -> bool {
    message.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
        && message.get("method").is_none()
        && message.get("id").and_then(Value::as_str) == Some(expected)
        && (message.get("result").is_some() || message.get("error").is_some())
}

fn extract_session_id(result: &Value) -> Option<String> {
    result
        .get("sessionId")
        .or_else(|| result.get("session_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn ensure_session_id(mut result: Value, session_id: &str) -> Value {
    if extract_session_id(&result).is_some() {
        return result;
    }
    if let Some(object) = result.as_object_mut() {
        object.insert(
            "sessionId".to_string(),
            Value::String(session_id.to_string()),
        );
    } else {
        result = json!({ "sessionId": session_id });
    }
    result
}

fn extract_agent_session_id(result: &Value) -> Option<String> {
    let meta = result.get("_meta")?;
    [
        "agentSessionId",
        "agent_session_id",
        "runtimeSessionId",
        "sessionId",
    ]
    .iter()
    .find_map(|key| meta.get(*key).and_then(Value::as_str).map(str::to_string))
}

fn extract_update_text(value: &Value) -> Option<String> {
    if let Some(content) = value.get("content") {
        if let Some(text) = content.get("text").and_then(Value::as_str) {
            return Some(text.to_string());
        }
        if let Some(items) = content.as_array() {
            let text = items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<String>();
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
    }
    extract_text_recursive(value)
}

fn extract_text_recursive(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.trim().is_empty() => Some(text.to_string()),
        Value::Array(items) => {
            let text = items
                .iter()
                .filter_map(extract_text_recursive)
                .collect::<String>();
            if text.trim().is_empty() {
                None
            } else {
                Some(text)
            }
        }
        Value::Object(object) => {
            for key in ["final_response", "message", "content", "text", "output"] {
                if let Some(found) = object.get(key).and_then(extract_text_recursive) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

fn format_json_rpc_error(error: &Value) -> String {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown JSON-RPC error");
    let code = error.get("code").and_then(Value::as_i64);
    match code {
        Some(code) => format!("{message} (code {code})"),
        None => message.to_string(),
    }
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unserializable>".to_string())
}

fn env_args(name: &str) -> Vec<String> {
    std::env::var(name)
        .ok()
        .map(|value| {
            value
                .split_whitespace()
                .filter(|arg| !arg.trim().is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn command_available(command: &str) -> bool {
    let path = Path::new(command);
    if path.components().count() > 1 {
        return path.exists();
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(command).is_file())
}

fn absolutize_cwd(cwd: &Path) -> PathBuf {
    if cwd.is_absolute() {
        return cwd.to_path_buf();
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(cwd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_permission_approves_read_like_request() {
        let response = native_permission_response(
            "approve-reads",
            &json!({
                "toolCall": {
                    "kind": "read",
                    "title": "Read src/lib.rs"
                },
                "options": [
                    {"optionId": "deny", "name": "Deny"},
                    {"optionId": "allow", "name": "Allow"}
                ]
            }),
        );

        assert_eq!(response["outcome"]["outcome"], json!("selected"));
        assert_eq!(response["outcome"]["optionId"], json!("allow"));
    }

    #[test]
    fn native_permission_denies_write_like_request() {
        let response = native_permission_response(
            "approve-reads",
            &json!({
                "toolCall": {
                    "kind": "edit",
                    "title": "Write src/lib.rs"
                },
                "options": [
                    {"optionId": "allow", "name": "Allow"},
                    {"optionId": "deny", "name": "Deny"}
                ]
            }),
        );

        assert_eq!(response["outcome"]["outcome"], json!("selected"));
        assert_eq!(response["outcome"]["optionId"], json!("deny"));
    }
}
