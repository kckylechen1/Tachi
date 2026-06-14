//! `tachi_arena` - tracked worker-mission document ledger.
//!
//! Arena deliberately separates run documents from memory. Prompt, plan,
//! result, status, stdout, and stderr live under `.tachi/arena/<arena_id>/`;
//! Tachi memory/wiki should only receive distilled conclusions after close.

use crate::{MemoryServer, TachiArenaParams, TachiDispatchParams};
use chrono::Utc;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
struct HarnessLane {
    id: &'static str,
    label: &'static str,
    kind: &'static str,
    launch_mode: &'static str,
    command_hint: &'static str,
    mcp_support: &'static str,
    artifact_contract: &'static str,
    notes: &'static [&'static str],
}

fn harness_lane(requested: Option<&str>) -> HarnessLane {
    let normalized = requested
        .unwrap_or("manual")
        .trim()
        .to_ascii_lowercase()
        .replace('_', "-");
    match normalized.as_str() {
        "" | "manual" | "tracked-document" | "document" => HarnessLane {
            id: "manual",
            label: "Manual tracked document",
            kind: "manual",
            launch_mode: "tracked_document",
            command_hint: "Give tracked_prompt to any worker and require plan.md/result.md writes.",
            mcp_support: "external",
            artifact_contract: "Worker writes plan.md and result.md in the mission directory.",
            notes: &[
                "Fallback lane for harnesses Tachi does not launch natively.",
                "Leader owns process start, permissions, and completion review.",
            ],
        },
        "opencode" | "omo" => HarnessLane {
            id: "opencode",
            label: "OpenCode worker",
            kind: "worker",
            launch_mode: "opencode_worker",
            command_hint: "opencode --pure run --model <provider/model> \"<tracked_prompt>\"",
            mcp_support: "profile/config dependent",
            artifact_contract: "OpenCode must write mission plan.md and result.md; stdout is advisory.",
            notes: &[
                "Default execution lane for external subagents.",
                "Prefer for explore, implementation drafts, critic passes, and verifier work.",
            ],
        },
        "claude" | "claude-code" => HarnessLane {
            id: "claude",
            label: "Claude Code worker",
            kind: "worker",
            launch_mode: "claude_worker",
            command_hint: "claude --print \"<tracked_prompt>\" --mcp-config <config.json>",
            mcp_support: "json mcp config",
            artifact_contract: "Claude must write mission plan.md and result.md; use MCP when granted.",
            notes: &[
                "Use when the mission needs strong MCP/tool execution.",
                "Good fallback when OpenCode provider routing is unavailable.",
            ],
        },
        "gemini" | "gemini-advisor" | "ask-gemini" => HarnessLane {
            id: "gemini-advisor",
            label: "Gemini advisor",
            kind: "advisor",
            launch_mode: "advisor_artifact",
            command_hint: "gemini -p \"<advisor_prompt>\"; save output as .omx/artifacts/gemini-<slug>-<timestamp>.md",
            mcp_support: "not required",
            artifact_contract: "Advisor output is captured as an artifact and linked back into result.md; Gemini is not expected to edit arena files directly.",
            notes: &[
                "Brainstorm, design feedback, process critique, and second opinions only.",
                "Do not treat this lane as a normal worker harness.",
            ],
        },
        _ => HarnessLane {
            id: "manual",
            label: "Manual tracked document",
            kind: "manual",
            launch_mode: "tracked_document",
            command_hint: "Unsupported harness hint; use tracked_prompt manually or choose opencode, claude, gemini-advisor, or manual.",
            mcp_support: "external",
            artifact_contract: "Worker writes plan.md and result.md in the mission directory.",
            notes: &[
                "Unknown harness hints are intentionally treated as manual document missions.",
                "Tachi keeps the mission contract stable instead of launching arbitrary adapters.",
            ],
        },
    }
}

fn current_git_root() -> Option<PathBuf> {
    std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

pub(crate) fn arena_root() -> PathBuf {
    if let Ok(p) = std::env::var("TACHI_ARENA_ROOT") {
        return PathBuf::from(p);
    }
    if let Some(root) = current_git_root() {
        return root.join(".tachi").join("arena");
    }
    if let Ok(home) = std::env::var("TACHI_HOME") {
        return PathBuf::from(home).join("arena");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".tachi").join("arena");
    }
    std::env::temp_dir().join("tachi").join("arena")
}

fn tachi_home() -> PathBuf {
    if let Ok(home) = std::env::var("TACHI_HOME") {
        PathBuf::from(home)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".tachi")
    } else {
        std::env::temp_dir().join("tachi")
    }
}

fn dispatch_run_dir(dispatch_id: &str, run_dir_hint: Option<&str>) -> PathBuf {
    run_dir_hint
        .filter(|hint| !hint.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| tachi_home().join("runs").join(dispatch_id))
}

fn dispatch_response_summary(response: &Value) -> Value {
    json!({
        "dispatch_id": response.get("dispatch_id").cloned().unwrap_or(Value::Null),
        "run_dir": response.get("run_dir").cloned().unwrap_or(Value::Null),
        "agent": response.get("agent").cloned().unwrap_or(Value::Null),
        "selected_profile": response.get("selected_profile").cloned().unwrap_or(Value::Null),
        "harness_transport": response.get("harness_transport").cloned().unwrap_or(Value::Null),
        "harness_server_url": response.get("harness_server_url").cloned().unwrap_or(Value::Null),
        "source": "dispatch_response_summary",
        "redacted": true,
    })
}

fn read_linked_dispatch_status(dispatch_id: &str, run_dir_hint: Option<&str>) -> Option<Value> {
    let run_dir = dispatch_run_dir(dispatch_id, run_dir_hint);
    let status_path = run_dir.join("status.json");
    let status = read_json_file(&status_path).ok()?;
    let result_written = run_dir.join("result.md").exists();
    Some(json!({
        "dispatch_id": dispatch_id,
        "state": status.get("state").cloned().unwrap_or(Value::Null),
        "agent": status.get("agent").cloned().unwrap_or(Value::Null),
        "profile": status.get("profile").cloned().unwrap_or(Value::Null),
        "harness_transport": status.get("harness_transport").cloned().unwrap_or(Value::Null),
        "harness_server_url": status.get("harness_server_url").cloned().unwrap_or(Value::Null),
        "exit_code": status.get("exit_code").cloned().unwrap_or(Value::Null),
        "updated_at": status.get("updated_at").cloned().unwrap_or(Value::Null),
        "run_dir": run_dir.to_string_lossy().to_string(),
        "result_written": result_written,
        "source": "dispatch_run_summary",
        "redacted": true,
    }))
}

fn read_linked_dispatch_result(dispatch_id: &str, run_dir_hint: Option<&str>) -> Option<String> {
    let raw =
        std::fs::read_to_string(dispatch_run_dir(dispatch_id, run_dir_hint).join("result.md"))
            .ok()?;
    if raw.trim().is_empty() {
        None
    } else {
        Some(raw)
    }
}

fn refresh_linked_dispatch_fields(status: &mut Value) {
    let Some(obj) = status.as_object_mut() else {
        return;
    };
    let Some(dispatch_id) = obj
        .get("dispatch_id")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return;
    };
    obj.remove("dispatch_response");
    let run_dir_hint = obj.get("run_dir").and_then(Value::as_str);
    if let Some(linked) = read_linked_dispatch_status(&dispatch_id, run_dir_hint) {
        let linked_result_written = linked
            .get("result_written")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mission_result_written = obj
            .get("result_written")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if linked_result_written && !mission_result_written {
            obj.insert(
                "collection_state".to_string(),
                json!("pending_collect_from_dispatch"),
            );
        }
        obj.insert("linked_dispatch".to_string(), linked);
    }
}

