use super::*;

const NEEDLE: &str = "KnowledgeContextNeedle";
const PROJECT: &str = "knowledge-context-fixture";

fn wiki(id: &str) -> memcore::MemoryEntry {
    let mut entry = make_entry(id);
    entry.path = format!("/wiki/knowledge-context/{id}");
    entry.summary = format!("{NEEDLE} stored summary for {id}");
    entry.text =
        format!("{NEEDLE} {id}\n\n  Preserve these spaces.\n```rust\nlet source = \"原文\";\n```");
    entry.metadata = json!({"lifecycle": "active"});
    entry
}

fn project_fixture() -> (crate::tests::TestServer, String) {
    let (server, db_path) = make_server_with_project_fixture(PROJECT);
    let project_root = crate::path_utils::plan_c_project_root_from_local_db(&db_path)
        .expect("fixture has a local project database");
    let project = crate::path_utils::plan_c_dir_name_from_root(&project_root)
        .expect("fixture has a canonical project identity");
    let manifest_path = server.tachi_home_dir().join("manifest.json");
    let mut manifest = crate::manifest::Manifest::load(&manifest_path).expect("fixture manifest");
    let mut alias = manifest
        .dbs
        .iter()
        .find(|db| db.scope_hint == format!("project:{PROJECT}"))
        .expect("fixture project registration")
        .clone();
    // Default Guide applicability resolves the identity from the physical DB path.
    alias.scope_hint = format!("project:{project}");
    manifest.dbs.push(alias);
    manifest
        .save(&manifest_path)
        .expect("register canonical fixture alias");
    assert_eq!(
        crate::memory_search_ops::resolve_effective_named_project(&server, None),
        Some(project.clone())
    );
    (server, project)
}

fn guide(id: &str, project: &str) -> memcore::MemoryEntry {
    let mut entry = wiki(id);
    entry.path = format!("/guide/project/{id}");
    entry.scope = "project".to_string();
    entry.text = format!("{NEEDLE}: verify the reviewed release checklist, then record evidence.");
    entry.metadata = json!({
        "artifact_kind": "guide", "knowledge_scope": "project",
        "lifecycle": "active", "authority": "playbook",
        "origin_projects": [project],
        "applies_to": {
            "repos": [PROJECT], "task_type": ["review"],
            "profiles": ["agent"], "stage": ["verify"]
        }
    });
    entry
}

fn context(epoch: &str) -> Value {
    json!({"context_epoch": epoch})
}

fn guide_context(epoch: &str) -> Value {
    json!({
        "context_epoch": epoch, "repo": PROJECT, "task_type": "review",
        "profile": "agent", "stage": "verify"
    })
}

fn request(context: Value) -> Value {
    json!({
        "action": "briefing", "format": "json", "query": NEEDLE,
        "compact": false, "top_k": 6, "enable_rerank": false,
        "auto_conversation_scope": false,
        "metadata": {"knowledge_context": context}
    })
}

fn retaining(mut context: Value, packet: &Value) -> Value {
    context["retained"] = packet["manifest"].clone();
    request(context)
}

async fn call(server: &crate::MemoryServer, args: Value) -> Result<String, String> {
    let params = serde_json::from_value(args).map_err(|error| error.to_string())?;
    crate::facade_memory_ops::handle_tachi_memory(server, params).await
}

async fn packet(server: &crate::MemoryServer, args: Value) -> Value {
    let raw = call(server, args)
        .await
        .expect("knowledge briefing succeeds");
    let response: Value = serde_json::from_str(&raw).expect("briefing JSON");
    assert!(
        response
            .get("wiki")
            .is_none_or(|value| value.is_null() || value.as_array().is_some_and(Vec::is_empty)),
        "knowledge mode must not repeat a separate Wiki summary: {response}"
    );
    let knowledge = response["knowledge"].clone();
    assert_eq!(knowledge["protocol"], "wiki-guide-context-v1", "{response}");
    assert_eq!(knowledge["content_budget_bytes"], 32768);
    for key in [
        "units",
        "unchanged",
        "withdrawn",
        "unverified",
        "body_deferred",
        "warnings",
    ] {
        rows(&knowledge, key);
    }
    rows(&knowledge["manifest"], "entries");
    knowledge
}

