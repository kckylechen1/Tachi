use super::*;

use crate::wiki_ops::{
    collect_wiki_browse_value, collect_wiki_read_value, collect_wiki_search_value, handle_wiki_read,
};

fn active_wiki_entry() -> memcore::MemoryEntry {
    let mut entry = make_entry("wiki-lifecycle-active");
    entry.path = "/wiki/engineering/lifecycle/active-entry".to_string();
    entry.summary = "Lifecycle gate active entry".to_string();
    entry.text = "LifecycleGateNeedle documents the reviewed active wiki entry.".to_string();
    entry.metadata = json!({"lifecycle": "active"});
    entry
}

fn pending_review_draft_entry() -> memcore::MemoryEntry {
    let mut entry = make_entry("wiki-lifecycle-draft");
    entry.path = "/wiki/drafts/lifecycle-gate-draft".to_string();
    entry.summary = "Lifecycle gate pending draft".to_string();
    entry.text = "LifecycleGateNeedle documents the unreviewed pending draft entry.".to_string();
    // Mirrors `foundry_runtime_ops::wiki_evolver::save_wiki_draft`'s real
    // metadata marker, not a fabricated one this leaf invented.
    entry.metadata = json!({"review_status": "pending"});
    entry
}

fn reviewed_unbounded_shared_entry() -> memcore::MemoryEntry {
    let mut entry = make_entry("wiki-reviewed-unbounded-shared");
    entry.path = "/wiki/engineering/lifecycle/reviewed-unbounded-shared".to_string();
    entry.summary = "Reviewed but unbounded shared entry".to_string();
    entry.text = "LifecycleGateNeedle must not make unbounded shared knowledge active.".to_string();
    entry.metadata = json!({
        "artifact_kind": "wiki",
        "knowledge_scope": "shared",
        "origin_projects": ["Sigil"],
        "lifecycle": "active",
        "authority": "advisory",
        "source_bundle_hash": "reviewed-source-bundle",
        "review_receipt": {
            "approver": "owner",
            "decision": "approved",
            "decided_at": "2026-07-31T00:00:00Z"
        }
    });
    entry
}

fn declared_malformed_applicability_entry() -> memcore::MemoryEntry {
    let mut entry = make_entry("wiki-declared-malformed-applicability");
    entry.path = "/wiki/engineering/lifecycle/declared-malformed".to_string();
    entry.text =
        "LifecycleGateNeedle declared malformed applicability must stay pending.".to_string();
    entry.metadata = json!({
        "artifact_kind": "wiki",
        "knowledge_scope": "project",
        "applies_to": {"repos": ["kckylechen1/tachi"]},
        "applicability_status": "malformed",
        "lifecycle": "active",
        "authority": "advisory"
    });
    entry
}

fn shared_with_only_legacy_origin_entry() -> memcore::MemoryEntry {
    let mut entry = make_entry("wiki-shared-legacy-origin-only");
    entry.path = "/wiki/engineering/lifecycle/shared-legacy-origin-only".to_string();
    entry.text = "LifecycleGateNeedle shared knowledge needs a typed origin.".to_string();
    entry.metadata = json!({
        "artifact_kind": "wiki",
        "knowledge_scope": "shared",
        "applies_to": {"repos": ["kckylechen1/tachi"]},
        "lifecycle": "active",
        "authority": "advisory",
        "source_bundle_hash": "reviewed-source-bundle",
        "review_receipt": {
            "approver": "owner",
            "decision": "approved",
            "decided_at": "2026-07-31T00:00:00Z"
        },
        "provenance": {"db_path": "/work/Sigil/.tachi/memory.db"}
    });
    entry
}

fn search_params(query: &str, lifecycle: Option<&str>) -> WikiSearchParams {
    WikiSearchParams {
        query: query.to_string(),
        path_prefix: Some("/wiki".to_string()),
        category: None,
        top_k: 10,
        include_archived: false,
        agent_role: None,
        project: Some("wiki".to_string()),
        domain: None,
        file_context: None,
        error_context: None,
        weights: None,
        lifecycle: lifecycle.map(str::to_string),
    }
}

