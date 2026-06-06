use super::*;

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn make_skill_capability(
    id: &str,
    name: &str,
    description: &str,
    definition: Value,
) -> Result<HubCapability, String> {
    let timestamp = now();
    Ok(HubCapability {
        id: id.to_string(),
        cap_type: "skill".to_string(),
        name: name.to_string(),
        version: 1,
        description: description.to_string(),
        definition: serde_json::to_string(&definition)
            .map_err(|e| format!("serialize builtin skill {id}: {e}"))?,
        enabled: true,
        review_status: "approved".to_string(),
        health_status: "healthy".to_string(),
        last_error: None,
        last_success_at: None,
        last_failure_at: None,
        fail_streak: 0,
        active_version: None,
        exposure_mode: "direct".to_string(),
        uses: 0,
        successes: 0,
        failures: 0,
        avg_rating: 0.0,
        last_used: None,
        created_at: timestamp.clone(),
        updated_at: timestamp,
    })
}

fn make_mcp_capability(
    id: &str,
    name: &str,
    description: &str,
    url: &str,
    auto_ingest: bool,
    ingest_domain: Option<&str>,
    ingest_path_prefix: Option<&str>,
) -> Result<HubCapability, String> {
    let timestamp = now();
    let definition = json!({
        "transport": "streamable-http",
        "url": url,
        "auth": {
            "type": "bearer",
            "token": "ZAI_API_KEY|BIGMODEL_API_KEY|REASONING_API_KEY"
        },
        "tool_exposure": "gateway",
        "policy": {
            "visibility": "discoverable"
        },
        "auto_ingest": auto_ingest,
        "ingest_scope": "global",
        "ingest_domain": ingest_domain,
        "ingest_path_prefix": ingest_path_prefix,
        "startup_timeout_ms": 10_000,
        "tool_timeout_ms": 30_000,
        "max_concurrency": 2,
        "tags": ["builtin", "bigmodel", "mcp"]
    });

    Ok(HubCapability {
        id: id.to_string(),
        cap_type: "mcp".to_string(),
        name: name.to_string(),
        version: 1,
        description: description.to_string(),
        definition: serde_json::to_string(&definition)
            .map_err(|e| format!("serialize builtin MCP {id}: {e}"))?,
        enabled: true,
        review_status: "approved".to_string(),
        health_status: "healthy".to_string(),
        last_error: None,
        last_success_at: None,
        last_failure_at: None,
        fail_streak: 0,
        active_version: None,
        exposure_mode: "gateway".to_string(),
        uses: 0,
        successes: 0,
        failures: 0,
        avg_rating: 0.0,
        last_used: None,
        created_at: timestamp.clone(),
        updated_at: timestamp,
    })
}

fn make_local_mcp_capability(
    id: &str,
    name: &str,
    description: &str,
    command: &str,
    args: &[&str],
    env: Value,
    auto_ingest: bool,
) -> Result<HubCapability, String> {
    let timestamp = now();
    let definition = json!({
        "transport": "stdio",
        "command": command,
        "args": args,
        "env": env,
        "tool_exposure": "gateway",
        "policy": {
            "visibility": "discoverable"
        },
        "auto_ingest": auto_ingest,
        "ingest_scope": "global",
        "startup_timeout_ms": 20_000,
        "tool_timeout_ms": 60_000,
        "max_concurrency": 1,
        "tags": ["builtin", "bigmodel", "mcp", "local"]
    });

    Ok(HubCapability {
        id: id.to_string(),
        cap_type: "mcp".to_string(),
        name: name.to_string(),
        version: 1,
        description: description.to_string(),
        definition: serde_json::to_string(&definition)
            .map_err(|e| format!("serialize builtin local MCP {id}: {e}"))?,
        enabled: true,
        review_status: "approved".to_string(),
        health_status: "healthy".to_string(),
        last_error: None,
        last_success_at: None,
        last_failure_at: None,
        fail_streak: 0,
        active_version: None,
        exposure_mode: "gateway".to_string(),
        uses: 0,
        successes: 0,
        failures: 0,
        avg_rating: 0.0,
        last_used: None,
        created_at: timestamp.clone(),
        updated_at: timestamp,
    })
}

