use super::*;

struct DownstreamFixture {
    shape: &'static str,
    memory_id: &'static str,
    path: &'static str,
    summary: &'static str,
    text: &'static str,
    query: &'static str,
    actor_voice: &'static str,
    event_id: &'static str,
    pattern_key: &'static str,
}

fn tachi_event_params(action: &str) -> TachiEventParams {
    TachiEventParams {
        action: action.to_string(),
        format: None,
        id: None,
        source_repo: None,
        adapter: None,
        project: None,
        domain: None,
        session_id: None,
        actor: None,
        event_type: None,
        authority: None,
        effects: Vec::new(),
        projection_hints: Vec::new(),
        payload: None,
        provenance: None,
        created_at: None,
        limit: 20,
        path_prefix: None,
        dry_run: false,
    }
}

fn downstream_fixtures() -> Vec<DownstreamFixture> {
    vec![
        DownstreamFixture {
            shape: "hypermem",
            memory_id: "dogfood-hypermem-portable-kernel",
            path: "/downstream/hypermem/portable-kernel",
            summary: "Hypermem portable kernel fixture",
            text: "HYPERMEM_PORTABLE_KERNEL_NEEDLE save search recall vector readiness user voice lane.",
            query: "HYPERMEM_PORTABLE_KERNEL_NEEDLE",
            actor_voice: "user",
            event_id: "dogfood-hypermem-pattern",
            pattern_key: "hypermem-portable-kernel",
        },
        DownstreamFixture {
            shape: "zeroclaw_chat_agent",
            memory_id: "dogfood-zeroclaw-chat-portable-kernel",
            path: "/downstream/zeroclaw/chat-agent/portable-kernel",
            summary: "zeroclaw chat agent portable kernel fixture",
            text: "ZEROCLAW_CHAT_AGENT_PORTABLE_NEEDLE save search recall vector readiness assistant voice lane.",
            query: "ZEROCLAW_CHAT_AGENT_PORTABLE_NEEDLE",
            actor_voice: "assistant",
            event_id: "dogfood-zeroclaw-pattern",
            pattern_key: "zeroclaw-chat-portable-kernel",
        },
    ]
}

async fn save_fixture(
    server: &crate::MemoryServer,
    fixture: &DownstreamFixture,
) -> Result<Value, String> {
    let mut save = tachi_memory_params("save");
    save.format = Some("json".to_string());
    save.id = Some(fixture.memory_id.to_string());
    save.force = true;
    save.scope = Some("project".to_string());
    save.kind = Some("memory".to_string());
    save.category = Some("fact".to_string());
    save.path = Some(fixture.path.to_string());
    save.summary = Some(fixture.summary.to_string());
    save.text = Some(fixture.text.to_string());
    save.keywords = vec![
        "downstream-dogfood".to_string(),
        fixture.shape.to_string(),
        "portable-kernel".to_string(),
    ];
    save.entities = vec![fixture.shape.to_string(), "portable-kernel".to_string()];
    save.domain = Some("downstream_dogfood".to_string());
    save.metadata = Some(json!({
        "fixture_shape": fixture.shape,
        "voice": fixture.actor_voice,
        "contract": "downstream_no_product_surface",
    }));

    crate::facade_memory_ops::handle_tachi_memory(server, save).await?;
    Ok(json!({"status": "completed", "memory_id": fixture.memory_id}))
}