/// #1072 RED case 2: "Pending `/wiki/drafts/...` currently leaks into normal
/// search; GREEN excludes it by default." Also exercises the explicit-scope
/// escape hatch canon doc §7 requires ("drafts use an explicit scope").
#[tokio::test]
async fn wiki_search_excludes_pending_review_drafts_by_default_and_includes_with_explicit_scope() {
    let (server, _home) =
        seed_wiki_project_entries(vec![active_wiki_entry(), pending_review_draft_entry()]);

    let default_scope =
        collect_wiki_search_value(&server, search_params("LifecycleGateNeedle", None))
            .await
            .expect("default-scope search should succeed");
    let default_paths: Vec<&str> = default_scope["results"]
        .as_array()
        .expect("results array")
        .iter()
        .filter_map(|row| row["path"].as_str())
        .collect();
    assert!(
        default_paths.contains(&"/wiki/engineering/lifecycle/active-entry"),
        "default search must still surface the active entry: {default_paths:?}"
    );
    assert!(
        !default_paths.contains(&"/wiki/drafts/lifecycle-gate-draft"),
        "RED: pending_review draft must not leak into default search: {default_paths:?}"
    );

    let draft_scope = collect_wiki_search_value(
        &server,
        search_params("LifecycleGateNeedle", Some("pending_review")),
    )
    .await
    .expect("explicit-scope search should succeed");
    let draft_paths: Vec<&str> = draft_scope["results"]
        .as_array()
        .expect("results array")
        .iter()
        .filter_map(|row| row["path"].as_str())
        .collect();
    assert!(
        draft_paths.contains(&"/wiki/drafts/lifecycle-gate-draft"),
        "explicit pending_review scope must surface the draft: {draft_paths:?}"
    );
    assert!(
        !draft_paths.contains(&"/wiki/engineering/lifecycle/active-entry"),
        "explicit pending_review scope must not surface the active entry: {draft_paths:?}"
    );
}

#[tokio::test]
async fn wiki_search_keeps_invalid_effective_artifacts_pending() {
    let entries = vec![
        reviewed_unbounded_shared_entry(),
        declared_malformed_applicability_entry(),
        shared_with_only_legacy_origin_entry(),
    ];
    let expected_paths = entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<Vec<_>>();
    let (server, _home) = seed_wiki_project_entries(entries);

    let default_scope =
        collect_wiki_search_value(&server, search_params("LifecycleGateNeedle", None))
            .await
            .expect("default-scope search should succeed");
    assert!(
        default_scope["results"]
            .as_array()
            .expect("results array")
            .iter()
            .all(|row| !expected_paths.iter().any(|path| row["path"] == *path)),
        "invalid effective artifacts must not be default-retrievable"
    );

    let pending_scope = collect_wiki_search_value(
        &server,
        search_params("LifecycleGateNeedle", Some("pending_review")),
    )
    .await
    .expect("pending-scope search should succeed");
    let pending_paths = pending_scope["results"]
        .as_array()
        .expect("results array")
        .iter()
        .filter_map(|row| row["path"].as_str())
        .collect::<Vec<_>>();
    for expected_path in &expected_paths {
        assert!(
            pending_paths.contains(&expected_path.as_str()),
            "invalid artifact {expected_path} must remain inspectable as pending: {pending_paths:?}"
        );
    }
}

#[tokio::test]
async fn wiki_facade_browse_preserves_active_default_and_forwards_all_lifecycle() {
    let mut fresh = make_entry("wiki-facade-fresh-pending");
    fresh.path = "/wiki/engineering/fresh-pending".to_string();
    fresh.metadata = json!({"lifecycle": "pending_review"});
    let (server, _home) = seed_wiki_project_entries(vec![fresh]);

    let default_params: TachiWikiParams = serde_json::from_value(json!({
        "action": "browse",
        "project": "wiki"
    }))
    .expect("default facade params");
    let default_body = server
        .tachi_wiki(Parameters(default_params))
        .await
        .expect("default browse");
    let default_json: Value = serde_json::from_str(&default_body).expect("default browse JSON");
    assert_eq!(default_json["total"], 0, "default must remain active-only");
    assert_eq!(default_json["categories"], json!([]));

    let all_params: TachiWikiParams = serde_json::from_value(json!({
        "action": "browse",
        "project": "wiki",
        "lifecycle": "all"
    }))
    .expect("all-lifecycle facade params");
    let all_body = server
        .tachi_wiki(Parameters(all_params))
        .await
        .expect("all-lifecycle browse");
    let all_json: Value = serde_json::from_str(&all_body).expect("all browse JSON");
    assert_eq!(all_json["total"], 1);
    assert_eq!(
        all_json["categories"],
        json!([{"path": "/wiki/engineering", "count": 1}])
    );
}