fn rows<'a>(value: &'a Value, key: &str) -> &'a Vec<Value> {
    value[key]
        .as_array()
        .unwrap_or_else(|| panic!("missing {key} array: {value}"))
}

fn only_unit(packet: &Value) -> &Value {
    let units = rows(packet, "units");
    assert_eq!(units.len(), 1, "{packet}");
    &units[0]
}

fn seed_project(server: &crate::MemoryServer, entry: &memcore::MemoryEntry) {
    server
        .with_project_store(|store| store.upsert(entry).map_err(|e| e.to_string()))
        .expect("seed project knowledge");
}

#[tokio::test]
async fn knowledge_context_rechecks_synthetic_policy_for_retained_ids() {
    let mut synthetic = wiki("synthetic-retained-body");
    synthetic.metadata["synthetic_fixture"] = json!(true);
    let (server, _home) = seed_wiki_project_entries(vec![synthetic.clone()]);
    let mut opted_in = request(context("synthetic-context"));
    opted_in["include_test"] = json!(true);
    let first = packet(&server, opted_in).await;
    assert_eq!(only_unit(&first)["version"]["id"], synthetic.id);

    let excluded = packet(&server, retaining(context("synthetic-context"), &first)).await;
    assert!(rows(&excluded, "units").is_empty(), "{excluded}");
    assert!(rows(&excluded, "unchanged").is_empty(), "{excluded}");
    assert!(
        rows(&excluded["manifest"], "entries").is_empty(),
        "{excluded}"
    );

    let mut repeated = retaining(context("synthetic-context"), &first);
    repeated["include_test"] = json!(true);
    let retained = packet(&server, repeated).await;
    assert_eq!(retained["unchanged"], first["manifest"]["entries"]);
}

#[tokio::test]
async fn knowledge_body_is_lossless_and_only_client_retained_units_are_omitted() {
    let entry = wiki("retained-body");
    let (server, _home) = seed_wiki_project_entries(vec![entry.clone()]);
    let count_known = |store: &mut memcore::MemoryStore| {
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM agent_known_state", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|e| e.to_string())
    };
    let known_before = (
        server.with_global_store_read(count_known).unwrap(),
        server
            .with_named_project_store_read("wiki", count_known)
            .unwrap(),
    );
    let first = packet(&server, request(context("retained-body-context"))).await;
    assert_eq!(first["reset_reason"], "no_manifest");
    let unit = only_unit(&first);
    assert_eq!(unit["content"]["text"], entry.text);
    assert_eq!(unit["content"]["summary"], entry.summary);
    assert_eq!(unit["content"]["path"], entry.path);
    assert_eq!(unit["content"]["kind"], "wiki");
    assert_eq!(unit["version"]["unit"], "body");
    assert_eq!(
        first["manifest"]["entries"],
        json!([unit["version"].clone()])
    );

    let repeated = packet(&server, retaining(context("retained-body-context"), &first)).await;
    assert!(repeated["reset_reason"].is_null());
    assert!(rows(&repeated, "units").is_empty());
    assert_eq!(repeated["unchanged"], first["manifest"]["entries"]);
    assert_eq!(repeated["manifest"], first["manifest"]);

    let absent = packet(&server, request(context("retained-body-context"))).await;
    assert_eq!(absent["reset_reason"], "no_manifest");
    assert_eq!(
        only_unit(&absent),
        unit,
        "server history is not a retention receipt"
    );
    let mut dropped = retaining(context("retained-body-context"), &first);
    dropped["metadata"]["knowledge_context"]["retained"]["entries"] = json!([]);
    assert_eq!(only_unit(&packet(&server, dropped).await), unit);

    let new_context = packet(&server, retaining(context("replacement-context"), &first)).await;
    assert_eq!(new_context["reset_reason"], "context_epoch_changed");
    assert_eq!(only_unit(&new_context), unit);
    assert!(rows(&new_context, "unchanged").is_empty());
    assert_eq!(
        new_context["manifest"]["context_epoch"],
        "replacement-context"
    );
    assert_eq!(
        (
            server.with_global_store_read(count_known).unwrap(),
            server
                .with_named_project_store_read("wiki", count_known)
                .unwrap()
        ),
        known_before,
        "briefing must not persist agent_known_state"
    );
}

