//! `tachi_shell` — skill-gated flow orchestration facade.
//!
//! Implements the MVP described in `wiki/agent/tachi/Tachi-Shell-重构计划-2026-05-04.md`.
//!
//! Each `tachi_shell(action=...)` call is a thin coordinator that:
//!   1. Resolves / creates a `flow_id` and its run directory.
//!   2. Injects the meta skill SOP file required for the stage.
//!   3. Writes / updates `instruction.md`, `status.json`, `events.jsonl`.
//!   4. For `kanban` / `status` it delegates to existing read-only handlers.
//!   5. For `dispatch` it can optionally hand off to the existing
//!      `dispatch_ops::handle_tachi_dispatch` (Phase 4 hook).
//!
//! This module deliberately does **not** re-implement clanker dispatch,
//! kanban, or skill discovery — it composes existing infra.

use crate::{
    MemoryServer, TachiBoardParams, TachiDispatchParams, TachiShellDispatchSliceParams,
    TachiShellParams,
};
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

// ─── Stage / meta-skill mapping ──────────────────────────────────────────────

/// MVP hardcoded stage → meta skill SOP file path (relative to repo root).
fn meta_skill_for_stage(stage: &str) -> Option<&'static str> {
    match stage {
        "brainstorm" => Some("skill/superpowers/skills/brainstorming/SKILL.md"),
        "plan" => Some("skill/superpowers/skills/writing-plans/SKILL.md"),
        "dispatch" => Some("skill/superpowers/skills/executing-plans/SKILL.md"),
        "review" => Some("skill/superpowers/skills/requesting-code-review/SKILL.md"),
        "ship" => Some("skill/superpowers/skills/finishing-a-development-branch/SKILL.md"),
        _ => None,
    }
}

/// Stages that bear a skill gate and create/advance a flow run.
const STAGE_ACTIONS: &[&str] = &["brainstorm", "plan", "dispatch", "review", "ship"];

// ─── Run-root resolution ─────────────────────────────────────────────────────

fn cached_git_root() -> Option<&'static PathBuf> {
    static GIT_ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
    GIT_ROOT
        .get_or_init(|| {
            std::process::Command::new("git")
                .args(["rev-parse", "--show-toplevel"])
                .output()
                .ok()
                .filter(|out| out.status.success())
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        })
        .as_ref()
}

/// Resolve the runs root directory.
///
/// Order:
/// 1. `$TACHI_RUN_ROOT`
/// 2. `<repo_root>/.tachi/runs/` if a git repo is detected via `git rev-parse --show-toplevel`
/// 3. `$TACHI_HOME/runs/`
/// 4. `$HOME/.tachi/runs/`
/// 5. `<temp>/tachi/runs/`
pub(crate) fn shell_runs_root() -> PathBuf {
    if let Ok(p) = std::env::var("TACHI_RUN_ROOT") {
        return PathBuf::from(p);
    }
    if let Some(root) = cached_git_root() {
        return root.join(".tachi").join("runs");
    }
    if let Ok(home) = std::env::var("TACHI_HOME") {
        return PathBuf::from(home).join("runs");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".tachi").join("runs");
    }
    std::env::temp_dir().join("tachi").join("runs")
}

fn slugify(s: &str) -> String {
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
        "flow".to_string()
    } else {
        trimmed
    }
}

fn new_flow_id(title: Option<&str>, task: Option<&str>) -> String {
    let now = Utc::now();
    let stamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    let basis = title
        .or(task)
        .map(|s| s.to_string())
        .unwrap_or_else(|| "flow".to_string());
    let suffix = uuid::Uuid::new_v4().as_simple().to_string()[..8].to_string();
    format!("flow_{}_{}_{}", stamp, slugify(&basis), suffix)
}

pub(crate) fn validate_flow_id(id: &str) -> Result<(), String> {
    if !id.starts_with("flow_")
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "Invalid flow_id: '{}'. Expected a safe id starting with 'flow_' and containing only ASCII letters, numbers, '_' or '-'. Example: flow_20260609T014037Z_tachi_dispatch_ux_smoke",
            id
        ));
    }
    Ok(())
}

pub(crate) fn run_dir_for_flow_id(flow_id: &str) -> Result<PathBuf, String> {
    validate_flow_id(flow_id)?;
    Ok(shell_runs_root().join(flow_id))
}

fn validate_slice_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!("Invalid convoy slice id: '{}'", id));
    }
    Ok(())
}

// ─── Status / events helpers ─────────────────────────────────────────────────

fn read_status(run_dir: &Path) -> Value {
    let status_path = run_dir.join("status.json");
    match std::fs::read_to_string(&status_path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|_| json!({})),
        Err(_) => json!({}),
    }
}

fn write_status(run_dir: &Path, status: &Value) -> Result<(), String> {
    let status_path = run_dir.join("status.json");
    let serialized =
        serde_json::to_string_pretty(status).map_err(|e| format!("serialize status.json: {e}"))?;
    // Atomic write: write to temp file then rename to avoid corruption on crash
    let tmp_path = status_path.with_extension("json.tmp");
    std::fs::write(&tmp_path, serialized).map_err(|e| format!("write status.json.tmp: {e}"))?;
    std::fs::rename(&tmp_path, &status_path).map_err(|e| format!("rename status.json.tmp: {e}"))?;
    Ok(())
}

fn append_event(run_dir: &Path, event: Value) -> Result<(), String> {
    use std::io::Write;
    let path = run_dir.join("events.jsonl");
    let line = serde_json::to_string(&event).map_err(|e| format!("serialize event: {e}"))?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("open events.jsonl: {e}"))?;
    writeln!(f, "{}", line).map_err(|e| format!("write events.jsonl: {e}"))?;
    Ok(())
}

// ─── Meta skill injection ────────────────────────────────────────────────────

