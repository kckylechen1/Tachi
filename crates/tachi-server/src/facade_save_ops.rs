//! Business logic for the `tachi_save` facade tool.
//!
//! Extracted from `tools.rs` (Stage 4 of large-rust-files refactor) to keep
//! the `#[tool]` wrapper thin. The wrapper in `impl MemoryServer` simply
//! delegates to [`handle_tachi_save`].

use crate::copilot_ops::handle_tachi_wiki_write;
use crate::facade_memory_ops::shape_save_facade_response;
use crate::memory_search_ops::{handle_remember, handle_save_memory_with_references};
use crate::pipeline_ops::handle_extract_facts;
use crate::tool_params::*;
use crate::MemoryServer;
#[cfg(test)]
use chrono::Utc;

pub(crate) async fn handle_tachi_save(
    server: &MemoryServer,
    params: TachiSaveParams,
) -> Result<String, String> {
    let kind = params.kind.as_deref().unwrap_or("").to_ascii_lowercase();

    if kind == "facts" || kind == "extract_facts" {
        let extract_params = ExtractFactsParams {
            text: params.text.clone(),
            source: params
                .source
                .clone()
                .unwrap_or_else(|| "tachi_save".to_string()),
            project: params.project.clone(),
        };
        return handle_extract_facts(server, extract_params).await;
    }

    // Detect scope=note: even if kind is empty, treat as note when scope="note"
    let scope_is_note = params
        .scope
        .as_deref()
        .map(|s| s.eq_ignore_ascii_case("note"))
        .unwrap_or(false);

    // Auto-detect: explicit DB-style path (starts with '/') signals memory,
    // wiki when caller provides a title, otherwise short text is treated as
    // a quick note. Path shape wins over text length so callers passing
    // memory paths like "/scratch/foo" don't get bounced to the note writer.
    let path_looks_like_db_path = params
        .path
        .as_deref()
        .map(|p| p.starts_with('/'))
        .unwrap_or(false);
    let resolved_kind = if kind.is_empty() {
        if scope_is_note {
            "note"
        } else if params.title.is_some() {
            "wiki"
        } else if path_looks_like_db_path {
            "memory"
        } else if params.text.chars().count() < 200 {
            "note"
        } else {
            "memory"
        }
    } else {
        &kind
    };

    match resolved_kind {
        "wiki" => {
            let title = params
                .title
                .clone()
                .unwrap_or_else(|| "Untitled".to_string());
            let wiki_params = WikiWriteParams {
                title,
                text: params.text.clone(),
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
                retention_policy: params
                    .retention_policy
                    .clone()
                    .unwrap_or_else(|| "permanent".to_string()),
                domain: params.domain.clone(),
                project: params.project.clone(),
                metadata: params.metadata.clone(),
                force: params.force,
                references: params.references.clone(),
                include_patterns: false,
                pattern_query: None,
                pattern_top_k: None,
            };
            handle_tachi_wiki_write(server, wiki_params).await
        }
        "note" => {
            let (abs_note_path, rel_note_path) = crate::notes_ops::write_note_file(
                &server.tachi_home_dir(),
                &params.text,
                params.path.as_deref(),
                params.title.as_deref(),
                params.topic.as_deref(),
                params.category.as_deref(),
                &params.keywords,
            )?;

            let db_path = format!("/notes/{}", rel_note_path);
            let db_scope = params
                .scope
                .as_deref()
                .filter(|s| !s.eq_ignore_ascii_case("note"))
                .unwrap_or("project")
                .to_string();

            let remember_params = RememberParams {
                text: params.text.clone(),
                summary: params.summary.clone().unwrap_or_default(),
                tags: params.keywords.clone(),
                topic: params.topic.clone().unwrap_or_default(),
                importance: params.importance,
                scope: Some(db_scope),
                project: params.project.clone(),
                project_explicit: params.project_explicit,
                path: Some(db_path),
                category: Some(
                    params
                        .category
                        .clone()
                        .unwrap_or_else(|| "note".to_string()),
                ),
                domain: params.domain.clone(),
                retention_policy: params
                    .retention_policy
                    .clone()
                    .or_else(|| Some("durable".to_string())),
                valid_from: params.valid_from.clone(),
                valid_until: params.valid_until.clone(),
                force: params.force,
            };
            let mut result_str = handle_remember(server, remember_params).await?;

            if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&result_str) {
                if let Some(obj) = val.as_object_mut() {
                    obj.insert(
                        "note_file".to_string(),
                        serde_json::json!(abs_note_path.to_string_lossy()),
                    );
                    obj.insert("note_path".to_string(), serde_json::json!(rel_note_path));
                }
                result_str = serde_json::to_string(&val)
                    .map_err(|e| format!("serialize note result: {e}"))?;
            }

            Ok(result_str)
        }
        _ => {
            // "memory" or any other value
            //
            // tachi#1288 (Fix B): `references[]` used to be consumed only by
            // the "wiki" arm above (-> `metadata.source_refs`); a plain
            // memory save silently dropped it on the floor. Validate first
            // (an invalid reference is a caller error to surface loudly, not
            // to swallow -- `wiki_ops::validate_references` is the same gate
            // `tachi_wiki_write` runs). Caller-controlled reserved reference
            // metadata is removed, then validated typed refs travel as a
            // separate internal argument to the atomic save seam.
            let metadata =
                merge_referenced_files(params.metadata.clone(), &params.files, &params.text);
            let mem_params = SaveMemoryParams {
                text: params.text.clone(),
                summary: params.summary.clone().unwrap_or_default(),
                path: params.path.clone().unwrap_or_else(|| "/".to_string()),
                importance: params.importance.unwrap_or(0.7),
                category: params
                    .category
                    .clone()
                    .unwrap_or_else(|| "fact".to_string()),
                topic: params.topic.clone().unwrap_or_default(),
                keywords: params.keywords.clone(),
                persons: Vec::new(), // legacy wire field; MCP uses entities for people
                entities: params.entities.clone(),
                location: String::new(),
                scope: params
                    .scope
                    .clone()
                    .unwrap_or_else(|| "project".to_string()),
                vector: None,
                id: params.id.clone(),
                force: params.force,
                auto_link: true,
                project: params.project.clone(),
                project_explicit: params.project_explicit,
                retention_policy: params.retention_policy.clone(),
                domain: params.domain.clone(),
                timestamp: None,
                valid_from: params.valid_from.clone(),
                valid_until: params.valid_until.clone(),
                metadata,
                emit_continuity: params.emit_continuity,
            };
            handle_save_memory_with_references(server, mem_params, params.references.clone()).await
        }
    }
}