#[tokio::test]
async fn knowledge_retained_summary_does_not_acknowledge_the_body() {
    let entry = wiki("summary-then-body");
    let (server, _home) = seed_wiki_project_entries(vec![entry.clone()]);
    let mut summary_context = context("summary-context");
    summary_context["unit"] = json!("summary");
    let summary = packet(&server, request(summary_context)).await;
    assert_eq!(only_unit(&summary)["version"]["unit"], "summary");
    assert!(only_unit(&summary)["content"].get("text").is_none());

    let body = packet(&server, retaining(context("summary-context"), &summary)).await;
    assert_eq!(only_unit(&body)["version"]["unit"], "body");
    assert_eq!(only_unit(&body)["content"]["text"], entry.text);
    assert!(rows(&body["manifest"], "entries")
        .iter()
        .any(|v| v["unit"] == "body"));
}

#[tokio::test]
async fn knowledge_body_and_same_revision_summary_changes_replace_the_whole_unit() {
    let entry = wiki("changed-unit");
    let (server, _home) = seed_wiki_project_entries(vec![entry.clone()]);
    let mut prior = packet(&server, request(context("change-context"))).await;
    let changed_text = format!("{NEEDLE}\nA completely revised stored body.");
    let changed_summary = format!("{NEEDLE} enriched summary of the revised body");
    for summary_only in [false, true] {
        server
            .with_named_project_store("wiki", |store| {
                let current = store.get(&entry.id).map_err(|e| e.to_string())?.unwrap();
                if summary_only {
                    // Normal summary enrichment changes the payload without bumping revision.
                    assert!(store
                        .update_enrichment_fields(
                            &entry.id,
                            Some(&changed_summary),
                            None,
                            None,
                            None,
                            current.revision,
                        )
                        .map_err(|e| e.to_string())?);
                    let enriched = store.get(&entry.id).map_err(|e| e.to_string())?.unwrap();
                    assert_eq!(enriched.revision, current.revision);
                    assert_eq!(enriched.text, current.text);
                    assert_eq!(enriched.summary, changed_summary);
                } else {
                    assert!(store
                        .update_with_revision(
                            &entry.id,
                            &changed_text,
                            &entry.summary,
                            &entry.source,
                            &current.metadata,
                            None,
                            current.revision
                        )
                        .map_err(|e| e.to_string())?);
                }
                Ok(())
            })
            .unwrap();
        let next = packet(&server, retaining(context("change-context"), &prior)).await;
        let old = &only_unit(&prior)["version"];
        let new = only_unit(&next);
        assert_eq!(
            next["withdrawn"],
            json!([{"version": old, "reason": "changed"}])
        );
        assert_eq!(new["content"]["text"], changed_text);
        assert_eq!(
            new["content"]["summary"],
            if summary_only {
                changed_summary.as_str()
            } else {
                entry.summary.as_str()
            }
        );
        assert_eq!(new["content"]["path"], entry.path);
        assert_ne!(new["version"]["digest"], old["digest"]);
        if summary_only {
            assert_eq!(new["version"]["revision"], old["revision"]);
        }
        assert_eq!(next["manifest"]["entries"], json!([new["version"].clone()]));
        assert!(rows(&next, "unchanged").is_empty());
        prior = next;
    }
}

#[tokio::test]
async fn knowledge_archived_wiki_and_inactive_guide_are_withdrawn() {
    for inactive_guide in [false, true] {
        let (server, project) = project_fixture();
        let entry = if inactive_guide {
            guide("withdraw-guide", &project)
        } else {
            wiki("withdraw-wiki")
        };
        seed_project(&server, &entry);
        let first = packet(&server, request(guide_context("withdraw-context"))).await;
        let version = only_unit(&first)["version"].clone();
        server
            .with_project_store(|store| {
                if inactive_guide {
                    let mut current = store.get(&entry.id).map_err(|e| e.to_string())?.unwrap();
                    current.metadata["lifecycle"] = json!("rejected");
                    store.upsert(&current).map_err(|e| e.to_string())?;
                } else {
                    assert!(store.archive_memory(&entry.id).map_err(|e| e.to_string())?);
                }
                Ok(())
            })
            .unwrap();
        let next = packet(
            &server,
            retaining(guide_context("withdraw-context"), &first),
        )
        .await;
        assert_eq!(
            next["withdrawn"],
            json!([{"version": version, "reason": "not_retrievable"}])
        );
        assert!(rows(&next, "units").is_empty());
        assert!(rows(&next, "unchanged").is_empty());
        assert!(rows(&next["manifest"], "entries").is_empty());
    }
}

