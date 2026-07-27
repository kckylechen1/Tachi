use super::*;

pub(crate) fn append_wiki_log(server: &MemoryServer, operation: &str, details: &str) {
    let now = Utc::now().to_rfc3339();
    let log_line = format!(
        "## [{}] {} | {}",
        now,
        operation,
        compact_log_details(details.trim())
    );
    let entry = MemoryEntry {
        id: "wiki-operation-log".to_string(),
        path: "/wiki/_log".to_string(),
        summary: "Wiki operation log".to_string(),
        text: log_line.clone(),
        importance: 0.3,
        timestamp: now,
        valid_from: String::new(),
        valid_until: None,
        category: "other".to_string(),
        topic: "wiki_log".to_string(),
        keywords: vec!["wiki".to_string(), "log".to_string()],
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        source: "mcp".to_string(),
        scope: "general".to_string(),
        archived: false,
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        metadata: json!({"wiki_log": true}),
        vector: None,
        retention_policy: Some("durable".to_string()),
        domain: Some("wiki".to_string()),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    };

    let append_result = server.with_named_project_store("wiki", |store| {
        let mut entry = entry.clone();
        if let Some(existing) = store
            .get("wiki-operation-log")
            .map_err(|e| format!("wiki_log get: {e}"))?
        {
            entry.text = compact_wiki_log(&existing.text, &log_line);
            entry.revision = existing.revision;
        }
        store
            .upsert(&entry)
            .map_err(|e| format!("wiki_log upsert: {e}"))
    });

    if append_result.is_err() {
        if let Err(error) = server.with_global_store(|store| {
            let mut entry = entry;
            entry.metadata = json!({"wiki_log": true, "fallback_db": "global"});
            if let Some(existing) = store
                .get("wiki-operation-log")
                .map_err(|e| format!("wiki_log fallback get: {e}"))?
            {
                entry.text = compact_wiki_log(&existing.text, &log_line);
                entry.revision = existing.revision;
            }
            store
                .upsert(&entry)
                .map_err(|e| format!("wiki_log fallback upsert: {e}"))
        }) {
            tracing::warn!(
                error = %error,
                "failed to write wiki operation log fallback"
            );
        }
    }
}

fn compact_wiki_log(existing: &str, new_line: &str) -> String {
    let mut entries = existing
        .split("\n\n")
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    entries.push(new_line.trim().to_string());
    if entries.len() > WIKI_LOG_MAX_ENTRIES {
        entries.drain(0..entries.len() - WIKI_LOG_MAX_ENTRIES);
    }
    while entries.join("\n\n").len() > WIKI_LOG_MAX_BYTES && entries.len() > 1 {
        entries.remove(0);
    }
    entries.join("\n\n")
}

fn compact_log_details(details: &str) -> String {
    if details.len() <= WIKI_LOG_ENTRY_MAX_BYTES {
        return details.to_string();
    }
    let marker = "... [truncated]";
    let mut end = WIKI_LOG_ENTRY_MAX_BYTES.saturating_sub(marker.len());
    while end > 0 && !details.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &details[..end], marker)
}