fn slugify(s: &str, fallback: &str) -> String {
    let s = s.trim().to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut last_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed: String = out.trim_matches('-').chars().take(40).collect();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed
    }
}

fn new_arena_id(title: Option<&str>, objective: Option<&str>) -> String {
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let basis = title.or(objective).unwrap_or("arena");
    let suffix = uuid::Uuid::new_v4().as_simple().to_string()[..8].to_string();
    format!("arena_{}_{}_{}", stamp, slugify(basis, "arena"), suffix)
}

fn new_mission_id(role: Option<&str>, prompt: Option<&str>) -> String {
    let suffix = uuid::Uuid::new_v4().as_simple().to_string()[..8].to_string();
    let basis = role.or(prompt).unwrap_or("mission");
    format!("mission_{}_{}", slugify(basis, "mission"), suffix)
}

pub(crate) fn validate_arena_id(id: &str) -> Result<(), String> {
    validate_id(id, "arena_", "arena_id")
}

pub(crate) fn validate_mission_id(id: &str) -> Result<(), String> {
    validate_id(id, "mission_", "mission_id")
}

fn validate_id(id: &str, prefix: &str, label: &str) -> Result<(), String> {
    if !id.starts_with(prefix)
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "Invalid {label}: '{id}'. Expected prefix '{prefix}' and only ASCII letters, numbers, '_' or '-' with no path traversal."
        ));
    }
    Ok(())
}

fn nonempty_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false)
}

fn arena_dir(arena_id: &str) -> Result<PathBuf, String> {
    validate_arena_id(arena_id)?;
    Ok(arena_root().join(arena_id))
}

fn mission_dir(arena_id: &str, mission_id: &str) -> Result<PathBuf, String> {
    validate_mission_id(mission_id)?;
    Ok(arena_dir(arena_id)?.join("missions").join(mission_id))
}

fn write_json_file(path: &Path, value: &Value) -> Result<(), String> {
    let serialized =
        serde_json::to_string_pretty(value).map_err(|e| format!("serialize json: {e}"))?;
    crate::utils::write_owner_only_file_atomic(path, format!("{serialized}\n").as_bytes())
}

fn read_json_file(path: &Path) -> Result<Value, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))
}

fn append_event(run_dir: &Path, event: Value) -> Result<(), String> {
    let path = run_dir.join("events.jsonl");
    let line = serde_json::to_string(&event).map_err(|e| format!("serialize event: {e}"))?;
    crate::utils::append_owner_only_jsonl_line(&path, &line)
}

fn update_mission_status(arena_id: &str, mission_id: &str, patch: Value) -> Result<Value, String> {
    let dir = mission_dir(arena_id, mission_id)?;
    let status_path = dir.join("status.json");
    let mut status = read_json_file(&status_path)?;
    if let Some(obj) = status.as_object_mut() {
        if let Some(patch_obj) = patch.as_object() {
            for (key, value) in patch_obj {
                obj.insert(key.clone(), value.clone());
            }
        }
        obj.insert("updated_at".to_string(), json!(Utc::now().to_rfc3339()));
        obj.insert(
            "plan_written".to_string(),
            json!(nonempty_file(&dir.join("plan.md"))),
        );
        obj.insert(
            "result_written".to_string(),
            json!(nonempty_file(&dir.join("result.md"))),
        );
    }
    refresh_linked_dispatch_fields(&mut status);
    write_json_file(&status_path, &status)?;
    Ok(status)
}

fn mission_statuses(arena_id: &str) -> Result<Vec<Value>, String> {
    let dir = arena_dir(arena_id)?;
    let missions_dir = dir.join("missions");
    if !missions_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&missions_dir)
        .map_err(|e| format!("read missions dir {}: {e}", missions_dir.display()))?
    {
        let entry = entry.map_err(|e| format!("read mission dir entry: {e}"))?;
        let status_path = entry.path().join("status.json");
        if status_path.exists() {
            let mut status = read_json_file(&status_path)?;
            if let Some(obj) = status.as_object_mut() {
                let dir = entry.path();
                obj.insert(
                    "plan_written".to_string(),
                    json!(nonempty_file(&dir.join("plan.md"))),
                );
                obj.insert(
                    "result_written".to_string(),
                    json!(nonempty_file(&dir.join("result.md"))),
                );
            }
            refresh_linked_dispatch_fields(&mut status);
            out.push(status);
        }
    }
    out.sort_by(|a, b| {
        a.get("created_at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(b.get("created_at").and_then(Value::as_str).unwrap_or(""))
    });
    Ok(out)
}

fn active_state(state: &str) -> bool {
    matches!(state, "ready" | "running")
}

fn tracked_worker_prompt(
    lane: &HarnessLane,
    prompt_path: &Path,
    plan_path: &Path,
    result_path: &Path,
) -> String {
    format!(
        "You are executing a tracked Tachi Arena mission.\n\n\
         Harness lane: {} ({})\n\
         Launch mode: {}\n\
         Command hint: {}\n\
\n\
         Read the mission prompt from:\n{}\n\n\
         Before substantive work, write a concise plan to:\n{}\n\n\
         When finished, blocked, or partially complete, write a completion report to:\n{}\n\n\
         Artifact contract: {}\n\n\
         The completion report must include:\n\
         - Summary\n\
         - Files changed\n\
         - Commands run\n\
         - Verification performed\n\
         - Remaining risks or blockers\n\n\
         Return a concise summary and mention the report path.",
        lane.id,
        lane.kind,
        lane.launch_mode,
        lane.command_hint,
        prompt_path.display(),
        plan_path.display(),
        result_path.display(),
        lane.artifact_contract
    )
}

fn default_opencode_model(role: Option<&str>) -> &'static str {
    match role.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "explore" | "search" | "librarian" => "deepseek/deepseek-v4-flash",
        "critic" | "review" | "reviewer" => "deepseek/deepseek-v4-pro",
        _ => "zhipuai-coding-plan/glm-5.1",
    }
}

fn dispatch_params_for_mission(
    params: &TachiArenaParams,
    lane: &HarnessLane,
    tracked_prompt: &str,
) -> Option<TachiDispatchParams> {
    let agent = match lane.id {
        "opencode" => "custom",
        "claude" => "claude",
        _ => return None,
    };
    let mut command = Vec::new();
    if lane.id == "opencode" && params.profile.is_none() {
        let model = params
            .model
            .as_deref()
            .unwrap_or_else(|| default_opencode_model(params.role.as_deref()));
        command = vec![
            "opencode".to_string(),
            "--pure".to_string(),
            "run".to_string(),
            "--model".to_string(),
            model.to_string(),
        ];
    }

    Some(TachiDispatchParams {
        agent: Some(agent.to_string()),
        profile: params.profile.clone(),
        credential_profiles: params.credential_profiles.clone(),
        task: tracked_prompt.to_string(),
        cwd: params.cwd.clone(),
        skills: params.skills.clone(),
        context_query: None,
        model: params.model.clone(),
        timeout_secs: params.timeout_secs.unwrap_or(600),
        permission_profile: params.permission_profile.clone(),
        allowed_tools: Vec::new(),
        max_turns: None,
        sandbox: params.sandbox.clone(),
        inject_tachi_mcp: None,
        inject_hub_mcps: None,
        command,
        harness_transport: None,
        harness_server_url: None,
        project: params.project.clone(),
        stage: params.role.clone(),
        issue_ref: params.issue_ref.clone(),
        pr_ref: params.pr_ref.clone(),
        flow_id: params.flow_id.clone(),
        tool_profile: params.tool_profile.clone(),
        auto_capability_bundle: params.auto_capability_bundle,
        mcp_access: None,
        allowed_mcp_servers: Vec::new(),
    })
}