#[tokio::test]
async fn knowledge_retains_a_revalidated_unit_absent_from_current_query_results() {
    let (server, _home) = seed_wiki_project_entries(vec![wiki("held-outside-query")]);
    let first = packet(&server, request(context("held-context"))).await;
    only_unit(&first);
    let mut unrelated = retaining(context("held-context"), &first);
    unrelated["query"] = json!("ZqxvUnrelatedNoKnowledgeMatch");
    let next = packet(&server, unrelated).await;
    assert!(
        next["reset_reason"].is_null(),
        "query text is not retention scope"
    );
    assert!(rows(&next, "units").is_empty());
    assert!(rows(&next, "withdrawn").is_empty());
    assert!(rows(&next, "unchanged").is_empty());
    assert_eq!(next["manifest"], first["manifest"]);
}

#[tokio::test]
async fn knowledge_read_error_is_unverified_not_a_false_withdrawal() {
    let entry = wiki("unreadable-held-unit");
    let (server, _home) = seed_wiki_project_entries(vec![entry.clone()]);
    let first = packet(&server, request(context("read-error-context"))).await;
    let version = only_unit(&first)["version"].clone();
    server
        .with_named_project_store("wiki", |store| {
            // Corrupt one disposable row's SQLite type, without adding trigger DDL.
            store
                .connection()
                .execute(
                    "UPDATE memories SET summary = X'80' WHERE id = ?1",
                    [&entry.id],
                )
                .map_err(|e| e.to_string())?;
            assert!(
                store.get(&entry.id).is_err(),
                "fixture must cause a real typed read error"
            );
            Ok(())
        })
        .unwrap();
    let next = packet(&server, retaining(context("read-error-context"), &first)).await;
    assert_eq!(next["incomplete"], true);
    assert!(!rows(&next, "warnings").is_empty());
    assert!(rows(&next, "withdrawn").is_empty());
    assert!(rows(&next, "unchanged").is_empty());
    assert!(rows(&next["manifest"], "entries").is_empty());
    assert_eq!(rows(&next, "unverified").len(), 1);
    assert_eq!(next["unverified"][0]["version"], version);
    assert!(next["unverified"][0]["reason"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));
}

#[tokio::test]
async fn knowledge_same_id_in_two_stores_is_distinct_and_explicit_project_resets_scope() {
    let (server, project_name) = project_fixture();
    crate::tests::register_logical_shared_wiki(&server);
    let project_guide = guide("shared-id", &project_name);
    let shared_wiki = wiki("shared-id");
    seed_project(&server, &project_guide);
    server
        .with_named_project_store("wiki", |store| {
            store.upsert(&shared_wiki).map_err(|e| e.to_string())
        })
        .unwrap();
    let first = packet(&server, request(guide_context("stores-context"))).await;
    let units = rows(&first, "units");
    assert_eq!(units.len(), 2, "{first}");
    assert_eq!(units[0]["version"]["id"], "shared-id");
    assert_eq!(units[1]["version"]["id"], "shared-id");
    assert_ne!(units[0]["version"]["store"], units[1]["version"]["store"]);
    let repeated = packet(&server, retaining(guide_context("stores-context"), &first)).await;
    assert!(rows(&repeated, "units").is_empty());
    assert_eq!(rows(&repeated, "unchanged").len(), 2);
    assert_eq!(rows(&repeated["manifest"], "entries").len(), 2);

    let mut prior = first;
    for (project, text) in [
        (project_name.as_str(), &project_guide.text),
        ("wiki", &shared_wiki.text),
    ] {
        let mut args = retaining(guide_context("stores-context"), &prior);
        args["project"] = json!(project);
        let next = packet(&server, args).await;
        assert_eq!(next["reset_reason"], "scope_changed");
        assert_ne!(next["manifest"]["scope"], prior["manifest"]["scope"]);
        assert_eq!(only_unit(&next)["content"]["text"], *text);
        assert!(rows(&next, "unchanged").is_empty());
        assert_eq!(rows(&next["manifest"], "entries").len(), 1);
        prior = next;
    }
}