fn builtin_trajectory_distiller() -> Result<HubCapability, String> {
    let content = r#"# trajectory-distiller

Turn a successful execution trajectory into a reusable skill document.

## Inputs
- task_description
- execution_trace
- final_outcome
- agent_id
- skill_path
- domain

## Output contract
Return Markdown with exactly these sections:
1. 适用场景
2. 核心步骤
3. 踩坑记录
4. 验证标准
5. 适用域标签

Keep it reusable, concrete, and procedural. Prefer SOP-style bullets over narrative."#;

    make_skill_capability(
        "skill:trajectory-distiller",
        "trajectory-distiller",
        "Distill execution traces into reusable skill documents.",
        json!({
            "system": "You turn execution trajectories into concise, reusable skill playbooks. Output Markdown only.",
            "prompt": "Distill the following execution trajectory into a reusable skill document.\n\nTask Description:\n{{task_description}}\n\nExecution Trace:\n{{execution_trace}}\n\nFinal Outcome:\n{{final_outcome}}\n\nAgent ID:\n{{agent_id}}\n\nSkill Path:\n{{skill_path}}\n\nDomain:\n{{domain}}\n\nReturn Markdown with these exact top-level sections:\n1. 适用场景\n2. 核心步骤\n3. 踩坑记录\n4. 验证标准\n5. 适用域标签",
            "content": content,
            "policy": { "visibility": "discoverable" },
            "tags": ["distillation", "trajectory", "builtin"],
            "retention_policy": "permanent",
            "skill_path": "/skills/general/trajectory-distiller",
            "inputSchema": {
                "type": "object",
                "required": ["task_description", "execution_trace", "final_outcome", "skill_path"],
                "properties": {
                    "task_description": {"type": "string"},
                    "execution_trace": {},
                    "final_outcome": {},
                    "agent_id": {"type": "string"},
                    "skill_path": {"type": "string"},
                    "domain": {"type": "string"}
                }
            }
        }),
    )
}

