use super::*;

pub(crate) async fn handle_ingest(
    server: &MemoryServer,
    params: IngestParams,
) -> Result<String, String> {
    match params.ingest_type.trim().to_ascii_lowercase().as_str() {
        "event" => {
            handle_ingest_event(
                server,
                IngestEventParams {
                    conversation_id: params.conversation_id.unwrap_or_default(),
                    turn_id: params.turn_id.unwrap_or_default(),
                    event_type: params.event_type,
                    content: params.content,
                    messages: params.messages,
                    path_prefix: params.path_prefix,
                    importance: Some(params.importance),
                    scope: params.scope,
                    project: params.project,
                    domain: params.domain,
                    metadata: params.metadata,
                },
            )
            .await
        }
        "source" => {
            let content = match params.content {
                Some(serde_json::Value::String(text)) => text,
                Some(other) => value_to_template_text(&other),
                None => String::new(),
            };
            handle_ingest_source(
                server,
                IngestSourceParams {
                    content,
                    source_url: params.source_url,
                    source: params.source,
                    path_prefix: params.path_prefix,
                    auto_chunk: params.auto_chunk,
                    auto_summarize: params.auto_summarize,
                    auto_link: params.auto_link,
                    importance: params.importance,
                    scope: params.scope,
                    project: params.project,
                    domain: params.domain,
                    chunk_size_chars: params.chunk_size_chars,
                    chunk_overlap_chars: params.chunk_overlap_chars,
                    metadata: params.metadata,
                },
            )
            .await
        }
        other => Err(format!(
            "Unsupported ingest_type '{}'. Expected 'event' or 'source'.",
            other
        )),
    }
}
