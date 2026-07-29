use super::*;

#[derive(Debug, Clone)]
pub(super) struct ValidatedWikiIngestHttpUrl {
    pub(super) url: reqwest::Url,
    pub(super) resolved_addrs: Option<Vec<SocketAddr>>,
}

/// `tachi_home` is the caller's server-bound home directory
/// (`MemoryServer::tachi_home_dir()`), already resolved through the
/// canonical `TACHI_HOME` → `SIGIL_HOME` → `TACHI_APP_HOME` → workspace →
/// `~/.tachi` precedence chain.
///
/// #1096 leaf-2a round-2 (codex C3-wiki): the first pass here replaced the
/// pre-#1096 allow-list — which read `TACHI_HOME` and `SIGIL_HOME`
/// independently and admitted BOTH roots when both were set — with just the
/// funnel's single resolved winner. That is a narrowing, not a widening: a
/// deployment with `TACHI_HOME=/A` and `SIGIL_HOME=/B` set simultaneously
/// used to allow local ingest from files under `/B` (the funnel picks `/A`
/// as `tachi_home`, but `/B` was still on the pre-#1096 allow-list); the
/// first-pass rewrite silently rejected `/B` files it used to accept. This
/// version restores the union: every one of `TACHI_HOME`/`SIGIL_HOME`/
/// `TACHI_APP_HOME` that is independently set (even the ones the funnel's
/// precedence didn't pick as `tachi_home`) is still an allow-list root,
/// alongside the resolved `tachi_home` and cwd. See
/// `ingest_local_file_allowed_tests::admits_union_of_all_three_home_env_roots`
/// below for the regression this closes.
fn wiki_ingest_local_file_allowed(source_path: &Path, tachi_home: &Path) -> bool {
    if std::env::var("TACHI_WIKI_INGEST_ALLOW_ANY_LOCAL_FILE")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
    {
        return true;
    }

    let canonical_source = match std::fs::canonicalize(source_path) {
        Ok(path) => path,
        Err(_) => return false,
    };
    let cwd = std::env::current_dir().ok();
    let mut roots = Vec::new();
    if let Some(cwd) = cwd {
        roots.push(cwd);
    }
    roots.push(tachi_home.to_path_buf());
    // Union, not narrowing: also admit whichever of the funnel's three home
    // keys are independently set as raw env, even the ones the funnel's
    // precedence didn't pick as the winning `tachi_home` above. This is what
    // restores the pre-#1096 TACHI_HOME+SIGIL_HOME union behavior (see the
    // function doc comment) while extending it to the funnel's third key.
    for env_key in ["TACHI_HOME", "SIGIL_HOME", "TACHI_APP_HOME"] {
        if let Ok(path) = std::env::var(env_key) {
            if !path.trim().is_empty() {
                roots.push(PathBuf::from(path));
            }
        }
    }
    // Preserved from the pre-#1096 behavior: the bare `~/.tachi` was always
    // an allow-list root regardless of TACHI_HOME/SIGIL_HOME/TACHI_APP_HOME
    // overrides, so keep it even when `tachi_home` resolved elsewhere —
    // narrowing this allow-list is out of scope for a pure plumbing change.
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".tachi"));
    }

    roots
        .into_iter()
        .filter_map(|root| std::fs::canonicalize(root).ok())
        .any(|root| canonical_source.starts_with(root))
}

async fn source_for_path(tachi_home: &Path, source: &str) -> Result<String, String> {
    if source.starts_with("http://") || source.starts_with("https://") {
        let validated = validate_wiki_ingest_http_url(source).await?;
        let client = wiki_ingest_http_client_for_url(&validated)?;
        let response = client
            .get(validated.url)
            .send()
            .await
            .map_err(|e| format!("fetch source URL: {e}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "fetch source URL failed with status {}",
                response.status()
            ));
        }
        read_limited_wiki_http_response(response).await
    } else {
        let path = Path::new(source);
        if !wiki_ingest_local_file_allowed(path, tachi_home) {
            return Err(
                "local wiki ingest is restricted to the current workspace or TACHI_HOME; set TACHI_WIKI_INGEST_ALLOW_ANY_LOCAL_FILE=1 to override"
                    .to_string(),
            );
        }
        tokio::fs::read_to_string(path)
            .await
            .map_err(|e| format!("read source file: {e}"))
    }
}