fn completion_draft_for_mission(status: &Value, result_path: &Path) -> Value {
    let task = status
        .get("task")
        .and_then(Value::as_str)
        .unwrap_or("tracked arena mission");
    let agent = status
        .get("dispatch_agent")
        .and_then(Value::as_str)
        .or_else(|| status.get("harness").and_then(Value::as_str))
        .unwrap_or("arena-worker");
    json!({
        "tool": "tachi_task",
        "arguments": {
            "action": "complete",
            "dispatch_id": status.get("dispatch_id").cloned().unwrap_or(Value::Null),
            "task": task,
            "agent": agent,
            "outcome": "success|failure|partial|aborted",
            "profile": status.get("dispatch_profile_name").cloned().unwrap_or(Value::Null),
            "flow_id": status.get("flow_id").cloned().unwrap_or(Value::Null),
            "issue_ref": status.get("issue_ref").cloned().unwrap_or(Value::Null),
            "pr_ref": status.get("pr_ref").cloned().unwrap_or(Value::Null),
            "evidence_refs": [result_path.to_string_lossy().to_string()],
            "tests_run": [],
            "diff_present": null,
        }
    })
}

fn render_arena_md(arena_id: &str, title: &str, objective: &str) -> String {
    format!(
        "# {title}\n\n\
         Arena: `{arena_id}`\n\n\
         ## Objective\n\n{objective}\n\n\
         ## Contract\n\n\
         Arena owns run documents. Memory owns distilled knowledge.\n\n\
         Workers must write `plan.md` before substantive work and `result.md` before completion.\n"
    )
}

fn render_prompt_md(
    params: &TachiArenaParams,
    arena_id: &str,
    mission_id: &str,
    feedback_rules_section: Option<&str>,
) -> String {
    let prompt = params.prompt.as_deref().unwrap_or("");
    let role = params.role.as_deref().unwrap_or("worker");
    let requested_harness = params.harness.as_deref().unwrap_or("manual");
    let lane = harness_lane(params.harness.as_deref());
    format!(
        "# Arena Mission\n\n\
         Arena: `{arena_id}`\n\
         Mission: `{mission_id}`\n\
         Requested harness: `{requested_harness}`\n\
         Harness lane: `{}` ({})\n\
         Launch mode: `{}`\n\
         Role: `{role}`\n\n\
         ## Lane Guidance\n\n\
         - Command hint: `{}`\n\
         - MCP support: `{}`\n\
         - Artifact contract: {}\n\
{}\n\
         ## Task\n\n{prompt}\n\n\
{}\n\
         ## Skills\n\n{}\n\n\
         ## Scope\n\n{}\n\n\
         ## Permissions\n\n{}\n\n\
         ## Worker Report Contract\n\n\
         Write `plan.md` before substantive work. Write `result.md` when finished, blocked, or partially complete.\n\
         Include Summary, Files changed, Commands run, Verification performed, and Remaining risks or blockers.\n",
        lane.id,
        lane.label,
        lane.launch_mode,
        lane.command_hint,
        lane.mcp_support,
        lane.artifact_contract,
        list_lines(
            &lane
                .notes
                .iter()
                .map(|note| note.to_string())
                .collect::<Vec<_>>()
        ),
        feedback_rules_section.unwrap_or(""),
        list_lines(&params.skills),
        list_lines(&params.scope),
        list_lines(&params.permissions)
    )
}

fn list_lines(items: &[String]) -> String {
    if items.is_empty() {
        "- none\n".to_string()
    } else {
        items
            .iter()
            .map(|item| format!("- {item}\n"))
            .collect::<String>()
    }
}

fn handle_open(params: TachiArenaParams) -> Result<String, String> {
    let title = params.title.unwrap_or_else(|| "Tachi Arena".to_string());
    let objective = params
        .objective
        .or(params.prompt)
        .ok_or_else(|| "objective or prompt is required for action='open'".to_string())?;
    let arena_id = params
        .arena_id
        .unwrap_or_else(|| new_arena_id(Some(&title), Some(&objective)));
    validate_arena_id(&arena_id)?;
    let dir = arena_dir(&arena_id)?;
    std::fs::create_dir_all(dir.join("missions"))
        .map_err(|e| format!("create arena dir {}: {e}", dir.display()))?;

    let now = Utc::now().to_rfc3339();
    let manifest = json!({
        "arena_id": arena_id,
        "title": title,
        "objective": objective,
        "state": "open",
        "created_at": now,
        "updated_at": now,
        "root": dir,
        "documents": {
            "arena": dir.join("arena.md"),
            "manifest": dir.join("manifest.json"),
            "board": dir.join("board.json"),
            "events": dir.join("events.jsonl"),
        },
    });
    write_json_file(&dir.join("manifest.json"), &manifest)?;
    write_json_file(
        &dir.join("board.json"),
        &json!({
            "arena_id": arena_id,
            "state": "open",
            "missions": [],
            "updated_at": now,
        }),
    )?;
    crate::utils::write_owner_only_file_atomic(
        &dir.join("arena.md"),
        render_arena_md(
            manifest["arena_id"].as_str().unwrap_or("arena"),
            manifest["title"].as_str().unwrap_or("Tachi Arena"),
            manifest["objective"].as_str().unwrap_or(""),
        )
        .as_bytes(),
    )
    .map_err(|e| format!("write arena.md: {e}"))?;
    append_event(
        &dir,
        json!({
            "type": "arena_opened",
            "arena_id": manifest["arena_id"],
            "timestamp": now,
        }),
    )?;
    serde_json::to_string(&json!({
        "tool": "tachi_arena",
        "action": "open",
        "arena_id": manifest["arena_id"],
        "state": "open",
        "arena_dir": dir,
        "manifest_path": dir.join("manifest.json"),
        "board_path": dir.join("board.json"),
        "arena_path": dir.join("arena.md"),
    }))
    .map_err(|e| format!("serialize arena open: {e}"))
}