fn builtin_coding_skills() -> Result<Vec<HubCapability>, String> {
    Ok(vec![
        make_skill_capability(
            "skill:coding-debug-pattern",
            "coding/debug-pattern",
            "Capture repeatable debug patterns for coding agents.",
            json!({
                "prompt": "Use this coding debug template to capture or apply a debugging pattern. Input:\n{{input}}",
                "content": "# /skills/coding/debug-pattern\n\n## Purpose\nCapture: symptom → likely causes → verification → fix.\n\n## Output\n- Error signature\n- Root cause shortlist\n- Fastest discriminators\n- Confirmed fix\n- Follow-up regression test\n\n## Retention\nEphemeral by default unless promoted into a gotcha or ADR.",
                "policy": { "visibility": "discoverable" },
                "domain": "coding",
                "skill_path": "/skills/coding/debug-pattern",
                "retention_policy": "ephemeral",
                "memory_path_templates": ["/coding/{repo_name}/pattern", "/coding/{repo_name}/debt"],
                "tags": ["coding", "debug", "preset"]
            }),
        )?,
        make_skill_capability(
            "skill:coding-gotcha-capture",
            "coding/gotcha-capture",
            "Promote a recurring coding pitfall into a permanent gotcha note.",
            json!({
                "prompt": "Convert this coding incident into a durable gotcha note. Input:\n{{input}}",
                "content": "# /skills/coding/gotcha-capture\n\n## Purpose\nTurn a one-off failure into a permanent gotcha record others can recall later.\n\n## Capture\n- Triggering condition\n- Misleading signal\n- Correct diagnosis\n- Preventive check\n\n## Storage\nWrite to `/coding/{repo_name}/gotcha`.\nRetention: permanent.",
                "policy": { "visibility": "discoverable" },
                "domain": "coding",
                "skill_path": "/skills/coding/gotcha-capture",
                "retention_policy": "permanent",
                "memory_path_templates": ["/coding/{repo_name}/gotcha"],
                "tags": ["coding", "gotcha", "preset"]
            }),
        )?,
        make_skill_capability(
            "skill:coding-architecture-decision",
            "coding/architecture-decision",
            "ADR template for architecture decisions and rejected options.",
            json!({
                "prompt": "Draft or apply an ADR using this template. Input:\n{{input}}",
                "content": "# /skills/coding/architecture-decision\n\n## Purpose\nRecord why a design was chosen, what alternatives were rejected, and what future signals should trigger re-evaluation.\n\n## Required sections\n- Context\n- Decision\n- Rejected alternatives\n- Validation / rollback\n- Drift signals\n\n## Storage\nWrite to `/coding/{repo_name}/decision`.\nRetention: permanent.",
                "policy": { "visibility": "discoverable" },
                "domain": "coding",
                "skill_path": "/skills/coding/architecture-decision",
                "retention_policy": "permanent",
                "memory_path_templates": ["/coding/{repo_name}/decision"],
                "tags": ["coding", "adr", "preset"]
            }),
        )?,
        make_skill_capability(
            "skill:coding-refactor-checklist",
            "coding/refactor-checklist",
            "Checklist for safe refactors.",
            json!({
                "prompt": "Apply this refactor checklist to the target change. Input:\n{{input}}",
                "content": "# /skills/coding/refactor-checklist\n\nBefore refactoring, confirm:\n1. Existing behavior is covered by regression tests\n2. Downstream dependencies are mapped\n3. Performance baseline is known\n4. Rollback path exists\n5. Diff is staged in reviewable slices",
                "policy": { "visibility": "discoverable" },
                "domain": "coding",
                "skill_path": "/skills/coding/refactor-checklist",
                "retention_policy": "durable",
                "tags": ["coding", "refactor", "preset"]
            }),
        )?,
        make_skill_capability(
            "skill:coding-code-review-lens",
            "coding/code-review-lens",
            "Four-lens code review scoring template.",
            json!({
                "prompt": "Review the target through the security / performance / readability / testability lenses. Input:\n{{input}}",
                "content": "# /skills/coding/code-review-lens\n\nScore the change across:\n- Security\n- Performance\n- Readability\n- Testability\n\nFor each lens, record risks, confidence, and required follow-up.",
                "policy": { "visibility": "hidden" },
                "domain": "coding",
                "skill_path": "/skills/coding/code-review-lens",
                "retention_policy": "durable",
                "tags": ["coding", "review", "preset"]
            }),
        )?,
        make_skill_capability(
            "skill:coding-test-strategy",
            "coding/test-strategy",
            "Testing strategy template by code type.",
            json!({
                "prompt": "Choose a test strategy using this template. Input:\n{{input}}",
                "content": "# /skills/coding/test-strategy\n\nMap the target into one of:\n- Pure function\n- Side-effectful integration\n- Async / concurrent workflow\n\nThen choose the minimum reliable test mix: unit, integration, snapshot, or replay.",
                "policy": { "visibility": "discoverable" },
                "domain": "coding",
                "skill_path": "/skills/coding/test-strategy",
                "retention_policy": "durable",
                "tags": ["coding", "testing", "preset"]
            }),
        )?,
    ])
}

