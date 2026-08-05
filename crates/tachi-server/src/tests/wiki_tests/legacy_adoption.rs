//! End-to-end discriminator for `tachi wiki corpus --adopt-legacy`
//! (tachi#1624).
//!
//! Every unit test around the adoption command can pass while the thing the
//! command exists for stays broken, because the receipt describes the write
//! and says nothing about whether a reader can see the result. These two tests
//! are the ones that close that gap: they drive a real `MemoryServer` over a
//! `--no-project-db` topology and assert that a federated Wiki search flips
//! from the tachi#1624 zero-store refusal to a real hit *because of* the
//! adoption, and stays refused without the confirmation token.

use super::*;

use crate::bootstrap::wiki_corpus::{
    run_wiki_corpus_legacy_adoption_command, WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN,
};
use crate::wiki_ops::collect_wiki_search_value;

const NEEDLE: &str = "AdoptionDiscriminatorNeedle";
const NEEDLE_ID: &str = "wiki-adoption-needle";

fn federated_wiki_search(query: &str) -> WikiSearchParams {
    WikiSearchParams {
        query: query.to_string(),
        path_prefix: Some("/wiki".to_string()),
        category: None,
        top_k: 10,
        include_archived: false,
        agent_role: None,
        // Omitted on purpose: an omitted project is what selects
        // `WikiReadPlan::Federated`, which is the plan the host's daemon uses.
        project: None,
        domain: None,
        file_context: None,
        error_context: None,
        weights: None,
        lifecycle: None,
    }
}

/// Seed the needle straight into the server's legacy global store, which is
/// the store `Federated` deliberately never reads.
fn seed_legacy_needle(global_db: &std::path::Path) {
    let mut entry = make_entry(NEEDLE_ID);
    entry.path = "/wiki/adoption/needle".to_string();
    entry.summary = format!("{NEEDLE} adoption discriminator summary");
    entry.text = format!("{NEEDLE} is the one token this discriminator searches for");
    entry.metadata = json!({"lifecycle": "active"});
    // `make_entry` defaults to `source: "test"`, which the corpus classifier
    // reads as explicit test-fixture evidence and excludes from adoption. This
    // row has to look like ordinary wiki content, because that is the class
    // adoption is for.
    entry.source = "wiki".to_string();
    entry.category = "wiki".to_string();
    entry.domain = Some("wiki".to_string());
    let mut store = MemoryStore::open(global_db.to_str().expect("utf8 global db"))
        .expect("open the legacy global store");
    store.upsert(&entry).expect("seed the legacy global needle");
    drop(store);
}

