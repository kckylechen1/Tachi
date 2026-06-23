use crate::server_state::MemoryServer;
use crate::tool_params::TachiSaveParams;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};

use super::DailyPipelineReport;

pub(crate) fn render_daily_report_markdown(
    report: &DailyPipelineReport,
    health_json: &Value,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Tachi Daily Pipeline - {}\n\n", report.date));
    out.push_str("## Summary\n\n");
    out.push_str(&format!(
        "- Health Check: {}\n",
        report.health_check.summary
    ));
    out.push_str(&format!(
        "- Agent Evolution: {}\n",
        report.agent_evolution.summary
    ));
    out.push_str(&format!(
        "- Skill Evolution: {}\n",
        report.skill_evolution.summary
    ));
    out.push_str(&format!(
        "- Routing Analysis: {}\n\n",
        report.routing_analysis.summary
    ));

    out.push_str("## Health Check\n\n");
    out.push_str("```json\n");
    out.push_str(&serde_json::to_string_pretty(health_json).unwrap_or_else(|_| "{}".to_string()));
    out.push_str("\n```\n\n");

    out.push_str("## Agent Evolution\n\n");
    out.push_str("```json\n");
    out.push_str(
        &serde_json::to_string_pretty(&report.agent_evolution.details)
            .unwrap_or_else(|_| "{}".to_string()),
    );
    out.push_str("\n```\n\n");

    out.push_str("## Skill Evolution\n\n");
    out.push_str("```json\n");
    out.push_str(
        &serde_json::to_string_pretty(&report.skill_evolution.details)
            .unwrap_or_else(|_| "{}".to_string()),
    );
    out.push_str("\n```\n\n");

    out.push_str("## Routing Analysis\n\n");
    out.push_str("```json\n");
    out.push_str(
        &serde_json::to_string_pretty(&report.routing_analysis.details)
            .unwrap_or_else(|_| "{}".to_string()),
    );
    out.push_str("\n```\n");
    out
}

pub(crate) async fn save_daily_health_wiki(
    server: &MemoryServer,
    date: &str,
    markdown: &str,
) -> Result<(), String> {
    let _ = server
        .tachi_save(Parameters(TachiSaveParams {
            text: markdown.to_string(),
            id: None,
            kind: Some("wiki".to_string()),
            title: Some(format!("Tachi Daily Health {date}")),
            summary: Some(format!("Daily Pipeline report for {date}")),
            path: Some("/tachi/daily-health".to_string()),
            importance: Some(0.85),
            category: Some("experience".to_string()),
            keywords: vec![
                "tachi".to_string(),
                "daily-pipeline".to_string(),
                "health-check".to_string(),
            ],
            entities: vec!["Tachi".to_string()],
            scope: Some("global".to_string()),
            project: None,
            domain: Some("wiki".to_string()),
            retention_policy: Some("permanent".to_string()),
            force: true,
            references: Vec::new(),
            topic: Some("daily-health".to_string()),
            source: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
            files: Vec::new(),
        }))
        .await?;
    Ok(())
}

pub(crate) fn parse_llm_json(raw: &str) -> Result<Value, String> {
    let stripped = crate::llm::LlmClient::strip_code_fence(raw);
    serde_json::from_str(stripped)
        .or_else(|_| {
            let start = stripped.find('{').unwrap_or(0);
            let end = stripped
                .rfind('}')
                .map(|idx| idx + 1)
                .unwrap_or(stripped.len());
            serde_json::from_str(&stripped[start..end])
        })
        .map_err(|e| {
            format!(
                "parse daily health JSON: {e}; raw={}",
                raw.chars().take(500).collect::<String>()
            )
        })
}

pub(crate) fn parse_json_or_raw(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| json!({ "raw": raw }))
}