async fn handle_spawn(server: &MemoryServer, params: TachiArenaParams) -> Result<String, String> {
    let arena_id = params
        .arena_id
        .as_deref()
        .ok_or_else(|| "arena_id is required for action='spawn'".to_string())?;
    let prompt = params
        .prompt
        .as_deref()
        .ok_or_else(|| "prompt is required for action='spawn'".to_string())?;
    let arena = arena_dir(arena_id)?;
    if !arena.join("manifest.json").exists() {
        return Err(format!("arena not found: {arena_id}"));
    }
    let mission_id = params
        .mission_id
        .clone()
        .unwrap_or_else(|| new_mission_id(params.role.as_deref(), Some(prompt)));
    validate_mission_id(&mission_id)?;
    let dir = mission_dir(arena_id, &mission_id)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("create mission dir: {e}"))?;

    let prompt_path = dir.join("prompt.md");
    let plan_path = dir.join("plan.md");
    let result_path = dir.join("result.md");
    let stdout_path = dir.join("stdout.log");
    let stderr_path = dir.join("stderr.log");
    let status_path = dir.join("status.json");
    let now = Utc::now().to_rfc3339();
    let lane = harness_lane(params.harness.as_deref());
    let requested_harness = params.harness.as_deref().unwrap_or("manual");
    let feedback_rules = crate::feedback_rule_ops::applicable_feedback_rules(
        server,
        crate::feedback_rule_ops::FeedbackRuleQuery {
            task: prompt.to_string(),
            task_type: params.role.clone(),
            profile: params.profile.clone(),
            stage: params.role.clone(),
            keywords: params.skills.clone(),
            project: params.project.clone(),
        },
    )
    .await;
    let feedback_rules_section =
        crate::feedback_rule_ops::render_feedback_rules_section(&feedback_rules);
    let feedback_rules_trace = crate::feedback_rule_ops::feedback_rules_trace(&feedback_rules);
    crate::utils::write_owner_only_file_atomic(
        &prompt_path,
        render_prompt_md(
            &params,
            arena_id,
            &mission_id,
            feedback_rules_section.as_deref(),
        )
        .as_bytes(),
    )
    .map_err(|e| format!("write prompt.md: {e}"))?;
    let status = json!({
        "arena_id": arena_id,
        "mission_id": mission_id,
        "state": "ready",
        "task": prompt,
        "harness": lane.id,
        "requested_harness": requested_harness,
        "harness_lane": {
            "id": lane.id,
            "label": lane.label,
            "kind": lane.kind,
            "launch_mode": lane.launch_mode,
            "command_hint": lane.command_hint,
            "mcp_support": lane.mcp_support,
            "artifact_contract": lane.artifact_contract,
            "notes": lane.notes,
        },
        "role": params.role.as_deref().unwrap_or("worker"),
        "cwd": params.cwd.clone(),
        "skills": params.skills.clone(),
        "scope": params.scope.clone(),
        "permissions": params.permissions.clone(),
        "timeout_secs": params.timeout_secs,
        "launch_requested": params.launch,
        "profile": params.profile.clone(),
        "model": params.model.clone(),
        "project": params.project.clone(),
        "flow_id": params.flow_id.clone(),
        "issue_ref": params.issue_ref.clone(),
        "pr_ref": params.pr_ref.clone(),
        "credential_profiles": params.credential_profiles.clone(),
        "tool_profile": params.tool_profile.clone(),
        "auto_capability_bundle": params.auto_capability_bundle,
        "feedback_rules": feedback_rules_trace,
        "created_at": now,
        "updated_at": now,
        "prompt_path": prompt_path,
        "plan_path": plan_path,
        "result_path": result_path,
        "stdout_path": stdout_path,
        "stderr_path": stderr_path,
        "plan_written": false,
        "result_written": false,
        "launched": false,
        "launch_mode": lane.launch_mode,
        "launch_status": if params.launch { "pending" } else { "not_requested" },
    });
    write_json_file(&status_path, &status)?;
    append_event(
        &arena,
        json!({
            "type": "mission_spawned",
            "arena_id": arena_id,
            "mission_id": mission_id,
            "timestamp": now,
            "harness": lane.id,
            "requested_harness": requested_harness,
            "launch_mode": lane.launch_mode,
        }),
    )?;
    refresh_board(arena_id)?;
    let tracked_prompt = tracked_worker_prompt(&lane, &prompt_path, &plan_path, &result_path);
    let launch_result = if params.launch {
        if let Some(dispatch_params) = dispatch_params_for_mission(&params, &lane, &tracked_prompt)
        {
            match crate::dispatch_ops::handle_tachi_dispatch(server, dispatch_params).await {
                Ok(raw) => {
                    let response =
                        serde_json::from_str::<Value>(&raw).unwrap_or_else(|_| json!({"raw": raw}));
                    let dispatch_id = response
                        .get("dispatch_id")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let run_dir = response
                        .get("run_dir")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let dispatch_agent = response
                        .get("agent")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let dispatch_profile_name = response
                        .get("selected_profile")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| params.profile.clone());
                    update_mission_status(
                        arena_id,
                        &mission_id,
                        json!({
                            "state": "running",
                            "launched": true,
                            "launch_status": "launched",
                            "dispatch_id": dispatch_id,
                            "run_dir": run_dir,
                            "dispatch_agent": dispatch_agent,
                            "dispatch_profile_name": dispatch_profile_name,
                            "dispatch_link": dispatch_response_summary(&response),
                        }),
                    )?;
                    Some(json!({
                        "status": "launched",
                        "dispatch_id": dispatch_id,
                        "run_dir": run_dir,
                    }))
                }
                Err(err) => {
                    update_mission_status(
                        arena_id,
                        &mission_id,
                        json!({
                            "state": "launch_failed",
                            "launched": false,
                            "launch_status": "failed",
                            "launch_error": err,
                        }),
                    )?;
                    return Err(err);
                }
            }
        } else {
            update_mission_status(
                arena_id,
                &mission_id,
                json!({
                    "launch_status": "document_only",
                    "launch_message": "Harness lane is document/advisor only; use tracked_prompt manually.",
                }),
            )?;
            Some(json!({
                "status": "document_only",
                "message": "Harness lane is document/advisor only; use tracked_prompt manually.",
            }))
        }
    } else {
        None
    };
    let final_status = read_json_file(&status_path)?;

    serde_json::to_string(&json!({
        "tool": "tachi_arena",
        "action": "spawn",
        "arena_id": arena_id,
        "mission_id": mission_id,
        "state": final_status.get("state").cloned().unwrap_or_else(|| json!("ready")),
        "harness": lane.id,
        "requested_harness": requested_harness,
        "launch": launch_result,
        "harness_lane": {
            "id": lane.id,
            "label": lane.label,
            "kind": lane.kind,
            "launch_mode": lane.launch_mode,
            "command_hint": lane.command_hint,
            "mcp_support": lane.mcp_support,
            "artifact_contract": lane.artifact_contract,
            "notes": lane.notes,
        },
        "mission_dir": dir,
        "prompt_path": prompt_path,
        "plan_path": plan_path,
        "result_path": result_path,
        "status_path": status_path,
        "tracked_prompt": tracked_prompt,
        "status": final_status,
    }))
    .map_err(|e| format!("serialize arena spawn: {e}"))
}

fn refresh_board(arena_id: &str) -> Result<Value, String> {
    let dir = arena_dir(arena_id)?;
    let missions = mission_statuses(arena_id)?;
    let board = json!({
        "arena_id": arena_id,
        "state": read_json_file(&dir.join("manifest.json"))
            .ok()
            .and_then(|v| v.get("state").cloned())
            .unwrap_or_else(|| json!("unknown")),
        "missions": missions,
        "updated_at": Utc::now().to_rfc3339(),
    });
    write_json_file(&dir.join("board.json"), &board)?;
    Ok(board)
}

