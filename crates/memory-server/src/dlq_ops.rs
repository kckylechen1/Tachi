use super::*;

pub(crate) async fn handle_dlq_list(
    server: &MemoryServer,
    params: DlqListParams,
) -> Result<String, String> {
    let limit = params.limit.unwrap_or(50).min(200);
    let now = Utc::now();

    let mut dlq = server
        .dead_letters
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    dlq.retain(|dl| {
        if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&dl.timestamp) {
            (now - ts.with_timezone(&Utc)).num_seconds() < DLQ_TTL_SECS as i64
        } else {
            false
        }
    });

    let entries: Vec<serde_json::Value> = dlq
        .iter()
        .filter(|dl| {
            if let Some(ref filter) = params.status_filter {
                dl.status == *filter
            } else {
                true
            }
        })
        .rev()
        .take(limit)
        .map(|dl| {
            json!({
                "id": dl.id,
                "tool_name": dl.tool_name,
                "error": dl.error,
                "error_category": dl.error_category,
                "timestamp": dl.timestamp,
                "retry_count": dl.retry_count,
                "max_retries": dl.max_retries,
                "status": dl.status,
            })
        })
        .collect();

    let total = dlq.len();
    drop(dlq);

    serde_json::to_string(&json!({
        "total": total,
        "returned": entries.len(),
        "entries": entries,
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_dlq_retry(
    server: &MemoryServer,
    params: DlqRetryParams,
) -> Result<String, String> {
    let dead_letter = {
        let mut dlq = server
            .dead_letters
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let pos = dlq.iter().position(|dl| dl.id == params.dead_letter_id);
        match pos {
            Some(idx) => {
                let mut dl = dlq[idx].clone();
                dl.retry_count += 1;
                dl.status = "retrying".to_string();
                dlq[idx] = dl.clone();
                dl
            }
            None => {
                return Err(format!(
                    "Dead letter entry '{}' not found",
                    params.dead_letter_id
                ))
            }
        }
    };

    let retry_result = server
        .retry_dispatch(&dead_letter.tool_name, dead_letter.arguments.clone())
        .await;

    match retry_result {
        Ok(res) => {
            let mut dlq = server
                .dead_letters
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(dl) = dlq.iter_mut().find(|dl| dl.id == params.dead_letter_id) {
                dl.status = "resolved".to_string();
            }
            let text = res
                .content
                .first()
                .and_then(|c| {
                    if let rmcp::model::RawContent::Text(t) = &c.raw {
                        Some(t.text.clone())
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| "ok".to_string());
            serde_json::to_string(&json!({
                "status": "resolved",
                "dead_letter_id": params.dead_letter_id,
                "result": text,
            }))
            .map_err(|e| format!("serialize: {e}"))
        }
        Err(e) => {
            let mut dlq = server
                .dead_letters
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(dl) = dlq.iter_mut().find(|dl| dl.id == params.dead_letter_id) {
                if dl.retry_count >= dl.max_retries {
                    dl.status = "abandoned".to_string();
                } else {
                    dl.status = "pending".to_string();
                }
                dl.error = format!("{e}");
            }
            Err(format!("Retry failed for '{}': {e}", params.dead_letter_id))
        }
    }
}