#[tokio::test]
async fn knowledge_guide_requires_matching_applicability_not_just_lexical_relevance() {
    let (server, project) = project_fixture();
    seed_project(&server, &guide("applicable-guide", &project));
    let first = packet(&server, request(guide_context("applicability-context"))).await;
    assert_eq!(only_unit(&first)["content"]["kind"], "guide");
    for field in ["repo", "task_type", "profile", "stage"] {
        let mut mismatched = guide_context("applicability-context");
        mismatched[field] = json!("different-context");
        let next = packet(&server, request(mismatched)).await;
        assert!(rows(&next, "units").is_empty(), "{field} mismatch: {next}");
        assert!(rows(&next["manifest"], "entries").is_empty());
    }
}

#[tokio::test]
async fn knowledge_oversize_utf8_body_falls_back_to_summary_without_acknowledging_body() {
    let mut entry = wiki("oversize-body");
    entry.text = format!("{NEEDLE}\n{}", "证".repeat(12_000));
    assert!(entry.text.chars().count() < 32768 && entry.text.len() > 32768);
    let (server, _home) = seed_wiki_project_entries(vec![entry.clone()]);
    let first = packet(&server, request(context("budget-context"))).await;
    assert_eq!(only_unit(&first)["version"]["unit"], "summary");
    assert_eq!(only_unit(&first)["content"]["summary"], entry.summary);
    assert!(only_unit(&first)["content"].get("text").is_none());
    let next = packet(&server, retaining(context("budget-context"), &first)).await;
    for response in [&first, &next] {
        assert!(rows(&response["manifest"], "entries")
            .iter()
            .all(|v| v["unit"] != "body"));
        assert!(rows(response, "units")
            .iter()
            .all(|v| v["version"]["unit"] != "body"));
        assert!(
            rows(response, "body_deferred").iter().any(|v| {
                v["id"] == entry.id
                    && v["store"] == first["units"][0]["version"]["store"]
                    && v["reason"].as_str().is_some_and(|s| !s.is_empty())
            }),
            "{response}"
        );
    }
}

#[tokio::test]
async fn knowledge_namespace_rejects_null_unknown_oversize_and_duplicate_claims() {
    let (server, _home) = seed_wiki_project_entries(vec![wiki("validated-manifest")]);
    let first = packet(&server, request(context("validation-context"))).await;
    let version = only_unit(&first)["version"].clone();
    let mut duplicate = context("validation-context");
    duplicate["retained"] = first["manifest"].clone();
    duplicate["retained"]["entries"] = json!([version.clone(), version]);
    let cases = [
        ("null namespace", Value::Null),
        (
            "unknown field",
            json!({"context_epoch":"validation-context", "surprise":true}),
        ),
        ("oversize epoch", context(&"x".repeat(16_384))),
        (
            "unknown unit",
            json!({"context_epoch":"validation-context", "unit":"excerpt"}),
        ),
        ("duplicate retained unit", duplicate),
    ];
    for (label, invalid) in cases {
        let error = call(&server, request(invalid)).await.expect_err(label);
        assert!(
            error.contains("knowledge_context"),
            "{label} must identify its namespace: {error}"
        );
    }
}

#[tokio::test]
async fn knowledge_markdown_embeds_the_same_complete_content_and_manifest_json() {
    let entry = wiki("markdown-body");
    let (server, _home) = seed_wiki_project_entries(vec![entry.clone()]);
    let args = request(context("markdown-context"));
    let expected = packet(&server, args.clone()).await;
    let mut markdown_args = args;
    markdown_args["format"] = json!("markdown");
    let markdown = call(&server, markdown_args)
        .await
        .expect("Markdown briefing");
    let embedded = markdown
        .split("```json")
        .skip(1)
        .find_map(|part| {
            let (body, _) = part.split_once("\n```")?;
            let value: Value = serde_json::from_str(body.trim()).ok()?;
            if value["protocol"] == "wiki-guide-context-v1" {
                Some(value)
            } else {
                value.get("knowledge").cloned()
            }
        })
        .expect("Markdown must carry machine-readable knowledge JSON");
    assert_eq!(embedded["units"], expected["units"]);
    assert_eq!(embedded["manifest"], expected["manifest"]);
    assert_eq!(only_unit(&embedded)["content"]["text"], entry.text);
}