pub(crate) fn finalize_tachi_save_response(
    params: &TachiSaveParams,
    raw: &str,
    echo: Option<&str>,
) -> Result<String, String> {
    shape_save_facade_response(
        raw,
        params.format.as_deref(),
        echo.or(Some(params.text.as_str())),
        params.path.as_deref(),
    )
}

/// Merge explicit `files` plus any `spec:`-pointer paths parsed from `text`
/// into `metadata.files` (a de-duplicated string array). Returns the metadata
/// unchanged when there are no referenced files to record, so old behaviour and
/// payloads stay byte-identical.
fn merge_referenced_files(
    metadata: Option<serde_json::Value>,
    explicit_files: &[String],
    text: &str,
) -> Option<serde_json::Value> {
    // Ordered de-dup: preserve insertion order, drop blanks/duplicates.
    let mut files: Vec<String> = Vec::new();
    let mut push = |raw: &str| {
        let f = raw.trim();
        if !f.is_empty() && !files.iter().any(|existing| existing == f) {
            files.push(f.to_string());
        }
    };

    // 1. Carry forward any files already present on the incoming metadata.
    if let Some(serde_json::Value::Array(existing)) = metadata
        .as_ref()
        .and_then(|m| m.get("files").cloned())
        .as_ref()
    {
        for v in existing {
            if let Some(s) = v.as_str() {
                push(s);
            }
        }
    }
    // 2. Explicit caller-supplied files.
    for f in explicit_files {
        push(f);
    }
    // 3. Conservative auto-parse: only `spec:` pointer lines from the text.
    for path in parse_spec_pointers(text) {
        push(&path);
    }

    if files.is_empty() {
        return metadata;
    }

    let mut obj = match metadata {
        Some(serde_json::Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    obj.insert("files".to_string(), serde_json::json!(files));
    Some(serde_json::Value::Object(obj))
}

/// tachi#1288 (Fix B): write validated `references[]` into
/// `metadata.evidence_refs_v1` (typed, canon doc §7.1 `WikiEvidenceRefV1`
/// shape -- same builder `tachi_wiki_write` uses via `wiki_layer_metadata`)
/// for the plain "memory" save path, which previously had no consumer for
/// this field at all. Returns the metadata unchanged when there are no
/// references, so old behaviour/payloads stay byte-identical. Callers must
/// validate `references` first (`wiki_ops::validate_references`) -- this
/// function assumes they are already well-formed.
#[cfg(test)]
fn merge_evidence_references(
    metadata: Option<serde_json::Value>,
    references: &[String],
) -> Option<serde_json::Value> {
    if references.is_empty() {
        return metadata;
    }
    let captured_at = Utc::now().to_rfc3339();
    let mut obj = match metadata {
        Some(serde_json::Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    let mut evidence_refs_v1 = obj
        .get("evidence_refs_v1")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    for new_ref in build_evidence_refs_v1(references, &captured_at) {
        let already_present = evidence_refs_v1.iter().any(|existing| {
            existing.get("ref").and_then(serde_json::Value::as_str)
                == Some(new_ref.target_ref.as_str())
        });
        if !already_present {
            evidence_refs_v1.push(serde_json::json!(new_ref));
        }
    }
    obj.insert(
        "evidence_refs_v1".to_string(),
        serde_json::json!(evidence_refs_v1),
    );
    Some(serde_json::Value::Object(obj))
}

/// Extract referenced file paths from `spec:` pointer lines, e.g.
/// `spec: docs/SPEC.md` or `spec:docs/SPEC.md, src/lib.rs`. Conservative by
/// design — only lines whose first non-space token is `spec:` are considered,
/// so prose mentioning the word "spec" elsewhere is never harvested.
fn parse_spec_pointers(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed
            .strip_prefix("spec:")
            .or_else(|| trimmed.strip_prefix("Spec:"))
            .or_else(|| trimmed.strip_prefix("SPEC:"))
        else {
            continue;
        };
        for token in rest.split(',') {
            let path = token.trim();
            if !path.is_empty() {
                out.push(path.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod referenced_files_tests {
    use super::{merge_referenced_files, parse_spec_pointers};
    use serde_json::json;

    #[test]
    fn parses_spec_pointer_lines_only() {
        let text =
            "Decision recorded.\nspec: docs/SPEC.md, src/lib.rs\nWe also spec-checked things.";
        assert_eq!(
            parse_spec_pointers(text),
            vec!["docs/SPEC.md".to_string(), "src/lib.rs".to_string()]
        );
    }

    #[test]
    fn no_pointers_leaves_metadata_untouched() {
        // No explicit files, no spec: line → metadata returned as-is (None).
        assert!(merge_referenced_files(None, &[], "just prose, no pointers").is_none());
    }

    #[test]
    fn merges_explicit_existing_and_parsed_deduped() {
        let metadata = Some(json!({ "tier": "raw", "files": ["docs/A.md"] }));
        let explicit = vec!["src/b.rs".to_string(), "docs/A.md".to_string()];
        let merged = merge_referenced_files(metadata, &explicit, "spec: docs/C.md, src/b.rs")
            .expect("metadata present");
        assert_eq!(merged["tier"], json!("raw"));
        assert_eq!(
            merged["files"],
            json!(["docs/A.md", "src/b.rs", "docs/C.md"])
        );
    }
}

#[cfg(test)]
mod evidence_references_tests {
    use super::{handle_tachi_save, merge_evidence_references};
    use serde_json::json;

    /// tachi#1288 Fix B: no references → metadata passes through unchanged,
    /// so old callers with no references field stay byte-identical.
    #[test]
    fn no_references_leaves_metadata_untouched() {
        let metadata = Some(json!({ "tier": "raw" }));
        assert_eq!(
            merge_evidence_references(metadata.clone(), &[]),
            metadata,
            "empty references must not touch metadata at all"
        );
        assert!(merge_evidence_references(None, &[]).is_none());
    }

    /// tachi#1288 Fix B: non-empty references land as the typed
    /// `evidence_refs_v1` shape (canon doc §7.1 `WikiEvidenceRefV1`), the
    /// same builder `tachi_wiki_write` uses -- not the legacy `source_refs`
    /// string array.
    #[test]
    fn references_land_as_typed_evidence_refs_v1() {
        let references = vec!["https://example.com/doc".to_string(), "#1288".to_string()];
        let merged = merge_evidence_references(Some(json!({ "tier": "raw" })), &references)
            .expect("metadata present");
        assert_eq!(merged["tier"], json!("raw"), "existing metadata preserved");
        let refs = merged["evidence_refs_v1"]
            .as_array()
            .expect("evidence_refs_v1 present as array");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0]["ref"], json!("https://example.com/doc"));
        assert_eq!(refs[1]["ref"], json!("#1288"));
        assert!(
            merged.get("source_refs").is_none(),
            "must not dual-write the legacy source_refs array: {merged}"
        );
    }

    /// References merge into a fresh object when no metadata was supplied,
    /// mirroring `merge_referenced_files`'s equivalent behavior for `files`.
    #[test]
    fn references_create_metadata_object_when_none_supplied() {
        let merged = merge_evidence_references(None, &["#42".to_string()])
            .expect("metadata created from references alone");
        assert_eq!(merged["evidence_refs_v1"].as_array().map(Vec::len), Some(1));
    }

    #[tokio::test]
    async fn updating_memory_preserves_and_dedupes_typed_evidence_references() {
        let (server, _temp_home) = crate::tests::make_server_with_temp_home();
        let first = handle_tachi_save(
            &server,
            serde_json::from_value(json!({
                "kind": "memory",
                "text": "Initial memory records the first evidence reference.",
                "path": "/audit/evidence-refs",
                "force": true,
                "references": ["#100"],
            }))
            .expect("first save params"),
        )
        .await
        .expect("first save");
        let id = serde_json::from_str::<serde_json::Value>(&first).expect("first save JSON")["id"]
            .as_str()
            .expect("first save id")
            .to_string();
        let original_a = server
            .with_global_store_read(|store| store.get(&id).map_err(|error| error.to_string()))
            .expect("load initial memory")
            .expect("initial memory exists")
            .metadata["evidence_refs_v1"][0]
            .clone();

        for metadata in [
            json!({ "caller_context": "update" }),
            json!({ "caller_context": "duplicate" }),
        ] {
            handle_tachi_save(
                &server,
                serde_json::from_value(json!({
                    "id": id,
                    "kind": "memory",
                    "text": "Initial memory records the first evidence reference.",
                    "force": true,
                    "metadata": metadata,
                    "references": ["#101"],
                }))
                .expect("update save params"),
            )
            .await
            .expect("update save");
        }

        let entry = server
            .with_global_store_read(|store| store.get(&id).map_err(|error| error.to_string()))
            .expect("load updated memory")
            .expect("updated memory exists");
        let refs = entry.metadata["evidence_refs_v1"]
            .as_array()
            .expect("typed evidence refs present");
        assert_eq!(refs.len(), 2, "existing A plus duplicate B must dedupe");
        assert_eq!(refs[0], original_a, "existing typed A must be preserved");
        assert_eq!(refs[0]["ref"], json!("#100"));
        assert_eq!(refs[1]["ref"], json!("#101"));
        assert_eq!(entry.metadata["caller_context"], json!("duplicate"));
        assert!(
            entry.metadata.get("source_refs").is_none(),
            "memory updates must not re-enable legacy source_refs: {}",
            entry.metadata
        );
    }

    #[tokio::test]
    async fn hostile_reference_metadata_cannot_populate_reserved_reference_fields() {
        let (server, _temp_home) = crate::tests::make_server_with_temp_home();
        let first = handle_tachi_save(
            &server,
            serde_json::from_value(json!({
                "kind": "memory",
                "text": "Initial memory carries one validated evidence reference.",
                "path": "/audit/hostile-evidence-metadata",
                "scope": "global",
                "force": true,
                "references": ["#100"],
            }))
            .expect("first save params"),
        )
        .await
        .expect("first save");
        let id = serde_json::from_str::<serde_json::Value>(&first).expect("first save JSON")["id"]
            .as_str()
            .expect("first save id")
            .to_string();

        handle_tachi_save(
            &server,
            serde_json::from_value(json!({
                "id": id,
                "kind": "memory",
                "text": "Initial memory carries one validated evidence reference.",
                "scope": "global",
                "force": true,
                "references": ["#101"],
                "metadata": {
                    "caller_context": "kept",
                    "evidence_refs_v1": [{
                        "ref": "#999",
                        "captured_at": "hostile"
                    }],
                    "source_refs": ["#998"]
                }
            }))
            .expect("hostile update params"),
        )
        .await
        .expect("hostile update");

        let entry = server
            .with_global_store_read(|store| store.get(&id).map_err(|error| error.to_string()))
            .expect("load updated memory")
            .expect("updated memory exists");
        let refs = entry.metadata["evidence_refs_v1"]
            .as_array()
            .expect("typed evidence refs present");
        assert_eq!(
            refs.iter()
                .map(|value| value["ref"].as_str().expect("typed ref"))
                .collect::<Vec<_>>(),
            vec!["#100", "#101"]
        );
        assert_eq!(entry.metadata["caller_context"], json!("kept"));
        assert!(entry.metadata.get("source_refs").is_none());
    }

    #[tokio::test]
    async fn empty_references_strip_malformed_reserved_metadata_without_erasing_persisted_refs() {
        let malformed_shapes = [
            json!(null),
            json!("not-an-array"),
            json!({ "ref": "#999" }),
            json!([null, "#999", { "captured_at": 17 }]),
        ];

        for (index, hostile_evidence) in malformed_shapes.into_iter().enumerate() {
            let (server, _temp_home) = crate::tests::make_server_with_temp_home();
            let first = handle_tachi_save(
                &server,
                serde_json::from_value(json!({
                    "kind": "memory",
                    "text": format!("Malformed metadata fixture {index} keeps validated evidence."),
                    "path": format!("/audit/malformed-evidence-{index}"),
                    "scope": "global",
                    "force": true,
                    "references": ["#100"],
                }))
                .expect("first save params"),
            )
            .await
            .expect("first save");
            let id = serde_json::from_str::<serde_json::Value>(&first).expect("first save JSON")
                ["id"]
                .as_str()
                .expect("first save id")
                .to_string();

            handle_tachi_save(
                &server,
                serde_json::from_value(json!({
                    "id": id,
                    "kind": "memory",
                    "text": format!("Malformed metadata fixture {index} keeps validated evidence."),
                    "scope": "global",
                    "force": true,
                    "references": [],
                    "metadata": {
                        "caller_context": index,
                        "evidence_refs_v1": hostile_evidence,
                        "source_refs": { "malformed": true }
                    }
                }))
                .expect("hostile update params"),
            )
            .await
            .expect("hostile update");

            let entry = server
                .with_global_store_read(|store| store.get(&id).map_err(|error| error.to_string()))
                .expect("load updated memory")
                .expect("updated memory exists");
            let refs = entry.metadata["evidence_refs_v1"]
                .as_array()
                .expect("persisted typed evidence refs");
            assert_eq!(refs.len(), 1);
            assert_eq!(refs[0]["ref"], json!("#100"));
            assert_eq!(entry.metadata["caller_context"], json!(index));
            assert!(entry.metadata.get("source_refs").is_none());
        }
    }

    #[tokio::test]
    async fn empty_references_do_not_create_caller_supplied_reference_metadata() {
        let (server, _temp_home) = crate::tests::make_server_with_temp_home();
        let saved = handle_tachi_save(
            &server,
            serde_json::from_value(json!({
                "kind": "memory",
                "text": "A hostile create cannot smuggle reserved reference metadata.",
                "path": "/audit/hostile-evidence-create",
                "scope": "global",
                "force": true,
                "references": [],
                "metadata": {
                    "caller_context": "kept",
                    "evidence_refs_v1": [{ "ref": "#999" }],
                    "source_refs": ["#998"]
                }
            }))
            .expect("hostile create params"),
        )
        .await
        .expect("hostile create");
        let id = serde_json::from_str::<serde_json::Value>(&saved).expect("save JSON")["id"]
            .as_str()
            .expect("save id")
            .to_string();

        let entry = server
            .with_global_store_read(|store| store.get(&id).map_err(|error| error.to_string()))
            .expect("load created memory")
            .expect("created memory exists");
        assert_eq!(entry.metadata["caller_context"], json!("kept"));
        assert!(entry.metadata.get("evidence_refs_v1").is_none());
        assert!(entry.metadata.get("source_refs").is_none());
    }

    #[tokio::test]
    async fn project_scope_preserves_and_appends_typed_evidence_references() {
        let (global_server, _temp_home) = crate::tests::make_server_with_temp_home();
        let global_db = global_server.global_db_path_buf();
        let project_db = global_db
            .parent()
            .expect("global db directory")
            .join("project-evidence.db");
        drop(global_server);
        let server = crate::MemoryServer::new(global_db, Some(project_db))
            .expect("create project-scoped server");
        let entry_id = "project-evidence-update";

        for reference in ["#100", "#101"] {
            handle_tachi_save(
                &server,
                serde_json::from_value(json!({
                    "id": entry_id,
                    "kind": "memory",
                    "text": "Project-scoped memory preserves typed evidence across updates.",
                    "path": "/audit/project-evidence-update",
                    "scope": "project",
                    "force": true,
                    "references": [reference]
                }))
                .expect("project save params"),
            )
            .await
            .expect("project save");
        }

        let entry = server
            .with_project_store_read(|store| store.get(entry_id).map_err(|error| error.to_string()))
            .expect("load project memory")
            .expect("project memory exists");
        assert_eq!(
            entry.metadata["evidence_refs_v1"]
                .as_array()
                .expect("typed evidence refs")
                .iter()
                .map(|value| value["ref"].as_str().expect("typed ref"))
                .collect::<Vec<_>>(),
            vec!["#100", "#101"]
        );
        assert!(entry.metadata.get("source_refs").is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_facade_updates_atomically_preserve_both_new_references() {
        let (seed_server, _temp_home) = crate::tests::make_server_with_temp_home();
        let entry_id = "atomic-evidence-two-writer";
        handle_tachi_save(
            &seed_server,
            serde_json::from_value(json!({
                "id": entry_id,
                "kind": "memory",
                "text": "Two independent writers append evidence to this memory.",
                "path": "/audit/atomic-evidence-two-writer",
                "scope": "global",
                "force": true,
                "references": ["#100"],
                "metadata": { "unrelated": "preserved" },
            }))
            .expect("seed save params"),
        )
        .await
        .expect("seed save");

        let db_path = seed_server.global_db_path_buf();
        let servers = [
            crate::MemoryServer::new(db_path.clone(), None).expect("open writer one"),
            crate::MemoryServer::new(db_path, None).expect("open writer two"),
        ];
        let _barrier_guard = crate::memory_search_ops::save_memory::install_pre_upsert_barrier(
            entry_id,
            std::sync::Arc::new(std::sync::Barrier::new(2)),
        );

        let tasks = servers
            .into_iter()
            .zip(["#101", "#102"])
            .map(|(server, reference)| {
                tokio::spawn(async move {
                    handle_tachi_save(
                        &server,
                        serde_json::from_value(json!({
                            "id": entry_id,
                            "kind": "memory",
                            "text": "Two independent writers append evidence to this memory.",
                            "scope": "global",
                            "force": true,
                            "references": [reference],
                        }))
                        .expect("writer params"),
                    )
                    .await
                })
            })
            .collect::<Vec<_>>();
        for task in tasks {
            task.await.expect("writer task").expect("writer save");
        }

        let entry = seed_server
            .with_global_store_read(|store| store.get(entry_id).map_err(|error| error.to_string()))
            .expect("load concurrently updated memory")
            .expect("concurrently updated memory exists");
        let mut refs = entry.metadata["evidence_refs_v1"]
            .as_array()
            .expect("typed evidence refs")
            .iter()
            .map(|value| value["ref"].as_str().expect("typed ref").to_string())
            .collect::<Vec<_>>();
        refs.sort();
        assert_eq!(refs, vec!["#100", "#101", "#102"]);
        assert_eq!(entry.metadata["unrelated"], json!("preserved"));
        assert!(entry.metadata.get("source_refs").is_none());
    }
}
