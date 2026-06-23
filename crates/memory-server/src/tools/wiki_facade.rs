use super::*;

pub(super) async fn handle_tachi_wiki_facade(
    server: &MemoryServer,
    params: TachiWikiParams,
) -> Result<String, String> {
    let action = params.action.to_ascii_lowercase();
    let format = params.format.clone();
    match action.as_str() {
        "search" => {
            let query = params
                .query
                .clone()
                .ok_or_else(|| "query is required when action='search'".to_string())?;
            let wiki_params = WikiSearchParams {
                query,
                path_prefix: None,
                category: params.category.clone(),
                top_k: crate::clamp_facade_top_k(params.top_k.unwrap_or(10)),
                include_archived: false,
                agent_role: None,
                project: params.project.clone(),
                domain: params.domain.clone(),
                file_context: None,
                error_context: None,
                weights: None,
            };
            if wants_json_format(format.as_deref()) {
                let value = collect_wiki_search_value(server, wiki_params).await?;
                serde_json::to_string(&value)
                    .map_err(|e| format!("serialize tachi_wiki search JSON: {e}"))
            } else {
                handle_tachi_wiki_search(server, wiki_params).await
            }
        }
        "browse" => {
            let browse_params = WikiBrowseParams {
                category: params.category.clone(),
                limit: params.limit.unwrap_or(50),
                project: params.project.clone().unwrap_or_else(|| "wiki".to_string()),
            };
            if wants_json_format(format.as_deref()) {
                let value = collect_wiki_browse_value(server, browse_params)?;
                serde_json::to_string(&value)
                    .map_err(|e| format!("serialize tachi_wiki browse JSON: {e}"))
            } else {
                handle_wiki_browse(server, browse_params)
            }
        }
        "read" => {
            let path = params
                .path
                .clone()
                .ok_or_else(|| "path is required when action='read'".to_string())?;
            let project = params.project.clone().unwrap_or_else(|| "wiki".to_string());
            if wants_json_format(format.as_deref()) {
                let value = collect_wiki_read_value(server, &path, &project)?;
                serde_json::to_string(&value)
                    .map_err(|e| format!("serialize tachi_wiki read JSON: {e}"))
            } else {
                handle_wiki_read(server, &path, &project)
            }
        }
        "write" => {
            let raw = if let Some(body) =
                crate::cli_client::maybe_forward_server_write(server, "tachi_wiki", &params).await?
            {
                body
            } else {
                let title = params
                    .title
                    .clone()
                    .ok_or_else(|| "title is required when action='write'".to_string())?;
                let text = params
                    .text
                    .clone()
                    .ok_or_else(|| "text is required when action='write'".to_string())?;
                let wiki_params = WikiWriteParams {
                    title,
                    text,
                    path: params.path.clone(),
                    topic: params.topic.clone(),
                    summary: params.summary.clone(),
                    category: params
                        .category
                        .clone()
                        .unwrap_or_else(|| "experience".to_string()),
                    keywords: params.keywords.clone(),
                    entities: params.entities.clone(),
                    importance: params.importance.unwrap_or(0.85),
                    scope: params.scope.clone().unwrap_or_else(|| "global".to_string()),
                    retention_policy: "permanent".to_string(),
                    domain: params.domain.clone(),
                    project: params.project.clone(),
                    metadata: params.metadata.clone(),
                    force: params.force,
                    references: params.references.clone(),
                };
                handle_tachi_wiki_write(server, wiki_params).await?
            };
            format_facade_response("Tachi wiki write", "write", &raw, format.as_deref())
        }
        _ => Err(format!(
            "Invalid action '{}'. Use 'search', 'browse', 'read', or 'write'.",
            params.action
        )),
    }
}