async fn search_fixture(
    server: &crate::MemoryServer,
    fixture: &DownstreamFixture,
) -> Result<(Value, Vec<Value>), String> {
    let mut search = tachi_memory_params("search");
    search.format = None;
    search.scope = Some("memory".to_string());
    search.query = Some(fixture.query.to_string());
    search.top_k = 3;

    let body = crate::facade_memory_ops::handle_tachi_memory(server, search).await?;
    let parsed: Value =
        serde_json::from_str(&body).map_err(|e| format!("parse search JSON: {e}"))?;
    let rows = parsed["sections"]
        .as_array()
        .and_then(|sections| {
            sections
                .iter()
                .find(|section| section["name"] == json!("Memory"))
        })
        .and_then(|section| section["rows"].as_array())
        .cloned()
        .unwrap_or_default();
    let stable_rows = rows
        .iter()
        .map(|row| {
            json!({
                "id": row.get("id").cloned().unwrap_or(Value::Null),
                "path": row.get("path").cloned().unwrap_or(Value::Null),
                "summary": row.get("summary").cloned().unwrap_or(Value::Null),
                "score": row
                    .get("relevance")
                    .or_else(|| row.get("score"))
                    .cloned()
                    .unwrap_or(Value::Null),
            })
        })
        .collect::<Vec<_>>();

    Ok((
        json!({
            "status": "completed",
            "query": fixture.query,
            "returned_ids": rows
                .iter()
                .filter_map(|row| row.get("id").and_then(Value::as_str))
                .collect::<Vec<_>>(),
            "stable_rows": stable_rows,
        }),
        rows,
    ))
}

async fn recall_fixture(
    server: &crate::MemoryServer,
    fixture: &DownstreamFixture,
) -> Result<Value, String> {
    let mut recall = tachi_memory_params("recall_simulate");
    recall.format = Some("json".to_string());
    recall.scope = Some("memory".to_string());
    recall.top_k = 3;
    recall.metadata = Some(json!({
        "cases": [
            {
                "name": format!("{}-hit", fixture.shape),
                "query": fixture.query,
                "expected_id": fixture.memory_id
            }
        ]
    }));

    let body = crate::facade_memory_ops::handle_tachi_memory(server, recall).await?;
    serde_json::from_str(&body).map_err(|e| format!("parse recall JSON: {e}"))
}

async fn true_empty_recall_count(
    server: &crate::MemoryServer,
    fixture: &DownstreamFixture,
) -> Result<usize, String> {
    let mut empty = tachi_memory_params("search");
    empty.format = None;
    empty.scope = Some("memory".to_string());
    empty.query = Some(fixture.query.to_string());
    empty.path_prefix = Some("/downstream/no-such-portable-kernel-fixture".to_string());
    empty.top_k = 3;

    let body = crate::facade_memory_ops::handle_tachi_memory(server, empty).await?;
    let parsed: Value =
        serde_json::from_str(&body).map_err(|e| format!("parse true-empty search JSON: {e}"))?;
    let rows = parsed["sections"]
        .as_array()
        .and_then(|sections| {
            sections
                .iter()
                .find(|section| section["name"] == json!("Memory"))
        })
        .and_then(|section| section["rows"].as_array())
        .cloned()
        .unwrap_or_default();

    Ok(usize::from(rows.is_empty()))
}

async fn readiness_fixture(server: &crate::MemoryServer) -> Result<Value, String> {
    let mut readiness = tachi_memory_params("readiness");
    readiness.format = Some("json".to_string());
    let body = crate::facade_memory_ops::handle_tachi_memory(server, readiness).await?;
    let parsed: Value =
        serde_json::from_str(&body).map_err(|e| format!("parse readiness JSON: {e}"))?;
    Ok(json!({
        "status": parsed["status"].clone(),
        "vector_health": parsed["vector_health"].clone(),
        "ready_path": "#789 portable vector/backfill readiness issue; first implementation merged as PR #801",
    }))
}

