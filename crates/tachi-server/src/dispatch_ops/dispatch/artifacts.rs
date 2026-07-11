use super::*;

pub(super) struct DispatchArtifactInputs<'a> {
    pub(super) workspace_dir: &'a Path,
    pub(super) dispatch_id: &'a str,
    pub(super) agent_norm: &'a str,
    pub(super) params: &'a TachiDispatchParams,
    pub(super) base_prompt: &'a str,
    pub(super) prompt_assembly: &'a crate::dispatch_ops::prompt::PromptAssembly,
    pub(super) effective_skills_for_files: &'a [String],
    pub(super) v2: bool,
}

pub(super) struct DispatchArtifacts {
    pub(super) plan_path: PathBuf,
    pub(super) prompt_md_path: PathBuf,
    pub(super) context_md_path: PathBuf,
    pub(super) trajectory_path: PathBuf,
    pub(super) capability_bundle_file: String,
    pub(super) capability_bundle_card: Value,
    pub(super) feedback_rules_trace: Value,
}

pub(super) async fn write_dispatch_artifacts(
    ctx: DispatchArtifactInputs<'_>,
) -> Result<DispatchArtifacts, String> {
    let plan_path = ctx.workspace_dir.join("plan.md");
    // V1 writes the assembled prompt as a placeholder plan.md (legacy);
    // V2 will overwrite this with the real LLM-generated plan later.
    crate::utils::write_owner_only_file_atomic(&plan_path, ctx.base_prompt.as_bytes())
        .map_err(|e| format!("Failed to write plan file: {e}"))?;

    let prompt_md_path = ctx.workspace_dir.join("prompt.md");
    tokio::fs::write(&prompt_md_path, ctx.base_prompt)
        .await
        .map_err(|e| format!("Failed to write prompt.md: {e}"))?;

    let capability_bundle_path = ctx.workspace_dir.join("capability_bundle.json");
    let mut capability_bundle_trace = ctx.prompt_assembly.capability_bundle.clone();
    if let Some(obj) = capability_bundle_trace.as_object_mut() {
        obj.insert(
            "feedback_rules".to_string(),
            ctx.prompt_assembly.feedback_rules.clone(),
        );
    }
    let feedback_rules_trace = ctx.prompt_assembly.feedback_rules.clone();
    let capability_bundle_artifact = serde_json::to_string_pretty(&capability_bundle_trace)
        .map_err(|e| format!("Failed to serialize capability bundle artifact: {e}"))?;
    tokio::fs::write(&capability_bundle_path, capability_bundle_artifact)
        .await
        .map_err(|e| format!("Failed to write capability_bundle.json: {e}"))?;
    let capability_bundle_file = capability_bundle_path.to_string_lossy().to_string();
    let capability_bundle_card =
        capability_bundle_summary(&capability_bundle_trace, Some(&capability_bundle_file));

    let context_md_path = ctx.workspace_dir.join("context.md");
    let context_summary = {
        let mut sections = Vec::new();
        sections.push(format!("# Dispatch Context: {}", ctx.dispatch_id));
        sections.push(format!("Agent: {}", ctx.agent_norm));
        sections.push(format!(
            "Dispatch profile: {}",
            ctx.params.profile.as_deref().unwrap_or("none")
        ));
        sections.push(format!(
            "Tool profile: {}",
            ctx.params.tool_profile.as_deref().unwrap_or("none")
        ));
        if let Some(flow_id) = ctx.params.flow_id.as_deref() {
            sections.push(format!("Flow: {}", flow_id));
        }
        if let Some(issue_ref) = ctx.params.issue_ref.as_deref() {
            sections.push(format!("Issue: {}", issue_ref));
        }
        if let Some(pr_ref) = ctx.params.pr_ref.as_deref() {
            sections.push(format!("PR: {}", pr_ref));
        }
        sections.push(format!(
            "Stage: {}",
            ctx.params.stage.as_deref().unwrap_or("none")
        ));
        sections.push(format!("V2: {}", ctx.v2));
        sections.push(format!("Skills: {:?}", ctx.effective_skills_for_files));
        sections.push(format!(
            "Capability bundle: status={} requested={} injected={} artifact={}",
            capability_bundle_trace
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown"),
            capability_bundle_trace
                .get("requested")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
            capability_bundle_trace
                .get("injected")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
            capability_bundle_file
        ));
        sections.push(String::new());
        sections.push(ctx.base_prompt.to_string());
        sections.join("\n\n")
    };
    tokio::fs::write(&context_md_path, &context_summary)
        .await
        .map_err(|e| format!("Failed to write context.md: {e}"))?;

    // #971 receipt-first: `handle_tachi_dispatch` now appends a
    // "dispatch_received" event to this same trajectory.jsonl path BEFORE
    // this function runs (right after the workspace directory is created,
    // ahead of prompt assembly / the V2 plan stage). Use APPEND here
    // (not overwrite) so that earlier event survives as line 1 and
    // "dispatch_started" lands as line 2 — no reader depends on
    // "dispatch_started" being the first line (grep confirmed: existing
    // trajectory assertions are all `.contains(...)`, not first-line/
    // ordinal checks). `create(true)` covers the V1-only / direct-unit-test
    // callers of this function that never went through the receipt-first
    // seed and so find no file here yet.
    let trajectory_path = ctx.workspace_dir.join("trajectory.jsonl");
    let started_event = json!({
        "event": "dispatch_started",
        "dispatch_id": ctx.dispatch_id,
        "agent": ctx.agent_norm,
        "stage": ctx.params.stage,
        "profile": ctx.params.profile,
        "tool_profile": ctx.params.tool_profile,
        "mcp_access": ctx.params.mcp_access,
        "allowed_mcp_servers": ctx.params.allowed_mcp_servers,
        "issue_ref": ctx.params.issue_ref,
        "pr_ref": ctx.params.pr_ref,
        "flow_id": ctx.params.flow_id,
        "auto_capability_bundle": ctx.params.auto_capability_bundle,
        "capability_bundle": capability_bundle_card.clone(),
        "feedback_rules": feedback_rules_trace.clone(),
        "v2": ctx.v2,
        "timestamp": Utc::now().to_rfc3339(),
    });
    let line = serde_json::to_string(&started_event)
        .map_err(|e| format!("Failed to serialize started event: {e}"))?;
    {
        use tokio::io::AsyncWriteExt;
        let mut f = tokio::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&trajectory_path)
            .await
            .map_err(|e| format!("Failed to open trajectory.jsonl: {e}"))?;
        f.write_all(format!("{}\n", line).as_bytes())
            .await
            .map_err(|e| format!("Failed to write trajectory.jsonl: {e}"))?;
    }
    // progress.jsonl mirrors trajectory.jsonl's event stream 1:1 (see
    // `append_trajectory_event`) — append here too so it stays in sync with
    // the pre-existing "dispatch_received" line already written there.
    let progress_path = ctx.workspace_dir.join("progress.jsonl");
    {
        use tokio::io::AsyncWriteExt;
        let mut f = tokio::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&progress_path)
            .await
            .map_err(|e| format!("Failed to open progress.jsonl: {e}"))?;
        f.write_all(format!("{}\n", line).as_bytes())
            .await
            .map_err(|e| format!("Failed to write progress.jsonl: {e}"))?;
    }

    Ok(DispatchArtifacts {
        plan_path,
        prompt_md_path,
        context_md_path,
        trajectory_path,
        capability_bundle_file,
        capability_bundle_card,
        feedback_rules_trace,
    })
}