fn builtin_trading_skills() -> Result<Vec<HubCapability>, String> {
    Ok(vec![
        make_skill_capability(
            "skill:trading-pre-market-briefing",
            "trading/pre-market-briefing",
            "Pre-market operating checklist for trading agents.",
            json!({
                "prompt": "Run the pre-market briefing checklist. Input:\n{{input}}",
                "content": "# /skills/trading/pre-market-briefing\n\n1. Review yesterday's daily summaries and unfinished items\n2. Pull current position vs shadow position diff\n3. Re-state regime and defense line\n4. Name the trades that are allowed today",
                "policy": { "visibility": "discoverable" },
                "domain": "trading",
                "skill_path": "/skills/trading/pre-market-briefing",
                "retention_policy": "durable",
                "tags": ["trading", "briefing", "preset"]
            }),
        )?,
        make_skill_capability(
            "skill:trading-regime-playbook",
            "trading/regime-playbook",
            "Market regime playbook template.",
            json!({
                "prompt": "Select or update the regime playbook. Input:\n{{input}}",
                "content": "# /skills/trading/regime-playbook\n\nMaintain playbooks for acceleration / pullback / range / bear.\nFor each regime define:\n- Setup quality\n- Entry / stop logic\n- Position sizing guardrail\n- What invalidates the playbook",
                "policy": { "visibility": "discoverable" },
                "domain": "trading",
                "skill_path": "/skills/trading/regime-playbook",
                "retention_policy": "durable",
                "memory_path_templates": ["/trading/regime/{date}"],
                "tags": ["trading", "regime", "preset"]
            }),
        )?,
        make_skill_capability(
            "skill:trading-post-trade-review",
            "trading/post-trade-review",
            "Post-trade review template.",
            json!({
                "prompt": "Write a post-trade review using this template. Input:\n{{input}}",
                "content": "# /skills/trading/post-trade-review\n\nFor each trade capture:\n- Snapshot at entry\n- Expected path\n- Actual path\n- Variance explanation\n- Whether to extract a permanent lesson",
                "policy": { "visibility": "discoverable" },
                "domain": "trading",
                "skill_path": "/skills/trading/post-trade-review",
                "retention_policy": "durable",
                "tags": ["trading", "review", "preset"]
            }),
        )?,
        make_skill_capability(
            "skill:trading-lesson-extractor",
            "trading/lesson-extractor",
            "Turn failed or costly trades into permanent lessons.",
            json!({
                "prompt": "Extract a durable trading lesson from this trade. Input:\n{{input}}",
                "content": "# /skills/trading/lesson-extractor\n\nClassify the miss: early entry / stop placement / regime misread / thesis drift.\nThen convert it into an actionable rule revision.\n\n## Storage\nWrite to `/trading/lesson/{ticker}`.\nRetention: permanent.",
                "policy": { "visibility": "discoverable" },
                "domain": "trading",
                "skill_path": "/skills/trading/lesson-extractor",
                "retention_policy": "permanent",
                "memory_path_templates": ["/trading/lesson/{ticker}"],
                "tags": ["trading", "lesson", "preset"]
            }),
        )?,
        make_skill_capability(
            "skill:trading-position-review",
            "trading/position-review",
            "Periodic position review template.",
            json!({
                "prompt": "Run a position review. Input:\n{{input}}",
                "content": "# /skills/trading/position-review\n\nReview hold / reduce / exit decisions against:\n- Trend integrity\n- Relative strength\n- Stop distance\n- Thesis drift\n- Liquidity risk",
                "policy": { "visibility": "discoverable" },
                "domain": "trading",
                "skill_path": "/skills/trading/position-review",
                "retention_policy": "durable",
                "tags": ["trading", "position", "preset"]
            }),
        )?,
        make_skill_capability(
            "skill:trading-position-snapshot",
            "trading/position-snapshot",
            "Ephemeral position snapshot template and integration contract.",
            json!({
                "prompt": "Capture a position snapshot. Input:\n{{input}}",
                "content": "# /skills/trading/position-snapshot\n\nCapture current holdings, cost basis, stop, and shadow-book diff.\n\n## Retention\nEphemeral unless promoted into a thesis or lesson.\n\n## Integration point\nAfter `PortfolioManager.buy()` / `PortfolioManager.sell()`, call:\n`tachi.ingest(ingest_type=\"event\", event_type=\"trade_execution\", content={...}, domain=\"trading\", path_prefix=f\"/trading/journal/{ticker}\")`",
                "policy": { "visibility": "discoverable" },
                "domain": "trading",
                "skill_path": "/skills/trading/position-snapshot",
                "retention_policy": "ephemeral",
                "memory_path_templates": ["/trading/position", "/trading/shadow/position"],
                "integration_points": [
                    {
                        "system": "PortfolioManager.buy()/sell()",
                        "call": "tachi.ingest(ingest_type='event', event_type='trade_execution', content={...}, domain='trading', path_prefix='/trading/journal/{ticker}')"
                    }
                ],
                "tags": ["trading", "position", "preset"]
            }),
        )?,
    ])
}

