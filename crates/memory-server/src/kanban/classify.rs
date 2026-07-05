use super::normalize::normalize_card_priority;
use super::types::KanbanClassification;
use super::*;

pub(super) async fn classify_kanban_message(
    title: &str,
    body: &str,
) -> Result<KanbanClassification, String> {
    let model_url = std::env::var("KANBAN_MODEL_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:11434/api/generate".to_string());
    let model_name =
        std::env::var("KANBAN_MODEL_NAME").unwrap_or_else(|_| "qwen2.5:32b".to_string());

    let prompt = format!(
        "Classify this inter-agent message. Return JSON only.\\nTitle: {title}\\nBody: {body}\\nOutput: {{\"topic\":\"...\",\"keywords\":[\"...\"],\"priority_suggestion\":\"...\"}}"
    );

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("build classifier client: {e}"))?;

    let response = client
        .post(&model_url)
        .json(&json!({
            "model": model_name,
            "prompt": prompt,
            "stream": false,
            "format": "json"
        }))
        .send()
        .await
        .map_err(|e| format!("kanban classifier request failed: {e}"))?;

    let status = response.status();
    let raw_text = response
        .text()
        .await
        .map_err(|e| format!("kanban classifier response read failed: {e}"))?;
    if !status.is_success() {
        return Err(format!("kanban classifier error {status}: {raw_text}"));
    }

    let raw_json: serde_json::Value = serde_json::from_str(&raw_text)
        .map_err(|e| format!("kanban classifier response is not valid JSON: {e}"))?;

    let payload_text = raw_json
        .get("response")
        .and_then(|v| v.as_str())
        .or_else(|| {
            raw_json
                .get("choices")
                .and_then(|v| v.as_array())
                .and_then(|choices| choices.first())
                .and_then(|choice| choice.get("message"))
                .and_then(|message| message.get("content"))
                .and_then(|v| v.as_str())
        })
        .ok_or_else(|| "classifier payload missing response text".to_string())?;

    let cleaned = tachi_llm::LlmClient::strip_code_fence(payload_text)
        .trim()
        .to_string();
    serde_json::from_str::<KanbanClassification>(&cleaned)
        .map_err(|e| format!("failed to parse classifier JSON payload: {e}; raw={cleaned}"))
}

pub(super) async fn enrich_kanban_card_classification(
    db_path: Arc<PathBuf>,
    card_id: String,
    card_text: String,
    card_summary: String,
    card_source: String,
    base_metadata: serde_json::Value,
    expected_revision: i64,
) -> Result<(), String> {
    let classification = classify_kanban_message(&card_summary, &card_text).await?;
    let mut metadata = base_metadata;
    if !metadata.is_object() {
        metadata = json!({});
    }

    if let Some(topic) = classification.topic.as_ref() {
        metadata["topic"] = json!(topic);
    }
    if !classification.keywords.is_empty() {
        metadata["keywords"] = json!(classification.keywords);
    }
    if let Some(priority) = classification.priority_suggestion.as_ref() {
        metadata["priority_suggestion"] = json!(normalize_card_priority(priority));
    }
    metadata["classified_at"] = json!(Utc::now().to_rfc3339());

    let mut store = MemoryStore::open(db_path.to_string_lossy().as_ref())
        .map_err(|e| format!("open db for kanban enrichment: {e}"))?;
    let updated = store
        .update_with_revision(
            &card_id,
            &card_text,
            &card_summary,
            &card_source,
            &metadata,
            None,
            expected_revision,
        )
        .map_err(|e| format!("kanban enrichment update failed: {e}"))?;

    if !updated {
        return Err("kanban enrichment skipped: revision changed".to_string());
    }

    Ok(())
}