pub(super) fn wiki_ingest_http_client_for_url(
    validated: &ValidatedWikiIngestHttpUrl,
) -> Result<reqwest::Client, String> {
    let Some(resolved_addrs) = validated.resolved_addrs.as_deref() else {
        return wiki_ingest_http_client().cloned();
    };
    let host = validated
        .url
        .host_str()
        .ok_or_else(|| "wiki ingest source URL must include a host".to_string())?;
    crate::ensure_tls_provider();
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(StdDuration::from_secs(30))
        .resolve_to_addrs(host, resolved_addrs)
        // SECURITY: same bypass as research_ops's fetch client (tachi#530
        // T3 review) — tachi-server's reqwest carries the `system-proxy`
        // feature, so without `.no_proxy()` a configured/system proxy would
        // do the real DNS resolution and connect, letting it rebind the
        // validated host to a private/local address and bypass the
        // `validate_wiki_ingest_http_url` SSRF guard entirely.
        .no_proxy()
        .build()
        .map_err(|e| format!("build source URL client: {e}"))
}

pub(super) fn wiki_ingest_http_client() -> Result<&'static reqwest::Client, String> {
    WIKI_INGEST_HTTP_CLIENT
        .get_or_init(|| {
            crate::ensure_tls_provider();
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(StdDuration::from_secs(30))
                // SECURITY: see the sibling `.no_proxy()` call above — same
                // proxy-bypass hole, same fix, for the no-DNS-override path.
                .no_proxy()
                .build()
                .map_err(|e| format!("build source URL client: {e}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

pub(super) async fn validate_wiki_ingest_http_url(
    source: &str,
) -> Result<ValidatedWikiIngestHttpUrl, String> {
    let url = reqwest::Url::parse(source).map_err(|e| format!("parse source URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("wiki ingest only supports http:// and https:// source URLs".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("wiki ingest source URLs must not include credentials".to_string());
    }

    let host = url
        .host_str()
        .ok_or_else(|| "wiki ingest source URL must include a host".to_string())?;
    if host.eq_ignore_ascii_case("localhost") || host.to_ascii_lowercase().ends_with(".localhost") {
        return Err("wiki ingest source URL host is not allowed".to_string());
    }
    let ip_literal = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = ip_literal.parse::<IpAddr>() {
        reject_blocked_wiki_ingest_ip(ip)?;
        return Ok(ValidatedWikiIngestHttpUrl {
            url,
            resolved_addrs: None,
        });
    }

    let port = url
        .port_or_known_default()
        .ok_or_else(|| "wiki ingest source URL has no usable port".to_string())?;
    let mut resolved_addrs = Vec::new();
    let mut resolved_any = false;
    for addr in lookup_host((host, port))
        .await
        .map_err(|e| format!("resolve source URL host: {e}"))?
    {
        resolved_any = true;
        reject_blocked_wiki_ingest_ip(addr.ip())?;
        resolved_addrs.push(addr);
    }
    if !resolved_any {
        return Err("wiki ingest source URL host resolved to no addresses".to_string());
    }

    Ok(ValidatedWikiIngestHttpUrl {
        url,
        resolved_addrs: Some(resolved_addrs),
    })
}

fn reject_blocked_wiki_ingest_ip(ip: IpAddr) -> Result<(), String> {
    if wiki_ingest_ip_is_blocked(ip) {
        Err("wiki ingest source URL resolves to a private or local address".to_string())
    } else {
        Ok(())
    }
}

fn wiki_ingest_ip_is_blocked(ip: IpAddr) -> bool {
    is_private_or_local_ip(ip)
}

async fn read_limited_wiki_http_response(
    mut response: reqwest::Response,
) -> Result<String, String> {
    if response
        .content_length()
        .is_some_and(|len| len > WIKI_INGEST_HTTP_MAX_BYTES as u64)
    {
        return Err(format!(
            "source response exceeds {} byte limit",
            WIKI_INGEST_HTTP_MAX_BYTES
        ));
    }

    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("read source response: {e}"))?
    {
        if body.len().saturating_add(chunk.len()) > WIKI_INGEST_HTTP_MAX_BYTES {
            return Err(format!(
                "source response exceeds {} byte limit",
                WIKI_INGEST_HTTP_MAX_BYTES
            ));
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|e| format!("read source response as UTF-8: {e}"))
}

fn string_list_from_value(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::trim))
                .filter(|item| !item.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

fn derive_ingest_fallback(source: &str, topic_hint: Option<&str>, content: &str) -> Value {
    let title = topic_hint
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            content
                .lines()
                .find(|line| !line.trim().is_empty())
                .map(|line| {
                    line.trim()
                        .trim_start_matches('#')
                        .trim()
                        .chars()
                        .take(80)
                        .collect()
                })
        })
        .unwrap_or_else(|| {
            Path::new(source)
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("ingested-source")
                .to_string()
        });
    let keywords = topic_hint
        .map(|topic| {
            topic
                .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let entities = topic_hint
        .filter(|topic| !topic.trim().is_empty())
        .map(|topic| vec![topic.trim().to_string()])
        .unwrap_or_default();
    json!({
        "title": title,
        "topic": topic_hint.unwrap_or("ingest"),
        "summary": content.chars().take(100).collect::<String>(),
        "keywords": keywords,
        "entities": entities,
    })
}

async fn extract_ingest_metadata(
    server: &MemoryServer,
    source: &str,
    topic_hint: Option<&str>,
    content: &str,
) -> Result<(Value, Option<tachi_llm::PersistedModelInvocationReceiptV1>), String> {
    let system = "Extract wiki ingestion metadata. Return JSON only with keys: title, topic, summary, keywords, entities.";
    let user = format!(
        "Source: {source}\nTopic hint: {}\n\nContent:\n{}",
        topic_hint.unwrap_or(""),
        content.chars().take(8000).collect::<String>()
    );
    match server
        .llm
        .call_extract_llm_with_receipt(system, &user, None, 0.2, 800)
        .await
    {
        Ok(response)
            if response.invocation.completion_status()
                == tachi_llm::CompletionStatusV1::Truncated =>
        {
            Err(tachi_llm::LLM_OUTPUT_TRUNCATED.to_string())
        }
        Ok(response) => {
            let payload = tachi_llm::LlmClient::extract_json_payload(&response.value)
                .map_err(|error| format!("wiki ingest metadata parse failed: {error}"))?;
            let value = serde_json::from_str::<Value>(payload)
                .map_err(|error| format!("wiki ingest metadata parse failed: {error}"))?;
            if !value.is_object() {
                return Err("wiki ingest metadata parse failed: expected a JSON object".to_string());
            }
            Ok((value, Some(response.invocation)))
        }
        Err(_) => Ok((derive_ingest_fallback(source, topic_hint, content), None)),
    }
}

/// Persist a new ingest entry only after it has claimed any active predecessor.
/// A false supersession CAS means another writer already owns that predecessor,
/// so the new entry must not become a competing wiki candidate.
fn persist_wiki_ingest_entry(
    store: &mut MemoryStore,
    entry: &MemoryEntry,
    old_id: Option<&str>,
    reference_appends: &[memcore::db::ValidatedReferenceMutation],
) -> Result<(), String> {
    let metadata_patch = entry.metadata.as_object().cloned().unwrap_or_default();
    store
        .with_immutable_supersession_transaction(|replacement| {
            if let Some(old_id) = old_id {
                replacement.claim_immutable_supersession(old_id, &entry.id)?;
            }
            replacement.upsert_with_validated_reference_mutations(
                entry,
                &metadata_patch,
                reference_appends,
            )?;
            if let Some(old_id) = old_id {
                replacement.archive_claimed_source(old_id)?;
            }
            Ok(())
        })
        .map_err(|e| format!("wiki ingest refused: {e}"))
}

pub(crate) async fn handle_wiki_ingest(
    server: &MemoryServer,
    params: TachiWikiIngestParams,
) -> Result<String, String> {
    let content = source_for_path(&server.tachi_home_dir(), &params.source).await?;
    if content.trim().is_empty() {
        append_wiki_log(
            server,
            "ingest",
            &format!("{} | skipped empty source", params.source),
        );
        return serde_json::to_string(&json!({
            "status": "skipped",
            "reason": "empty_source",
            "source": params.source,
        }))
        .map_err(|e| format!("serialize wiki_ingest: {e}"));
    }

    let (metadata, model_invocation) =
        extract_ingest_metadata(server, &params.source, params.topic.as_deref(), &content).await?;
    let title = metadata
        .get("title")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("Ingested Source")
        .to_string();
    let topic = metadata
        .get("topic")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .or(params.topic.clone())
        .unwrap_or_else(|| "ingest".to_string());
    let summary = metadata
        .get("summary")
        .and_then(Value::as_str)
        .map(|value| value.chars().take(120).collect::<String>())
        .unwrap_or_else(|| content.chars().take(100).collect());
    let mut keywords = string_list_from_value(metadata.get("keywords"));
    if !keywords.iter().any(|keyword| keyword == "ingest") {
        keywords.push("ingest".to_string());
    }
    let entities = string_list_from_value(metadata.get("entities"));
    let path = format!("/wiki/general/{}", sanitize_safe_path_name(&topic));
    let id = uuid::Uuid::new_v4().to_string();
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let evidence_refs_v1 = build_evidence_refs_v1(std::slice::from_ref(&params.source), &timestamp);
    let reference_appends = evidence_refs_v1
        .into_iter()
        .map(|reference| {
            let target_kind = reference
                .target_kind
                .map(serde_json::to_value)
                .transpose()
                .map_err(|error| format!("serialize wiki ingest target kind: {error}"))?
                .and_then(|value| value.as_str().map(str::to_string));
            memcore::db::ValidatedReferenceMutation::evidence(
                reference.target_ref,
                reference.captured_at,
                target_kind,
            )
            .map_err(|error| format!("validate wiki ingest reference: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let metadata = crate::provenance::inject_provenance(
        server,
        json!({
            "wiki": true,
            "wiki_title": title,
            "ingest_source": params.source.clone(),
            "allow_cross_project": true,
            // #1072 fix-round (#1215 BUG 6): `wiki_ingest` used to upsert
            // straight into `/wiki/general/...` with no lifecycle/authority
            // marker at all, so `derive_wiki_lifecycle`'s no-marker default
            // (`Active`, kept for pre-#1072 back-compat on entries that
            // predate the lifecycle vocabulary) silently promoted arbitrary
            // fetched URL/file content to reviewed truth — a bypass named
            // explicitly in the cross-vendor review ("ingest writers").
            // Ingested content is unreviewed by construction (no approval
            // step exists here); stamp it `pending_review` honestly, same
            // vocabulary `wiki_layer_metadata` stamps for the MCP write
            // path, with the ingest source recorded as its typed evidence ref.
            "lifecycle": WikiLifecycleV1::PendingReview.as_str(),
            "authority": WikiAuthorityV1::Advisory.as_str(),
            "artifact_kind": WikiArtifactKindV1::Wiki.as_str(),
        }),
        "wiki_ingest",
        "wiki_ingest",
        Some("global"),
        crate::server_state::DbScope::Project,
        json!({"source": params.source.clone()}),
    );
    let metadata = match model_invocation.as_ref() {
        Some(invocation) => crate::provenance::attach_model_invocation(metadata, invocation)
            .map_err(|error| format!("attach wiki ingest receipt: {error}"))?,
        None => metadata,
    };

    let entry = MemoryEntry {
        id: id.clone(),
        path: path.clone(),
        summary: summary.clone(),
        text: content.clone(),
        importance: 0.8,
        timestamp: timestamp.clone(),
        valid_from: String::new(),
        valid_until: None,
        category: "experience".to_string(),
        topic: topic.clone(),
        keywords,
        persons: Vec::new(),
        entities: entities.clone(),
        location: String::new(),
        source: "wiki".to_string(),
        scope: "general".to_string(),
        archived: false,
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        metadata,
        vector: None,
        retention_policy: Some("permanent".to_string()),
        domain: Some("wiki".to_string()),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    };

    server.with_named_project_store("wiki", |store| {
        let old_id = {
            let mut stmt = store
                .connection()
                .prepare(
                    "SELECT id FROM memories
                     WHERE (path = ?1 OR (domain = 'wiki' AND topic = ?2))
                       AND archived = 0
                       AND superseded_by IS NULL
                     LIMIT 1",
                )
                .map_err(|e| format!("prepare wiki duplicate query failed: {e}"))?;
            let mut rows = stmt
                .query_map((&path, &topic), |row| row.get::<_, String>(0))
                .map_err(|e| format!("query wiki duplicate failed: {e}"))?;
            if let Some(row) = rows.next() {
                Some(row.map_err(|e| format!("read wiki duplicate row failed: {e}"))?)
            } else {
                None
            }
        };

        persist_wiki_ingest_entry(store, &entry, old_id.as_deref(), &reference_appends)
    })?;

    let mut related = Vec::new();
    if params.update_related {
        related = find_related_by_entities(server, "wiki", &entities, &id, 10);
        for related_entry in &related {
            let Some(target_id) = related_entry.get("id").and_then(Value::as_str) else {
                continue;
            };
            let edge = memcore::MemoryEdge {
                source_id: id.clone(),
                target_id: target_id.to_string(),
                relation: "references".to_string(),
                weight: 0.6,
                metadata: json!({
                    "wiki_ingest": true,
                    "shared_entities": entities.clone(),
                }),
                created_at: Utc::now().to_rfc3339(),
                valid_from: String::new(),
                valid_to: None,
            };
            if let Err(e) = server.with_named_project_store("wiki", |store| {
                store
                    .add_edge(&edge)
                    .map_err(|e| format!("wiki ingest edge: {e}"))
            }) {
                tracing::warn!("wiki ingest edge write failed: {e}");
                return Err(e);
            }
        }
    }

    // #1413 concern 1: bust the shared (global) recall cache AFTER every
    // content-changing write in this ingest has committed — the entry upsert
    // (+ optional supersede/archive of the prior wiki entry) above AND the
    // related-edge writes in the `update_related` loop just above, since those
    // edges can surface via graph-expanded searches. This runs only on the
    // fully-successful path: an edge write failure `return Err(e)`-bails before
    // reaching here, so failure propagation is retained. The invalidator
    // re-takes the global write gate via `with_global_store`, so it stays OUT
    // of every `with_named_project_store` closure above — never inside one.
    let _ = crate::memory_search_ops::invalidate_recall_cache_after_write(server, "wiki_ingest");

    server.enqueue_enrichment(crate::enrichment::build_enrichment_item(
        &entry,
        true,
        false,
        DbScope::Project,
        Some("wiki".to_string()),
        None,
        None,
        None,
        1,
    ));

    append_wiki_log(
        server,
        "ingest",
        &format!("{} | created {} at {}", params.source, id, path),
    );

    serde_json::to_string(&json!({
        "status": "created",
        "id": id,
        "path": path,
        "title": title,
        "summary": summary,
        "related_entries": related,
    }))
    .map_err(|e| format!("serialize wiki_ingest: {e}"))
}

#[cfg(test)]
mod ingest_local_file_allowed_tests {
    use super::wiki_ingest_local_file_allowed;
    use crate::test_support::EnvRestore;

    /// #1096 leaf-2a round-2 (codex C3-wiki): RED against the first-pass
    /// implementation, which passed only the funnel-resolved `tachi_home`
    /// (the single precedence winner) as an allow-list root. With
    /// `TACHI_HOME=/A` and `SIGIL_HOME=/B` both set, the funnel resolves
    /// `tachi_home` to `/A`; the first-pass code then rejected a file under
    /// `/B` that the pre-#1096 implementation used to accept. This asserts
    /// the union: a file under the LOSING key's root is still allowed.
    #[test]
    fn admits_union_of_all_three_home_env_roots() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tachi_home_dir = tempfile::tempdir().expect("tachi_home tempdir");
        let sigil_home_dir = tempfile::tempdir().expect("sigil_home tempdir");
        let app_home_dir = tempfile::tempdir().expect("app_home tempdir");

        let tachi_file = tachi_home_dir.path().join("under-tachi-home.md");
        let sigil_file = sigil_home_dir.path().join("under-sigil-home.md");
        let app_file = app_home_dir.path().join("under-app-home.md");
        std::fs::write(&tachi_file, "tachi").expect("write tachi fixture");
        std::fs::write(&sigil_file, "sigil").expect("write sigil fixture");
        std::fs::write(&app_file, "app").expect("write app fixture");

        let _tachi_env = EnvRestore::set_path("TACHI_HOME", tachi_home_dir.path());
        let _sigil_env = EnvRestore::set_path("SIGIL_HOME", sigil_home_dir.path());
        let _app_env = EnvRestore::set_path("TACHI_APP_HOME", app_home_dir.path());
        // `TACHI_WIKI_INGEST_ALLOW_ANY_LOCAL_FILE` would short-circuit the
        // allow-list entirely and defeat this test's whole point.
        let _allow_any_off = EnvRestore::remove("TACHI_WIKI_INGEST_ALLOW_ANY_LOCAL_FILE");

        // The funnel picks TACHI_HOME as the resolved winner passed in here,
        // matching what `MemoryServer::tachi_home_dir()` would resolve to.
        let resolved_tachi_home = tachi_home_dir.path();

        assert!(
            wiki_ingest_local_file_allowed(&tachi_file, resolved_tachi_home),
            "file under the resolved (winning) TACHI_HOME must be allowed"
        );
        assert!(
            wiki_ingest_local_file_allowed(&sigil_file, resolved_tachi_home),
            "file under the losing key SIGIL_HOME must still be allowed (union, not narrowing)"
        );
        assert!(
            wiki_ingest_local_file_allowed(&app_file, resolved_tachi_home),
            "file under the losing key TACHI_APP_HOME must still be allowed (union, not narrowing)"
        );
    }
}

#[cfg(test)]
mod immutable_supersession_tests {
    use super::*;

    fn wiki_entry(id: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: format!("/wiki/general/{id}"),
            summary: format!("wiki summary {id}"),
            text: format!("wiki body {id}"),
            importance: 0.7,
            timestamp: chrono::Utc::now().to_rfc3339(),
            valid_from: String::new(),
            valid_until: None,
            category: "experience".to_string(),
            topic: id.to_string(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "wiki".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            metadata: serde_json::json!({"wiki": true}),
            vector: None,
            retention_policy: Some("permanent".to_string()),
            domain: Some("wiki".to_string()),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn conflicted_predecessor_refuses_before_saving_a_new_wiki_candidate() {
        let temp = tempfile::tempdir().expect("wiki immutable-edge tempdir");
        let db_path = temp.path().join("wiki.db");
        let mut store = MemoryStore::open(db_path.to_str().expect("utf8 db path"))
            .expect("open wiki test store");
        let old = wiki_entry("old-wiki-entry");
        let canonical = wiki_entry("canonical-wiki-entry");
        let candidate = wiki_entry("stale-wiki-candidate");
        store.upsert(&old).expect("seed old wiki entry");
        store.upsert(&canonical).expect("seed canonical wiki entry");
        assert!(store
            .supersede_memory(&old.id, &canonical.id)
            .expect("seed immutable predecessor edge"));

        let err = persist_wiki_ingest_entry(&mut store, &candidate, Some(&old.id), &[])
            .expect_err("conflicted predecessor must refuse wiki candidate");
        assert!(err.contains("immutable supersession CAS"), "err: {err}");
        let old_after = store
            .get_with_options(&old.id, true)
            .expect("read old wiki entry")
            .expect("old wiki entry remains");
        assert!(
            !old_after.archived,
            "failed CAS must not archive the established predecessor"
        );
        assert_eq!(
            store
                .supersession_target(&old.id)
                .expect("read predecessor edge"),
            Some(Some(canonical.id)),
            "failed CAS must preserve the original predecessor edge"
        );
        assert!(
            store
                .get_with_options(&candidate.id, true)
                .expect("read candidate")
                .is_none(),
            "failed CAS must not leave a new competing wiki candidate"
        );
    }
}