fn builtin_waza_skills() -> Result<Vec<HubCapability>, String> {
    const WAZA_SKILLS: &[(&str, &str, &str, &str)] = &[
        (
            "check",
            "Waza review, PR, issue, ship, release, and audit workflow.",
            "skill/waza/skills/check/SKILL.md",
            include_str!("../../../skill/waza/skills/check/SKILL.md"),
        ),
        (
            "design",
            "Waza production-grade UI and frontend design workflow.",
            "skill/waza/skills/design/SKILL.md",
            include_str!("../../../skill/waza/skills/design/SKILL.md"),
        ),
        (
            "health",
            "Waza agent/runtime configuration health audit workflow.",
            "skill/waza/skills/health/SKILL.md",
            include_str!("../../../skill/waza/skills/health/SKILL.md"),
        ),
        (
            "hunt",
            "Waza root-cause debugging and regression diagnosis workflow.",
            "skill/waza/skills/hunt/SKILL.md",
            include_str!("../../../skill/waza/skills/hunt/SKILL.md"),
        ),
        (
            "learn",
            "Waza research and source synthesis workflow.",
            "skill/waza/skills/learn/SKILL.md",
            include_str!("../../../skill/waza/skills/learn/SKILL.md"),
        ),
        (
            "read",
            "Waza URL/PDF reading and clean source extraction workflow.",
            "skill/waza/skills/read/SKILL.md",
            include_str!("../../../skill/waza/skills/read/SKILL.md"),
        ),
        (
            "tachi",
            "Waza Tachi memory and task workflow.",
            "skill/waza/skills/tachi/SKILL.md",
            include_str!("../../../skill/waza/skills/tachi/SKILL.md"),
        ),
        (
            "think",
            "Waza decision-complete planning and tradeoff workflow.",
            "skill/waza/skills/think/SKILL.md",
            include_str!("../../../skill/waza/skills/think/SKILL.md"),
        ),
        (
            "write",
            "Waza Chinese and English prose polishing workflow.",
            "skill/waza/skills/write/SKILL.md",
            include_str!("../../../skill/waza/skills/write/SKILL.md"),
        ),
    ];

    WAZA_SKILLS
        .iter()
        .map(|(name, description, source_path, content)| {
            make_skill_capability(
                &format!("skill:waza-{name}"),
                &format!("waza/{name}"),
                description,
                json!({
                    "system": "You are applying a Waza workflow skill. Follow the embedded SKILL.md contract and keep the output grounded in the current task evidence.",
                    "prompt": "Apply the Waza skill `waza/{{skill_name}}` to the task below.\n\nTask:\n{{task}}\n\nContext:\n{{context}}\n\nReturn the skill-guided result without restating the full skill document.",
                    "content": content,
                    "policy": { "visibility": "discoverable" },
                    "tags": ["waza", "workflow", "builtin", "skill-set"],
                    "source_path": source_path,
                    "skill_path": format!("/skills/waza/{name}"),
                    "retention_policy": "permanent",
                    "inputSchema": {
                        "type": "object",
                        "required": ["task"],
                        "properties": {
                            "task": {"type": "string"},
                            "context": {"type": "string"},
                            "skill_name": {"type": "string", "default": name}
                        }
                    }
                }),
            )
        })
        .collect()
}

