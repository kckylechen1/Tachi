use super::*;

#[tool_router(router = wiki_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Run health checks over wiki memories and skill graph state. Returns orphan nodes, contradiction candidates, stale nodes, missing edge hints, and current skill quality guard status."
    )]
    pub(crate) async fn wiki_lint(
        &self,
        Parameters(params): Parameters<WikiLintParams>,
    ) -> Result<String, String> {
        handle_wiki_lint(self, params).await
    }

    #[tool(
        description = "Write a durable wiki entry under /wiki with sane defaults for path, retention, metadata, and auto-linking."
    )]
    pub(crate) async fn tachi_wiki_write(
        &self,
        Parameters(params): Parameters<WikiWriteParams>,
    ) -> Result<String, String> {
        if let Some(body) =
            crate::cli_client::maybe_forward_server_write(self, "tachi_wiki_write", &params).await?
        {
            return Ok(body);
        }
        handle_tachi_wiki_write(self, params).await
    }

    #[tool(
        description = "Search wiki entries under /wiki. Use this before debugging from scratch or when a prior lesson may exist."
    )]
    pub(crate) async fn tachi_wiki_search(
        &self,
        Parameters(params): Parameters<WikiSearchParams>,
    ) -> Result<String, String> {
        handle_tachi_wiki_search(self, params).await
    }

    #[tool(
        description = "Ingest a URL or local file into the wiki project DB, extract metadata when possible, and link related wiki entries by shared entities."
    )]
    pub(crate) async fn tachi_wiki_ingest(
        &self,
        Parameters(params): Parameters<TachiWikiIngestParams>,
    ) -> Result<String, String> {
        handle_wiki_ingest(self, params).await
    }

    #[tool(
        description = "Organize workspace docs: automatically classify/move files, sync task checkmarks, and rebuild docs/_index.md tree. Pass dry_run=true to preview planned moves/frontmatter/task-sync changes without modifying any files."
    )]
    pub(crate) async fn tachi_wiki_organize(
        &self,
        Parameters(params): Parameters<TachiWikiOrganizeParams>,
    ) -> Result<String, String> {
        crate::docs_ops::handle_wiki_organize(self, &params.dir_path, params.dry_run).await
    }

    #[tool(
        description = "Search the wiki knowledge base for relevant entries. The wiki contains distilled knowledge from past development sessions organized by category (quant, engineering, agent, product). Supports short category aliases like 'quant', 'strategy', 'tachi', 'debugging', etc."
    )]
    pub(crate) async fn wiki_search(
        &self,
        Parameters(params): Parameters<WikiSearchParams>,
    ) -> Result<String, String> {
        handle_wiki_search(self, params).await
    }

    #[tool(
        description = "Browse wiki entries by category. Without a category, returns category stats. Supports short aliases like 'quant', 'engineering', 'tachi', etc."
    )]
    pub(crate) async fn tachi_browse(
        &self,
        Parameters(params): Parameters<WikiBrowseParams>,
    ) -> Result<String, String> {
        handle_wiki_browse(self, params)
    }

    #[tool(
        description = "Stable, reusable knowledge base. action='search': look up lessons, patterns, and how-tos BEFORE debugging from scratch or reaching for web search — a prior lesson may already exist. action='browse': explore available categories. action='read': load a specific entry by path. action='write': persist a reusable lesson, architecture decision, pattern, or how-to. WHEN: wiki for durable knowledge that helps future sessions (patterns, lessons, decisions, conventions). Use tachi_memory for session-specific facts (decisions, findings, commands for the current task). Pass project to target a named library."
    )]
    pub(crate) async fn tachi_wiki(
        &self,
        Parameters(params): Parameters<TachiWikiParams>,
    ) -> Result<String, String> {
        handle_tachi_wiki_facade(self, params).await
    }
}

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
                    include_patterns: params.include_patterns,
                    pattern_query: params.pattern_query.clone(),
                    pattern_top_k: params.pattern_top_k,
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