/// #1072 RED case 3: "Read/search currently hide authority/revision/source
/// refs; GREEN exposes them."
#[tokio::test]
async fn wiki_search_exposes_lifecycle_authority_revision_provenance_fields() {
    let mut entry = active_wiki_entry();
    entry.metadata = json!({
        "lifecycle": "active",
        "authority": "advisory",
        "source_refs": ["kckylechen1/tachi#1072"],
        "evidence_refs_v1": [{"ref": "kckylechen1/tachi#1072", "target_kind": "issue", "captured_at": "2026-07-17T00:00:00Z"}],
    });
    let (server, _home) = seed_wiki_project_entries(vec![entry]);

    let result = collect_wiki_search_value(&server, search_params("LifecycleGateNeedle", None))
        .await
        .expect("search should succeed");
    let row = result["results"]
        .as_array()
        .expect("results array")
        .first()
        .expect("one result");
    assert_eq!(row["lifecycle"], json!("active"));
    assert_eq!(row["authority"], json!("advisory"));
    assert!(row["revision"].is_number(), "revision: {row:?}");
    assert_eq!(row["source_refs"], json!(["kckylechen1/tachi#1072"]));
    assert_eq!(
        row["evidence_refs_v1"][0]["ref"],
        json!("kckylechen1/tachi#1072")
    );
}

#[tokio::test]
async fn search_read_and_browse_expose_the_same_effective_artifact() {
    let mut entry = active_wiki_entry();
    entry.metadata = json!({
        "artifact_kind": "wiki",
        "knowledge_scope": "shared",
        "origin_projects": ["Sigil"],
        "applies_to": {"repos": ["Sigil", "Quant_Analyzer_2026"]},
        "known_exceptions": ["Quant uses a separate persistence adapter"],
        "lifecycle": "active",
        "authority": "advisory",
        "source_bundle_hash": "reviewed-source-bundle",
        "review_receipt": {
            "approver": "owner",
            "decision": "approved",
            "decided_at": "2026-07-31T00:00:00Z"
        },
    });
    let path = entry.path.clone();
    let (server, _home) = seed_wiki_project_entries(vec![entry]);

    let search = collect_wiki_search_value(&server, search_params("LifecycleGateNeedle", None))
        .await
        .expect("search");
    let search_artifact = search["results"][0]["effective_artifact"].clone();
    let read = collect_wiki_read_value(&server, &path, "wiki").expect("read");
    let read_artifact = read["entry"]["effective_artifact"].clone();
    let browse = collect_wiki_browse_value(
        &server,
        WikiBrowseParams {
            category: Some("/wiki/engineering/lifecycle".to_string()),
            limit: 10,
            project: Some("wiki".to_string()),
            lifecycle: None,
        },
    )
    .expect("browse");
    let browse_artifact = browse["entries"][0]["effective_artifact"].clone();

    assert_eq!(search_artifact, read_artifact);
    assert_eq!(search_artifact, browse_artifact);
    assert_eq!(search_artifact["knowledge_scope"], json!("shared"));
    assert_eq!(search_artifact["applicability_status"], json!("bounded"));
    assert_eq!(
        search_artifact["applies_to"]["repos"],
        json!(["Quant_Analyzer_2026", "Sigil"])
    );
}