/// Resolve the meta skill SOP file, falling back through several roots.
fn resolve_meta_skill(rel_path: &str) -> Option<PathBuf> {
    // 1. repo root (git toplevel)
    if let Some(root) = cached_git_root() {
        let p = root.join(rel_path);
        if p.exists() {
            return Some(p);
        }
    }
    // 2. cwd
    let p = PathBuf::from(rel_path);
    if p.exists() {
        return Some(p);
    }
    // 3. cargo manifest dir (for tests)
    if let Ok(d) = std::env::var("CARGO_MANIFEST_DIR") {
        let p = PathBuf::from(d).join("..").join("..").join(rel_path);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

#[derive(Debug)]
struct InjectionResult {
    required: bool,
    rel_path: Option<String>,
    source_path: Option<String>,
    injected_path: Option<String>,
    content_hash: Option<String>,
    loaded: bool,
    warning: Option<String>,
}

fn inject_meta_skill(stage: &str, run_dir: &Path) -> InjectionResult {
    let rel = match meta_skill_for_stage(stage) {
        Some(r) => r,
        None => {
            return InjectionResult {
                required: false,
                rel_path: None,
                source_path: None,
                injected_path: None,
                content_hash: None,
                loaded: false,
                warning: None,
            };
        }
    };
    let injected_dir = run_dir.join("injected");
    if let Err(e) = std::fs::create_dir_all(&injected_dir) {
        return InjectionResult {
            required: true,
            rel_path: Some(rel.to_string()),
            source_path: None,
            injected_path: None,
            content_hash: None,
            loaded: false,
            warning: Some(format!("create injected dir failed: {e}")),
        };
    }
    let resolved = match resolve_meta_skill(rel) {
        Some(p) => p,
        None => {
            return InjectionResult {
                required: true,
                rel_path: Some(rel.to_string()),
                source_path: None,
                injected_path: None,
                content_hash: None,
                loaded: false,
                warning: Some(format!(
                    "meta skill file '{}' not found in any known root",
                    rel
                )),
            };
        }
    };
    let bytes = match std::fs::read(&resolved) {
        Ok(b) => b,
        Err(e) => {
            return InjectionResult {
                required: true,
                rel_path: Some(rel.to_string()),
                source_path: Some(resolved.to_string_lossy().to_string()),
                injected_path: None,
                content_hash: None,
                loaded: false,
                warning: Some(format!("read meta skill failed: {e}")),
            };
        }
    };
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    let digest = format!("{:016x}", hasher.finish());
    // Flatten path: `superpowers-<stage>.md`
    let basename = format!("superpowers-{}.md", stage);
    let target = injected_dir.join(&basename);
    if let Err(e) = std::fs::write(&target, &bytes) {
        return InjectionResult {
            required: true,
            rel_path: Some(rel.to_string()),
            source_path: Some(resolved.to_string_lossy().to_string()),
            injected_path: None,
            content_hash: Some(digest),
            loaded: false,
            warning: Some(format!("write injected meta skill failed: {e}")),
        };
    }
    InjectionResult {
        required: true,
        rel_path: Some(rel.to_string()),
        source_path: Some(resolved.to_string_lossy().to_string()),
        injected_path: Some(target.to_string_lossy().to_string()),
        content_hash: Some(digest),
        loaded: true,
        warning: None,
    }
}

// (removed hex_encode helper — DefaultHasher used inline above for content fingerprinting.)

// ─── instruction.md generation ───────────────────────────────────────────────

fn build_instruction_md(
    flow_id: &str,
    stage: &str,
    task: &str,
    injection: &InjectionResult,
    notes: Option<&str>,
    validation: &[String],
    allowed_scope: &[String],
) -> String {
    let mut s = String::new();
    s.push_str(&format!("# Tachi Flow Instruction — {}\n\n", flow_id));
    s.push_str(&format!("Stage: **{}**\n\n", stage));
    s.push_str("## Task\n\n");
    s.push_str(task.trim());
    s.push_str("\n\n");

    let lifecycle_skills = crate::skill_policy::shell_stage_skills(stage);
    s.push_str("## Native Lifecycle Policy\n\n");
    s.push_str(
        "- Treat Superpowers and Waza as native workflow gates, not optional Hub suggestions.\n",
    );
    s.push_str("- Leader owns mode choice, worker slicing, integration, final verification, and the user-facing completion claim.\n");
    s.push_str("- Use native child agents or external lanes only for bounded, independent, verifiable work; max 6 concurrent workers.\n");
    s.push_str("- Give every worker goal, allowed scope, forbidden scope when relevant, required skills, validation commands, and report-back contract.\n");
    s.push_str("- Keep dependent work serial: plan-before-code, same-file edits, chained transforms, and reviewer gates.\n");
    s.push_str("- Inject MCP/tool permissions by worker role/profile; fail or report impact when required MCP access is missing.\n");
    s.push_str("- Workers must report back; child output is draft evidence until the leader reviews and verifies it.\n\n");
    if lifecycle_skills.is_empty() {
        s.push_str("Required leader skills: (none)\n\n");
    } else {
        s.push_str("Required leader skills:\n");
        for skill in &lifecycle_skills {
            s.push_str(&format!("- `{}`\n", skill));
        }
        s.push('\n');
    }

    s.push_str("## Required Reading (Injected SOP)\n\n");
    if let Some(p) = injection.injected_path.as_deref() {
        s.push_str(&format!("- `{}`", p));
        if let Some(rel) = injection.rel_path.as_deref() {
            s.push_str(&format!(" (source: `{}`)", rel));
        }
        if let Some(hash) = injection.content_hash.as_deref() {
            s.push_str(&format!(" fingerprint={}", &hash[..16]));
        }
        s.push('\n');
    } else if injection.required {
        s.push_str(&format!(
            "- WARNING: required meta skill `{}` could not be loaded: {}\n",
            injection.rel_path.as_deref().unwrap_or("?"),
            injection.warning.as_deref().unwrap_or("unknown error")
        ));
    } else {
        s.push_str("- (no meta skill required for this stage)\n");
    }
    s.push('\n');

    s.push_str("## Allowed Scope\n\n");
    if allowed_scope.is_empty() {
        s.push_str("- (unspecified — keep changes minimal and reversible)\n");
    } else {
        for a in allowed_scope {
            s.push_str(&format!("- {}\n", a));
        }
    }
    s.push('\n');

    s.push_str("## Validation Commands\n\n");
    if validation.is_empty() {
        match stage {
            "ship" | "dispatch" | "review" => {
                s.push_str("- `cargo test -p memory-server`\n");
            }
            _ => {
                s.push_str("- (none specified)\n");
            }
        }
    } else {
        for v in validation {
            s.push_str(&format!("- `{}`\n", v));
        }
    }
    s.push('\n');

    if stage == "ship" {
        s.push_str("## Release Flow\n\n");
        s.push_str("Follow this PR-first release sequence unless the human explicitly authorizes a direct push to the protected branch:\n\n");
        s.push_str("1. Finish the feature branch.\n");
        s.push_str("2. Run tests and required verification.\n");
        s.push_str("3. Push the feature branch.\n");
        s.push_str("4. Open a PR.\n");
        s.push_str("5. Pass the PR gate: CI checks and review gate.\n");
        s.push_str("6. Merge the PR.\n");
        s.push_str("7. Deploy / release.\n\n");
    }

    s.push_str("## Expected Outputs\n\n");
    s.push_str("Write all artifacts under the run directory:\n\n");
    s.push_str("- `result.md` — what was done, decisions taken, blockers\n");
    s.push_str("- `validation.md` — verification evidence (logs, test summaries)\n");
    s.push_str("- `status.json` — updated status (handled by tachi_shell when stage advances)\n");
    s.push_str("- `events.jsonl` — append-only event log\n");
    s.push_str("- `artifacts/` — anything else (diffs, gitleaks output, PR body, etc.)\n\n");

    if let Some(n) = notes {
        s.push_str("## Notes\n\n");
        s.push_str(n.trim());
        s.push_str("\n\n");
    }

    s
}

// ─── Action handlers ─────────────────────────────────────────────────────────

/// Top-level dispatcher used by `MemoryServer::tachi_shell`.
pub(crate) async fn handle_tachi_shell(
    server: &MemoryServer,
    params: TachiShellParams,
) -> Result<String, String> {
    let action = params.action.to_ascii_lowercase();
    match action.as_str() {
        "brainstorm" | "plan" | "review" | "ship" => {
            handle_stage_action(server, &action, params).await
        }
        "dispatch" => handle_dispatch_action(server, params).await,
        "kanban" => handle_kanban_action(server, params).await,
        "status" => handle_status_action(params).await,
        _ => Err(format!(
            "Invalid action '{}'. Use 'brainstorm', 'plan', 'dispatch', 'kanban', 'status', 'review', or 'ship'.",
            params.action
        )),
    }
}

async fn handle_stage_action(
    _server: &MemoryServer,
    stage: &str,
    params: TachiShellParams,
) -> Result<String, String> {
    let task = params
        .task
        .clone()
        .ok_or_else(|| format!("'task' is required for action='{}'", stage))?;
    let (flow_id, run_dir, created) = resolve_or_create_flow(&params, &task)?;
    let injection = inject_meta_skill(stage, &run_dir);

    let instruction = build_instruction_md(
        &flow_id,
        stage,
        &task,
        &injection,
        params.notes.as_deref(),
        &params.validation,
        &params.allowed_scope,
    );
    let instr_path = run_dir.join("instruction.md");
    std::fs::write(&instr_path, instruction).map_err(|e| format!("write instruction.md: {e}"))?;

    advance_stage(&run_dir, &flow_id, stage, &task, &injection, created)?;
    let required_skills = crate::skill_policy::shell_stage_skills(stage);

    let resp = json!({
        "flow_id": flow_id,
        "stage": stage,
        "run_dir": run_dir.to_string_lossy(),
        "instruction_path": instr_path.to_string_lossy(),
        "injected_skill": injection_to_json(&injection),
        "native_skill_policy": crate::skill_policy::native_policy_summary(stage, &required_skills),
        "async": false,
        "created": created,
    });
    serde_json::to_string(&resp).map_err(|e| format!("serialize: {e}"))
}

async fn handle_dispatch_action(
    server: &MemoryServer,
    params: TachiShellParams,
) -> Result<String, String> {
    let task = params
        .task
        .clone()
        .ok_or_else(|| "'task' is required for action='dispatch'".to_string())?;
    let (flow_id, run_dir, created) = resolve_or_create_flow(&params, &task)?;
    let injection = inject_meta_skill("dispatch", &run_dir);

    let instruction = build_instruction_md(
        &flow_id,
        "dispatch",
        &task,
        &injection,
        params.notes.as_deref(),
        &params.validation,
        &params.allowed_scope,
    );
    let instr_path = run_dir.join("instruction.md");
    std::fs::write(&instr_path, &instruction).map_err(|e| format!("write instruction.md: {e}"))?;

    advance_stage(&run_dir, &flow_id, "dispatch", &task, &injection, created)?;
    let required_skills = crate::skill_policy::shell_stage_skills("dispatch");

    if !params.slices.is_empty() {
        return handle_convoy_dispatch_action(
            server,
            params,
            &flow_id,
            &run_dir,
            created,
            &injection,
            &instr_path,
        )
        .await;
    }

    // Phase 4 hook: optionally invoke the existing async dispatcher.
    let mut dispatch_id: Option<String> = None;
    let mut dispatch_error: Option<String> = None;
    let mut async_fired = false;
    if params.async_dispatch {
        let agent = params.agent.clone().or_else(|| {
            if params.profile.is_some() {
                None
            } else {
                Some("claude".to_string())
            }
        });
        // Prefix the subagent prompt with a pointer to the instruction packet
        // so the clanker reads from disk rather than chat context.
        let prompt = format!(
            "You are executing Tachi flow `{flow_id}`, stage `dispatch`.\n\n\
             Read and follow these injected SOP files before changing code:\n\
             - {injected}\n\n\
             Then read the full instruction packet:\n\
             - {instr}\n\n\
             Original task:\n\n{task}\n",
            flow_id = flow_id,
            injected = injection.injected_path.as_deref().unwrap_or("(none)"),
            instr = instr_path.to_string_lossy(),
            task = task,
        );
        let dp = TachiDispatchParams {
            agent,
            profile: params.profile.clone(),
            task: prompt,
            cwd: params.cwd.clone(),
            skills: Vec::new(),
            context_query: None,
            model: None,
            timeout_secs: 600,
            permission_profile: None,
            allowed_tools: Vec::new(),
            max_turns: None,
            sandbox: None,
            inject_tachi_mcp: None,
            inject_hub_mcps: None,
            command: Vec::new(),
            harness_transport: None,
            harness_server_url: None,
            project: params.project.clone(),
            stage: Some("execute".to_string()),
            credential_profiles: Vec::new(),
            issue_ref: None,
            pr_ref: None,
            flow_id: Some(flow_id.to_string()),
            tool_profile: params.tool_profile.clone(),
            auto_capability_bundle: None,
            mcp_access: params.mcp_access.clone(),
            allowed_mcp_servers: params.allowed_mcp_servers.clone(),
        };
        match crate::dispatch_ops::handle_tachi_dispatch(server, dp).await {
            Ok(s) => {
                async_fired = true;
                if let Ok(v) = serde_json::from_str::<Value>(&s) {
                    if let Some(d) = v.get("dispatch_id").and_then(|d| d.as_str()) {
                        dispatch_id = Some(d.to_string());
                    }
                }
                // Append dispatch_id to flow status
                if let Some(d) = dispatch_id.as_deref() {
                    let mut status = read_status(&run_dir);
                    let arr = status
                        .as_object_mut()
                        .map(|o| o.entry("dispatch_ids").or_insert_with(|| json!([])));
                    if let Some(v) = arr {
                        if let Some(a) = v.as_array_mut() {
                            a.push(json!(d));
                        }
                    }
                    let _ = write_status(&run_dir, &status);
                    let _ = append_event(
                        &run_dir,
                        json!({
                            "event": "dispatch_spawned",
                            "flow_id": flow_id,
                            "dispatch_id": d,
                            "timestamp": Utc::now().to_rfc3339(),
                        }),
                    );
                }
            }
            Err(e) => {
                dispatch_error = Some(e);
            }
        }
    }

    let resp = json!({
        "flow_id": flow_id,
        "stage": "dispatch",
        "run_dir": run_dir.to_string_lossy(),
        "instruction_path": instr_path.to_string_lossy(),
        "injected_skill": injection_to_json(&injection),
        "native_skill_policy": crate::skill_policy::native_policy_summary("dispatch", &required_skills),
        "async": async_fired,
        "dispatch_id": dispatch_id,
        "dispatch_error": dispatch_error,
        "created": created,
    });
    serde_json::to_string(&resp).map_err(|e| format!("serialize: {e}"))
}

async fn handle_convoy_dispatch_action(
    server: &MemoryServer,
    params: TachiShellParams,
    flow_id: &str,
    run_dir: &Path,
    created: bool,
    injection: &InjectionResult,
    parent_instr_path: &Path,
) -> Result<String, String> {
    let parent_task = params
        .task
        .as_deref()
        .ok_or_else(|| "'task' is required for action='dispatch'".to_string())?;
    let convoy_superpowers = vec![
        crate::skill_policy::SUPERPOWER_SUBAGENT_DRIVEN_DEVELOPMENT.to_string(),
        crate::skill_policy::SUPERPOWER_EXECUTING_PLANS.to_string(),
        crate::skill_policy::SUPERPOWER_REQUESTING_CODE_REVIEW.to_string(),
    ];
    let parent_worker_skills = crate::skill_policy::worker_skills_for_convoy_slice(parent_task, "");
    let mut convoy_worker_skills = convoy_superpowers.clone();
    convoy_worker_skills.extend(parent_worker_skills);
    crate::skill_policy::dedupe_preserve_order(&mut convoy_worker_skills);
    let mut parent_instruction = build_instruction_md(
        flow_id,
        "dispatch",
        parent_task,
        injection,
        params.notes.as_deref(),
        &params.validation,
        &params.allowed_scope,
    );
    parent_instruction.push_str("## Required Worker Skills\n\n");
    for contract in &convoy_worker_skills {
        parent_instruction.push_str(&format!("- `{}`\n", contract));
    }
    parent_instruction.push('\n');
    parent_instruction.push_str("## Worker Factory Contract\n\n");
    parent_instruction.push_str("- Split only independent, bounded, verifiable slices; keep dependent or same-file work serial.\n");
    parent_instruction.push_str("- Prefer read-only sidecars for inventory/review, and writable workers only with explicit scope.\n");
    parent_instruction.push_str(
        "- Require each worker to report back; leader reviews results before integration.\n\n",
    );
    std::fs::write(parent_instr_path, parent_instruction)
        .map_err(|e| format!("write convoy parent instruction.md: {e}"))?;
    let mut seen = std::collections::HashSet::new();
    let mut slice_records = Vec::new();
    let mut dispatch_ids = Vec::new();
    let mut async_fired = false;

    for (idx, slice) in params.slices.iter().enumerate() {
        let slice_id = resolve_slice_id(idx, slice)?;
        if !seen.insert(slice_id.clone()) {
            return Err(format!("Duplicate convoy slice id: '{}'", slice_id));
        }

        let slice_task = slice
            .task
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(parent_task);
        let slice_worker_skills =
            crate::skill_policy::worker_skills_for_convoy_slice(parent_task, slice_task);
        let slice_agent = slice
            .agent
            .clone()
            .or_else(|| params.agent.clone())
            .or_else(|| {
                if slice.profile.is_some() || params.profile.is_some() {
                    None
                } else {
                    Some("claude".to_string())
                }
            });
        let slice_profile = slice.profile.clone().or_else(|| params.profile.clone());
        let slice_cwd = slice.cwd.clone().or_else(|| params.cwd.clone());
        let slice_tool_profile = slice
            .tool_profile
            .clone()
            .or_else(|| params.tool_profile.clone());
        let slice_mcp_access = slice
            .mcp_access
            .clone()
            .or_else(|| params.mcp_access.clone());
        let slice_allowed_mcp_servers = if slice.allowed_mcp_servers.is_empty() {
            params.allowed_mcp_servers.clone()
        } else {
            slice.allowed_mcp_servers.clone()
        };
        let slice_validation = if slice.validation.is_empty() {
            params.validation.clone()
        } else {
            slice.validation.clone()
        };
        let slice_allowed_scope = if slice.allowed_scope.is_empty() {
            params.allowed_scope.clone()
        } else {
            slice.allowed_scope.clone()
        };
        let slice_notes = slice.notes.as_deref().or(params.notes.as_deref());
        let slice_dir = run_dir.join("slices").join(&slice_id);
        std::fs::create_dir_all(slice_dir.join("artifacts"))
            .map_err(|e| format!("create convoy slice dir: {e}"))?;

        let task_packet = match slice.title.as_deref() {
            Some(title) if !title.trim().is_empty() => {
                format!(
                    "Convoy slice `{}` — {}\n\n{}",
                    slice_id,
                    title.trim(),
                    slice_task
                )
            }
            _ => format!("Convoy slice `{}`\n\n{}", slice_id, slice_task),
        };
        let mut instruction = build_instruction_md(
            flow_id,
            "dispatch",
            &task_packet,
            injection,
            slice_notes,
            &slice_validation,
            &slice_allowed_scope,
        );
        instruction.push_str("## Required Worker Skills\n\n");
        for contract in &slice_worker_skills {
            instruction.push_str(&format!("- `{}`\n", contract));
        }
        instruction.push('\n');
        instruction.push_str("## Worker Report-Back Contract\n\n");
        instruction.push_str("- Start final output with `Using skills: <ids>`.\n");
        instruction.push_str("- Report changed files, verification commands and outcomes, blockers, and recommended handoff.\n");
        instruction.push_str("- Do not claim the parent flow is complete; the leader owns integration and final verification.\n\n");
        let slice_instr_path = slice_dir.join("instruction.md");
        std::fs::write(&slice_instr_path, instruction)
            .map_err(|e| format!("write convoy slice instruction.md: {e}"))?;

        append_event(
            run_dir,
            json!({
                "event": "convoy_slice_prepared",
                "flow_id": flow_id,
                "slice_id": slice_id,
                "agent": slice_agent.clone(),
                "profile": slice_profile.clone(),
                "cwd": slice_cwd,
                "instruction_path": slice_instr_path.to_string_lossy(),
                "timestamp": Utc::now().to_rfc3339(),
            }),
        )?;

        let mut dispatch_id = None;
        let mut dispatch_error = None;
        if params.async_dispatch {
            let prompt = format!(
                "You are executing Tachi flow `{flow_id}`, convoy slice `{slice_id}`.\n\n\
                 Read and follow these injected SOP files before changing code:\n\
                 - {injected}\n\n\
                 Then read the full slice instruction packet:\n\
                 - {instr}\n\n\
                 Parent flow instruction packet:\n\
                 - {parent_instr}\n\n\
                 Original slice task:\n\n{task}\n",
                flow_id = flow_id,
                slice_id = slice_id,
                injected = injection.injected_path.as_deref().unwrap_or("(none)"),
                instr = slice_instr_path.to_string_lossy(),
                parent_instr = parent_instr_path.to_string_lossy(),
                task = slice_task,
            );
            let dp = TachiDispatchParams {
                agent: slice_agent.clone(),
                profile: slice_profile.clone(),
                task: prompt,
                cwd: slice_cwd.clone(),
                skills: Vec::new(),
                context_query: None,
                model: None,
                timeout_secs: 600,
                permission_profile: None,
                allowed_tools: Vec::new(),
                max_turns: None,
                sandbox: None,
                inject_tachi_mcp: None,
                inject_hub_mcps: None,
                command: Vec::new(),
                harness_transport: None,
                harness_server_url: None,
                project: params.project.clone(),
                stage: Some(format!("execute:{}", slice_id)),
                credential_profiles: Vec::new(),
                issue_ref: None,
                pr_ref: None,
                flow_id: Some(flow_id.to_string()),
                tool_profile: slice_tool_profile.clone(),
                auto_capability_bundle: None,
                mcp_access: slice_mcp_access.clone(),
                allowed_mcp_servers: slice_allowed_mcp_servers.clone(),
            };
            match crate::dispatch_ops::handle_tachi_dispatch(server, dp).await {
                Ok(s) => {
                    async_fired = true;
                    if let Ok(v) = serde_json::from_str::<Value>(&s) {
                        if let Some(d) = v.get("dispatch_id").and_then(|d| d.as_str()) {
                            dispatch_ids.push(d.to_string());
                            dispatch_id = Some(d.to_string());
                        }
                    }
                }
                Err(e) => {
                    dispatch_error = Some(e);
                }
            }
        }

        if let Some(d) = dispatch_id.as_deref() {
            append_event(
                run_dir,
                json!({
                    "event": "convoy_dispatch_spawned",
                    "flow_id": flow_id,
                    "slice_id": slice_id,
                    "dispatch_id": d,
                    "timestamp": Utc::now().to_rfc3339(),
                }),
            )?;
        }

        slice_records.push(json!({
            "slice_id": slice_id,
            "title": slice.title,
            "agent": slice_agent.clone(),
            "profile": slice_profile.clone(),
            "tool_profile": slice_tool_profile.clone(),
            "cwd": slice_cwd,
            "instruction_path": slice_instr_path.to_string_lossy(),
            "required_superpowers": convoy_superpowers.clone(),
            "required_worker_skills": slice_worker_skills,
            "dispatch_id": dispatch_id,
            "dispatch_error": dispatch_error,
        }));
    }

    let mut status = read_status(run_dir);
    if let Some(obj) = status.as_object_mut() {
        let arr = obj.entry("dispatch_ids").or_insert_with(|| json!([]));
        if let Some(a) = arr.as_array_mut() {
            for d in &dispatch_ids {
                a.push(json!(d));
            }
        }
        obj.insert(
            "convoy".to_string(),
            json!({
                "mode": "parallel",
                "slice_count": slice_records.len(),
                "required_superpowers": convoy_superpowers.clone(),
                "required_worker_skills": convoy_worker_skills.clone(),
                "native_skill_policy": crate::skill_policy::native_policy_summary("dispatch", &convoy_worker_skills),
                "slices": slice_records,
                "updated_at": Utc::now().to_rfc3339(),
            }),
        );
    }
    write_status(run_dir, &status)?;

    let resp = json!({
        "flow_id": flow_id,
        "stage": "dispatch",
        "run_dir": run_dir.to_string_lossy(),
        "instruction_path": parent_instr_path.to_string_lossy(),
        "injected_skill": injection_to_json(injection),
        "native_skill_policy": crate::skill_policy::native_policy_summary("dispatch", &convoy_worker_skills),
        "async": async_fired,
        "convoy": true,
        "dispatch_ids": dispatch_ids,
        "slices": status.get("convoy").and_then(|v| v.get("slices")).cloned().unwrap_or_else(|| json!([])),
        "created": created,
    });
    serde_json::to_string(&resp).map_err(|e| format!("serialize: {e}"))
}

fn resolve_slice_id(idx: usize, slice: &TachiShellDispatchSliceParams) -> Result<String, String> {
    let basis = slice
        .id
        .as_deref()
        .or(slice.title.as_deref())
        .or(slice.task.as_deref())
        .unwrap_or("slice");
    let mut id = slugify(basis);
    if id == "flow" || id == "slice" {
        id = format!("slice-{}", idx + 1);
    }
    validate_slice_id(&id)?;
    Ok(id)
}

async fn handle_kanban_action(
    server: &MemoryServer,
    params: TachiShellParams,
) -> Result<String, String> {
    let bp = TachiBoardParams {
        state_filter: params.state_filter.clone(),
        limit: params.limit,
        project: params.project.clone(),
        flow_id: params.flow_id.clone(),
    };
    crate::dispatch_ops::handle_tachi_board(server, bp).await
}

async fn handle_status_action(params: TachiShellParams) -> Result<String, String> {
    let runs_root = shell_runs_root();
    if let Some(flow_id) = params.flow_id.as_deref() {
        validate_flow_id(flow_id)?;
        let run_dir = runs_root.join(flow_id);
        if !run_dir.exists() {
            return serde_json::to_string(&json!({
                "flow_id": flow_id,
                "found": false,
                "message": "no run directory exists for this flow_id",
            }))
            .map_err(|e| format!("serialize: {e}"));
        }
        let status = read_status(&run_dir);
        return serde_json::to_string(&json!({
            "flow_id": flow_id,
            "found": true,
            "run_dir": run_dir.to_string_lossy(),
            "status": status,
        }))
        .map_err(|e| format!("serialize: {e}"));
    }
    // List recent flows
    let limit = params.limit.unwrap_or(20);
    let mut flows = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(&runs_root) {
        for entry in read_dir.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("flow_") {
                continue;
            }
            let status = read_status(&entry.path());
            flows.push(json!({
                "flow_id": name,
                "stage": status.get("stage"),
                "state": status.get("state"),
                "updated_at": status.get("updated_at"),
            }));
        }
    }
    flows.sort_by(|a, b| {
        let ta = a.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
        let tb = b.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
        tb.cmp(ta)
    });
    flows.truncate(limit);
    serde_json::to_string(&json!({
        "flows": flows,
        "count": flows.len(),
        "runs_root": runs_root.to_string_lossy(),
    }))
    .map_err(|e| format!("serialize: {e}"))
}

// ─── Helpers: flow lifecycle ─────────────────────────────────────────────────

fn resolve_or_create_flow(
    params: &TachiShellParams,
    task: &str,
) -> Result<(String, PathBuf, bool), String> {
    let runs_root = shell_runs_root();
    std::fs::create_dir_all(&runs_root).map_err(|e| format!("create runs root: {e}"))?;
    if let Some(fid) = params.flow_id.clone() {
        validate_flow_id(&fid)?;
        let run_dir = runs_root.join(&fid);
        let created = !run_dir.exists();
        std::fs::create_dir_all(&run_dir).map_err(|e| format!("create flow run dir: {e}"))?;
        std::fs::create_dir_all(run_dir.join("artifacts")).ok();
        return Ok((fid, run_dir, created));
    }
    let fid = new_flow_id(params.title.as_deref(), Some(task));
    let run_dir = runs_root.join(&fid);
    std::fs::create_dir_all(&run_dir).map_err(|e| format!("create flow run dir: {e}"))?;
    std::fs::create_dir_all(run_dir.join("artifacts")).ok();
    Ok((fid, run_dir, true))
}

fn injection_to_json(inj: &InjectionResult) -> Value {
    json!({
        "required": inj.required,
        "rel_path": inj.rel_path,
        "source_path": inj.source_path,
        "injected_path": inj.injected_path,
        "content_hash": inj.content_hash,
        "loaded": inj.loaded,
        "warning": inj.warning,
    })
}

fn advance_stage(
    run_dir: &Path,
    flow_id: &str,
    stage: &str,
    task: &str,
    injection: &InjectionResult,
    created: bool,
) -> Result<(), String> {
    let now = Utc::now().to_rfc3339();
    let mut status = read_status(run_dir);
    let obj = status.as_object_mut();
    let mut new_status: serde_json::Map<String, Value> = match obj {
        Some(o) => o.clone(),
        None => serde_json::Map::new(),
    };
    if created || !new_status.contains_key("flow_id") {
        new_status.insert("flow_id".into(), json!(flow_id));
        new_status.insert("created_at".into(), json!(now));
        new_status.insert("task".into(), json!(task));
        new_status.insert("dispatch_ids".into(), json!([]));
        new_status.insert("history".into(), json!([]));
    }
    let prev_stage = new_status
        .get("stage")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    new_status.insert("stage".into(), json!(stage));
    new_status.insert("state".into(), json!(stage_state_for(stage)));
    new_status.insert("updated_at".into(), json!(now));
    new_status.insert(
        "injected".into(),
        json!({
            "stage": stage,
            "rel_path": injection.rel_path,
            "injected_path": injection.injected_path,
            "content_hash": injection.content_hash,
            "loaded": injection.loaded,
            "warning": injection.warning,
        }),
    );
    if let Some(arr) = new_status.get_mut("history").and_then(|v| v.as_array_mut()) {
        arr.push(json!({
            "stage": stage,
            "from": prev_stage,
            "at": now,
        }));
    }
    write_status(run_dir, &Value::Object(new_status))?;
    let event_kind = if created {
        "flow_created"
    } else {
        "stage_entered"
    };
    append_event(
        run_dir,
        json!({
            "event": event_kind,
            "flow_id": flow_id,
            "stage": stage,
            "from_stage": prev_stage,
            "timestamp": now,
            "injected": injection.injected_path,
        }),
    )?;
    let _ = STAGE_ACTIONS; // touch to silence dead-code if list trims later
    Ok(())
}

fn stage_state_for(stage: &str) -> &'static str {
    match stage {
        "brainstorm" | "plan" | "review" | "ship" => "instruction_ready",
        "dispatch" => "dispatch_ready",
        _ => "unknown",
    }
}