fn handle_board(params: TachiArenaParams) -> Result<String, String> {
    if let Some(arena_id) = params.arena_id.as_deref() {
        let board = refresh_board(arena_id)?;
        return serde_json::to_string(&json!({
            "tool": "tachi_arena",
            "action": "board",
            "arena_id": arena_id,
            "result": board,
        }))
        .map_err(|e| format!("serialize arena board: {e}"));
    }

    let root = arena_root();
    let mut arenas = Vec::new();
    if root.exists() {
        for entry in std::fs::read_dir(&root)
            .map_err(|e| format!("read arena root {}: {e}", root.display()))?
        {
            let entry = entry.map_err(|e| format!("read arena entry: {e}"))?;
            let manifest_path = entry.path().join("manifest.json");
            if manifest_path.exists() {
                arenas.push(read_json_file(&manifest_path)?);
            }
        }
    }
    arenas.sort_by(|a, b| {
        a.get("created_at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(b.get("created_at").and_then(Value::as_str).unwrap_or(""))
    });
    serde_json::to_string(&json!({
        "tool": "tachi_arena",
        "action": "board",
        "arena_root": root,
        "arenas": arenas,
    }))
    .map_err(|e| format!("serialize arena board: {e}"))
}

fn handle_collect(params: TachiArenaParams) -> Result<String, String> {
    let arena_id = params
        .arena_id
        .as_deref()
        .ok_or_else(|| "arena_id is required for action='collect'".to_string())?;
    let mission_ids = if let Some(mission_id) = params.mission_id.as_deref() {
        validate_mission_id(mission_id)?;
        vec![mission_id.to_string()]
    } else {
        mission_statuses(arena_id)?
            .into_iter()
            .filter_map(|s| {
                s.get("mission_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect()
    };
    let mut collected = Vec::new();
    for mission_id in mission_ids {
        let dir = mission_dir(arena_id, &mission_id)?;
        let result_path = dir.join("result.md");
        let plan_path = dir.join("plan.md");
        let mut status_before = read_json_file(&dir.join("status.json"))?;
        refresh_linked_dispatch_fields(&mut status_before);
        let mut result = std::fs::read_to_string(&result_path).unwrap_or_default();
        let mut result_source = if result.is_empty() {
            "missing"
        } else {
            "mission_result"
        };
        if result.trim().is_empty() {
            if let Some(dispatch_id) = status_before.get("dispatch_id").and_then(Value::as_str) {
                let run_dir_hint = status_before.get("run_dir").and_then(Value::as_str);
                if let Some(dispatch_result) =
                    read_linked_dispatch_result(dispatch_id, run_dir_hint)
                {
                    crate::utils::write_owner_only_file_atomic(
                        &result_path,
                        dispatch_result.as_bytes(),
                    )
                    .map_err(|e| format!("write linked dispatch result.md: {e}"))?;
                    result = dispatch_result;
                    result_source = "linked_dispatch_result";
                }
            }
        }
        let plan = std::fs::read_to_string(&plan_path).unwrap_or_default();
        let result_written = !result.is_empty();
        let plan_written = !plan.is_empty();
        let state = if result_written {
            "collected"
        } else {
            "pending_result"
        };
        let status = update_mission_status(
            arena_id,
            &mission_id,
            json!({
                "state": state,
                "collected_at": Utc::now().to_rfc3339(),
                "result_source": result_source,
                "completion_draft": if result_written {
                    completion_draft_for_mission(&status_before, &result_path)
                } else {
                    Value::Null
                },
            }),
        )?;
        collected.push(json!({
            "mission_id": mission_id,
            "state": state,
            "plan_written": plan_written,
            "result_written": result_written,
            "result_source": result_source,
            "plan_path": plan_path,
            "result_path": result_path,
            "result": result,
            "completion_draft": status.get("completion_draft").cloned().unwrap_or(Value::Null),
            "status": status,
        }));
    }
    refresh_board(arena_id)?;
    append_event(
        &arena_dir(arena_id)?,
        json!({
            "type": "missions_collected",
            "arena_id": arena_id,
            "count": collected.len(),
            "timestamp": Utc::now().to_rfc3339(),
        }),
    )?;
    serde_json::to_string(&json!({
        "tool": "tachi_arena",
        "action": "collect",
        "arena_id": arena_id,
        "missions": collected,
    }))
    .map_err(|e| format!("serialize arena collect: {e}"))
}

fn handle_abort(params: TachiArenaParams) -> Result<String, String> {
    let arena_id = params
        .arena_id
        .as_deref()
        .ok_or_else(|| "arena_id is required for action='abort'".to_string())?;
    let mission_id = params
        .mission_id
        .as_deref()
        .ok_or_else(|| "mission_id is required for action='abort'".to_string())?;
    let reason = params
        .reason
        .unwrap_or_else(|| "aborted by leader".to_string());
    let status = update_mission_status(
        arena_id,
        mission_id,
        json!({
            "state": "aborted",
            "completed_at": Utc::now().to_rfc3339(),
            "abort_reason": reason,
        }),
    )?;
    refresh_board(arena_id)?;
    append_event(
        &arena_dir(arena_id)?,
        json!({
            "type": "mission_aborted",
            "arena_id": arena_id,
            "mission_id": mission_id,
            "timestamp": Utc::now().to_rfc3339(),
        }),
    )?;
    serde_json::to_string(&json!({
        "tool": "tachi_arena",
        "action": "abort",
        "arena_id": arena_id,
        "mission_id": mission_id,
        "status": status,
    }))
    .map_err(|e| format!("serialize arena abort: {e}"))
}

fn handle_reap(params: TachiArenaParams) -> Result<String, String> {
    let dry_run = params.dry_run.unwrap_or(true);
    let arena_filter = params.arena_id.clone();
    let arenas = if let Some(arena_id) = arena_filter.as_deref() {
        vec![arena_id.to_string()]
    } else {
        let root = arena_root();
        if !root.exists() {
            Vec::new()
        } else {
            std::fs::read_dir(&root)
                .map_err(|e| format!("read arena root: {e}"))?
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|name| validate_arena_id(name).is_ok())
                .collect()
        }
    };
    let mut stale = Vec::new();
    for arena_id in arenas {
        let mut changed = false;
        for status in mission_statuses(&arena_id)? {
            let state = status.get("state").and_then(Value::as_str).unwrap_or("");
            if !active_state(state) {
                continue;
            }
            let Some(mission_id) = status.get("mission_id").and_then(Value::as_str) else {
                continue;
            };
            stale.push(json!({
                "arena_id": arena_id,
                "mission_id": mission_id,
                "state": state,
            }));
            if !dry_run {
                update_mission_status(
                    &arena_id,
                    mission_id,
                    json!({
                        "state": "reaped",
                        "completed_at": Utc::now().to_rfc3339(),
                        "reap_reason": params.reason.as_deref().unwrap_or("arena reap"),
                    }),
                )?;
                changed = true;
            }
        }
        if changed {
            refresh_board(&arena_id)?;
        }
    }
    serde_json::to_string(&json!({
        "tool": "tachi_arena",
        "action": "reap",
        "dry_run": dry_run,
        "stale_missions": stale,
    }))
    .map_err(|e| format!("serialize arena reap: {e}"))
}

fn handle_close(params: TachiArenaParams) -> Result<String, String> {
    let arena_id = params
        .arena_id
        .as_deref()
        .ok_or_else(|| "arena_id is required for action='close'".to_string())?;
    let force = params.force;
    let require_collected = params.require_collected.unwrap_or(true);
    let dir = arena_dir(arena_id)?;
    let missions = mission_statuses(arena_id)?;
    let mut blockers = Vec::new();
    for mission in &missions {
        let state = mission.get("state").and_then(Value::as_str).unwrap_or("");
        let result_written = mission
            .get("result_written")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if active_state(state) {
            blockers.push(json!({
                "mission_id": mission.get("mission_id"),
                "reason": "mission still active",
                "state": state,
            }));
        } else if require_collected && result_written && state != "collected" {
            blockers.push(json!({
                "mission_id": mission.get("mission_id"),
                "reason": "result written but not collected",
                "state": state,
            }));
        }
    }
    if !force && !blockers.is_empty() {
        return serde_json::to_string(&json!({
            "tool": "tachi_arena",
            "action": "close",
            "arena_id": arena_id,
            "state": "blocked",
            "blockers": blockers,
            "message": "abort/reap active missions or collect written results before close; pass force=true to override",
        }))
        .map_err(|e| format!("serialize arena close blocked: {e}"));
    }

    let mut manifest = read_json_file(&dir.join("manifest.json"))?;
    if let Some(obj) = manifest.as_object_mut() {
        obj.insert("state".to_string(), json!("closed"));
        obj.insert("closed_at".to_string(), json!(Utc::now().to_rfc3339()));
        obj.insert("updated_at".to_string(), json!(Utc::now().to_rfc3339()));
    }
    write_json_file(&dir.join("manifest.json"), &manifest)?;
    let summary = render_summary_md(arena_id, &missions);
    crate::utils::write_owner_only_file_atomic(&dir.join("summary.md"), summary.as_bytes())
        .map_err(|e| format!("write summary.md: {e}"))?;
    append_event(
        &dir,
        json!({
            "type": "arena_closed",
            "arena_id": arena_id,
            "timestamp": Utc::now().to_rfc3339(),
            "force": force,
        }),
    )?;
    refresh_board(arena_id)?;
    serde_json::to_string(&json!({
        "tool": "tachi_arena",
        "action": "close",
        "arena_id": arena_id,
        "state": "closed",
        "summary_path": dir.join("summary.md"),
    }))
    .map_err(|e| format!("serialize arena close: {e}"))
}

fn mission_result_preview(arena_id: &str, mission_id: &str) -> String {
    let Ok(dir) = mission_dir(arena_id, mission_id) else {
        return String::new();
    };
    let raw = std::fs::read_to_string(dir.join("result.md")).unwrap_or_default();
    raw.lines()
        .find(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .unwrap_or("")
        .trim()
        .chars()
        .take(180)
        .collect()
}

fn render_summary_md(arena_id: &str, missions: &[Value]) -> String {
    let mut out = format!(
        "# Arena Summary\n\nArena: `{arena_id}`\n\nMissions: {}\n\nState: closed\n\n## Mission Results\n\n",
        missions.len()
    );
    if missions.is_empty() {
        out.push_str("- none\n");
        return out;
    }
    for mission in missions {
        let mission_id = mission
            .get("mission_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let harness = mission
            .get("harness")
            .and_then(Value::as_str)
            .unwrap_or("manual");
        let role = mission
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("worker");
        let state = mission
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let preview = mission_result_preview(arena_id, mission_id);
        out.push_str(&format!(
            "- `{mission_id}` [{harness}/{role}] {state}: {}\n",
            if preview.is_empty() {
                "no result preview"
            } else {
                preview.as_str()
            }
        ));
    }
    out
}

pub(crate) async fn handle_tachi_arena(
    _server: &MemoryServer,
    params: TachiArenaParams,
) -> Result<String, String> {
    match params.action.to_ascii_lowercase().as_str() {
        "open" => handle_open(params),
        "spawn" => handle_spawn(_server, params).await,
        "board" => handle_board(params),
        "collect" => handle_collect(params),
        "abort" => handle_abort(params),
        "reap" => handle_reap(params),
        "close" => handle_close(params),
        other => Err(format!(
            "Invalid action '{other}'. Use open, spawn, board, collect, abort, reap, or close."
        )),
    }
}

#[cfg(test)]
pub(crate) fn tachi_arena_root_env_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_params::SaveMemoryParams;

    struct ArenaRootGuard {
        _guard: std::sync::MutexGuard<'static, ()>,
        original: Option<std::ffi::OsString>,
        path: PathBuf,
    }

    struct EnvGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set_path(key: &'static str, value: &Path) -> Self {
            let original = std::env::var_os(key);
            // SAFETY: arena tests that use this helper hold tachi_arena_root_env_lock.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, original }
        }

        fn set_os(key: &'static str, value: std::ffi::OsString) -> Self {
            let original = std::env::var_os(key);
            // SAFETY: arena tests that use this helper hold tachi_arena_root_env_lock.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: arena tests that use this helper hold tachi_arena_root_env_lock.
            unsafe {
                if let Some(value) = self.original.as_ref() {
                    std::env::set_var(self.key, value);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    impl Drop for ArenaRootGuard {
        fn drop(&mut self) {
            // SAFETY: arena tests serialize access to TACHI_ARENA_ROOT with
            // tachi_arena_root_env_lock(), so no concurrent env mutation occurs.
            unsafe {
                if let Some(value) = self.original.as_ref() {
                    std::env::set_var("TACHI_ARENA_ROOT", value);
                } else {
                    std::env::remove_var("TACHI_ARENA_ROOT");
                }
            }
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn temp_arena_root() -> ArenaRootGuard {
        let guard = tachi_arena_root_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original = std::env::var_os("TACHI_ARENA_ROOT");
        let path = std::env::temp_dir().join(format!("tachi-arena-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        // SAFETY: protected by tachi_arena_root_env_lock(); see Drop impl.
        unsafe {
            std::env::set_var("TACHI_ARENA_ROOT", &path);
        }
        ArenaRootGuard {
            _guard: guard,
            original,
            path,
        }
    }

    fn server() -> MemoryServer {
        let db_path = std::env::temp_dir().join(format!(
            "memory-server-arena-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        MemoryServer::new(db_path, None).expect("test server")
    }

    fn params(action: &str) -> TachiArenaParams {
        TachiArenaParams {
            action: action.to_string(),
            format: None,
            arena_id: None,
            mission_id: None,
            title: None,
            objective: None,
            prompt: None,
            harness: None,
            role: None,
            cwd: None,
            skills: Vec::new(),
            scope: Vec::new(),
            permissions: Vec::new(),
            timeout_secs: None,
            launch: false,
            profile: None,
            model: None,
            project: None,
            flow_id: None,
            issue_ref: None,
            pr_ref: None,
            permission_profile: None,
            sandbox: None,
            credential_profiles: Vec::new(),
            tool_profile: None,
            auto_capability_bundle: None,
            reason: None,
            dry_run: None,
            force: false,
            require_collected: None,
        }
    }

    #[tokio::test]
    async fn tachi_arena_facade_defaults_to_json_and_keeps_markdown_escape_hatch() {
        let server = server();

        let json_params: rmcp::handler::server::wrapper::Parameters<TachiArenaParams> =
            rmcp::handler::server::wrapper::Parameters(params("board"));
        let json_body: String = server
            .tachi_arena(json_params)
            .await
            .expect("default board should succeed");
        let parsed: Value = serde_json::from_str(&json_body).expect("default board JSON");
        assert_eq!(parsed["action"], json!("board"));

        let mut markdown_params = params("board");
        markdown_params.format = Some("markdown".to_string());
        let markdown_params: rmcp::handler::server::wrapper::Parameters<TachiArenaParams> =
            rmcp::handler::server::wrapper::Parameters(markdown_params);
        let markdown: String = server
            .tachi_arena(markdown_params)
            .await
            .expect("markdown board should succeed");
        assert!(markdown.starts_with("## Tachi arena board"), "{markdown}");
        assert!(markdown.contains("```json"), "{markdown}");
    }

    async fn wait_for_nonempty_file(path: &Path) -> String {
        for _ in 0..40 {
            if let Ok(raw) = std::fs::read_to_string(path) {
                if !raw.trim().is_empty() {
                    return raw;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        std::fs::read_to_string(path).unwrap_or_default()
    }

    async fn save_arena_feedback_rule(server: &MemoryServer) -> String {
        let raw = crate::memory_search_ops::handle_save_memory(
            server,
            SaveMemoryParams {
                text: "Arena code-audit workers must report grep evidence for unused or dead-code claims.".to_string(),
                summary: "Subagent audit prompts require explicit search evidence".to_string(),
                path: "/feedback/subagent/code-audit/grep-evidence".to_string(),
                importance: 0.8,
                category: "prompt_rule".to_string(),
                topic: "Subagent audit prompts require explicit search evidence".to_string(),
                keywords: vec![
                    "feedback_rule".to_string(),
                    "prompt_rule".to_string(),
                    "dead_code".to_string(),
                    "grep".to_string(),
                ],
                persons: Vec::new(),
                entities: Vec::new(),
                location: String::new(),
                scope: "project".to_string(),
                vector: None,
                id: None,
                force: true,
                auto_link: true,
                project: None,
                retention_policy: Some("durable".to_string()),
                domain: None,
                timestamp: None,
                valid_from: None,
                valid_until: None,
                metadata: Some(json!({
                    "kind": "feedback_rule",
                    "category": "prompt_rule",
                    "applies_to": {
                        "task_type": ["explore"],
                        "profiles": ["codex_55_review"],
                        "stage": ["explore"]
                    },
                    "trigger_keywords": ["unused", "dead code", "grep"],
                    "prompt_patch": "Search both identifier and call forms before making dead-code claims.",
                    "evidence_contract": ["grep_commands", "paths_searched", "uncertainty_notes"]
                })),
            },
        )
        .await
        .expect("feedback rule save should succeed");
        serde_json::from_str::<Value>(&raw)
            .expect("save JSON")
            .get("id")
            .and_then(Value::as_str)
            .expect("saved rule id")
            .to_string()
    }

    #[tokio::test]
    async fn arena_open_spawn_collect_close_writes_tracked_documents() {
        let _root = temp_arena_root();
        let server = server();
        let mut open = params("open");
        open.title = Some("Arena Test".into());
        open.objective = Some("coordinate tracked workers".into());
        let raw = handle_tachi_arena(&server, open).await.unwrap();
        let opened: Value = serde_json::from_str(&raw).unwrap();
        let arena_id = opened["arena_id"].as_str().unwrap().to_string();
        let arena_dir = PathBuf::from(opened["arena_dir"].as_str().unwrap());
        assert!(arena_dir.join("arena.md").exists());
        assert!(arena_dir.join("manifest.json").exists());

        let mut spawn = params("spawn");
        spawn.arena_id = Some(arena_id.clone());
        spawn.prompt = Some("inspect the code".into());
        spawn.harness = Some("codex".into());
        spawn.role = Some("explore".into());
        spawn.skills = vec!["skill:waza-check".into()];
        let raw = handle_tachi_arena(&server, spawn).await.unwrap();
        let spawned: Value = serde_json::from_str(&raw).unwrap();
        let mission_id = spawned["mission_id"].as_str().unwrap().to_string();
        let mission_dir = PathBuf::from(spawned["mission_dir"].as_str().unwrap());
        assert!(mission_dir.join("prompt.md").exists());
        assert!(spawned["tracked_prompt"]
            .as_str()
            .unwrap()
            .contains("write a completion report"));
        std::fs::write(
            mission_dir.join("plan.md"),
            "Plan: inspect files before reporting.\n",
        )
        .unwrap();
        std::fs::write(
            mission_dir.join("result.md"),
            "Summary: done\nFiles changed: none\nCommands run: none\nVerification performed: read-only\nRemaining risks or blockers: none\n",
        )
        .unwrap();

        let mut collect = params("collect");
        collect.arena_id = Some(arena_id.clone());
        collect.mission_id = Some(mission_id.clone());
        let raw = handle_tachi_arena(&server, collect).await.unwrap();
        let collected: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(collected["missions"][0]["state"], "collected");
        assert_eq!(collected["missions"][0]["result_written"], true);
        assert_eq!(collected["missions"][0]["result_source"], "mission_result");
        assert_eq!(
            collected["missions"][0]["status"]["result_source"],
            "mission_result"
        );

        let mut close = params("close");
        close.arena_id = Some(arena_id);
        let raw = handle_tachi_arena(&server, close).await.unwrap();
        let closed: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(closed["state"], "closed");
        assert!(arena_dir.join("summary.md").exists());
        let summary = std::fs::read_to_string(arena_dir.join("summary.md")).unwrap();
        assert!(summary.contains(&mission_id));
        assert!(summary.contains("Summary: done"));
    }

    #[tokio::test]
    async fn arena_spawn_injects_applicable_feedback_rules_into_mission_prompt() {
        let _root = temp_arena_root();
        let server = server();
        let rule_id = save_arena_feedback_rule(&server).await;

        let mut open = params("open");
        open.title = Some("Feedback Arena".into());
        open.objective = Some("coordinate code-audit workers".into());
        let opened: Value =
            serde_json::from_str(&handle_tachi_arena(&server, open).await.unwrap()).unwrap();
        let arena_id = opened["arena_id"].as_str().unwrap().to_string();

        let mut spawn = params("spawn");
        spawn.arena_id = Some(arena_id);
        spawn.prompt = Some("Explore unused functions and dead code with grep evidence.".into());
        spawn.harness = Some("codex".into());
        spawn.role = Some("explore".into());
        spawn.profile = Some("codex_55_review".into());
        let spawned: Value =
            serde_json::from_str(&handle_tachi_arena(&server, spawn).await.unwrap()).unwrap();
        let mission_dir = PathBuf::from(spawned["mission_dir"].as_str().unwrap());
        let prompt = std::fs::read_to_string(mission_dir.join("prompt.md")).unwrap();
        assert!(prompt.contains("## Applicable feedback rules"), "{prompt}");
        assert!(
            prompt.contains("Search both identifier and call forms"),
            "{prompt}"
        );

        let status: Value = serde_json::from_str(
            &std::fs::read_to_string(mission_dir.join("status.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(status["feedback_rules"]["rules"][0]["id"], json!(rule_id));
    }

    #[tokio::test]
    async fn arena_spawn_normalizes_golden_harness_lanes() {
        let _root = temp_arena_root();
        let server = server();
        let mut open = params("open");
        open.objective = Some("golden lanes".into());
        let opened: Value =
            serde_json::from_str(&handle_tachi_arena(&server, open).await.unwrap()).unwrap();
        let arena_id = opened["arena_id"].as_str().unwrap().to_string();

        let mut spawn = params("spawn");
        spawn.arena_id = Some(arena_id.clone());
        spawn.prompt = Some("brainstorm the design".into());
        spawn.harness = Some("gemini".into());
        let spawned: Value =
            serde_json::from_str(&handle_tachi_arena(&server, spawn).await.unwrap()).unwrap();
        assert_eq!(spawned["harness"], "gemini-advisor");
        assert_eq!(spawned["harness_lane"]["kind"], "advisor");
        assert_eq!(spawned["harness_lane"]["launch_mode"], "advisor_artifact");
        assert!(spawned["tracked_prompt"]
            .as_str()
            .unwrap()
            .contains("Advisor output is captured as an artifact"));
        let prompt = std::fs::read_to_string(spawned["prompt_path"].as_str().unwrap()).unwrap();
        assert!(prompt.contains("Gemini advisor"));
        assert!(prompt.contains("Advisor output is captured as an artifact"));

        let mut spawn = params("spawn");
        spawn.arena_id = Some(arena_id);
        spawn.prompt = Some("unknown harness stays document-only".into());
        spawn.harness = Some("experimental-harness".into());
        let spawned: Value =
            serde_json::from_str(&handle_tachi_arena(&server, spawn).await.unwrap()).unwrap();
        assert_eq!(spawned["harness"], "manual");
        assert_eq!(spawned["requested_harness"], "experimental-harness");
        assert!(spawned["harness_lane"]["command_hint"]
            .as_str()
            .unwrap()
            .contains("Unsupported harness hint"));
    }

    #[tokio::test]
    async fn arena_board_refreshes_external_plan_and_result_writes() {
        let _root = temp_arena_root();
        let server = server();
        let mut open = params("open");
        open.objective = Some("refresh board".into());
        let opened: Value =
            serde_json::from_str(&handle_tachi_arena(&server, open).await.unwrap()).unwrap();
        let arena_id = opened["arena_id"].as_str().unwrap().to_string();

        let mut spawn = params("spawn");
        spawn.arena_id = Some(arena_id.clone());
        spawn.prompt = Some("write files externally".into());
        let spawned: Value =
            serde_json::from_str(&handle_tachi_arena(&server, spawn).await.unwrap()).unwrap();
        let mission_dir = PathBuf::from(spawned["mission_dir"].as_str().unwrap());
        std::fs::write(mission_dir.join("plan.md"), "Plan: external write\n").unwrap();
        std::fs::write(mission_dir.join("result.md"), "Summary: external result\n").unwrap();

        let mut board = params("board");
        board.arena_id = Some(arena_id);
        let board: Value =
            serde_json::from_str(&handle_tachi_arena(&server, board).await.unwrap()).unwrap();
        let mission = &board["result"]["missions"][0];
        assert_eq!(mission["plan_written"], true);
        assert_eq!(mission["result_written"], true);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn arena_spawn_launches_opencode_dispatch_and_collects_result() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _root = temp_arena_root();
        let temp_home = tempfile::tempdir().expect("temp tachi home");
        let fake_bin = tempfile::tempdir().expect("fake bin");
        let opencode_path = fake_bin.path().join("opencode");
        std::fs::write(
            &opencode_path,
            "#!/bin/sh\nprintf '%s\\n' 'Summary: fake opencode completed' 'Files changed: none' 'Commands run: fake opencode' 'Verification performed: fake smoke' 'Remaining risks or blockers: none'\nexit 7\n",
        )
        .expect("write fake opencode");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&opencode_path)
                .expect("fake opencode metadata")
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&opencode_path, perms).expect("chmod fake opencode");
        }
        let _home = EnvGuard::set_path("TACHI_HOME", temp_home.path());
        let path_value = std::env::join_paths(
            std::iter::once(fake_bin.path().to_path_buf()).chain(
                std::env::var_os("PATH")
                    .and_then(|raw| std::env::split_paths(&raw).next().map(|_| raw))
                    .into_iter()
                    .flat_map(|raw| std::env::split_paths(&raw).collect::<Vec<_>>()),
            ),
        )
        .expect("join PATH");
        let _path = EnvGuard::set_os("PATH", path_value);

        let server = server();
        let mut open = params("open");
        open.objective = Some("launch worker".into());
        let opened: Value =
            serde_json::from_str(&handle_tachi_arena(&server, open).await.unwrap()).unwrap();
        let arena_id = opened["arena_id"].as_str().unwrap().to_string();

        let mut spawn = params("spawn");
        spawn.arena_id = Some(arena_id.clone());
        spawn.prompt = Some("run a fake worker".into());
        spawn.harness = Some("opencode".into());
        spawn.role = Some("explore".into());
        spawn.launch = true;
        spawn.timeout_secs = Some(5);
        let spawned: Value =
            serde_json::from_str(&handle_tachi_arena(&server, spawn).await.unwrap()).unwrap();
        let mission_id = spawned["mission_id"].as_str().unwrap().to_string();
        let dispatch_id = spawned["status"]["dispatch_id"]
            .as_str()
            .expect("linked dispatch id")
            .to_string();
        let run_dir = PathBuf::from(
            spawned["status"]["run_dir"]
                .as_str()
                .expect("linked run dir"),
        );
        let spawned_status = spawned["status"]
            .as_object()
            .expect("spawned status object");
        assert!(
            !spawned_status.contains_key("dispatch_response"),
            "arena status must not persist full dispatch response: {spawned_status:#?}"
        );
        assert_eq!(spawned["status"]["dispatch_link"]["redacted"], json!(true));
        assert_eq!(
            spawned["status"]["dispatch_link"]["source"],
            json!("dispatch_response_summary")
        );
        let dispatch_result = wait_for_nonempty_file(&run_dir.join("result.md")).await;
        assert!(dispatch_result.contains("fake opencode completed"));

        let mut board = params("board");
        board.arena_id = Some(arena_id.clone());
        let board: Value =
            serde_json::from_str(&handle_tachi_arena(&server, board).await.unwrap()).unwrap();
        let mission = &board["result"]["missions"][0];
        assert_eq!(mission["dispatch_id"], json!(dispatch_id));
        assert_eq!(mission["linked_dispatch"]["redacted"], json!(true));
        assert_eq!(
            mission["linked_dispatch"]["source"],
            json!("dispatch_run_summary")
        );
        assert!(
            mission["linked_dispatch"].get("status").is_none(),
            "linked dispatch must be a redacted summary: {mission:#}"
        );
        assert_eq!(
            mission["collection_state"],
            json!("pending_collect_from_dispatch")
        );

        let mut collect = params("collect");
        collect.arena_id = Some(arena_id);
        collect.mission_id = Some(mission_id);
        let collected: Value =
            serde_json::from_str(&handle_tachi_arena(&server, collect).await.unwrap()).unwrap();
        assert_eq!(collected["missions"][0]["state"], json!("collected"));
        assert_eq!(
            collected["missions"][0]["result_source"],
            json!("linked_dispatch_result")
        );
        assert_eq!(
            collected["missions"][0]["status"]["result_source"],
            json!("linked_dispatch_result")
        );
        assert!(collected["missions"][0]["result"]
            .as_str()
            .unwrap()
            .contains("fake opencode completed"));
        assert_eq!(
            collected["missions"][0]["completion_draft"]["arguments"]["action"],
            json!("complete")
        );
    }

    #[tokio::test]
    async fn arena_close_blocks_active_missions_until_reaped_or_aborted() {
        let _root = temp_arena_root();
        let server = server();
        let mut open = params("open");
        open.objective = Some("block close".into());
        let opened: Value =
            serde_json::from_str(&handle_tachi_arena(&server, open).await.unwrap()).unwrap();
        let arena_id = opened["arena_id"].as_str().unwrap().to_string();
        let mut spawn = params("spawn");
        spawn.arena_id = Some(arena_id.clone());
        spawn.prompt = Some("stay active".into());
        handle_tachi_arena(&server, spawn).await.unwrap();

        let mut close = params("close");
        close.arena_id = Some(arena_id.clone());
        let blocked: Value =
            serde_json::from_str(&handle_tachi_arena(&server, close).await.unwrap()).unwrap();
        assert_eq!(blocked["state"], "blocked");

        let mut reap = params("reap");
        reap.arena_id = Some(arena_id.clone());
        reap.dry_run = Some(false);
        let reaped: Value =
            serde_json::from_str(&handle_tachi_arena(&server, reap).await.unwrap()).unwrap();
        assert_eq!(reaped["stale_missions"].as_array().unwrap().len(), 1);

        let mut close = params("close");
        close.arena_id = Some(arena_id);
        let closed: Value =
            serde_json::from_str(&handle_tachi_arena(&server, close).await.unwrap()).unwrap();
        assert_eq!(closed["state"], "closed");
    }

    #[test]
    fn arena_ids_reject_traversal() {
        for invalid in ["../../x", "arena_../x", "arena_bad/name", "notarena_x"] {
            assert!(validate_arena_id(invalid).is_err(), "{invalid}");
        }
        assert!(validate_arena_id("arena_20260606T000000Z_demo_deadbeef").is_ok());
        assert!(validate_mission_id("mission_explore_deadbeef").is_ok());
        let err = validate_mission_id("bad/name").unwrap_err();
        assert!(err.contains("Expected prefix 'mission_'"));
    }

    #[test]
    #[cfg(unix)]
    fn append_event_writes_synced_and_owner_only_file() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        append_event(tmp.path(), json!({"event": "test"})).unwrap();

        let path = tmp.path().join("events.jsonl");
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("\"event\":\"test\""));

        let meta = std::fs::metadata(&path).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "events.jsonl should be owner-readable only");
    }
}