#[tokio::test]
async fn knowledge_retained_wiki_obeys_a_new_deny_rule_for_the_same_role() {
    let entry = wiki("role-revoked-body");
    let (server, _home) = seed_wiki_project_entries(vec![entry.clone()]);
    let mut args = request(context("role-revoked-context"));
    args["agent_role"] = json!("knowledge-reader");
    let first = packet(&server, args).await;
    let version = only_unit(&first)["version"].clone();
    assert_eq!(only_unit(&first)["content"]["text"], entry.text);
    server
        .tachi_sandbox(Parameters(crate::tests::wire_sandbox_params(
            "set_rule",
            json!({
                "agent_role": "knowledge-reader", "path_pattern": entry.path,
                "access_level": "deny"
            }),
        )))
        .await
        .expect("revoke the same role's access after delivery");

    let mut args = retaining(context("role-revoked-context"), &first);
    args["agent_role"] = json!("knowledge-reader");
    args["query"] = json!("ZqxvUnrelatedNoKnowledgeMatch");
    let next = packet(&server, args).await;
    assert_eq!(next["reset_reason"], Value::Null);
    assert_eq!(next["manifest"]["scope"], first["manifest"]["scope"]);
    assert_eq!(
        next["withdrawn"],
        json!([{
            "version": version, "reason": "not_retrievable"
        }])
    );
    for key in ["units", "unchanged", "unverified"] {
        assert!(rows(&next, key).is_empty(), "{key}: {next}");
    }
    assert!(rows(&next["manifest"], "entries").is_empty());
}

#[tokio::test]
async fn knowledge_retained_wiki_is_not_retrievable_after_expiry_without_a_query_match() {
    let mut entry = wiki("expired-held-body");
    entry.valid_from = "1999-01-01T00:00:00.000Z".to_string();
    let (server, _home) = seed_wiki_project_entries(vec![entry.clone()]);
    let first = packet(&server, request(context("expiry-context"))).await;
    let version = only_unit(&first)["version"].clone();
    let stored = server
        .with_named_project_store("wiki", |store| {
            let mut current = store
                .get(&entry.id)
                .map_err(|error| error.to_string())?
                .unwrap();
            current.valid_until = Some("2000-01-01T00:00:00.000Z".to_string());
            store.upsert(&current).map_err(|error| error.to_string())?;
            store.get(&entry.id).map_err(|error| error.to_string())
        })
        .expect("expire only the temporary Wiki row")
        .expect("expired row remains physically present");
    assert_eq!(stored.revision, version["revision"].as_i64().unwrap() + 1);
    assert_eq!(stored.text, entry.text);
    assert_eq!(
        stored.valid_until.as_deref(),
        Some("2000-01-01T00:00:00.000Z")
    );

    let mut args = retaining(context("expiry-context"), &first);
    args["query"] = json!("ZqxvUnrelatedNoKnowledgeMatch");
    let next = packet(&server, args).await;
    assert_eq!(next["reset_reason"], Value::Null);
    assert_eq!(next["manifest"]["scope"], first["manifest"]["scope"]);
    assert_eq!(
        next["withdrawn"],
        json!([{
            "version": version, "reason": "not_retrievable"
        }])
    );
    for key in ["units", "unchanged", "unverified"] {
        assert!(rows(&next, key).is_empty(), "{key}: {next}");
    }
    assert!(rows(&next["manifest"], "entries").is_empty());
}