async fn event_projection_fixture(
    server: &crate::MemoryServer,
    fixture: &DownstreamFixture,
) -> Result<Value, String> {
    let mut emit = tachi_event_params("emit");
    emit.id = Some(fixture.event_id.to_string());
    emit.source_repo = Some("downstream-dogfood".to_string());
    emit.adapter = Some(fixture.shape.to_string());
    emit.domain = Some("downstream_dogfood".to_string());
    emit.session_id = Some(format!("{}-session", fixture.shape));
    emit.actor = Some(fixture.actor_voice.to_string());
    emit.event_type = Some("pattern.observed".to_string());
    emit.authority = Some("collect_only".to_string());
    emit.effects = vec!["recall".to_string()];
    emit.projection_hints = vec!["pattern".to_string()];
    emit.payload = Some(json!({
        "pattern_key": fixture.pattern_key,
        "summary": fixture.summary,
        "text": format!("{} projection basics through the portable event surface.", fixture.summary),
    }));
    crate::event_ops::handle_tachi_event(server, emit).await?;

    let mut project = tachi_event_params("project");
    project.domain = Some("downstream_dogfood".to_string());
    project.projection_hints = vec!["pattern".to_string()];
    project.limit = 10;
    let body = crate::event_ops::handle_tachi_event(server, project).await?;
    let parsed: Value =
        serde_json::from_str(&body).map_err(|e| format!("parse project JSON: {e}"))?;
    Ok(json!({
        "status": parsed["status"].clone(),
        "projected_count": parsed["projected_count"].clone(),
    }))
}

fn adapter_policy_denials() -> Value {
    json!([
        {
            "surface": "tachi_gh",
            "classification": "adapter_policy_failure",
            "verification": "declared_static_denial",
            "reason": "downstream kernel fixture must not depend on Tachi GitHub/product lifecycle tools"
        },
        {
            "surface": "dispatch_task_lifecycle",
            "classification": "adapter_policy_failure",
            "verification": "declared_static_denial",
            "reason": "dispatch-only task lifecycle is outside the portable memory kernel"
        },
        {
            "surface": "ship_release_automation",
            "classification": "adapter_policy_failure",
            "verification": "declared_static_denial",
            "reason": "ship/release automation belongs to the Tachi product adapter, not downstream forks"
        },
        {
            "surface": "github_pr_lifecycle",
            "classification": "adapter_policy_failure",
            "verification": "declared_static_denial",
            "reason": "GitHub PR lifecycle is explicitly excluded from no-product-surface dogfood"
        }
    ])
}

fn recall_diagnostics(
    fixture: &DownstreamFixture,
    recall: &Value,
    rows: &[Value],
    true_empty_count: usize,
) -> Value {
    let mut voice_contribution = serde_json::Map::new();
    voice_contribution.insert(fixture.actor_voice.to_string(), json!(1));

    let total_recall_cases = recall["case_count"].as_u64().unwrap_or(0);
    let fallback_needed_count = recall["metrics"]["miss_count"].as_u64().unwrap_or(0);
    let fallback_rate = if total_recall_cases == 0 {
        0.0
    } else {
        fallback_needed_count as f64 / total_recall_cases as f64
    };

    json!({
        "lane_contribution": {
            "memory": rows.iter().filter(|row| row["id"] == json!(fixture.memory_id)).count()
        },
        "voice_contribution": voice_contribution,
        "fallback_rate": {
            "value": fallback_rate,
            "fallback_needed_count": fallback_needed_count,
            "total_recall_cases": total_recall_cases,
            "source": "recall_simulate.metrics.miss_count"
        },
        "true_empty_recall_count": true_empty_count,
        "stable_result_shape": ["id", "path", "summary", "score"],
        "recall_metrics": recall["metrics"].clone(),
    })
}

async fn run_fixture(
    server: &crate::MemoryServer,
    fixture: &DownstreamFixture,
) -> Result<Value, String> {
    let save = save_fixture(server, fixture).await?;
    let (search, rows) = search_fixture(server, fixture).await?;
    let recall = recall_fixture(server, fixture).await?;
    let true_empty_count = true_empty_recall_count(server, fixture).await?;
    let vector_status = readiness_fixture(server).await?;
    let event_projection = event_projection_fixture(server, fixture).await?;
    let diagnostics = recall_diagnostics(fixture, &recall, &rows, true_empty_count);

    Ok(json!({
        "shape": fixture.shape,
        "kernel": {
            "save": save,
            "search": search,
            "recall": {
                "status": recall["status"].clone(),
                "case_count": recall["case_count"].clone(),
                "metrics": recall["metrics"].clone(),
            },
            "vector_status": vector_status,
            "event_projection": event_projection,
        },
        "recall_diagnostics": diagnostics,
        "adapter_policy": {
            "status": "completed",
            "verification_mode": "declared_static_denial_not_runtime_tool_probe",
            "denied_surfaces": adapter_policy_denials(),
        },
    }))
}

