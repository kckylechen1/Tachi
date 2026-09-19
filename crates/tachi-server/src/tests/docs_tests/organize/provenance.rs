use super::*;
use axum::{response::IntoResponse, routing::post, Json, Router};
use blake2::{Blake2s256, Digest};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tachi_llm::{
    ChatLaneConfig, LlmClient, ProviderRuntimeConfig, ProviderSecret, RerankConfig,
    RerankProviderKind,
};

const PROVENANCE_PREFIX: &str = "tachi_model_invocation_v1: ";

#[derive(Clone)]
enum MockFinishReason {
    Missing,
    Null,
    Named(String),
}

impl MockFinishReason {
    fn named(value: &str) -> Self {
        Self::Named(value.to_string())
    }
}

struct MockDocsClassifier {
    llm: LlmClient,
    requests: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockDocsClassifier {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockDocsClassifier {
    async fn start(content: &str, finish_reason: MockFinishReason) -> Self {
        let content = content.to_string();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured_requests = Arc::clone(&requests);
        let app = Router::new().route(
            "/chat/completions",
            post(move |Json(request): Json<Value>| {
                let content = content.clone();
                let finish_reason = finish_reason.clone();
                captured_requests
                    .lock()
                    .expect("capture docs classification request")
                    .push(request);
                async move {
                    let mut choice = json!({
                        "message": {"role": "assistant", "content": content},
                    });
                    let choice_object = choice.as_object_mut().expect("choice object");
                    match finish_reason {
                        MockFinishReason::Missing => {}
                        MockFinishReason::Null => {
                            choice_object.insert("finish_reason".to_string(), Value::Null);
                        }
                        MockFinishReason::Named(reason) => {
                            choice_object
                                .insert("finish_reason".to_string(), Value::String(reason));
                        }
                    }
                    Json(json!({
                        "choices": [choice],
                        "usage": {"prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18},
                        "model": "mock-docs-classifier-v1",
                    }))
                    .into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock docs classifier");
        let port = listener
            .local_addr()
            .expect("mock classifier address")
            .port();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve mock docs classifier");
        });
        let endpoint = format!("http://127.0.0.1:{port}/chat/completions");
        let lane = || ChatLaneConfig {
            base_url: endpoint.clone(),
            model: "mock-docs-classifier-v1".to_string(),
            api_key_envs: vec!["DOCS_CLASSIFIER_TEST_API_KEY"],
        };
        let llm = LlmClient::new_with_config(
            ProviderRuntimeConfig {
                extract: lane(),
                summary: lane(),
                reasoning: lane(),
                distill: lane(),
                rerank: RerankConfig {
                    provider: RerankProviderKind::Voyage,
                    local_endpoint: None,
                },
            },
            None,
        )
        .expect("initialize mock docs classifier");
        assert!(llm.set_provider_secret_pool(
            "DOCS_CLASSIFIER_TEST_API_KEY",
            vec![ProviderSecret {
                key_id: "docs-classifier-test-key".to_string(),
                value: "docs-classifier-secret".to_string(),
            }],
        ));
        Self {
            llm,
            requests,
            task,
        }
    }

    fn request_count(&self) -> usize {
        self.requests
            .lock()
            .expect("read docs classification requests")
            .len()
    }
}

fn install_classifier(server: &mut crate::tests::TestServer, classifier: &MockDocsClassifier) {
    server.replace_llm(classifier.llm.clone());
}

fn model_response(category_path: &str, title: &str, summary: &str) -> String {
    json!({
        "category_path": category_path,
        "title": title,
        "summary": summary,
    })
    .to_string()
}

fn receipt_from_document(content: &str) -> Option<Value> {
    content.lines().find_map(|line| {
        line.strip_prefix(PROVENANCE_PREFIX)
            .map(|json| serde_json::from_str(json).expect("valid receipt JSON"))
    })
}

fn canonical_payload(category: &str, title: &str, summary: &str) -> String {
    format!(
        "{{\"category\":{},\"title\":{},\"summary\":{}}}",
        serde_json::to_string(category).unwrap(),
        serde_json::to_string(title).unwrap(),
        serde_json::to_string(summary).unwrap(),
    )
}

fn independent_content_hash(content: &str) -> String {
    let mut hasher = Blake2s256::new();
    hasher.update(content.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn binding_matches(receipt: &Value, payload: &str, object_id: &str, revision: i64) -> bool {
    receipt["content_hash"].as_str() == Some(independent_content_hash(payload).as_str())
        && receipt["memory_id"].as_str() == Some(object_id)
        && receipt["revision"].as_i64() == Some(revision)
}

fn valid_bound_receipt(
    category: &str,
    title: &str,
    summary: &str,
    object_id: &str,
    revision: i64,
) -> Value {
    json!({
        "schema": "model-invocation-v1",
        "lane": "extract",
        "engine_kind": "provider_http",
        "effective_provider": "mock-provider",
        "effective_model": "mock-docs-classifier-v1",
        "effective_version": null,
        "fallback_chain": [],
        "degraded": false,
        "completion_status": "complete",
        "prompt_tokens": 11,
        "completion_tokens": 7,
        "total_tokens": 18,
        "latency_ms": 1,
        "content_hash": independent_content_hash(&canonical_payload(category, title, summary)),
        "memory_id": object_id,
        "revision": revision,
    })
}

fn document_with_receipt(
    title: &str,
    summary: &str,
    category: &str,
    organize: bool,
    receipt: &Value,
    body: &str,
) -> String {
    format!(
        "---\ntitle: {}\nsummary: {}\ncategory: {}\norganize: {organize}\n{PROVENANCE_PREFIX}{}\n---\n{body}",
        serde_json::to_string(title).unwrap(),
        serde_json::to_string(summary).unwrap(),
        serde_json::to_string(category).unwrap(),
        receipt
    )
}

fn assert_model_document(
    content: &str,
    title: &str,
    summary: &str,
    category: &str,
    object_id: &str,
    revision: i64,
) {
    assert!(
        content.contains(&format!("title: \"{title}\"")),
        "{content}"
    );
    assert!(
        content.contains(&format!("summary: \"{summary}\"")),
        "{content}"
    );
    assert!(
        content.contains(&format!("category: \"{category}\"")),
        "{content}"
    );
    let receipt = receipt_from_document(content).expect("model document must carry receipt");
    assert_eq!(receipt["schema"], "model-invocation-v1");
    assert_eq!(receipt["lane"], "extract");
    assert_eq!(receipt["effective_model"], "mock-docs-classifier-v1");
    assert_eq!(receipt["completion_status"], "complete");
    let payload = canonical_payload(category, title, summary);
    assert_eq!(
        receipt["content_hash"],
        independent_content_hash(&payload),
        "receipt must bind the canonical category/title/summary payload"
    );
    assert_eq!(receipt["memory_id"], object_id);
    assert_eq!(receipt["revision"], revision);
    assert!(binding_matches(&receipt, &payload, object_id, revision));
    assert!(!binding_matches(
        &receipt,
        &canonical_payload(category, title, "drifted summary"),
        object_id,
        revision
    ));
    assert!(!binding_matches(
        &receipt,
        &payload,
        "docs/drifted-path.md",
        revision
    ));
    assert!(!binding_matches(
        &receipt,
        &payload,
        object_id,
        revision + 1
    ));
    let serialized = receipt.to_string();
    for forbidden in [
        "docs-classifier-secret",
        "docs-classifier-test-key",
        "DOCS_CLASSIFIER_TEST_API_KEY",
        "Source Path:",
        "Durable body sentinel",
    ] {
        assert!(
            !serialized.contains(forbidden),
            "receipt leaked forbidden model-call material: {serialized}"
        );
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn model_classified_moved_and_in_place_documents_persist_matching_receipts() {
    let classifier = MockDocsClassifier::start(
        &model_response(
            "docs/engineering/devops",
            "Model Deployment Guide",
            "Deployment steps selected by the model",
        ),
        MockFinishReason::named("stop"),
    )
    .await;
    let mut server = make_server();
    install_classifier(&mut server, &classifier);
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path();
    let moved_source = docs.join("scattered.md");
    fs::write(
        &moved_source,
        "# Old heading\nDurable body sentinel moved.\n",
    )
    .unwrap();
    let in_place = docs.join("engineering/devops/in-place.md");
    fs::create_dir_all(in_place.parent().unwrap()).unwrap();
    fs::write(
        &in_place,
        "---\ntitle: \"Stale title\"\nsummary: \"Stale summary\"\norganize: true\n---\nDurable body sentinel in place.\n",
    )
    .unwrap();
    let _model_mode = crate::docs_ops::enable_model_classification_for_test();

    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect("organize model-classified documents");

    assert!(!moved_source.exists());
    let moved = fs::read_to_string(docs.join("engineering/devops/scattered.md")).unwrap();
    let updated = fs::read_to_string(&in_place).unwrap();
    assert_model_document(
        &moved,
        "Model Deployment Guide",
        "Deployment steps selected by the model",
        "engineering/devops",
        "docs/engineering/devops/scattered.md",
        1,
    );
    assert_model_document(
        &updated,
        "Model Deployment Guide",
        "Deployment steps selected by the model",
        "engineering/devops",
        "docs/engineering/devops/in-place.md",
        1,
    );
    assert_eq!(classifier.request_count(), 2);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn heuristic_reclassification_replaces_fields_and_removes_stale_model_provenance() {
    let server = make_server();
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path();
    let source = docs.join("debug-fix.md");
    let stale_receipt = valid_bound_receipt(
        "legacy",
        "Stale model title",
        "Stale model summary",
        "docs/debug-fix.md",
        4,
    );
    fs::write(
        &source,
        document_with_receipt(
            "Stale model title",
            "Stale model summary",
            "legacy",
            true,
            &stale_receipt,
            "# Heuristic body\n",
        ),
    )
    .unwrap();

    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect("organize with deterministic fallback");

    let destination = docs.join("engineering/debugging/debug-fix.md");
    let content = fs::read_to_string(destination).unwrap();
    assert!(content.contains("title: \"debug fix\""), "{content}");
    assert!(content.contains("summary: \"Heuristic body\""), "{content}");
    assert!(receipt_from_document(&content).is_none(), "{content}");
    assert!(!content.contains("mock-docs-classifier-v1"), "{content}");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn invalid_truncated_and_unsafe_model_outputs_never_create_clean_provenance() {
    for (label, response, finish_reason) in [
        (
            "invalid-json",
            "not-json".to_string(),
            MockFinishReason::named("stop"),
        ),
        (
            "unsafe-category",
            model_response(
                "docs/../../outside",
                "Unsafe model title",
                "Unsafe model summary",
            ),
            MockFinishReason::named("stop"),
        ),
        (
            "truncated",
            model_response(
                "docs/product/unsafe",
                "Truncated model title",
                "Truncated model summary",
            ),
            MockFinishReason::named("length"),
        ),
        (
            "missing-finish-reason",
            model_response(
                "docs/product/unsafe",
                "Missing model title",
                "Missing model summary",
            ),
            MockFinishReason::Missing,
        ),
        (
            "null-finish-reason",
            model_response(
                "docs/product/unsafe",
                "Null model title",
                "Null model summary",
            ),
            MockFinishReason::Null,
        ),
        (
            "content-filter-finish-reason",
            model_response(
                "docs/product/unsafe",
                "Filtered model title",
                "Filtered model summary",
            ),
            MockFinishReason::named("content_filter"),
        ),
        (
            "unknown-finish-reason",
            model_response(
                "docs/product/unsafe",
                "Unknown model title",
                "Unknown model summary",
            ),
            MockFinishReason::named("provider_specific_reason"),
        ),
    ] {
        let classifier = MockDocsClassifier::start(&response, finish_reason).await;
        let mut server = make_server();
        install_classifier(&mut server, &classifier);
        let workspace = DocsWorktree::new();
        let docs = workspace.docs_path();
        let source = docs.join(format!("debug-fix-{label}.md"));
        fs::write(&source, "# Deterministic fallback body\n").unwrap();
        let _model_mode = crate::docs_ops::enable_model_classification_for_test();

        crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
            .await
            .expect("invalid model output follows existing fallback policy");

        let destination = docs
            .join("engineering/debugging")
            .join(format!("debug-fix-{label}.md"));
        let content = fs::read_to_string(destination).unwrap();
        assert!(
            receipt_from_document(&content).is_none(),
            "{label}: {content}"
        );
        assert!(!content.contains("model title"), "{label}: {content}");
        assert_eq!(classifier.request_count(), 1, "{label}");
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn model_classification_dry_run_leaves_bytes_and_paths_unchanged() {
    let classifier = MockDocsClassifier::start(
        &model_response(
            "docs/product/dry-run",
            "Dry Run Model Title",
            "Dry run model summary",
        ),
        MockFinishReason::named("stop"),
    )
    .await;
    let mut server = make_server();
    install_classifier(&mut server, &classifier);
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path();
    let source = docs.join("dry-run-source.md");
    let original = "# Original bytes\nDurable body sentinel.\n";
    fs::write(&source, original).unwrap();
    let _model_mode = crate::docs_ops::enable_model_classification_for_test();

    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), true)
        .await
        .expect("preview model classification");

    assert_eq!(fs::read_to_string(&source).unwrap(), original);
    assert!(!docs.join("product/dry-run/dry-run-source.md").exists());
    assert!(!docs.join("_index.md").exists());
    assert_eq!(classifier.request_count(), 1);
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn model_move_write_failure_preserves_source_and_archived_destination() {
    let classifier = MockDocsClassifier::start(
        &model_response(
            "docs/product/acme",
            "Atomic Model Title",
            "Atomic model summary",
        ),
        MockFinishReason::named("stop"),
    )
    .await;
    let mut server = make_server();
    install_classifier(&mut server, &classifier);
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path().canonicalize().unwrap();
    let source = docs.join("conflict.md");
    let source_bytes = "# New source\nDurable body sentinel.\n";
    fs::write(&source, source_bytes).unwrap();
    let destination = docs.join("product/acme/conflict.md");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(&destination, "old destination bytes").unwrap();
    fs::File::open(&destination)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
        .unwrap();
    let _model_mode = crate::docs_ops::enable_model_classification_for_test();
    let _hook = crate::docs_ops::set_new_file_test_hook(
        crate::docs_ops::OrganizeTestPoint::NewFileWriteFailure,
        destination.parent().unwrap().to_path_buf(),
        Box::new(|| {}),
    );

    let error = crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect_err("destination write failure must stop before source deletion");

    assert!(error.contains("injected partial write failure"), "{error}");
    assert_eq!(fs::read_to_string(&source).unwrap(), source_bytes);
    assert!(!destination.exists());
    assert_eq!(
        fs::read_to_string(docs.join("archive/conflict.md")).unwrap(),
        "old destination bytes"
    );
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn model_in_place_rename_failure_preserves_original_complete_document() {
    let classifier = MockDocsClassifier::start(
        &model_response(
            "docs/engineering/devops",
            "Replacement Model Title",
            "Replacement model summary",
        ),
        MockFinishReason::named("stop"),
    )
    .await;
    let mut server = make_server();
    install_classifier(&mut server, &classifier);
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path().canonicalize().unwrap();
    let source = docs.join("engineering/devops/in-place.md");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    let original = "---\ntitle: \"Original title\"\norganize: true\n---\nOriginal body bytes.\n";
    fs::write(&source, original).unwrap();
    let _model_mode = crate::docs_ops::enable_model_classification_for_test();
    crate::docs_ops::set_organize_test_hook(
        crate::docs_ops::OrganizeTestPoint::ExistingFileRenameFailure,
        source.clone(),
        Box::new(|| {}),
    );

    let error = crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect_err("in-place rename failure must preserve old bytes");

    assert!(error.contains("injected rename failure"), "{error}");
    assert_eq!(fs::read_to_string(&source).unwrap(), original);
    assert!(!fs::read_dir(source.parent().unwrap())
        .unwrap()
        .any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("tachi-organize")));
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn archive_rename_failure_preserves_both_source_and_destination() {
    let classifier = MockDocsClassifier::start(
        &model_response(
            "docs/product/acme",
            "Archive Model Title",
            "Archive model summary",
        ),
        MockFinishReason::named("stop"),
    )
    .await;
    let mut server = make_server();
    install_classifier(&mut server, &classifier);
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path().canonicalize().unwrap();
    let source = docs.join("archive-note.md");
    fs::write(&source, "new source bytes").unwrap();
    let destination = docs.join("product/acme/archive-note.md");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(&destination, "old destination bytes").unwrap();
    fs::File::open(&destination)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
        .unwrap();
    let archive = docs.join("archive/archive-note.md");
    let _model_mode = crate::docs_ops::enable_model_classification_for_test();
    crate::docs_ops::set_organize_test_hook(
        crate::docs_ops::OrganizeTestPoint::RenameFileFailure,
        archive.clone(),
        Box::new(|| {}),
    );

    let error = crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect_err("archive rename failure must preserve both live files");

    assert!(error.contains("injected archive failure"), "{error}");
    assert_eq!(fs::read_to_string(&source).unwrap(), "new source bytes");
    assert_eq!(
        fs::read_to_string(&destination).unwrap(),
        "old destination bytes"
    );
    assert!(!archive.exists());
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn source_removal_failure_occurs_after_complete_receipted_destination_is_durable() {
    let classifier = MockDocsClassifier::start(
        &model_response(
            "docs/product/acme",
            "Durable Model Title",
            "Durable model summary",
        ),
        MockFinishReason::named("stop"),
    )
    .await;
    let mut server = make_server();
    install_classifier(&mut server, &classifier);
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path().canonicalize().unwrap();
    let source = docs.join("remove-failure.md");
    let original = "# Original source\nDurable body sentinel.\n";
    fs::write(&source, original).unwrap();
    let destination = docs.join("product/acme/remove-failure.md");
    let _model_mode = crate::docs_ops::enable_model_classification_for_test();
    crate::docs_ops::set_organize_test_hook(
        crate::docs_ops::OrganizeTestPoint::SourceRemovalFailure,
        source.clone(),
        Box::new(|| {}),
    );

    let error = crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect_err("source removal fault must be explicit");

    assert!(error.contains("injected removal failure"), "{error}");
    assert_eq!(fs::read_to_string(&source).unwrap(), original);
    let published = fs::read_to_string(destination).unwrap();
    assert!(published.contains("Durable body sentinel."), "{published}");
    assert_model_document(
        &published,
        "Durable Model Title",
        "Durable model summary",
        "product/acme",
        "docs/product/acme/remove-failure.md",
        1,
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn replacing_a_bound_destination_advances_its_committed_revision() {
    let classifier = MockDocsClassifier::start(
        &model_response(
            "docs/product/acme",
            "Versioned Model Title",
            "Versioned model summary",
        ),
        MockFinishReason::named("stop"),
    )
    .await;
    let mut server = make_server();
    install_classifier(&mut server, &classifier);
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path().canonicalize().unwrap();
    let source = docs.join("a.md");
    let destination = docs.join("product/acme/a.md");
    let _model_mode = crate::docs_ops::enable_model_classification_for_test();

    fs::write(&source, "first model-derived body\n").unwrap();
    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect("publish first bound revision");
    assert_model_document(
        &fs::read_to_string(&destination).unwrap(),
        "Versioned Model Title",
        "Versioned model summary",
        "product/acme",
        "docs/product/acme/a.md",
        1,
    );

    fs::write(&source, "second model-derived body\n").unwrap();
    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect("replace bound destination");
    let replaced = fs::read_to_string(&destination).unwrap();
    assert!(replaced.contains("second model-derived body"), "{replaced}");
    assert_model_document(
        &replaced,
        "Versioned Model Title",
        "Versioned model summary",
        "product/acme",
        "docs/product/acme/a.md",
        2,
    );
    assert_eq!(classifier.request_count(), 2);
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn older_model_source_is_archived_with_receipt_bound_to_archive_path() {
    let classifier = MockDocsClassifier::start(
        &model_response(
            "docs/product/acme",
            "Older Model Title",
            "Older model summary",
        ),
        MockFinishReason::named("stop"),
    )
    .await;
    let mut server = make_server();
    install_classifier(&mut server, &classifier);
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path().canonicalize().unwrap();
    let source = docs.join("older.md");
    fs::write(&source, "older model source body\n").unwrap();
    fs::File::open(&source)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
        .unwrap();
    let destination = docs.join("product/acme/older.md");
    let destination_bytes = "---\ntitle: \"Newer destination\"\ncategory: \"product/acme\"\norganize: false\n---\nnewer destination body\n";
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(&destination, destination_bytes).unwrap();
    let _model_mode = crate::docs_ops::enable_model_classification_for_test();

    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect("archive older classified source");

    assert!(!source.exists());
    assert_eq!(fs::read_to_string(&destination).unwrap(), destination_bytes);
    let archived = fs::read_to_string(docs.join("archive/older.md")).unwrap();
    assert!(archived.contains("older model source body"), "{archived}");
    assert_model_document(
        &archived,
        "Older Model Title",
        "Older model summary",
        "product/acme",
        "docs/archive/older.md",
        1,
    );
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn older_source_archive_write_failure_preserves_source_and_newer_destination() {
    let classifier = MockDocsClassifier::start(
        &model_response(
            "docs/product/acme",
            "Older Failure Title",
            "Older failure summary",
        ),
        MockFinishReason::named("stop"),
    )
    .await;
    let mut server = make_server();
    install_classifier(&mut server, &classifier);
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path().canonicalize().unwrap();
    let source = docs.join("older-failure.md");
    let source_bytes = "older source survives\n";
    fs::write(&source, source_bytes).unwrap();
    fs::File::open(&source)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
        .unwrap();
    let destination = docs.join("product/acme/older-failure.md");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(&destination, "newer destination survives\n").unwrap();
    let _model_mode = crate::docs_ops::enable_model_classification_for_test();
    let _hook = crate::docs_ops::set_new_file_test_hook(
        crate::docs_ops::OrganizeTestPoint::NewFileWriteFailure,
        docs.join("archive"),
        Box::new(|| {}),
    );

    let error = crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect_err("archive write fault must retain both recoverable inputs");

    assert!(error.contains("injected partial write failure"), "{error}");
    assert_eq!(fs::read_to_string(&source).unwrap(), source_bytes);
    assert_eq!(
        fs::read_to_string(&destination).unwrap(),
        "newer destination survives\n"
    );
    assert!(!docs.join("archive/older-failure.md").exists());
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn in_place_partial_write_failure_preserves_original_complete_document() {
    let classifier = MockDocsClassifier::start(
        &model_response(
            "docs/engineering/devops",
            "Partial Write Title",
            "Partial write summary",
        ),
        MockFinishReason::named("stop"),
    )
    .await;
    let mut server = make_server();
    install_classifier(&mut server, &classifier);
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path().canonicalize().unwrap();
    let source = docs.join("engineering/devops/partial.md");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    let original = "---\ntitle: \"Original title\"\norganize: true\n---\nOriginal complete body.\n";
    fs::write(&source, original).unwrap();
    let _model_mode = crate::docs_ops::enable_model_classification_for_test();
    crate::docs_ops::set_organize_test_hook(
        crate::docs_ops::OrganizeTestPoint::ExistingFileWriteFailure,
        source.clone(),
        Box::new(|| {}),
    );

    let error = crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect_err("partial in-place write must not replace original bytes");

    assert!(error.contains("injected partial write failure"), "{error}");
    assert_eq!(fs::read_to_string(&source).unwrap(), original);
    assert!(!fs::read_dir(source.parent().unwrap())
        .unwrap()
        .any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("tachi-organize")));
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn mkdir_and_created_parent_sync_failures_never_delete_source() {
    for (label, point, expected_error) in [
        (
            "mkdir",
            crate::docs_ops::OrganizeTestPoint::DirectoryCreateFailure,
            "injected mkdir failure",
        ),
        (
            "parent-sync",
            crate::docs_ops::OrganizeTestPoint::DirectorySyncFailure,
            "injected directory sync failure",
        ),
    ] {
        let classifier = MockDocsClassifier::start(
            &model_response(
                "docs/product/newspace",
                "Directory Failure Title",
                "Directory failure summary",
            ),
            MockFinishReason::named("stop"),
        )
        .await;
        let mut server = make_server();
        install_classifier(&mut server, &classifier);
        let workspace = DocsWorktree::new();
        let docs = workspace.docs_path().canonicalize().unwrap();
        let source = docs.join(format!("directory-{label}.md"));
        let original = format!("directory failure body {label}\n");
        fs::write(&source, &original).unwrap();
        let _model_mode = crate::docs_ops::enable_model_classification_for_test();
        let hook_path = if label == "parent-sync" {
            docs.join("product")
        } else {
            docs.join("product/newspace")
        };
        crate::docs_ops::set_organize_test_hook(
            point,
            hook_path,
            Box::new(|| {}),
        );

        let error = crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
            .await
            .expect_err("directory durability fault must stop publication");

        assert!(error.contains(expected_error), "{label}: {error}");
        assert_eq!(fs::read_to_string(&source).unwrap(), original);
        assert!(!docs
            .join("product/newspace")
            .join(format!("directory-{label}.md"))
            .exists());
    }
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn retry_reproves_parent_sync_after_directory_creation_sync_failure() {
    let classifier = MockDocsClassifier::start(
        &model_response(
            "docs/product/retry-space",
            "Retry Directory Title",
            "Retry directory summary",
        ),
        MockFinishReason::named("stop"),
    )
    .await;
    let mut server = make_server();
    install_classifier(&mut server, &classifier);
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path().canonicalize().unwrap();
    let source = docs.join("retry-directory.md");
    fs::write(&source, "retry body\n").unwrap();
    let destination = docs.join("product/retry-space/retry-directory.md");
    let _model_mode = crate::docs_ops::enable_model_classification_for_test();
    crate::docs_ops::set_organize_test_hook(
        crate::docs_ops::OrganizeTestPoint::DirectorySyncFailure,
        docs.join("product"),
        Box::new(|| {}),
    );

    let error = crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect_err("first parent sync must fail after mkdir");
    assert!(error.contains("injected directory sync failure"), "{error}");
    assert!(docs.join("product/retry-space").is_dir());
    assert!(source.exists());
    assert!(!destination.exists());

    let destination_sync_observed = Arc::new(Mutex::new(false));
    let observed = Arc::clone(&destination_sync_observed);
    crate::docs_ops::set_organize_test_hook(
        crate::docs_ops::OrganizeTestPoint::DirectorySyncCompleted,
        docs.join("product/retry-space"),
        Box::new(move || {
            *observed.lock().expect("record completed directory sync") = true;
        }),
    );
    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect("retry must re-prove the existing directory chain");

    assert!(*destination_sync_observed.lock().unwrap());
    assert!(!source.exists());
    assert_model_document(
        &fs::read_to_string(destination).unwrap(),
        "Retry Directory Title",
        "Retry directory summary",
        "product/retry-space",
        "docs/product/retry-space/retry-directory.md",
        1,
    );
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn post_rename_destination_sync_failure_stops_before_source_parent_sync() {
    let classifier = MockDocsClassifier::start(
        &model_response(
            "docs/product/acme",
            "Rename Sync Title",
            "Rename sync summary",
        ),
        MockFinishReason::named("stop"),
    )
    .await;
    let mut server = make_server();
    install_classifier(&mut server, &classifier);
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path().canonicalize().unwrap();
    // Sort before `product/` so the conflict source is processed before the
    // existing destination can receive an unrelated in-place mtime update.
    let source = docs.join("a-rename-sync.md");
    let source_bytes = "new classified source remains recoverable\n";
    fs::write(&source, source_bytes).unwrap();
    let destination = docs.join("product/acme/a-rename-sync.md");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(&destination, "old destination moved to archive\n").unwrap();
    fs::File::open(&destination)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
        .unwrap();
    let _model_mode = crate::docs_ops::enable_model_classification_for_test();
    crate::docs_ops::set_organize_test_hook(
        crate::docs_ops::OrganizeTestPoint::DirectorySyncFailure,
        docs.join("archive"),
        Box::new(|| {}),
    );

    let error = crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect_err("destination-parent sync fault must abort before source deletion durability");

    assert!(
        error.contains("Failed to sync rename destination parent")
            && error.contains("injected directory sync failure"),
        "{error}"
    );
    assert_eq!(fs::read_to_string(&source).unwrap(), source_bytes);
    assert!(!destination.exists());
    assert_eq!(
        fs::read_to_string(docs.join("archive/a-rename-sync.md")).unwrap(),
        "old destination moved to archive\n"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn drifted_prior_binding_fails_before_conflict_mutation() {
    let classifier = MockDocsClassifier::start(
        &model_response(
            "docs/product/acme",
            "Drift Guard Title",
            "Drift guard summary",
        ),
        MockFinishReason::named("stop"),
    )
    .await;
    let mut server = make_server();
    install_classifier(&mut server, &classifier);
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path().canonicalize().unwrap();
    let source = docs.join("b.md");
    let destination = docs.join("product/acme/b.md");
    let _model_mode = crate::docs_ops::enable_model_classification_for_test();

    fs::write(&source, "first bound body\n").unwrap();
    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect("publish initial bound document");
    let original_destination = fs::read_to_string(&destination).unwrap();
    let drifted_destination = original_destination.replacen(
        "title: \"Drift Guard Title\"",
        "title: \"Unreceipted drift\"",
        1,
    );
    assert_ne!(drifted_destination, original_destination);
    fs::write(&destination, &drifted_destination).unwrap();
    fs::write(&source, "replacement body must not publish\n").unwrap();

    let error = crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect_err("drifted receipt binding must fail closed");

    assert!(error.contains("content binding does not match"), "{error}");
    assert_eq!(
        fs::read_to_string(&source).unwrap(),
        "replacement body must not publish\n"
    );
    assert_eq!(
        fs::read_to_string(&destination).unwrap(),
        drifted_destination
    );
    assert!(!docs.join("archive/b.md").exists());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn accepted_category_drift_fails_closed_in_preview_and_apply() {
    for dry_run in [true, false] {
        let server = make_server();
        let workspace = DocsWorktree::new();
        let docs = workspace.docs_path().canonicalize().unwrap();
        let path = docs.join("engineering/devops/drift.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let receipt = valid_bound_receipt(
            "engineering/devops",
            "Original title",
            "Original summary",
            "docs/engineering/devops/drift.md",
            3,
        );
        let valid = document_with_receipt(
            "Original title",
            "Original summary",
            "engineering/devops",
            true,
            &receipt,
            "accepted-category body\n",
        );
        let drifted = valid.replacen("Original title", "Drifted title", 1);
        fs::write(&path, &drifted).unwrap();

        let error = crate::docs_ops::handle_wiki_organize(
            &server,
            docs.to_str().unwrap(),
            dry_run,
        )
        .await
        .expect_err("accepted-category metadata drift must fail closed");

        assert!(error.contains("content binding does not match"), "{error}");
        assert_eq!(fs::read_to_string(path).unwrap(), drifted);
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn accepted_category_in_place_normalization_rebinds_payload_and_revision() {
    let server = make_server();
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path().canonicalize().unwrap();
    let path = docs.join("engineering/devops/normalized.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let receipt = valid_bound_receipt(
        "docs/engineering/devops",
        "Normalized title",
        "Normalized summary",
        "docs/engineering/devops/normalized.md",
        5,
    );
    fs::write(
        &path,
        document_with_receipt(
            "Normalized title",
            "Normalized summary",
            "docs/engineering/devops",
            true,
            &receipt,
            "in-place body\n",
        ),
    )
    .unwrap();

    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect("normalize accepted-category frontmatter");

    assert_model_document(
        &fs::read_to_string(path).unwrap(),
        "Normalized title",
        "Normalized summary",
        "engineering/devops",
        "docs/engineering/devops/normalized.md",
        6,
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn accepted_category_move_rebinds_final_path_and_revision_without_model_call() {
    let server = make_server();
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path().canonicalize().unwrap();
    let source = docs.join("scattered-receipted.md");
    let receipt = valid_bound_receipt(
        "product/acme",
        "Existing model title",
        "Existing model summary",
        "docs/scattered-receipted.md",
        7,
    );
    fs::write(
        &source,
        document_with_receipt(
            "Existing model title",
            "Existing model summary",
            "product/acme",
            true,
            &receipt,
            "move body\n",
        ),
    )
    .unwrap();

    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect("move existing receipted document");

    assert!(!source.exists());
    assert_model_document(
        &fs::read_to_string(docs.join("product/acme/scattered-receipted.md")).unwrap(),
        "Existing model title",
        "Existing model summary",
        "product/acme",
        "docs/product/acme/scattered-receipted.md",
        8,
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn source_newer_conflict_rebinds_receipted_predecessor_to_archive() {
    let server = make_server();
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path().canonicalize().unwrap();
    let source = docs.join("conflicted.md");
    fs::write(
        &source,
        "---\ntitle: \"Replacement title\"\nsummary: \"Replacement summary\"\ncategory: \"product/acme\"\norganize: true\n---\nreplacement body\n",
    )
    .unwrap();
    let destination = docs.join("product/acme/conflicted.md");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    let predecessor = valid_bound_receipt(
        "product/acme",
        "Predecessor title",
        "Predecessor summary",
        "docs/product/acme/conflicted.md",
        9,
    );
    fs::write(
        &destination,
        document_with_receipt(
            "Predecessor title",
            "Predecessor summary",
            "product/acme",
            true,
            &predecessor,
            "predecessor body\n",
        ),
    )
    .unwrap();
    fs::File::open(&destination)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
        .unwrap();

    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect("archive receipted predecessor");

    let replacement = fs::read_to_string(&destination).unwrap();
    assert!(replacement.contains("replacement body"), "{replacement}");
    assert!(receipt_from_document(&replacement).is_none(), "{replacement}");
    assert_model_document(
        &fs::read_to_string(docs.join("archive/conflicted.md")).unwrap(),
        "Predecessor title",
        "Predecessor summary",
        "product/acme",
        "docs/archive/conflicted.md",
        10,
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn malformed_or_incomplete_prior_receipts_fail_closed_before_rewrite() {
    let base = valid_bound_receipt(
        "engineering/devops",
        "Strict title",
        "Strict summary",
        "docs/engineering/devops/strict.md",
        2,
    );
    let mut cases = Vec::new();
    let mut missing_lane = base.clone();
    missing_lane.as_object_mut().unwrap().remove("lane");
    cases.push(("missing-lane", missing_lane));
    let mut wrong_lane = base.clone();
    wrong_lane["lane"] = json!("distill");
    cases.push(("wrong-lane", wrong_lane));
    let mut unknown_field = base.clone();
    unknown_field["forged"] = json!(true);
    cases.push(("unknown-field", unknown_field));
    let mut unknown_completion = base.clone();
    unknown_completion["completion_status"] = json!("unknown");
    cases.push(("unknown-completion", unknown_completion));
    let mut absent_model = base.clone();
    absent_model["effective_model"] = Value::Null;
    cases.push(("absent-model", absent_model));
    let mut invalid_tokens = base;
    invalid_tokens["prompt_tokens"] = json!("eleven");
    cases.push(("invalid-token-type", invalid_tokens));

    for (label, receipt) in cases {
        let server = make_server();
        let workspace = DocsWorktree::new();
        let docs = workspace.docs_path().canonicalize().unwrap();
        let path = docs.join("engineering/devops/strict.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = document_with_receipt(
            "Strict title",
            "Strict summary",
            "engineering/devops",
            true,
            &receipt,
            "strict body\n",
        );
        fs::write(&path, &original).unwrap();

        let error = crate::docs_ops::handle_wiki_organize(
            &server,
            docs.to_str().unwrap(),
            false,
        )
        .await
        .expect_err("malformed prior receipt must fail closed");

        assert!(error.contains("existing model receipt"), "{label}: {error}");
        assert_eq!(fs::read_to_string(path).unwrap(), original, "{label}");
    }
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn index_write_failure_is_reported_as_recoverable_after_document_publication() {
    let server = make_server();
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path().canonicalize().unwrap();
    let source = docs.join("debug-index-failure.md");
    fs::write(&source, "# Published document\n").unwrap();
    let index = docs.join("_index.md");
    let old_index = "old recoverable index bytes\n";
    fs::write(&index, old_index).unwrap();
    crate::docs_ops::set_organize_test_hook(
        crate::docs_ops::OrganizeTestPoint::ExistingFileWriteFailure,
        index.clone(),
        Box::new(|| {}),
    );

    let result = crate::docs_ops::handle_wiki_organize(
        &server,
        docs.to_str().unwrap(),
        false,
    )
    .await
    .expect("derived index failure remains warning-only");
    let result: Value = serde_json::from_str(&result).unwrap();

    assert!(
        result["log"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry
                .as_str()
                .is_some_and(|entry| entry.contains("WARN: failed to write _index.md"))),
        "{result}"
    );
    assert_eq!(fs::read_to_string(index).unwrap(), old_index);
    assert!(!source.exists());
    assert!(docs
        .join("engineering/debugging/debug-index-failure.md")
        .exists());
}