/// **The** discriminator. Before adoption a federated Wiki search on a
/// `--no-project-db` host resolves zero stores and refuses; after adoption the
/// same query on the same home returns the adopted row.
#[tokio::test]
async fn bootstrapped_and_adopted_store_makes_federated_wiki_search_return_results() {
    let (server, _home) = crate::tests::make_server_with_temp_home();
    let global_db = server.global_db_path_buf();
    let app_home = server.tachi_home_dir();
    assert!(
        server.project_db_path_buf().is_none(),
        "the discriminator only means anything on the host's --no-project-db topology"
    );
    let target = app_home.join("projects").join("wiki");
    assert!(
        std::fs::symlink_metadata(&target).is_err(),
        "the named wiki store must not exist before adoption"
    );

    seed_legacy_needle(&global_db);

    // (a) Before: the plan resolves no stores at all, so the search refuses
    // rather than reporting a clean empty result (tachi#1624).
    let before = collect_wiki_search_value(&server, federated_wiki_search(NEEDLE))
        .await
        .expect_err("a federated wiki search with no reachable store must refuse");
    assert!(
        before.contains("resolved zero stores"),
        "unexpected pre-adoption refusal: {before}"
    );
    assert!(
        before.contains("named project 'wiki' not found"),
        "the refusal must name the leg adoption is about to satisfy: {before}"
    );

    // Release the server's handle on the legacy global before the adoption
    // command takes its own immutable snapshot of it.
    drop(server);

    // (b) Adopt.
    let report = run_wiki_corpus_legacy_adoption_command(
        false,
        Some(WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN.to_string()),
        None,
        None,
        &global_db,
        None,
        &app_home,
    )
    .expect("confirmed adoption");
    assert!(
        !report.legacy_adoption_had_failures(),
        "adoption must complete cleanly: {:?}",
        serde_json::to_value(&report).unwrap()["legacy_adoption"]["errors"]
    );
    let adoption = serde_json::to_value(&report).unwrap()["legacy_adoption"].clone();
    assert!(
        adoption["rows_imported"].as_u64().unwrap_or_default() >= 1,
        "adoption imported nothing: {adoption}"
    );
    assert!(
        adoption["adopted_ids"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|id| id == NEEDLE_ID)),
        "the needle must be in the adopted set: {adoption}"
    );
    assert_eq!(adoption["checksums_match"], json!(true));
    assert_eq!(adoption["target_store_role_stamped"], json!(true));
    assert_eq!(
        adoption["legacy_row_digest_after"], adoption["legacy_row_digest_before"],
        "adoption must not mutate the legacy global"
    );
    assert!(
        adoption["default_retrievable_rows"]
            .as_u64()
            .unwrap_or_default()
            >= 1,
        "a green receipt that predicts no retrievable row is not a working adoption: {adoption}"
    );
    assert!(
        std::fs::symlink_metadata(target.join(memcore::MEMORY_DB_FILENAME)).is_ok(),
        "adoption must have created the named wiki store"
    );

    // (c) A fresh server over the same home now resolves the named store.
    let server = MemoryServer::new(global_db.clone(), None).expect("rebuild the server");

    // (d) After: the same query, the same plan, a real result.
    let after = collect_wiki_search_value(&server, federated_wiki_search(NEEDLE))
        .await
        .expect("federated wiki search must succeed once the shared store exists");
    assert!(
        after["stores"].as_array().is_some_and(|stores| stores
            .iter()
            .any(|store| store["kind"] == "named_project" && store["project"] == "wiki")),
        "the federated plan must now include the named wiki store: {after}"
    );
    let results = after["results"].as_array().cloned().unwrap_or_default();
    assert!(
        results.iter().any(|row| row["id"] == NEEDLE_ID),
        "the adopted needle must be retrievable through federated wiki search: {after}"
    );
    assert!(
        after["count"].as_u64().unwrap_or_default() >= 1,
        "count must agree with the results it returned: {after}"
    );
}

/// The refusal half: without the exact token nothing is created and the search
/// stays exactly as broken as it was. A gate that only exists in a unit test
/// is not a gate on this path.
#[tokio::test]
async fn federated_wiki_search_stays_empty_without_the_confirm_token() {
    let (server, _home) = crate::tests::make_server_with_temp_home();
    let global_db = server.global_db_path_buf();
    let app_home = server.tachi_home_dir();
    let target = app_home.join("projects").join("wiki");

    seed_legacy_needle(&global_db);
    drop(server);

    // No token at all: a preview that creates nothing.
    let preview = run_wiki_corpus_legacy_adoption_command(
        false, None, None, None, &global_db, None, &app_home,
    )
    .expect("preview must succeed");
    let preview_value = serde_json::to_value(&preview).unwrap();
    assert_eq!(preview_value["mode"], json!("legacy_adoption_preview"));
    assert_eq!(
        preview_value["legacy_adoption"]["target_created"],
        json!(false)
    );
    assert!(
        std::fs::symlink_metadata(&target).is_err(),
        "a preview must not create the named wiki store"
    );

    let server = MemoryServer::new(global_db.clone(), None).expect("rebuild the server");
    let error = collect_wiki_search_value(&server, federated_wiki_search(NEEDLE))
        .await
        .expect_err("a preview must not change what search can reach");
    assert!(error.contains("resolved zero stores"), "{error}");
    drop(server);

    // The wrong token: a hard refusal, still nothing created.
    let error = run_wiki_corpus_legacy_adoption_command(
        false,
        Some("MIGRATE_WIKI_CORPUS_V1".to_string()),
        None,
        None,
        &global_db,
        None,
        &app_home,
    )
    .expect_err("the migration token must not confirm an adoption");
    assert!(
        error.contains(WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN),
        "{error}"
    );
    assert!(
        std::fs::symlink_metadata(&target).is_err(),
        "a refused adoption must not create the named wiki store"
    );

    let server = MemoryServer::new(global_db, None).expect("rebuild the server");
    let error = collect_wiki_search_value(&server, federated_wiki_search(NEEDLE))
        .await
        .expect_err("a refused adoption must not change what search can reach");
    assert!(error.contains("resolved zero stores"), "{error}");
}