#[tokio::test]
async fn knowledge_retained_store_cannot_expand_the_explicit_project_plan() {
    let (server, _home) = seed_wiki_project_entries(vec![wiki("forged-store-body")]);
    let mut args = request(context("forged-store-context"));
    args["project"] = json!("wiki");
    let first = packet(&server, args).await;
    let mut forged = only_unit(&first)["version"].clone();
    forged["store"] = json!({"kind": "named_project", "project": "unregistered-private"});
    let forbidden_dir = server
        .tachi_home_dir()
        .join("projects/unregistered-private");
    assert!(!forbidden_dir.exists());
    let mut args = retaining(context("forged-store-context"), &first);
    args["metadata"]["knowledge_context"]["retained"]["entries"] = json!([forged.clone()]);
    args["project"] = json!("wiki");
    args["query"] = json!("ZqxvUnrelatedNoKnowledgeMatch");
    let next = packet(&server, args).await;
    assert_eq!(next["reset_reason"], Value::Null);
    assert_eq!(next["manifest"]["scope"], first["manifest"]["scope"]);
    assert_eq!(
        next["withdrawn"],
        json!([{
            "version": forged, "reason": "not_retrievable"
        }])
    );
    for key in ["units", "unchanged", "unverified"] {
        assert!(rows(&next, key).is_empty(), "{key}: {next}");
    }
    assert!(rows(&next["manifest"], "entries").is_empty());
    assert!(
        !forbidden_dir.exists(),
        "retained claims cannot open a new store"
    );
}

#[tokio::test]
async fn knowledge_repair_selection_changes_do_not_reset_or_resend_generic_wiki() {
    let mut entry = wiki("selection-independent-wiki");
    entry.domain = Some("science".to_string());
    let (server, _home) = seed_wiki_project_entries(vec![entry]);
    let mut initial = request(guide_context("selection-context"));
    initial["agent_role"] = json!("knowledge-reader");
    initial["domain"] = json!("science");
    let first = packet(&server, initial.clone()).await;
    let version = only_unit(&first)["version"].clone();
    for (field, value) in [
        ("stage", "plan"),
        ("repo", "another-repository"),
        ("task_type", "implementation"),
        ("profile", "another-profile"),
        ("domain", "engineering"),
        ("path_prefix", "/guide"),
    ] {
        let mut args = initial.clone();
        args["metadata"]["knowledge_context"]["retained"] = first["manifest"].clone();
        if matches!(field, "domain" | "path_prefix") {
            args[field] = json!(value);
        } else {
            args["metadata"]["knowledge_context"][field] = json!(value);
        }
        let next = packet(&server, args).await;
        assert_eq!(next["reset_reason"], Value::Null, "{field}: {next}");
        assert_eq!(next["manifest"], first["manifest"], "{field}: {next}");
        for key in ["units", "withdrawn", "unverified"] {
            assert!(rows(&next, key).is_empty(), "{field}/{key}: {next}");
        }
        if !matches!(field, "domain" | "path_prefix") {
            assert_eq!(
                next["unchanged"],
                json!([version.clone()]),
                "{field}: {next}"
            );
        } else {
            assert!(rows(&next, "unchanged").iter().all(|held| held == &version));
        }
        if field == "path_prefix" {
            assert!(rows(&next, "unchanged").is_empty());
        }
    }
}

#[tokio::test]
async fn knowledge_repair_stage_change_withdraws_only_the_inapplicable_guide() {
    let (server, project) = project_fixture();
    let generic = wiki("stage-independent-wiki");
    let restricted = guide("verification-only-guide", &project);
    seed_project(&server, &generic);
    seed_project(&server, &restricted);
    let first = packet(&server, request(guide_context("stage-context"))).await;
    let units = rows(&first, "units");
    assert_eq!(units.len(), 2, "{first}");
    let version_for = |id: &str| {
        units
            .iter()
            .find(|unit| unit["version"]["id"] == id)
            .expect("both Wiki and Guide must be delivered")["version"]
            .clone()
    };
    let wiki_version = version_for(&generic.id);
    let guide_version = version_for(&restricted.id);
    let mut planning = guide_context("stage-context");
    planning["stage"] = json!("plan");
    let next = packet(&server, retaining(planning, &first)).await;
    assert_eq!(next["reset_reason"], Value::Null);
    assert_eq!(next["manifest"]["scope"], first["manifest"]["scope"]);
    assert!(rows(&next, "units").is_empty());
    assert!(rows(&next, "unverified").is_empty());
    assert_eq!(
        next["withdrawn"],
        json!([{
            "version": guide_version, "reason": "not_retrievable"
        }])
    );
    assert_eq!(next["unchanged"], json!([wiki_version.clone()]));
    assert_eq!(next["manifest"]["entries"], json!([wiki_version]));
}