#[tokio::test]
async fn public_search_read_and_browse_reach_guide_artifacts() {
    let mut entry = active_wiki_entry();
    entry.path = "/guide/global/workflows/agent-review".to_string();
    entry.metadata = json!({
        "artifact_kind": "guide",
        "knowledge_scope": "shared",
        "origin_projects": ["Sigil"],
        "applies_to": {"task_type": ["agent_review"]},
        "lifecycle": "active",
        "authority": "playbook",
        "source_bundle_hash": "reviewed-guide-source-bundle",
        "review_receipt": {
            "approver": "owner",
            "decision": "approved",
            "decided_at": "2026-07-31T00:00:00Z"
        },
    });
    let expected_path = entry.path.clone();
    let (server, _home) = seed_wiki_project_entries(vec![entry]);

    let search_params: TachiWikiParams = serde_json::from_value(json!({
        "action": "search",
        "query": "LifecycleGateNeedle",
        "project": "wiki"
    }))
    .expect("search params");
    let search: Value = serde_json::from_str(
        &server
            .tachi_wiki(Parameters(search_params))
            .await
            .expect("search guide"),
    )
    .expect("search JSON");

    let read_params: TachiWikiParams = serde_json::from_value(json!({
        "action": "read",
        "path": "/guide/global/workflows/agent-review",
        "project": "wiki"
    }))
    .expect("read params");
    let read: Value = serde_json::from_str(
        &server
            .tachi_wiki(Parameters(read_params))
            .await
            .expect("read guide"),
    )
    .expect("read JSON");

    let browse_params: TachiWikiParams = serde_json::from_value(json!({
        "action": "browse",
        "category": "/guide/global/workflows",
        "project": "wiki"
    }))
    .expect("browse params");
    let browse: Value = serde_json::from_str(
        &server
            .tachi_wiki(Parameters(browse_params))
            .await
            .expect("browse guide"),
    )
    .expect("browse JSON");

    assert_eq!(search["results"][0]["path"], expected_path);
    assert_eq!(read["status"], json!("found"));
    assert_eq!(read["entry"]["path"], expected_path);
    assert_eq!(browse["entries"][0]["path"], expected_path);
    assert_eq!(
        search["results"][0]["effective_artifact"],
        read["entry"]["effective_artifact"]
    );
    assert_eq!(
        search["results"][0]["effective_artifact"],
        browse["entries"][0]["effective_artifact"]
    );
}

#[tokio::test]
async fn public_read_and_browse_normalize_backslash_guide_paths() {
    let mut entry = active_wiki_entry();
    entry.path = "/guide/global/workflows/agent-review".to_string();
    let expected_path = entry.path.clone();
    let (server, _home) = seed_wiki_project_entries(vec![entry]);

    for path in [
        "guide\\global\\workflows\\agent-review",
        "/guide\\global\\workflows\\agent-review",
    ] {
        let read_params: TachiWikiParams = serde_json::from_value(json!({
            "action": "read",
            "path": path,
            "project": "wiki"
        }))
        .expect("read params");
        let read: Value = serde_json::from_str(
            &server
                .tachi_wiki(Parameters(read_params))
                .await
                .expect("read guide through normalized path"),
        )
        .expect("read JSON");
        assert_eq!(
            read["entry"]["path"], expected_path,
            "RED: accepted backslash read path {path:?} did not reach the stored guide"
        );
    }

    let browse_params: TachiWikiParams = serde_json::from_value(json!({
        "action": "browse",
        "category": "guide\\global\\workflows",
        "project": "wiki"
    }))
    .expect("browse params");
    let browse: Value = serde_json::from_str(
        &server
            .tachi_wiki(Parameters(browse_params))
            .await
            .expect("browse guide through normalized category"),
    )
    .expect("browse JSON");

    assert_eq!(
        browse["entries"][0]["path"], expected_path,
        "RED: accepted backslash browse category did not reach the stored guide"
    );
}

/// #1072 fail-closed truthful-retrieval: a direct read of a
/// `pending_review` draft's exact path is NOT gated (the caller asked for
/// this exact path), but it must never render as plain reviewed wiki
/// content — the markdown response carries a visible lifecycle warning.
#[tokio::test]
async fn wiki_read_marks_pending_review_drafts_as_not_reviewed() {
    let (server, _home) = seed_wiki_project_entries(vec![pending_review_draft_entry()]);

    let markdown = handle_wiki_read(&server, "/wiki/drafts/lifecycle-gate-draft", "wiki")
        .expect("read should succeed even for a draft path");
    assert!(
        markdown.contains("pending_review") && markdown.contains("not reviewed"),
        "draft read must be visibly labeled unreviewed: {markdown}"
    );
}