async fn run_downstream_no_product_surface_dogfood_harness() -> Result<Value, String> {
    let server = make_server();
    let mut fixtures = Vec::new();
    for fixture in downstream_fixtures() {
        fixtures.push(run_fixture(&server, &fixture).await?);
    }

    Ok(json!({
        "status": "completed",
        "contract": "downstream_no_product_surface",
        "classification": {
            "kernel_failures": [],
            "adapter_policy_failures": [
                "tachi_gh",
                "dispatch_task_lifecycle",
                "ship_release_automation",
                "github_pr_lifecycle"
            ],
            "adapter_policy_verification": "declared_static_denial_not_runtime_tool_probe"
        },
        "fixtures": fixtures,
    }))
}

#[tokio::test]
async fn downstream_no_product_surface_dogfood_harness_reports_kernel_and_policy_results() {
    let report = run_downstream_no_product_surface_dogfood_harness()
        .await
        .expect("dogfood harness should run");

    assert_eq!(report["status"], json!("completed"));
    assert_eq!(report["contract"], json!("downstream_no_product_surface"));
    assert_eq!(report["fixtures"][0]["shape"], json!("hypermem"));
    assert_eq!(report["fixtures"][1]["shape"], json!("zeroclaw_chat_agent"));

    for fixture in report["fixtures"].as_array().expect("fixtures array") {
        assert_eq!(fixture["kernel"]["save"]["status"], json!("completed"));
        assert_eq!(fixture["kernel"]["search"]["status"], json!("completed"));
        assert_eq!(fixture["kernel"]["recall"]["status"], json!("completed"));
        assert_eq!(
            fixture["kernel"]["vector_status"]["ready_path"],
            json!("#789 portable vector/backfill readiness issue; first implementation merged as PR #801")
        );
        assert_eq!(
            fixture["kernel"]["event_projection"]["status"],
            json!("completed")
        );

        let diagnostics = &fixture["recall_diagnostics"];
        assert!(
            diagnostics["lane_contribution"]["memory"]
                .as_u64()
                .unwrap_or(0)
                >= 1
        );
        let voice_total: u64 = diagnostics["voice_contribution"]
            .as_object()
            .expect("voice contribution object")
            .values()
            .filter_map(Value::as_u64)
            .sum();
        assert!(voice_total >= 1, "voice contribution should not be empty");
        assert_eq!(diagnostics["fallback_rate"]["value"], json!(0.0));
        assert_eq!(
            diagnostics["fallback_rate"]["fallback_needed_count"],
            json!(0)
        );
        assert_eq!(diagnostics["fallback_rate"]["total_recall_cases"], json!(1));
        assert_eq!(
            diagnostics["fallback_rate"]["source"],
            json!("recall_simulate.metrics.miss_count")
        );
        assert_eq!(diagnostics["true_empty_recall_count"], json!(1));
        assert_eq!(
            diagnostics["stable_result_shape"],
            json!(["id", "path", "summary", "score"])
        );

        let denied = fixture["adapter_policy"]["denied_surfaces"]
            .as_array()
            .expect("denied surface array");
        assert_eq!(
            fixture["adapter_policy"]["verification_mode"],
            json!("declared_static_denial_not_runtime_tool_probe")
        );
        for surface in [
            "tachi_gh",
            "dispatch_task_lifecycle",
            "ship_release_automation",
            "github_pr_lifecycle",
        ] {
            assert!(
                denied.iter().any(|entry| {
                    entry["surface"] == json!(surface)
                        && entry["classification"] == json!("adapter_policy_failure")
                        && entry["verification"] == json!("declared_static_denial")
                }),
                "{surface} should be declared as an adapter policy failure: {denied:?}"
            );
        }
    }
}