#[tokio::test]
async fn knowledge_repair_root_and_trailing_slash_prefixes_select_the_expected_roots() {
    let (server, project) = project_fixture();
    seed_project(&server, &wiki("prefix-wiki"));
    seed_project(&server, &guide("prefix-guide", &project));
    for (prefix, expected) in [
        ("/", vec!["prefix-guide", "prefix-wiki"]),
        ("/wiki/", vec!["prefix-wiki"]),
        ("/guide/", vec!["prefix-guide"]),
    ] {
        let mut args = request(guide_context("prefix-context"));
        args["path_prefix"] = json!(prefix);
        let next = packet(&server, args).await;
        let mut ids = rows(&next, "units")
            .iter()
            .map(|unit| unit["version"]["id"].as_str().expect("unit id"))
            .collect::<Vec<_>>();
        ids.sort_unstable();
        assert_eq!(ids, expected, "{prefix}: {next}");
        assert_eq!(rows(&next["manifest"], "entries").len(), ids.len());
        assert!(rows(&next, "unverified").is_empty());
    }
}

#[tokio::test]
async fn knowledge_repair_final_limit_follows_expiry_and_applicability_filtering() {
    for expired in [true, false] {
        let mut invalid = wiki("high-ranked-ineligible");
        invalid.path = format!("/wiki/{NEEDLE}/priority");
        invalid.summary = format!("{NEEDLE} {NEEDLE} transport incident response");
        invalid.text = format!(
            "{NEEDLE} rebuild the transport control plane.\n\
             {NEEDLE} inspect broker queues and reconnect workers.\n\
             {NEEDLE} verify protocol handshakes and delivery acknowledgements.\n\
             {NEEDLE} audit routing tables and packet retries."
        );
        invalid.importance = 1.0;
        invalid.topic = NEEDLE.to_string();
        invalid.keywords = vec![NEEDLE.to_string()];
        invalid.entities = vec![NEEDLE.to_string()];
        if expired {
            invalid.valid_from = "2000-01-01T00:00:00.000Z".to_string();
            invalid.valid_until = Some("2001-01-01T00:00:00.000Z".to_string());
        } else {
            invalid.scope = "project".to_string();
            invalid.metadata = json!({
                "lifecycle": "active", "knowledge_scope": "project",
                "applies_to": {"projects": ["unrelated-private-project"]}
            });
        }
        let mut valid = wiki("low-ranked-valid");
        valid.summary = "Independent marine field observations".to_string();
        valid.text = format!(
            "A coastal survey records salinity, algae, tidal habitats and seasonal fish counts. \
             The field notebook uses reference {NEEDLE} for its sampling ledger."
        );
        valid.importance = 0.1;
        let (server, _home) = seed_wiki_project_entries(vec![invalid.clone(), valid.clone()]);
        let mut args = request(context("final-limit-context"));
        args["top_k"] = json!(1);
        let mut legacy_args = args.clone();
        legacy_args["metadata"] = Value::Null;
        let legacy: Value = serde_json::from_str(
            &call(&server, legacy_args)
                .await
                .expect("ordinary briefing ranking probe"),
        )
        .expect("ordinary briefing JSON");
        assert_eq!(
            rows(&legacy, "wiki").len(),
            1,
            "expired={expired}: {legacy}"
        );
        assert_eq!(
            legacy["wiki"][0]["id"], invalid.id,
            "fixture must rank the ineligible candidate first; expired={expired}: {legacy}"
        );

        let next = packet(&server, args).await;
        let selected = only_unit(&next);
        assert_eq!(
            selected["version"]["id"], valid.id,
            "expired={expired}: {next}"
        );
        assert_eq!(selected["content"]["text"], valid.text);
        assert_eq!(
            next["manifest"]["entries"],
            json!([selected["version"].clone()])
        );
        assert!(rows(&next, "unverified").is_empty());
    }
}