/// Canon doc §7.1: "readers prefer typed refs and fall back to strings."
/// An entry written before this leaf's dual-write (legacy `source_refs`
/// only, no `evidence_refs_v1`) must still resolve a populated `references`
/// list on read.
#[tokio::test]
async fn wiki_read_falls_back_to_legacy_source_refs_when_no_typed_refs_present() {
    let mut entry = active_wiki_entry();
    entry.path = "/wiki/engineering/lifecycle/legacy-refs-only".to_string();
    entry.metadata = json!({"source_refs": ["kckylechen1/tachi#1072"]});
    let (server, _home) = seed_wiki_project_entries(vec![entry]);

    let value = collect_wiki_read_value(
        &server,
        "/wiki/engineering/lifecycle/legacy-refs-only",
        "wiki",
    )
    .expect("read should succeed");
    assert_eq!(value["status"], json!("found"));
    assert_eq!(
        value["entry"]["references"],
        json!(["kckylechen1/tachi#1072"])
    );
}

#[tokio::test]
async fn wiki_read_falls_back_when_typed_refs_have_no_usable_targets() {
    let mut entry = active_wiki_entry();
    entry.path = "/wiki/engineering/lifecycle/invalid-typed-refs".to_string();
    entry.metadata = json!({
        "evidence_refs_v1": [{"ref": "  "}, {"ref": 42}],
        "source_refs": [null, "", "kckylechen1/tachi#1072"]
    });
    let (server, _home) = seed_wiki_project_entries(vec![entry]);

    let value = collect_wiki_read_value(
        &server,
        "/wiki/engineering/lifecycle/invalid-typed-refs",
        "wiki",
    )
    .expect("read should succeed");
    assert_eq!(
        value["entry"]["references"],
        json!(["kckylechen1/tachi#1072"])
    );
}

/// Cross-vendor review (#1215 BUG 4): "The gate itself fails open: missing
/// IDs or lookup failures convert to 'serve everything'
/// (wiki_ops/provenance.rs:47-60, .unwrap_or_default())." Exercises
/// `apply_wiki_lifecycle_gate` directly with a row whose `id` cannot be
/// resolved against the store (simulating a >5000-candidate truncation miss
/// or a stale/foreign id) — this used to be kept unfiltered ("leave it
/// alone") under EVERY scope; it must now be excluded from the default
/// (active-only) scope and any explicit named scope, and kept ONLY under
/// the explicit `"all"` opt-out.
#[tokio::test]
async fn apply_wiki_lifecycle_gate_fails_closed_on_unresolved_row() {
    let (server, _home) = seed_wiki_project_entries(vec![active_wiki_entry()]);
    let unresolved_row = || {
        json!({
            "id": "wiki-does-not-exist-in-store",
            "path": "/wiki/engineering/lifecycle/unresolved",
        })
    };

    let mut default_scope_rows = vec![unresolved_row()];
    crate::wiki_ops::apply_wiki_lifecycle_gate(
        &server,
        Some("wiki"),
        &mut default_scope_rows,
        None,
    )
    .expect("gate should not error");
    assert!(
        default_scope_rows.is_empty(),
        "RED: an unresolved row must not be served under the default (active-only) scope: {default_scope_rows:?}"
    );

    let mut named_scope_rows = vec![unresolved_row()];
    crate::wiki_ops::apply_wiki_lifecycle_gate(
        &server,
        Some("wiki"),
        &mut named_scope_rows,
        Some("pending_review"),
    )
    .expect("gate should not error");
    assert!(
        named_scope_rows.is_empty(),
        "an unresolved row must not be served under an explicit named scope either: {named_scope_rows:?}"
    );

    let mut all_scope_rows = vec![unresolved_row()];
    crate::wiki_ops::apply_wiki_lifecycle_gate(
        &server,
        Some("wiki"),
        &mut all_scope_rows,
        Some("all"),
    )
    .expect("gate should not error");
    assert_eq!(
        all_scope_rows.len(),
        1,
        "the explicit 'all' opt-out is the one scope that still serves an unresolved row"
    );
}
