use super::*;

pub(super) const KANBAN_CATEGORY: &str = "kanban";
pub(super) const KANBAN_PATH_PREFIX: &str = "/kanban/";
pub(super) const DEFAULT_KANBAN_GC_MAX_AGE_DAYS: u64 = 30;

fn default_card_priority() -> String {
    "medium".to_string()
}

fn default_card_type() -> String {
    "request".to_string()
}

fn default_include_broadcast() -> bool {
    true
}

fn default_inbox_limit() -> usize {
    100
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct PostCardParams {
    /// Source agent ID (e.g. "hapi")
    pub from_agent: String,
    /// Destination agent ID, or "*" for broadcast
    pub to_agent: String,
    /// Short card title
    pub title: String,
    /// Card body content
    pub body: String,
    /// Priority: low | medium | high | critical
    #[serde(default = "default_card_priority")]
    pub priority: String,
    /// Card type: request | report | alert | handoff
    #[serde(default = "default_card_type")]
    pub card_type: String,
    /// Optional thread correlation ID
    #[serde(default)]
    pub thread_id: Option<String>,
    /// Workspace or project identifier this card relates to
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// Project identifier (legacy alias for workspace_id)
    #[serde(default)]
    pub project_id: Option<String>,
    /// Conversation/session ID that spawned this card
    #[serde(default)]
    pub conversation_id: Option<String>,
    /// Agent session identifier (unique per agent instance)
    #[serde(default)]
    pub agent_session_id: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct CheckInboxParams {
    /// Agent ID receiving cards
    pub agent_id: String,
    /// Optional status filter (e.g. "open")
    #[serde(default)]
    pub status_filter: Option<String>,
    /// Optional ISO timestamp lower bound (inclusive)
    #[serde(default)]
    pub since: Option<String>,
    /// Include broadcast cards addressed to "*"
    #[serde(default = "default_include_broadcast")]
    pub include_broadcast: bool,
    /// Maximum cards returned
    #[serde(default = "default_inbox_limit")]
    pub limit: usize,
    /// Optional workspace filter — only return cards from this workspace
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// Optional conversation filter — only return cards from this conversation
    #[serde(default)]
    pub conversation_id: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct UpdateCardParams {
    /// Kanban card memory ID
    pub card_id: String,
    /// New status: open | acknowledged | resolved | expired
    pub new_status: String,
    /// Optional threaded response appended to the card
    #[serde(default)]
    pub response_text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct KanbanClassification {
    #[serde(default)]
    topic: Option<String>,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    priority_suggestion: Option<String>,
}

pub(super) fn normalize_agent_id(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

pub(super) fn normalize_card_priority(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "low" => "low".to_string(),
        "medium" => "medium".to_string(),
        "high" => "high".to_string(),
        "critical" => "critical".to_string(),
        _ => default_card_priority(),
    }
}

pub(super) fn normalize_card_type(value: &str) -> String {
    match value.trim().to_ascii_lowercase() {
        s if matches!(
            s.as_str(),
            "request" | "report" | "alert" | "handoff" | "ack" | "progress" | "result"
        ) =>
        {
            s
        }
        _ => default_card_type(),
    }
}

pub(super) fn normalize_card_status(value: &str) -> Option<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "open" => Some("open".to_string()),
        "acknowledged" => Some("acknowledged".to_string()),
        "resolved" => Some("resolved".to_string()),
        "expired" => Some("expired".to_string()),
        _ => None,
    }
}

pub(super) fn kanban_priority_rank(priority: &str) -> u8 {
    match priority {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        _ => 3,
    }
}

pub(super) fn kanban_priority_importance(priority: &str) -> f64 {
    match priority {
        "critical" => 1.0,
        "high" => 0.9,
        "medium" => 0.75,
        _ => 0.6,
    }
}

pub(super) fn card_metadata_str(entry: &MemoryEntry, key: &str) -> Option<String> {
    entry
        .metadata
        .get(key)
        .and_then(|v| v.as_str())
        .map(|v| v.to_string())
}

pub(super) fn card_to_agent(entry: &MemoryEntry) -> Option<String> {
    card_metadata_str(entry, "to_agent")
        .map(|v| normalize_agent_id(&v))
        .or_else(|| {
            entry
                .path
                .strip_prefix(KANBAN_PATH_PREFIX)
                .and_then(|rest| rest.split('/').nth(1))
                .map(normalize_agent_id)
        })
}

pub(super) fn card_from_agent(entry: &MemoryEntry) -> Option<String> {
    card_metadata_str(entry, "from_agent")
        .map(|v| normalize_agent_id(&v))
        .or_else(|| {
            entry
                .path
                .strip_prefix(KANBAN_PATH_PREFIX)
                .and_then(|rest| rest.split('/').next())
                .map(normalize_agent_id)
        })
}

pub(super) fn card_status(entry: &MemoryEntry) -> Option<String> {
    card_metadata_str(entry, "status").map(|v| v.to_ascii_lowercase())
}

pub(super) fn card_priority(entry: &MemoryEntry) -> String {
    card_metadata_str(entry, "priority")
        .map(|v| normalize_card_priority(&v))
        .unwrap_or_else(default_card_priority)
}

pub(super) fn card_type(entry: &MemoryEntry) -> String {
    card_metadata_str(entry, "card_type")
        .map(|v| normalize_card_type(&v))
        .unwrap_or_else(default_card_type)
}

pub(super) fn gc_expired_kanban_cards(
    store: &mut MemoryStore,
    max_age_days: u64,
) -> Result<usize, String> {
    let cutoff = chrono::Utc::now()
        - chrono::Duration::days(std::cmp::min(max_age_days, i64::MAX as u64) as i64);
    let mut stmt = store
        .connection()
        .prepare(
            "SELECT id, timestamp, metadata
             FROM memories
             WHERE category = ?1 AND path LIKE ?2",
        )
        .map_err(|e| format!("prepare kanban GC query failed: {e}"))?;
    let rows = stmt
        .query_map((KANBAN_CATEGORY, format!("{KANBAN_PATH_PREFIX}%")), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| format!("query expired kanban cards failed: {e}"))?;

    let mut ids_to_delete = Vec::new();
    for row in rows {
        let (id, timestamp, metadata_json) =
            row.map_err(|e| format!("read expired kanban card candidate failed: {e}"))?;
        let metadata: serde_json::Value = serde_json::from_str(&metadata_json)
            .map_err(|e| format!("parse kanban card metadata for '{id}' failed: {e}"))?;
        let Some(status) = metadata
            .get("status")
            .and_then(|value| value.as_str())
            .and_then(normalize_card_status)
        else {
            continue;
        };
        if status != "resolved" && status != "expired" {
            continue;
        }

        let timestamp = chrono::DateTime::parse_from_rfc3339(&timestamp)
            .map_err(|e| format!("parse kanban card timestamp for '{id}' failed: {e}"))?
            .with_timezone(&chrono::Utc);
        if timestamp < cutoff {
            ids_to_delete.push(id);
        }
    }
    drop(stmt);

    let mut deleted = 0usize;
    for id in ids_to_delete {
        if store
            .delete(&id)
            .map_err(|e| format!("delete expired kanban card '{id}' failed: {e}"))?
        {
            deleted += 1;
        }
    }

    Ok(deleted)
}

fn add_trimmed_str_to_metadata(
    metadata: &mut serde_json::Value,
    key: &str,
    value: &Option<String>,
) {
    if let Some(value) = value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        metadata[key] = json!(value);
    }
}

pub(super) fn card_matches_inbox(
    entry: &MemoryEntry,
    params: &CheckInboxParams,
    agent_id: &str,
) -> bool {
    if entry.category != KANBAN_CATEGORY || entry.archived {
        return false;
    }
    let to_agent = match card_to_agent(entry) {
        Some(v) => v,
        None => return false,
    };
    if to_agent != agent_id && !(params.include_broadcast && to_agent == "*") {
        return false;
    }
    if let Some(filter) = params
        .status_filter
        .as_ref()
        .and_then(|v| normalize_card_status(v))
    {
        if card_status(entry).as_deref() != Some(filter.as_str()) {
            return false;
        }
    }
    if let Some(since) = params.since.as_ref() {
        if entry.timestamp < *since {
            return false;
        }
    }
    if let Some(ref ws_filter) = params.workspace_id {
        let card_ws = card_metadata_str(entry, "workspace_id")
            .or_else(|| card_metadata_str(entry, "project_id"))
            .unwrap_or_default();
        if card_ws != *ws_filter {
            return false;
        }
    }
    if let Some(ref conv_filter) = params.conversation_id {
        let card_conv = card_metadata_str(entry, "conversation_id").unwrap_or_default();
        if card_conv != *conv_filter {
            return false;
        }
    }
    true
}

async fn classify_kanban_message(title: &str, body: &str) -> Result<KanbanClassification, String> {
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

    let cleaned = llm::LlmClient::strip_code_fence(payload_text)
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

pub(crate) async fn handle_post_card(
    server: &MemoryServer,
    params: PostCardParams,
) -> Result<String, String> {
    let from_agent = normalize_agent_id(&params.from_agent);
    let to_agent = normalize_agent_id(&params.to_agent);
    if from_agent.is_empty() {
        return Err("from_agent cannot be empty".to_string());
    }
    if to_agent.is_empty() {
        return Err("to_agent cannot be empty".to_string());
    }

    let title = params.title.trim().to_string();
    let body = params.body.trim().to_string();
    if title.is_empty() {
        return Err("title cannot be empty".to_string());
    }
    if body.is_empty() {
        return Err("body cannot be empty".to_string());
    }

    let priority = normalize_card_priority(&params.priority);
    let card_type = normalize_card_type(&params.card_type);
    let status = "open".to_string();
    let now = Utc::now().to_rfc3339();
    let card_id = uuid::Uuid::new_v4().to_string();

    let mut metadata = json!({
        "from_agent": from_agent,
        "to_agent": to_agent,
        "status": status,
        "priority": priority,
        "card_type": card_type,
        "created_at": now,
    });
    add_trimmed_str_to_metadata(&mut metadata, "thread_id", &params.thread_id);
    add_trimmed_str_to_metadata(&mut metadata, "workspace_id", &params.workspace_id);
    add_trimmed_str_to_metadata(&mut metadata, "project_id", &params.project_id);
    add_trimmed_str_to_metadata(&mut metadata, "conversation_id", &params.conversation_id);
    add_trimmed_str_to_metadata(&mut metadata, "agent_session_id", &params.agent_session_id);
    metadata = crate::provenance::inject_provenance(
        server,
        metadata,
        "post_card",
        "kanban_card",
        Some("global"),
        DbScope::Global,
        json!({
            "from_agent": params.from_agent.clone(),
            "to_agent": params.to_agent.clone(),
            "workspace_id": params.workspace_id.clone(),
            "project_id": params.project_id.clone(),
            "conversation_id": params.conversation_id.clone(),
            "agent_session_id": params.agent_session_id.clone(),
        }),
    );
    // Kanban lives in the global DB but uses per-agent paths under /kanban;
    // opt in to cross-project routing so path validation lets it through.
    if let Some(obj) = metadata.as_object_mut() {
        obj.insert(
            "allow_cross_project".to_string(),
            serde_json::Value::Bool(true),
        );
    }

    let entry = MemoryEntry {
        id: card_id.clone(),
        path: format!(
            "/kanban/{}/{}",
            normalize_agent_id(&params.from_agent),
            normalize_agent_id(&params.to_agent)
        ),
        summary: title.clone(),
        text: body.clone(),
        importance: kanban_priority_importance(metadata["priority"].as_str().unwrap_or("medium")),
        timestamp: now,
        category: KANBAN_CATEGORY.to_string(),
        topic: String::new(),
        keywords: vec![],
        persons: vec![],
        entities: vec![
            normalize_agent_id(&params.from_agent),
            normalize_agent_id(&params.to_agent),
        ],
        location: String::new(),
        source: "agent".to_string(),
        scope: "global".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        vector: None,
        metadata: metadata.clone(),
        retention_policy: None,
        domain: None,
    };

    server.with_global_store(|store| {
        store
            .upsert(&entry)
            .map_err(|e| format!("failed to save kanban card: {e}"))
    })?;

    let classify_enabled = parse_env_bool("KANBAN_CLASSIFY_ENABLED").unwrap_or(false);
    if classify_enabled {
        let db_path = server.global_db_path.clone();
        let card_id_clone = card_id.clone();
        let body_clone = body.clone();
        let title_clone = title.clone();
        let source_clone = entry.source.clone();
        let metadata_clone = metadata.clone();
        let expected_revision = entry.revision;
        tokio::spawn(async move {
            if let Err(e) = enrich_kanban_card_classification(
                db_path,
                card_id_clone,
                body_clone,
                title_clone,
                source_clone,
                metadata_clone,
                expected_revision,
            )
            .await
            {
                eprintln!("[kanban] classification skipped for card: {e}");
            }
        });
    }

    let mut resp = serde_json::Map::new();
    resp.insert("status".into(), json!("posted"));
    resp.insert("card_id".into(), json!(card_id));
    resp.insert("db".into(), json!("global"));
    resp.insert("classification_enqueued".into(), json!(classify_enabled));
    serde_json::to_string(&serde_json::Value::Object(resp)).map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_check_inbox(
    server: &MemoryServer,
    params: CheckInboxParams,
) -> Result<String, String> {
    let agent_id = normalize_agent_id(&params.agent_id);
    if agent_id.is_empty() {
        return Err("agent_id cannot be empty".to_string());
    }
    let limit = params.limit.clamp(1, 1000);

    let mut cards: Vec<MemoryEntry> = server
        .with_global_store_read(|store| {
            store
                .list_by_path(KANBAN_PATH_PREFIX, limit * 4, false)
                .map_err(|e| format!("list kanban cards failed: {e}"))
        })?
        .into_iter()
        .filter(|entry| card_matches_inbox(entry, &params, &agent_id))
        .collect();

    cards.sort_by(|a, b| {
        let pa = kanban_priority_rank(card_priority(a).as_str());
        let pb = kanban_priority_rank(card_priority(b).as_str());
        pa.cmp(&pb).then_with(|| b.timestamp.cmp(&a.timestamp))
    });
    cards.truncate(limit);

    let payload: Vec<serde_json::Value> = cards
        .into_iter()
        .map(|entry| {
            json!({
                "id": entry.id,
                "db": "global",
                "from_agent": card_from_agent(&entry),
                "to_agent": card_to_agent(&entry),
                "status": card_status(&entry).unwrap_or_else(|| "open".to_string()),
                "priority": card_priority(&entry),
                "card_type": card_type(&entry),
                "thread_id": card_metadata_str(&entry, "thread_id"),
                "workspace_id": card_metadata_str(&entry, "workspace_id"),
                "project_id": card_metadata_str(&entry, "project_id"),
                "conversation_id": card_metadata_str(&entry, "conversation_id"),
                "agent_session_id": card_metadata_str(&entry, "agent_session_id"),
                "title": entry.summary,
                "body": entry.text,
                "path": entry.path,
                "timestamp": entry.timestamp,
                "topic": entry.topic,
                "keywords": entry.keywords,
                "metadata": entry.metadata,
            })
        })
        .collect();

    serde_json::to_string(&json!({
        "agent_id": agent_id,
        "count": payload.len(),
        "cards": payload,
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(crate) async fn handle_update_card(
    server: &MemoryServer,
    params: UpdateCardParams,
) -> Result<String, String> {
    let new_status = normalize_card_status(&params.new_status).ok_or_else(|| {
        "new_status must be one of open|acknowledged|resolved|expired".to_string()
    })?;

    let mut entry = server
        .with_global_store_read(|store| {
            store
                .get(&params.card_id)
                .map_err(|e| format!("get card failed: {e}"))
        })?
        .ok_or_else(|| format!("kanban card '{}' not found in global db", params.card_id))?;
    if entry.category != KANBAN_CATEGORY {
        return Err(format!(
            "memory '{}' is not a kanban card (category={})",
            entry.id, entry.category
        ));
    }

    let mut metadata = entry.metadata.clone();
    if !metadata.is_object() {
        metadata = json!({});
    }
    metadata["status"] = json!(new_status);
    metadata["updated_at"] = json!(Utc::now().to_rfc3339());

    if let Some(response_text) = params
        .response_text
        .as_ref()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
    {
        let mut replies = metadata
            .get("replies")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        replies.push(json!({
            "timestamp": Utc::now().to_rfc3339(),
            "text": response_text,
        }));
        metadata["replies"] = json!(replies);
        entry.text = format!(
            "{}\n\n[{}] {}",
            entry.text,
            Utc::now().to_rfc3339(),
            response_text
        );
    }

    let updated = server.with_global_store(|store| {
        store
            .update_with_revision(
                &entry.id,
                &entry.text,
                &entry.summary,
                &entry.source,
                &metadata,
                None,
                entry.revision,
            )
            .map_err(|e| format!("update card failed: {e}"))
    })?;

    if !updated {
        return Err(format!(
            "kanban card '{}' update rejected due to revision mismatch",
            entry.id
        ));
    }

    serde_json::to_string(&json!({
        "updated": true,
        "db": "global",
        "card_id": entry.id,
        "status": metadata["status"],
        "revision": entry.revision + 1,
    }))
    .map_err(|e| format!("serialize: {e}"))
}