fn builtin_mcp_capabilities() -> Result<Vec<HubCapability>, String> {
    Ok(vec![
        make_mcp_capability(
            "mcp:web-reader",
            "web-reader",
            "BigModel reader MCP for turning URLs into markdown.",
            "https://open.bigmodel.cn/api/mcp/web_reader/mcp",
            true,
            Some("general"),
            Some("/wiki/general/web-reader"),
        )?,
        make_mcp_capability(
            "mcp:zread",
            "zread",
            "BigModel zread MCP for repo/document reading.",
            "https://open.bigmodel.cn/api/mcp/zread/mcp",
            true,
            Some("coding"),
            Some("/wiki/coding/zread"),
        )?,
        make_mcp_capability(
            "mcp:web-search",
            "web-search",
            "BigModel search MCP for web search results.",
            "https://open.bigmodel.cn/api/mcp/web_search_prime/mcp",
            true,
            Some("general"),
            Some("/wiki/general/web-search"),
        )?,
        make_local_mcp_capability(
            "mcp:vision",
            "vision",
            "BigModel vision MCP for screenshot and chart analysis (local stdio package).",
            "npx",
            &["-y", "@z_ai/mcp-server@latest"],
            json!({
                "Z_AI_API_KEY": "${vault:ZAI_API_KEY|BIGMODEL_API_KEY|REASONING_API_KEY}",
                "Z_AI_MODE": "ZAI"
            }),
            false,
        )?,
    ])
}

fn builtin_capabilities() -> Result<Vec<HubCapability>, String> {
    let mut caps = vec![builtin_trajectory_distiller()?];
    caps.extend(builtin_coding_skills()?);
    caps.extend(builtin_trading_skills()?);
    caps.extend(builtin_waza_skills()?);
    caps.extend(builtin_mcp_capabilities()?);
    Ok(caps)
}

fn seed_builtin_sandbox_policy(server: &MemoryServer, capability_id: &str) -> Result<(), String> {
    server.with_global_store(|store| {
        let existing = store
            .get_sandbox_policy(capability_id)
            .map_err(|e| format!("lookup builtin sandbox policy {capability_id}: {e}"))?;
        if existing.is_none() {
            store
                .set_sandbox_policy(
                    capability_id,
                    "process",
                    "[]",
                    "[]",
                    "[]",
                    "[]",
                    10_000,
                    30_000,
                    2,
                    true,
                )
                .map_err(|e| format!("seed builtin sandbox policy {capability_id}: {e}"))?;
        }
        Ok(())
    })
}

pub(crate) fn seed_builtin_capabilities(server: &MemoryServer) -> Result<(), String> {
    let caps = builtin_capabilities()?;
    let inserted_or_updated = server.with_global_store(|store| {
        let mut changed = Vec::new();
        for cap in &caps {
            let existing = store
                .hub_get(&cap.id)
                .map_err(|e| format!("lookup builtin capability {}: {e}", cap.id))?;
            let should_upsert = match existing {
                None => true,
                Some(ref prev) => {
                    prev.definition != cap.definition
                        || prev.description != cap.description
                        || prev.version != cap.version
                        || prev.enabled != cap.enabled
                        || prev.review_status != cap.review_status
                        || prev.health_status != cap.health_status
                }
            };
            if should_upsert {
                store
                    .hub_register(cap)
                    .map_err(|e| format!("register builtin capability {}: {e}", cap.id))?;
                changed.push(cap.clone());
            }
        }
        Ok(changed)
    })?;

    for cap in &caps {
        if cap.cap_type.eq_ignore_ascii_case("mcp") {
            seed_builtin_sandbox_policy(server, &cap.id)?;
        }
    }

    for cap in inserted_or_updated {
        if cap.cap_type.eq_ignore_ascii_case("skill") && should_expose_skill_tool(&cap) {
            server
                .register_skill_tool(&cap)
                .map_err(|e| format!("register builtin skill tool {}: {e}", cap.id))?;
        }
    }

    Ok(())
}
