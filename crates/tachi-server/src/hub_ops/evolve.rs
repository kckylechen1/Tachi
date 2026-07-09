use crate::tool_params::SkillEvolveParams;
use crate::MemoryServer;
use chrono::Utc;
use memcore::HubCapability;
use serde_json::json;

/// Evolve a skill by analyzing its telemetry and using LLM to produce an improved prompt.
///
/// Process:
/// 1. Retrieve the current skill from Hub
/// 2. Gather telemetry (uses, successes, failures, avg_rating)
/// 3. Construct an evolution prompt with the current skill definition + feedback
/// 4. Call LLM to generate an improved prompt
/// 5. Create a new versioned capability and optionally activate it
pub(crate) async fn handle_skill_evolve(
    server: &MemoryServer,
    params: SkillEvolveParams,
) -> Result<String, String> {
    // ── 1. Retrieve current skill ────────────────────────────────────────────
    let cap = server
        .get_capability(&params.skill_id)
        .map_err(|e| format!("Skill lookup failed: {e}"))?;

    if cap.cap_type != "skill" {
        return Err(format!(
            "'{}' is type '{}', not 'skill'",
            params.skill_id, cap.cap_type
        ));
    }

    let def: serde_json::Value = serde_json::from_str(&cap.definition)
        .map_err(|e| format!("invalid skill definition JSON: {e}"))?;

    let current_prompt = def
        .get("prompt")
        .or(def.get("template"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let _current_system = def.get("system").and_then(|v| v.as_str()).unwrap_or("");
    let current_content = def.get("content").and_then(|v| v.as_str()).unwrap_or("");

    // ── 2. Build telemetry summary ───────────────────────────────────────────
    let telemetry = format!(
        "Uses: {}, Successes: {}, Failures: {}, Avg Rating: {:.1}/5.0, Health: {}, Fail Streak: {}",
        cap.uses, cap.successes, cap.failures, cap.avg_rating, cap.health_status, cap.fail_streak
    );

    // ── 3. Construct the evolution prompt ────────────────────────────────────
    let user_feedback = params
        .feedback
        .as_deref()
        .unwrap_or("No specific feedback provided.");

    let evolution_prompt = format!(
        r#"You are a senior prompt engineer. Your job is to diagnose why a skill prompt is underperforming and produce a strictly improved version.

## Current Skill
- **Name:** {name}
- **Description:** {description}
- **Version:** {version}
- **Telemetry:** {telemetry}

## Current Prompt Template
```
{current_prompt}
```

{content_section}

## User Feedback
{user_feedback}

## Analysis Process

### Step 1: Diagnose from telemetry
- High failure count → prompt likely has ambiguous instructions, missing constraints, or assumes context the model doesn't have.
- Low avg rating but low failures → output quality issue: wrong format, too verbose, missing key details, or poor reasoning.
- High fail streak → recent regression. Check if a dependency changed or an assumption broke.
- Healthy stats (success > 90%, rating > 4.0) → make only surgical, conservative improvements.

### Step 2: Read the prompt for common anti-patterns
- Vague verbs ("handle", "process", "deal with") instead of specific actions ("extract", "classify", "generate")
- Missing output format specification (no JSON schema, no example output)
- No edge case handling (what if input is empty? malformed? too long?)
- No constraints on output length or structure
- Placeholders used but not explained (what does {{{{context}}}} contain?)
- Missing "do NOT" instructions for common failure modes

### Step 3: Produce the improved prompt
- Preserve ALL existing `{{{{placeholder}}}}` variables exactly as-is (double-braced in template).
- Add structure if missing: split into Context / Task / Constraints / Output Format sections.
- Add explicit edge case handling: empty input, malformed data, missing fields.
- Add output format specification: if the skill produces structured data, define the exact schema with an example.
- Add negative constraints: what the model should NOT do (common failure modes from telemetry).
- Keep the prompt concise — do not add padding or filler. Every sentence should earn its place.

### Step 4: Quality checklist (verify before output)
- All original placeholder variables are preserved
- Output format is strictly defined (JSON schema or exact structure)
- At least one edge case is handled explicitly
- The prompt is shorter or equal length to original (unless structure demands more)
- No vague instructions remain
- Description accurately reflects what the improved skill does

## Output Format
Respond with ONLY a JSON object (no markdown fences, no commentary before or after):
{{
  "prompt": "<improved prompt template>",
  "description": "<improved 1-2 sentence description>",
  "system": "<improved system prompt, or empty string to keep current>",
  "reasoning": "<diagnosis of the problem + what was changed and why, 3-5 sentences>"
}}"#,
        name = cap.name,
        description = cap.description,
        version = cap.version,
        telemetry = telemetry,
        current_prompt = current_prompt,
        content_section = if !current_content.is_empty() {
            format!("## Current SKILL.md Content\n```\n{}\n```", current_content)
        } else {
            String::new()
        },
        user_feedback = user_feedback,
    );

    // ── 4. Call LLM for evolution ────────────────────────────────────────────
    // Phase 2: route through Claude CLI pool first; on Err, fall back to the
    // raw extract lane (SiliconFlow/Qwen) without spawning an unbounded CLI.
    const EVOLVE_SYSTEM: &str = "You are a senior prompt engineer specializing in agentic skill optimization. Analyze telemetry, diagnose failure modes, and produce a strictly improved prompt. Output valid JSON only, no markdown fences.";
    let llm_for_fallback = server.llm.clone();
    let evolution_prompt_for_fallback = evolution_prompt.clone();
    let (llm_response, source) = tachi_llm::claude_pool::pool_call_with_fallback(
        &server.claude_pool,
        EVOLVE_SYSTEM,
        &evolution_prompt,
        "skill-evolve",
        move || async move {
            llm_for_fallback
                .call_extract_llm(
                    EVOLVE_SYSTEM,
                    &evolution_prompt_for_fallback,
                    None,
                    0.4,
                    4000,
                )
                .await
        },
    )
    .await
    .map_err(|e| format!("LLM evolution call failed: {e}"))?;
    eprintln!(
        "[skill-evolve] {}: backend={}",
        params.skill_id,
        source.as_str()
    );

    // Parse LLM response as JSON
    let evolved: serde_json::Value = serde_json::from_str(llm_response.trim())
        .or_else(|_| {
            // Try extracting JSON from markdown code fences
            let trimmed = llm_response.trim();
            let json_str = if let Some(start) = trimmed.find('{') {
                if let Some(end) = trimmed.rfind('}') {
                    &trimmed[start..=end]
                } else {
                    trimmed
                }
            } else {
                trimmed
            };
            serde_json::from_str(json_str)
        })
        .map_err(|e| {
            format!("Failed to parse LLM evolution response as JSON: {e}\nRaw: {llm_response}")
        })?;

    let new_prompt = evolved
        .get("prompt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "LLM response missing 'prompt' field".to_string())?;
    let new_description = evolved
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or(&cap.description);
    let new_system = evolved
        .get("system")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let reasoning = evolved
        .get("reasoning")
        .and_then(|v| v.as_str())
        .unwrap_or("No reasoning provided");

    // ── 5. Dry run — just return the proposal ────────────────────────────────
    if params.dry_run {
        return serde_json::to_string(&json!({
            "skill_id": params.skill_id,
            "dry_run": true,
            "current_version": cap.version,
            "proposed_prompt": new_prompt,
            "proposed_description": new_description,
            "proposed_system": new_system,
            "reasoning": reasoning
        }))
        .map_err(|e| format!("serialize: {e}"));
    }

    // ── 6. Create new versioned capability ───────────────────────────────────
    let new_version = cap.version + 1;
    let new_id = format!("{}/v{}", params.skill_id, new_version);

    // Build new definition by merging evolved fields into current definition
    let mut new_def = def.clone();
    if let Some(obj) = new_def.as_object_mut() {
        obj.insert("prompt".to_string(), json!(new_prompt));
        if let Some(sys) = new_system {
            obj.insert("system".to_string(), json!(sys));
        }
        obj.insert(
            "evolution".to_string(),
            json!({
                "evolved_from": params.skill_id,
                "evolved_from_version": cap.version,
                "reasoning": reasoning,
                "evolved_at": Utc::now().to_rfc3339(),
            }),
        );
    }

    let new_cap = HubCapability {
        id: new_id.clone(),
        cap_type: "skill".to_string(),
        name: cap.name.clone(),
        version: new_version,
        description: new_description.to_string(),
        definition: serde_json::to_string(&new_def).map_err(|e| format!("serialize def: {e}"))?,
        enabled: true,
        review_status: "approved".to_string(),
        health_status: "healthy".to_string(),
        last_error: None,
        last_success_at: None,
        last_failure_at: None,
        fail_streak: 0,
        active_version: None,
        exposure_mode: cap.exposure_mode.clone(),
        uses: 0,
        successes: 0,
        failures: 0,
        avg_rating: 0.0,
        last_used: None,
        created_at: Utc::now().to_rfc3339(),
        updated_at: Utc::now().to_rfc3339(),
    };

    // Store the new version in the same scope as the original
    server.with_global_store(|store| {
        store
            .hub_register(&new_cap)
            .map_err(|e| format!("persist evolved skill: {e}"))
    })?;

    // ── 7. Optionally activate the new version ───────────────────────────────
    if params.auto_activate {
        server.with_global_store(|store| {
            store
                .hub_set_active_version_route(&params.skill_id, &new_id)
                .map_err(|e| format!("set version route: {e}"))
        })?;

        // Also update the original skill's active_version pointer
        if let Err(error) = server.with_global_store(|store| {
            // Read, modify, write back
            if let Some(mut orig) = store
                .hub_get(&params.skill_id)
                .map_err(|e| format!("{e}"))?
            {
                orig.active_version = Some(new_id.clone());
                store.hub_register(&orig).map_err(|e| format!("{e}"))?;
            }
            Ok::<(), String>(())
        }) {
            tracing::warn!(
                skill_id = %params.skill_id,
                active_version = %new_id,
                error = %error,
                "failed to update original skill active_version pointer"
            );
        }
    }

    serde_json::to_string(&json!({
        "skill_id": params.skill_id,
        "evolved_id": new_id,
        "new_version": new_version,
        "auto_activated": params.auto_activate,
        "description": new_description,
        "reasoning": reasoning,
        "prompt_preview": if new_prompt.len() > 200 {
            &new_prompt[..new_prompt.char_indices().nth(200).map(|(i, _)| i).unwrap_or(new_prompt.len())]
        } else {
            new_prompt
        }
    }))
    .map_err(|e| format!("serialize: {e}"))
}
