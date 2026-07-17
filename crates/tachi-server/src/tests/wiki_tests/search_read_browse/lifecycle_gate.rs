use super::*;

use crate::wiki_ops::{collect_wiki_read_value, collect_wiki_search_value, handle_wiki_read};

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