mod shell_github;
pub(crate) use shell_github::*;

/// Global test lock for `TACHI_RUN_ROOT` env var mutations.
/// All test modules that set this env var must acquire this lock to avoid races.
#[cfg(test)]
pub(crate) fn tachi_run_root_env_lock() -> &'static std::sync::Mutex<()> {
    crate::utils::global_test_lock()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn runs_env_lock() -> &'static std::sync::Mutex<()> {
        tachi_run_root_env_lock()
    }

    struct RunsRootGuard {
        _guard: std::sync::MutexGuard<'static, ()>,
        path: PathBuf,
    }

    impl std::ops::Deref for RunsRootGuard {
        type Target = PathBuf;
        fn deref(&self) -> &PathBuf {
            &self.path
        }
    }

    fn temp_runs_root() -> RunsRootGuard {
        let guard = runs_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let d = std::env::temp_dir().join(format!(
            "tachi-shell-test-{}",
            Utc::now().format("%Y%m%dT%H%M%S%fZ")
        ));
        std::fs::create_dir_all(&d).unwrap();
        // SAFETY: `set_var` is unsafe on edition 2021 because it can race with
        // other threads reading the same env key. This call is safe because:
        //   1. The `runs_env_lock` mutex is held for the entire lifetime of
        //      `RunsRootGuard`, serialising all `temp_runs_root()` callers.
        //   2. The `Drop` impl restores the original value under the same lock.
        //   3. No other code path mutates `TACHI_RUN_ROOT`.
        unsafe {
            std::env::set_var("TACHI_RUN_ROOT", &d);
        }
        RunsRootGuard {
            _guard: guard,
            path: d,
        }
    }

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
        assert_eq!(slugify("   "), "flow");
        assert_eq!(slugify("已经-OK_test"), "ok-test");
    }

    #[test]
    fn meta_skill_mapping_is_complete() {
        for stage in &["brainstorm", "plan", "dispatch", "review", "ship"] {
            assert!(meta_skill_for_stage(stage).is_some(), "stage {stage}");
        }
        assert!(meta_skill_for_stage("kanban").is_none());
        assert!(meta_skill_for_stage("status").is_none());
    }

    #[test]
    fn superpowers_meta_skills_resolve_for_all_shell_stages() {
        for stage in STAGE_ACTIONS {
            let rel = meta_skill_for_stage(stage).expect("mapped stage");
            let resolved = resolve_meta_skill(rel).unwrap_or_else(|| {
                panic!("superpowers skill not found for stage {stage} at {rel}")
            });
            assert!(
                resolved.ends_with("SKILL.md"),
                "stage {stage} should resolve to SKILL.md, got {}",
                resolved.display()
            );
            let content = std::fs::read_to_string(&resolved)
                .unwrap_or_else(|e| panic!("read superpowers skill for {stage}: {e}"));
            assert!(
                content.contains("name:") || content.starts_with("# "),
                "stage {stage} skill should look like a SKILL.md front matter or heading"
            );
        }
    }

    #[test]
    fn build_instruction_includes_required_sections() {
        let inj = InjectionResult {
            required: true,
            rel_path: Some("skill/x/SKILL.md".into()),
            source_path: Some("/abs/skill/x/SKILL.md".into()),
            injected_path: Some(".tachi/runs/flow_x/injected/superpowers-plan.md".into()),
            content_hash: Some("a".repeat(16)),
            loaded: true,
            warning: None,
        };
        let s = build_instruction_md(
            "flow_x",
            "plan",
            "do the thing",
            &inj,
            Some("be careful"),
            &["cargo test".to_string()],
            &["crates/memory-server/**".to_string()],
        );
        assert!(s.contains("flow_x"));
        assert!(s.contains("Stage: **plan**"));
        assert!(s.contains("do the thing"));
        assert!(s.contains("superpowers-plan.md"));
        assert!(s.contains("## Native Lifecycle Policy"));
        assert!(s.contains("skill:superpowers-writing-plans"));
        assert!(s.contains("skill:waza-think"));
        assert!(s.contains("max 6 concurrent workers"));
        assert!(s.contains("cargo test"));
        assert!(s.contains("crates/memory-server/**"));
        assert!(s.contains("be careful"));
    }

    #[test]
    fn ship_instruction_includes_pr_first_release_flow() {
        let inj = InjectionResult {
            required: true,
            rel_path: Some("skill/x/SKILL.md".into()),
            source_path: None,
            injected_path: Some(".tachi/runs/flow_x/injected/superpowers-ship.md".into()),
            content_hash: Some("b".repeat(16)),
            loaded: true,
            warning: None,
        };
        let s = build_instruction_md("flow_x", "ship", "ship it", &inj, None, &[], &[]);
        assert!(s.contains("## Release Flow"));
        assert!(s.contains("Push the feature branch"));
        assert!(s.contains("Open a PR"));
        assert!(s.contains("Pass the PR gate"));
        assert!(s.contains("CI checks"));
        assert!(s.contains("Merge the PR"));
        assert!(!s.contains("direct push to the protected branch:\n\n1. Merge"));
    }

    #[test]
    fn flow_id_is_well_formed() {
        let id = new_flow_id(Some("Refactor Tachi Shell"), None);
        assert!(id.starts_with("flow_"));
        assert!(id.contains("refactor-tachi-shell"));
    }

    #[test]
    fn resolve_or_create_flow_creates_dir() {
        let _root = temp_runs_root();
        let p = TachiShellParams {
            action: "plan".into(),
            format: None,
            flow_id: None,
            task: Some("hello".into()),
            title: Some("hello".into()),
            agent: None,
            profile: None,
            cwd: None,
            tool_profile: None,
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
            async_dispatch: false,
            project: None,
            state_filter: None,
            limit: None,
            notes: None,
            validation: vec![],
            allowed_scope: vec![],
            slices: vec![],
        };
        let (fid, dir, created) = resolve_or_create_flow(&p, "hello").unwrap();
        assert!(created);
        assert!(dir.exists());
        assert!(fid.starts_with("flow_"));
    }

    #[test]
    fn advance_stage_writes_status_and_events() {
        let _root = temp_runs_root();
        let p = TachiShellParams {
            action: "plan".into(),
            format: None,
            flow_id: None,
            task: Some("t".into()),
            title: Some("t".into()),
            agent: None,
            profile: None,
            cwd: None,
            tool_profile: None,
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
            async_dispatch: false,
            project: None,
            state_filter: None,
            limit: None,
            notes: None,
            validation: vec![],
            allowed_scope: vec![],
            slices: vec![],
        };
        let (fid, dir, _created) = resolve_or_create_flow(&p, "t").unwrap();
        let inj = InjectionResult {
            required: true,
            rel_path: Some("skill/x".into()),
            source_path: None,
            injected_path: None,
            content_hash: None,
            loaded: false,
            warning: Some("missing".into()),
        };
        advance_stage(&dir, &fid, "plan", "t", &inj, true).unwrap();
        let status = read_status(&dir);
        assert_eq!(status.get("stage").and_then(|v| v.as_str()), Some("plan"));
        assert_eq!(
            status.get("state").and_then(|v| v.as_str()),
            Some("instruction_ready")
        );
        let events = std::fs::read_to_string(dir.join("events.jsonl")).unwrap();
        assert!(events.contains("flow_created"));
    }

    #[test]
    fn flow_id_rejects_path_traversal() {
        for invalid in [
            "../../etc",
            "flow_../../etc",
            "/tmp/evil",
            "flow_/tmp/evil",
            "flow_..",
            "flow_bad/name",
            "flow_bad\\name",
            "notflow_20260505",
        ] {
            assert!(
                validate_flow_id(invalid).is_err(),
                "expected invalid flow_id to be rejected: {invalid}"
            );
        }
        assert!(validate_flow_id("flow_20260505T000000Z_demo-1").is_ok());
    }

    #[tokio::test]
    async fn status_action_returns_not_found_for_missing_flow() {
        let _root = temp_runs_root();
        let p = TachiShellParams {
            action: "status".into(),
            format: None,
            flow_id: Some("flow_does_not_exist".into()),
            task: None,
            title: None,
            agent: None,
            profile: None,
            cwd: None,
            tool_profile: None,
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
            async_dispatch: false,
            project: None,
            state_filter: None,
            limit: None,
            notes: None,
            validation: vec![],
            allowed_scope: vec![],
            slices: vec![],
        };
        let out = handle_status_action(p).await.unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v.get("found").and_then(|x| x.as_bool()), Some(false));
    }

    #[tokio::test]
    async fn status_action_lists_known_flows() {
        let root = temp_runs_root();
        // Pre-create a flow dir + status.json
        let fdir = root.join("flow_20260505T000000Z_demo");
        std::fs::create_dir_all(&fdir).unwrap();
        std::fs::write(
            fdir.join("status.json"),
            r#"{"flow_id":"flow_20260505T000000Z_demo","stage":"plan","state":"instruction_ready","updated_at":"2026-05-05T00:00:00Z"}"#,
        )
        .unwrap();
        let p = TachiShellParams {
            action: "status".into(),
            format: None,
            flow_id: None,
            task: None,
            title: None,
            agent: None,
            profile: None,
            cwd: None,
            tool_profile: None,
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
            async_dispatch: false,
            project: None,
            state_filter: None,
            limit: Some(10),
            notes: None,
            validation: vec![],
            allowed_scope: vec![],
            slices: vec![],
        };
        let out = handle_status_action(p).await.unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let flows = v
            .get("flows")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(
            flows
                .iter()
                .any(|f| f.get("flow_id").and_then(|x| x.as_str())
                    == Some("flow_20260505T000000Z_demo")),
            "expected demo flow in listing, got {v}"
        );
    }

    #[test]
    fn resolve_slice_id_uses_explicit_id() {
        let slice = TachiShellDispatchSliceParams {
            id: Some("my-slice".into()),
            task: Some("do thing".into()),
            title: Some("My Slice".into()),
            agent: None,
            profile: None,
            cwd: None,
            tool_profile: None,
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
            notes: None,
            validation: Vec::new(),
            allowed_scope: Vec::new(),
        };
        assert_eq!(resolve_slice_id(0, &slice).unwrap(), "my-slice");
    }

    #[test]
    fn resolve_slice_id_falls_back_to_title() {
        let slice = TachiShellDispatchSliceParams {
            id: None,
            task: Some("do thing".into()),
            title: Some("Hello World".into()),
            agent: None,
            profile: None,
            cwd: None,
            tool_profile: None,
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
            notes: None,
            validation: Vec::new(),
            allowed_scope: Vec::new(),
        };
        assert_eq!(resolve_slice_id(0, &slice).unwrap(), "hello-world");
    }

    #[test]
    fn resolve_slice_id_falls_back_to_task() {
        let slice = TachiShellDispatchSliceParams {
            id: None,
            task: Some("Refactor Core".into()),
            title: None,
            agent: None,
            profile: None,
            cwd: None,
            tool_profile: None,
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
            notes: None,
            validation: Vec::new(),
            allowed_scope: Vec::new(),
        };
        assert_eq!(resolve_slice_id(0, &slice).unwrap(), "refactor-core");
    }

    #[test]
    fn resolve_slice_id_falls_back_to_index() {
        let slice = TachiShellDispatchSliceParams {
            id: None,
            task: None,
            title: None,
            agent: None,
            profile: None,
            cwd: None,
            tool_profile: None,
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
            notes: None,
            validation: Vec::new(),
            allowed_scope: Vec::new(),
        };
        assert_eq!(resolve_slice_id(3, &slice).unwrap(), "slice-4");
    }

    #[test]
    fn resolve_slice_id_rejects_traversal() {
        for invalid in ["../../etc", "slice/evil", "slice\\bad", "slice..whoops"] {
            assert!(
                validate_slice_id(invalid).is_err(),
                "expected invalid slice id to be rejected: {invalid}"
            );
        }
        assert!(validate_slice_id("alpha-1").is_ok());
    }

    #[tokio::test]
    async fn convoy_dispatch_creates_slice_dirs_and_status() {
        let _root = temp_runs_root();
        let server = {
            let db_path = std::env::temp_dir().join(format!(
                "memory-server-convoy-test-{}.sqlite",
                uuid::Uuid::new_v4()
            ));
            crate::MemoryServer::new(db_path, None).expect("test server")
        };
        let params = TachiShellParams {
            action: "dispatch".into(),
            format: None,
            flow_id: None,
            task: Some("parent task: review GitHub PRs and issues".into()),
            title: Some("convoy test".into()),
            agent: None,
            profile: None,
            cwd: None,
            tool_profile: None,
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
            async_dispatch: false,
            project: None,
            state_filter: None,
            limit: None,
            notes: None,
            validation: vec![],
            allowed_scope: vec![],
            slices: vec![
                TachiShellDispatchSliceParams {
                    id: Some("alpha".into()),
                    task: Some("slice alpha task: inspect the new PR".into()),
                    title: Some("Alpha Slice".into()),
                    agent: None,
                    profile: None,
                    cwd: None,
                    tool_profile: None,
                    mcp_access: None,
                    allowed_mcp_servers: Vec::new(),
                    notes: None,
                    validation: Vec::new(),
                    allowed_scope: Vec::new(),
                },
                TachiShellDispatchSliceParams {
                    id: Some("beta".into()),
                    task: Some("slice beta task".into()),
                    title: None,
                    agent: None,
                    profile: None,
                    cwd: None,
                    tool_profile: None,
                    mcp_access: None,
                    allowed_mcp_servers: Vec::new(),
                    notes: None,
                    validation: Vec::new(),
                    allowed_scope: Vec::new(),
                },
            ],
        };
        let out = handle_tachi_shell(&server, params).await.unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v.get("convoy").and_then(|x| x.as_bool()), Some(true));
        assert_eq!(v.get("async").and_then(|x| x.as_bool()), Some(false));
        assert_eq!(
            v.get("dispatch_ids")
                .and_then(|x| x.as_array())
                .map(|a| a.len()),
            Some(0)
        );
        let slices = v
            .get("slices")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default();
        assert_eq!(slices.len(), 2);

        let run_dir = PathBuf::from(v.get("run_dir").unwrap().as_str().unwrap());
        assert!(run_dir.join("slices/alpha/instruction.md").exists());
        assert!(run_dir.join("slices/beta/instruction.md").exists());
        let parent_instruction = std::fs::read_to_string(run_dir.join("instruction.md")).unwrap();
        assert!(
            parent_instruction.contains("skill:superpowers-subagent-driven-development"),
            "{parent_instruction}"
        );
        assert!(
            parent_instruction.contains("## Worker Factory Contract"),
            "{parent_instruction}"
        );
        let alpha_instruction =
            std::fs::read_to_string(run_dir.join("slices/alpha/instruction.md")).unwrap();
        assert!(
            alpha_instruction.contains("skill:waza-check"),
            "{alpha_instruction}"
        );
        assert!(
            alpha_instruction.contains("## Worker Report-Back Contract"),
            "{alpha_instruction}"
        );

        let status = read_status(&run_dir);
        let convoy = status.get("convoy").unwrap();
        assert_eq!(
            convoy.get("mode").and_then(|x| x.as_str()),
            Some("parallel")
        );
        assert_eq!(convoy.get("slice_count").and_then(|x| x.as_u64()), Some(2));
        assert_eq!(
            convoy
                .get("native_skill_policy")
                .and_then(|x| x.get("policy"))
                .and_then(|x| x.as_str()),
            Some("native")
        );

        let events_raw = std::fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
        let prepared_count = events_raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v.get("event").and_then(|x| x.as_str()) == Some("convoy_slice_prepared"))
            .count();
        assert_eq!(prepared_count, 2);
    }

    // ─── GitHub status / events helpers ──────────────────────────────────

    fn read_events_jsonl(run_dir: &Path) -> Vec<Value> {
        let raw = std::fs::read_to_string(run_dir.join("events.jsonl")).unwrap_or_default();
        raw.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str::<Value>(l).expect("event line is JSON"))
            .collect()
    }

    fn read_status_obj(run_dir: &Path) -> Value {
        let raw = std::fs::read_to_string(run_dir.join("status.json")).unwrap_or_default();
        serde_json::from_str(&raw).unwrap_or(json!({}))
    }

    #[test]
    fn merge_github_status_creates_block_when_absent() {
        let _root = temp_runs_root();
        let run_dir = _root.join("flow-gh-create");
        std::fs::create_dir_all(&run_dir).unwrap();

        let merged = merge_github_status(
            &run_dir,
            json!({
                "repo": "kckylec/sigil",
                "issue_number": 42,
                "issue_url": "https://github.com/kckylec/sigil/issues/42",
            }),
        )
        .expect("merge should succeed on empty status");

        assert_eq!(merged["repo"], json!("kckylec/sigil"));
        assert_eq!(merged["issue_number"], json!(42));

        let on_disk = read_status_obj(&run_dir);
        assert_eq!(on_disk["github"]["repo"], json!("kckylec/sigil"));
        assert!(
            on_disk["updated_at"].is_string(),
            "merge_github_status must stamp top-level updated_at"
        );
    }

    #[test]
    fn merge_github_status_deep_merges_partial_patches() {
        let _root = temp_runs_root();
        let run_dir = _root.join("flow-gh-merge");
        std::fs::create_dir_all(&run_dir).unwrap();

        // Seed: PR created with full checks block.
        merge_github_status(
            &run_dir,
            json!({
                "repo": "owner/repo",
                "pr_number": 7,
                "pr_url": "https://github.com/owner/repo/pull/7",
                "merge_state": "pending",
                "checks": { "state": "pending", "updated_at": "T0" },
            }),
        )
        .unwrap();

        // Patch: only the checks.state changes — checks.updated_at must
        // survive (deep merge), and pr_number / repo must be untouched.
        let merged = merge_github_status(
            &run_dir,
            json!({
                "checks": { "state": "success", "updated_at": "T1" },
            }),
        )
        .unwrap();

        assert_eq!(merged["repo"], json!("owner/repo"));
        assert_eq!(merged["pr_number"], json!(7));
        assert_eq!(merged["checks"]["state"], json!("success"));
        assert_eq!(merged["checks"]["updated_at"], json!("T1"));
        assert_eq!(merged["merge_state"], json!("pending"));

        // Patch: advance merge_state without touching anything else.
        let merged = merge_github_status(&run_dir, json!({ "merge_state": "ready" })).unwrap();
        assert_eq!(merged["merge_state"], json!("ready"));
        assert_eq!(merged["pr_number"], json!(7), "pr_number must persist");
    }

    #[test]
    fn merge_github_status_null_value_clears_field() {
        let _root = temp_runs_root();
        let run_dir = _root.join("flow-gh-clear");
        std::fs::create_dir_all(&run_dir).unwrap();
        merge_github_status(&run_dir, json!({ "issue_number": 1, "pr_number": 2 })).unwrap();
        let merged = merge_github_status(&run_dir, json!({ "issue_number": null })).unwrap();
        assert!(
            merged.get("issue_number").is_none(),
            "null patch value must remove the key, got: {merged}"
        );
        assert_eq!(merged["pr_number"], json!(2), "pr_number must persist");
    }

    #[test]
    fn merge_github_status_rejects_non_object_patch() {
        let _root = temp_runs_root();
        let run_dir = _root.join("flow-gh-bad-shape");
        std::fs::create_dir_all(&run_dir).unwrap();
        let err = merge_github_status(&run_dir, json!("not-an-object"))
            .expect_err("string patch must be rejected");
        assert!(err.contains("must be a JSON object"), "got: {err}");
        let err =
            merge_github_status(&run_dir, json!(null)).expect_err("null patch must be rejected");
        assert!(err.contains("must be a JSON object"), "got: {err}");
    }

    #[test]
    fn merge_github_status_rejects_invalid_merge_state() {
        let _root = temp_runs_root();
        let run_dir = _root.join("flow-gh-bad-state");
        std::fs::create_dir_all(&run_dir).unwrap();
        let err = merge_github_status(&run_dir, json!({ "merge_state": "exploded" }))
            .expect_err("invalid merge_state must be rejected");
        assert!(err.contains("invalid merge_state"), "got: {err}");
        // No status.json should have been written.
        assert!(
            !run_dir.join("status.json").exists(),
            "rejected patch must not partially write status.json"
        );
    }

    #[test]
    fn append_github_event_writes_typed_event_with_framing() {
        let _root = temp_runs_root();
        let run_dir = _root.join("flow-gh-event");
        std::fs::create_dir_all(&run_dir).unwrap();

        append_github_event(
            &run_dir,
            "flow-abc",
            "github_pr_created",
            json!({ "pr_number": 99, "pr_url": "https://github.com/o/r/pull/99" }),
        )
        .unwrap();
        append_github_event(
            &run_dir,
            "flow-abc",
            "github_checks_polled",
            json!({ "state": "pending" }),
        )
        .unwrap();

        let events = read_events_jsonl(&run_dir);
        assert_eq!(events.len(), 2, "two events expected, got: {events:?}");

        assert_eq!(events[0]["event"], json!("github_pr_created"));
        assert_eq!(events[0]["flow_id"], json!("flow-abc"));
        assert_eq!(events[0]["pr_number"], json!(99));
        assert!(
            events[0]["timestamp"].is_string(),
            "event must carry an RFC3339 timestamp"
        );

        assert_eq!(events[1]["event"], json!("github_checks_polled"));
        assert_eq!(events[1]["state"], json!("pending"));
    }

    #[test]
    fn append_github_event_rejects_unknown_kind() {
        let _root = temp_runs_root();
        let run_dir = _root.join("flow-gh-bad-kind");
        std::fs::create_dir_all(&run_dir).unwrap();
        let err = append_github_event(&run_dir, "flow", "github_nukes_launched", json!({}))
            .expect_err("unknown kind must be rejected");
        assert!(err.contains("unknown kind"), "got: {err}");
        assert!(
            !run_dir.join("events.jsonl").exists(),
            "rejected event must not be partially written"
        );
    }

    #[test]
    fn append_github_event_reserved_keys_cannot_be_overridden() {
        let _root = temp_runs_root();
        let run_dir = _root.join("flow-gh-reserved");
        std::fs::create_dir_all(&run_dir).unwrap();
        append_github_event(
            &run_dir,
            "real-flow",
            "github_pr_merged",
            json!({
                "event": "spoofed",
                "flow_id": "spoofed",
                "timestamp": "spoofed",
                "merge_sha": "deadbeef",
            }),
        )
        .unwrap();
        let events = read_events_jsonl(&run_dir);
        assert_eq!(events[0]["event"], json!("github_pr_merged"));
        assert_eq!(events[0]["flow_id"], json!("real-flow"));
        assert_ne!(events[0]["timestamp"], json!("spoofed"));
        assert_eq!(events[0]["merge_sha"], json!("deadbeef"));
    }

    #[test]
    fn github_block_coexists_with_existing_status_fields() {
        let _root = temp_runs_root();
        let run_dir = _root.join("flow-gh-coexist");
        std::fs::create_dir_all(&run_dir).unwrap();
        // Pre-seed a status.json that mimics a flow already in `dispatch`.
        write_status(
            &run_dir,
            &json!({
                "flow_id": "flow-coexist",
                "stage": "dispatch",
                "state": "dispatch_ready",
                "history": [{"stage": "dispatch", "from": "plan", "at": "T0"}],
            }),
        )
        .unwrap();

        merge_github_status(
            &run_dir,
            json!({ "repo": "o/r", "pr_number": 1, "merge_state": "pending" }),
        )
        .unwrap();

        let on_disk = read_status_obj(&run_dir);
        // Pre-existing fields must survive.
        assert_eq!(on_disk["flow_id"], json!("flow-coexist"));
        assert_eq!(on_disk["stage"], json!("dispatch"));
        assert_eq!(on_disk["history"][0]["stage"], json!("dispatch"));
        // New github block was added.
        assert_eq!(on_disk["github"]["repo"], json!("o/r"));
        assert_eq!(on_disk["github"]["pr_number"], json!(1));
    }
}
