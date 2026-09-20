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

#[derive(Clone)]
enum MockResponseModel {
    Missing,
    Null,
    Named(String),
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
        Self::start_with_model(
            content,
            finish_reason,
            MockResponseModel::Named("mock-docs-classifier-v1".to_string()),
        )
        .await
    }

    async fn start_with_model(
        content: &str,
        finish_reason: MockFinishReason,
        response_model: MockResponseModel,
    ) -> Self {
        let content = content.to_string();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured_requests = Arc::clone(&requests);
        let app = Router::new().route(
            "/chat/completions",
            post(move |Json(request): Json<Value>| {
                let content = content.clone();
                let finish_reason = finish_reason.clone();
                let response_model = response_model.clone();
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
                    let mut response = json!({
                        "choices": [choice],
                        "usage": {"prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18},
                    });
                    match response_model {
                        MockResponseModel::Missing => {}
                        MockResponseModel::Null => {
                            response["model"] = Value::Null;
                        }
                        MockResponseModel::Named(model) => {
                            response["model"] = Value::String(model);
                        }
                    }
                    Json(response).into_response()
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

fn insert_resolved_card(server: &crate::tests::TestServer, id: &str) {
    let mut card = crate::tests::make_entry(id);
    card.category = "kanban".to_string();
    card.metadata = json!({"status": "resolved"});
    server
        .with_global_store(|store| store.upsert(&card).map_err(|error| error.to_string()))
        .expect("insert resolved task card");
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

fn canonical_bound_receipt_wire(
    category: &str,
    title: &str,
    summary: &str,
    object_id: &str,
    revision: i64,
) -> String {
    let hash = independent_content_hash(&canonical_payload(category, title, summary));
    format!(
        concat!(
            "{{\"schema\":\"model-invocation-v1\",",
            "\"lane\":\"extract\",\"engine_kind\":\"provider_http\",",
            "\"effective_provider\":\"mock-provider\",",
            "\"effective_model\":\"mock-docs-classifier-v1\",",
            "\"effective_version\":null,\"fallback_chain\":[],",
            "\"degraded\":false,\"completion_status\":\"complete\",",
            "\"prompt_tokens\":11,\"completion_tokens\":7,\"total_tokens\":18,",
            "\"latency_ms\":1,\"content_hash\":{},\"memory_id\":{},",
            "\"revision\":{revision}}}"
        ),
        serde_json::to_string(&hash).unwrap(),
        serde_json::to_string(object_id).unwrap(),
        revision = revision,
    )
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

fn document_with_raw_receipt(
    title: &str,
    summary: &str,
    category: &str,
    organize: bool,
    receipt: &str,
    body: &str,
) -> String {
    format!(
        "---\ntitle: {}\nsummary: {}\ncategory: {}\norganize: {organize}\n{PROVENANCE_PREFIX}{receipt}\n---\n{body}",
        serde_json::to_string(title).unwrap(),
        serde_json::to_string(summary).unwrap(),
        serde_json::to_string(category).unwrap(),
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
async fn heuristic_routing_preserves_authored_metadata_without_model_provenance() {
    let server = make_server();
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path();
    let source = docs.join("authored-prd.md");
    fs::write(
        &source,
        "---\ntitle: \"Authored product title\"\nsummary: \"Authored product summary\"\norganize: true\n---\n# Heuristic body heading\n",
    )
    .unwrap();

    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect("heuristic routing must preserve authored metadata");

    let destination = docs.join("product/test_product/authored-prd.md");
    let content = fs::read_to_string(destination).unwrap();
    assert!(!source.exists());
    assert!(
        content.contains("title: \"Authored product title\""),
        "{content}"
    );
    assert!(
        content.contains("summary: \"Authored product summary\""),
        "{content}"
    );
    assert!(receipt_from_document(&content).is_none(), "{content}");
    assert!(!content.contains("title: \"authored prd\""), "{content}");
    assert!(
        !content.contains("summary: \"Heuristic body heading\""),
        "{content}"
    );
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
async fn missing_null_or_blank_response_model_falls_back_without_self_invalidating_receipt() {
    for (label, response_model) in [
        ("missing", MockResponseModel::Missing),
        ("null", MockResponseModel::Null),
        ("blank", MockResponseModel::Named("   ".to_string())),
    ] {
        let classifier = MockDocsClassifier::start_with_model(
            &model_response(
                "docs/product/acme",
                "Identityless model title",
                "Identityless model summary",
            ),
            MockFinishReason::named("stop"),
            response_model,
        )
        .await;
        let mut server = make_server();
        install_classifier(&mut server, &classifier);
        let workspace = DocsWorktree::new();
        let docs = workspace.docs_path();
        let filename = format!("debug-fix-model-{label}.md");
        let source = docs.join(&filename);
        fs::write(&source, "# Identity fallback body\n").unwrap();
        let _model_mode = crate::docs_ops::enable_model_classification_for_test();

        crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
            .await
            .expect("identityless model response follows heuristic fallback policy");

        let destination = docs.join("engineering/debugging").join(filename);
        let first = fs::read_to_string(&destination).unwrap();
        assert!(receipt_from_document(&first).is_none(), "{label}: {first}");
        assert!(
            !first.contains("Identityless model title"),
            "{label}: {first}"
        );

        crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), true)
            .await
            .expect("fallback document must remain valid in preview");
        crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
            .await
            .expect("fallback document must remain valid on repeat apply");

        let repeated = fs::read_to_string(destination).unwrap();
        assert!(
            receipt_from_document(&repeated).is_none(),
            "{label}: {repeated}"
        );
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
        crate::docs_ops::set_organize_test_hook(point, hook_path, Box::new(|| {}));

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

    crate::docs_ops::set_organize_test_hook(
        crate::docs_ops::OrganizeTestPoint::DirectorySyncFailure,
        docs.join("product"),
        Box::new(|| {}),
    );
    let retry_error = crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect_err("retry must attempt the previously failed ancestor sync again");
    assert!(
        retry_error.contains("injected directory sync failure"),
        "{retry_error}"
    );
    assert!(source.exists());
    assert!(!destination.exists());

    let (_trace_guard, sync_trace) = crate::docs_ops::capture_directory_sync_trace();
    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect("retry must re-prove the existing directory chain");

    assert!(sync_trace.lock().unwrap().iter().any(|event| event
        == &format!(
            "complete existing directory parent:{}",
            docs.join("product").display()
        )));
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
    let (_trace_guard, sync_trace) = crate::docs_ops::capture_directory_sync_trace();

    let error = crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect_err("destination-parent sync fault must abort before source deletion durability");

    assert!(
        error.contains("Failed to sync rename destination parent")
            && error.contains("injected directory sync failure"),
        "{error}"
    );
    let rename_syncs = sync_trace
        .lock()
        .unwrap()
        .iter()
        .filter(|event| {
            event.contains("rename destination parent")
                || event.contains("rename source parent")
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        rename_syncs,
        vec![format!(
            "attempt rename destination parent:{}",
            docs.join("archive").display()
        )],
        "source-parent sync must not be attempted before destination-parent durability"
    );
    assert_eq!(fs::read_to_string(&source).unwrap(), source_bytes);
    assert!(!destination.exists());
    assert_eq!(
        fs::read_to_string(docs.join("archive/a-rename-sync.md")).unwrap(),
        "old destination moved to archive\n"
    );
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn rename_only_archive_syncs_verified_file_bytes_before_namespace_change() {
    let classifier = MockDocsClassifier::start(
        &model_response(
            "docs/product/acme",
            "File Sync Title",
            "File sync summary",
        ),
        MockFinishReason::named("stop"),
    )
    .await;
    let mut server = make_server();
    install_classifier(&mut server, &classifier);
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path().canonicalize().unwrap();
    let source = docs.join("a-file-sync.md");
    fs::write(&source, "replacement source\n").unwrap();
    let destination = docs.join("product/acme/a-file-sync.md");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    let predecessor = "unreceipted predecessor bytes\n";
    fs::write(&destination, predecessor).unwrap();
    fs::File::open(&destination)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
        .unwrap();
    let _model_mode = crate::docs_ops::enable_model_classification_for_test();
    crate::docs_ops::set_organize_test_hook(
        crate::docs_ops::OrganizeTestPoint::RenameSourceSyncFailure,
        destination.clone(),
        Box::new(|| {}),
    );
    let (_trace_guard, sync_trace) = crate::docs_ops::capture_directory_sync_trace();

    let error = crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect_err("rename-only predecessor byte-sync failure must stop before rename");

    assert!(
        error.contains("Failed to sync rename source file")
            && error.contains("injected file sync failure"),
        "{error}"
    );
    let rename_events = sync_trace
        .lock()
        .unwrap()
        .iter()
        .filter(|event| event.contains("rename "))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        rename_events,
        vec![format!(
            "attempt rename source file:{}",
            destination.display()
        )]
    );
    assert_eq!(fs::read_to_string(&source).unwrap(), "replacement source\n");
    assert_eq!(fs::read_to_string(&destination).unwrap(), predecessor);
    assert!(!docs.join("archive/a-file-sync.md").exists());
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn successful_cross_directory_rename_traces_file_then_destination_then_source() {
    let classifier = MockDocsClassifier::start(
        &model_response(
            "docs/product/acme",
            "Cross Rename Title",
            "Cross rename summary",
        ),
        MockFinishReason::named("stop"),
    )
    .await;
    let mut server = make_server();
    install_classifier(&mut server, &classifier);
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path().canonicalize().unwrap();
    let source = docs.join("a-cross-rename.md");
    fs::write(&source, "replacement source\n").unwrap();
    let destination = docs.join("product/acme/a-cross-rename.md");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(&destination, "cross predecessor\n").unwrap();
    fs::File::open(&destination)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
        .unwrap();
    let _model_mode = crate::docs_ops::enable_model_classification_for_test();
    let (_trace_guard, sync_trace) = crate::docs_ops::capture_directory_sync_trace();

    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect("cross-directory archive rename must complete durability sequence");

    let rename_events = sync_trace
        .lock()
        .unwrap()
        .iter()
        .filter(|event| event.contains("rename "))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        rename_events,
        vec![
            format!("attempt rename source file:{}", destination.display()),
            format!("complete rename source file:{}", destination.display()),
            format!(
                "attempt rename destination parent:{}",
                docs.join("archive").display()
            ),
            format!(
                "complete rename destination parent:{}",
                docs.join("archive").display()
            ),
            format!(
                "attempt rename source parent:{}",
                docs.join("product/acme").display()
            ),
            format!(
                "complete rename source parent:{}",
                docs.join("product/acme").display()
            ),
        ]
    );
    assert_eq!(
        fs::read_to_string(docs.join("archive/a-cross-rename.md")).unwrap(),
        "cross predecessor\n"
    );
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn successful_same_directory_rename_traces_one_completed_namespace_sync() {
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path().canonicalize().unwrap();
    let directory = docs.join("engineering/devops");
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("same-source.md");
    let destination = directory.join("same-destination.md");
    fs::write(&source, "same-directory bytes\n").unwrap();
    let (_trace_guard, sync_trace) = crate::docs_ops::capture_directory_sync_trace();

    crate::docs_ops::rename_file_for_test(
        docs.to_str().unwrap(),
        &source,
        &destination,
    )
    .expect("same-directory rename must complete one namespace sync");

    let rename_events = sync_trace
        .lock()
        .unwrap()
        .iter()
        .filter(|event| event.contains("rename "))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        rename_events,
        vec![
            format!("attempt rename source file:{}", source.display()),
            format!("complete rename source file:{}", source.display()),
            format!(
                "attempt rename source and destination parent:{}",
                directory.display()
            ),
            format!(
                "complete rename source and destination parent:{}",
                directory.display()
            ),
        ]
    );
    assert!(!source.exists());
    assert_eq!(fs::read_to_string(destination).unwrap(), "same-directory bytes\n");
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

        let error = crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), dry_run)
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

    let normalized = fs::read_to_string(&path).unwrap();
    assert_model_document(
        &normalized,
        "Normalized title",
        "Normalized summary",
        "engineering/devops",
        "docs/engineering/devops/normalized.md",
        6,
    );

    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect("canonical category spelling must remain stable on reapply");
    assert_eq!(fs::read_to_string(path).unwrap(), normalized);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn accepted_category_aliases_fail_closed_before_preview_or_apply() {
    for (label, category) in [
        ("repeated", "engineering//devops"),
        ("dot", "engineering/./devops"),
    ] {
        for dry_run in [true, false] {
            let server = make_server();
            let workspace = DocsWorktree::new();
            let docs = workspace.docs_path().canonicalize().unwrap();
            let path = docs.join(format!("engineering/devops/{label}-{dry_run}.md"));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let object_id = format!("docs/engineering/devops/{label}-{dry_run}.md");
            let receipt = valid_bound_receipt(
                category,
                "Alias title",
                "Alias summary",
                &object_id,
                3,
            );
            let original = document_with_receipt(
                "Alias title",
                "Alias summary",
                category,
                true,
                &receipt,
                "alias body\n",
            );
            fs::write(&path, &original).unwrap();

            let error =
                crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), dry_run)
                    .await
                    .expect_err("ambiguous accepted-category alias must fail closed");

            assert!(error.contains("ambiguous path spelling"), "{label}: {error}");
            assert_eq!(fs::read_to_string(path).unwrap(), original);
        }
    }
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn literal_backslash_document_is_rejected_while_nested_components_remain_distinct() {
    {
        let server = make_server();
        let workspace = DocsWorktree::new();
        let docs = workspace.docs_path().canonicalize().unwrap();
        let ambiguous = docs.join("a\\b.md");
        let original = "literal backslash filename\n";
        fs::write(&ambiguous, original).unwrap();

        for dry_run in [true, false] {
            let error =
                crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), dry_run)
                    .await
                    .expect_err("literal backslash identity must be rejected, never rewritten");
            assert!(error.contains("ambiguous path component"), "{error}");
            assert_eq!(fs::read_to_string(&ambiguous).unwrap(), original);
        }
    }

    {
        let server = make_server();
        let workspace = DocsWorktree::new();
        let docs = workspace.docs_path().canonicalize().unwrap();
        let nested = docs.join("engineering/devops/a/b.md");
        fs::create_dir_all(nested.parent().unwrap()).unwrap();
        let receipt = valid_bound_receipt(
            "engineering/devops",
            "Nested title",
            "Nested summary",
            "docs/engineering/devops/a/b.md",
            4,
        );
        fs::write(
            &nested,
            document_with_receipt(
                "Nested title",
                "Nested summary",
                "engineering/devops",
                true,
                &receipt,
                "nested body\n",
            ),
        )
        .unwrap();

        crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
            .await
            .expect("nested components have an injective canonical identity");

        assert!(!nested.exists());
        assert_model_document(
            &fs::read_to_string(docs.join("engineering/devops/b.md")).unwrap(),
            "Nested title",
            "Nested summary",
            "engineering/devops",
            "docs/engineering/devops/b.md",
            5,
        );
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn receipted_organize_false_task_sync_rebinds_each_committed_publication() {
    let server = make_server();
    insert_resolved_card(&server, "receipt-task");
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path().canonicalize().unwrap();
    let path = docs.join("product/acme/task-note.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let receipt = valid_bound_receipt(
        "product/acme",
        "Task note",
        "Task summary",
        "docs/product/acme/task-note.md",
        12,
    );
    let original = document_with_receipt(
        "Task note",
        "Task summary",
        "product/acme",
        false,
        &receipt,
        "- [ ] Finish <!-- tachi:receipt-task -->\n",
    );
    fs::write(&path, &original).unwrap();

    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), true)
        .await
        .expect("preview validates the next receipt revision without publishing it");
    assert_eq!(fs::read_to_string(&path).unwrap(), original);

    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect("task sync republishes the surviving receipt");
    let published = fs::read_to_string(&path).unwrap();
    assert!(
        published.contains("- [x] Finish <!-- tachi:receipt-task -->"),
        "{published}"
    );
    assert_model_document(
        &published,
        "Task note",
        "Task summary",
        "product/acme",
        "docs/product/acme/task-note.md",
        13,
    );

    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect("a no-op task sync keeps the committed receipt revision");
    assert_eq!(fs::read_to_string(path).unwrap(), published);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn organize_false_task_sync_preserves_a_bound_category_alias_as_non_routing_metadata() {
    let server = make_server();
    insert_resolved_card(&server, "alias-task");
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path().canonicalize().unwrap();
    let path = docs.join("product/acme/alias-task.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let receipt = valid_bound_receipt(
        "product//acme",
        "Alias task",
        "Alias task summary",
        "docs/product/acme/alias-task.md",
        21,
    );
    let original = document_with_receipt(
        "Alias task",
        "Alias task summary",
        "product//acme",
        false,
        &receipt,
        "- [ ] Finish <!-- tachi:alias-task -->\n",
    );
    fs::write(&path, &original).unwrap();

    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), true)
        .await
        .expect("preview the non-routing alias task publication");
    assert_eq!(fs::read_to_string(&path).unwrap(), original);

    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect("publish task state without normalizing an opted-out category");
    let published = fs::read_to_string(&path).unwrap();
    assert!(
        published.contains("- [x] Finish <!-- tachi:alias-task -->"),
        "{published}"
    );
    assert_model_document(
        &published,
        "Alias task",
        "Alias task summary",
        "product//acme",
        "docs/product/acme/alias-task.md",
        22,
    );

    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect("reapply the opted-out task document without another publication");
    assert_eq!(fs::read_to_string(path).unwrap(), published);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn receipted_organize_false_task_sync_rejects_revision_overflow_before_write() {
    let server = make_server();
    insert_resolved_card(&server, "overflow-task");
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path().canonicalize().unwrap();
    let path = docs.join("product/acme/overflow-task.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let receipt = valid_bound_receipt(
        "product/acme",
        "Overflow task",
        "Overflow summary",
        "docs/product/acme/overflow-task.md",
        i64::MAX,
    );
    let original = document_with_receipt(
        "Overflow task",
        "Overflow summary",
        "product/acme",
        false,
        &receipt,
        "- [ ] Finish <!-- tachi:overflow-task -->\n",
    );
    fs::write(&path, &original).unwrap();

    for dry_run in [true, false] {
        let error = crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), dry_run)
            .await
            .expect_err("revision overflow must fail before preview or publication");
        assert!(error.contains("receipt revision overflow"), "{error}");
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }
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
async fn subtree_organize_keeps_routing_relative_and_receipt_identity_repo_relative() {
    let server = make_server();
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path().canonicalize().unwrap();
    let subtree = docs.join("guides");
    let path = subtree.join("engineering/devops/subtree.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let receipt = canonical_bound_receipt_wire(
        "engineering/devops",
        "Subtree title",
        "Subtree summary",
        "docs/guides/engineering/devops/subtree.md",
        6,
    );
    let original = document_with_raw_receipt(
        "Subtree title",
        "Subtree summary",
        "engineering/devops",
        true,
        &receipt,
        "subtree body",
    );
    fs::write(&path, &original).unwrap();

    crate::docs_ops::handle_wiki_organize(&server, subtree.to_str().unwrap(), true)
        .await
        .expect("preview an already positioned subtree document");
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    assert!(!subtree.join("archive/subtree.md").exists());

    crate::docs_ops::handle_wiki_organize(&server, subtree.to_str().unwrap(), false)
        .await
        .expect("apply an already positioned subtree document");
    assert!(path.exists());
    assert!(!subtree.join("archive/subtree.md").exists());
    let normalized = fs::read_to_string(&path).unwrap();
    assert_eq!(normalized, original, "the first apply must be a true no-op");
    assert_model_document(
        &normalized,
        "Subtree title",
        "Subtree summary",
        "engineering/devops",
        "docs/guides/engineering/devops/subtree.md",
        6,
    );

    crate::docs_ops::handle_wiki_organize(&server, subtree.to_str().unwrap(), false)
        .await
        .expect("reapply an already positioned subtree document");
    assert_eq!(fs::read_to_string(path).unwrap(), normalized);
    assert!(!subtree.join("archive/subtree.md").exists());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn equivalent_receipt_whitespace_is_a_true_in_place_no_op() {
    let server = make_server();
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path().canonicalize().unwrap();
    let path = docs.join("engineering/devops/receipt-whitespace.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let canonical = canonical_bound_receipt_wire(
        "engineering/devops",
        "Whitespace title",
        "Whitespace summary",
        "docs/engineering/devops/receipt-whitespace.md",
        6,
    );
    let receipt = canonical.replacen("{\"schema\"", "{ \"schema\"", 1);
    assert_ne!(receipt, canonical);
    let original = document_with_raw_receipt(
        "Whitespace title",
        "Whitespace summary",
        "engineering/devops",
        true,
        &receipt,
        "whitespace body",
    );
    fs::write(&path, &original).unwrap();

    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), true)
        .await
        .expect("preview equivalent retained receipt whitespace");
    assert_eq!(fs::read_to_string(&path).unwrap(), original);

    crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect("apply must not normalize receipt wire without a revision");
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    assert_model_document(
        &original,
        "Whitespace title",
        "Whitespace summary",
        "engineering/devops",
        "docs/engineering/devops/receipt-whitespace.md",
        6,
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
    assert!(
        receipt_from_document(&replacement).is_none(),
        "{replacement}"
    );
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
async fn source_newer_conflict_rejects_predecessor_category_alias_before_preview_or_apply() {
    for dry_run in [true, false] {
        let server = make_server();
        let workspace = DocsWorktree::new();
        let docs = workspace.docs_path().canonicalize().unwrap();
        let source = docs.join("conflicted-alias.md");
        let source_content = "---\ntitle: \"Replacement\"\nsummary: \"Replacement summary\"\ncategory: \"product/acme\"\norganize: true\n---\nreplacement body\n";
        fs::write(&source, source_content).unwrap();
        let destination = docs.join("product/acme/conflicted-alias.md");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        let predecessor = valid_bound_receipt(
            "product//acme",
            "Predecessor",
            "Predecessor summary",
            "docs/product/acme/conflicted-alias.md",
            4,
        );
        let destination_content = document_with_receipt(
            "Predecessor",
            "Predecessor summary",
            "product//acme",
            true,
            &predecessor,
            "predecessor body\n",
        );
        fs::write(&destination, &destination_content).unwrap();
        fs::File::open(&destination)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
            .unwrap();

        let error =
            crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), dry_run)
                .await
                .expect_err("conflict predecessor aliases must fail closed");

        assert!(error.contains("ambiguous path spelling"), "{error}");
        assert_eq!(fs::read_to_string(&source).unwrap(), source_content);
        assert_eq!(
            fs::read_to_string(&destination).unwrap(),
            destination_content
        );
        assert!(!docs.join("archive/conflicted-alias.md").exists());
    }
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
    let mut wrong_schema = base.clone();
    wrong_schema["schema"] = json!("model-invocation-v2");
    cases.push(("wrong-schema", wrong_schema));
    let mut wrong_engine = base.clone();
    wrong_engine["engine_kind"] = json!("claude_cli");
    cases.push(("wrong-engine", wrong_engine));
    let mut blank_provider = base.clone();
    blank_provider["effective_provider"] = json!(" ");
    cases.push(("blank-provider", blank_provider));
    let mut blank_version = base.clone();
    blank_version["effective_version"] = json!("");
    cases.push(("blank-version", blank_version));
    let mut unknown_fallback = base.clone();
    unknown_fallback["fallback_chain"] = json!(["unbounded-provider-label"]);
    cases.push(("unknown-fallback", unknown_fallback));
    let mut excessive_fallback = base.clone();
    excessive_fallback["fallback_chain"] = json!([
        "provider_http_fallback",
        "provider_http_fallback",
        "provider_http_fallback",
        "provider_http_fallback",
        "provider_http_fallback"
    ]);
    cases.push(("excessive-fallback", excessive_fallback));
    let mut unknown_field = base.clone();
    unknown_field["forged"] = json!(true);
    cases.push(("unknown-field", unknown_field));
    let mut unknown_completion = base.clone();
    unknown_completion["completion_status"] = json!("unknown");
    cases.push(("unknown-completion", unknown_completion));
    let mut absent_model = base.clone();
    absent_model["effective_model"] = Value::Null;
    cases.push(("absent-model", absent_model));
    let mut blank_model = base.clone();
    blank_model["effective_model"] = json!("");
    cases.push(("blank-model", blank_model));
    let mut invalid_degraded = base.clone();
    invalid_degraded["degraded"] = json!("false");
    cases.push(("invalid-degraded-type", invalid_degraded));
    let mut invalid_latency_type = base.clone();
    invalid_latency_type["latency_ms"] = json!("1");
    cases.push(("invalid-latency-type", invalid_latency_type));
    let mut invalid_tokens = base;
    invalid_tokens["prompt_tokens"] = json!("eleven");
    cases.push(("invalid-token-type", invalid_tokens));
    let mut negative_tokens = valid_bound_receipt(
        "engineering/devops",
        "Strict title",
        "Strict summary",
        "docs/engineering/devops/strict.md",
        2,
    );
    negative_tokens["completion_tokens"] = json!(-1);
    cases.push(("negative-token", negative_tokens));
    let mut invalid_latency = valid_bound_receipt(
        "engineering/devops",
        "Strict title",
        "Strict summary",
        "docs/engineering/devops/strict.md",
        2,
    );
    invalid_latency["latency_ms"] = json!(-1);
    cases.push(("negative-latency", invalid_latency));
    let mut zero_revision = valid_bound_receipt(
        "engineering/devops",
        "Strict title",
        "Strict summary",
        "docs/engineering/devops/strict.md",
        1,
    );
    zero_revision["revision"] = json!(0);
    cases.push(("zero-revision", zero_revision));
    let mut out_of_range_tokens = valid_bound_receipt(
        "engineering/devops",
        "Strict title",
        "Strict summary",
        "docs/engineering/devops/strict.md",
        2,
    );
    out_of_range_tokens["prompt_tokens"] = json!(u64::MAX);
    cases.push(("out-of-range-token", out_of_range_tokens));

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

        let error = crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
            .await
            .expect_err("malformed prior receipt must fail closed");

        assert!(error.contains("existing model receipt"), "{label}: {error}");
        assert_eq!(fs::read_to_string(path).unwrap(), original, "{label}");
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn duplicate_prior_receipt_fields_fail_closed_before_preview_or_rewrite() {
    let base = valid_bound_receipt(
        "engineering/devops",
        "Duplicate title",
        "Duplicate summary",
        "docs/engineering/devops/duplicate.md",
        2,
    )
    .to_string();
    let cases = [
        (
            "completion-status",
            base.replace(
                "\"completion_status\":\"complete\"",
                "\"completion_status\":\"truncated\",\"completion_status\":\"complete\"",
            ),
        ),
        (
            "binding-memory-id",
            base.replace(
                "\"memory_id\":\"docs/engineering/devops/duplicate.md\"",
                "\"memory_id\":\"docs/forged.md\",\"memory_id\":\"docs/engineering/devops/duplicate.md\"",
            ),
        ),
    ];

    for (label, receipt) in cases {
        assert_ne!(receipt, base, "fixture must contain a duplicate {label}");
        for dry_run in [true, false] {
            let server = make_server();
            let workspace = DocsWorktree::new();
            let docs = workspace.docs_path().canonicalize().unwrap();
            let path = docs.join("engineering/devops/duplicate.md");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let original = document_with_raw_receipt(
                "Duplicate title",
                "Duplicate summary",
                "engineering/devops",
                true,
                &receipt,
                "duplicate body\n",
            );
            fs::write(&path, &original).unwrap();

            let error =
                crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), dry_run)
                    .await
                    .expect_err("duplicate prior receipt fields must fail closed");

            assert!(error.contains("closed model-invocation-v1"), "{label}: {error}");
            assert_eq!(fs::read_to_string(path).unwrap(), original, "{label}");
        }
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn duplicate_raw_reserved_headers_fail_before_preview_or_apply() {
    let receipt = valid_bound_receipt(
        "engineering/devops",
        "Retained title",
        "Retained summary",
        "docs/engineering/devops/duplicate-header.md",
        3,
    );
    let valid = document_with_receipt(
        "Retained title",
        "Retained summary",
        "engineering/devops",
        true,
        &receipt,
        "duplicate header body\n",
    );
    let cases = [
        (
            "title",
            valid.replacen(
                "title: \"Retained title\"",
                "title: \"Forged title\"\ntitle: \"Retained title\"",
                1,
            ),
        ),
        (
            "summary",
            valid.replacen(
                "summary: \"Retained summary\"",
                "summary: \"Forged summary\"\nsummary: \"Retained summary\"",
                1,
            ),
        ),
        (
            "category",
            valid.replacen(
                "category: \"engineering/devops\"",
                "category: \"product//acme\"\ncategory: \"engineering/devops\"",
                1,
            ),
        ),
        (
            "tachi_model_invocation_v1",
            valid.replacen(
                PROVENANCE_PREFIX,
                &format!("{PROVENANCE_PREFIX}{{}}\n{PROVENANCE_PREFIX}"),
                1,
            ),
        ),
    ];

    for (field, original) in cases {
        assert_ne!(original, valid, "fixture must duplicate {field}");
        for dry_run in [true, false] {
            let server = make_server();
            let workspace = DocsWorktree::new();
            let docs = workspace.docs_path().canonicalize().unwrap();
            let path = docs.join("engineering/devops/duplicate-header.md");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, &original).unwrap();

            let error =
                crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), dry_run)
                    .await
                    .expect_err("duplicate raw reserved headers must fail closed");

            assert!(
                error.contains("duplicate reserved frontmatter field"),
                "{field}: {error}"
            );
            assert!(error.contains(field), "{field}: {error}");
            assert_eq!(fs::read_to_string(path).unwrap(), original, "{field}");
        }
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn duplicate_raw_receipt_on_conflict_destination_fails_before_mutation() {
    for dry_run in [true, false] {
        let server = make_server();
        let workspace = DocsWorktree::new();
        let docs = workspace.docs_path().canonicalize().unwrap();
        let source = docs.join("duplicate-destination.md");
        let source_content = "---\ntitle: \"Replacement\"\nsummary: \"Replacement summary\"\ncategory: \"product/acme\"\norganize: true\n---\nreplacement body\n";
        fs::write(&source, source_content).unwrap();
        let destination = docs.join("product/acme/duplicate-destination.md");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        let receipt = valid_bound_receipt(
            "product/acme",
            "Predecessor",
            "Predecessor summary",
            "docs/product/acme/duplicate-destination.md",
            8,
        );
        let valid_destination = document_with_receipt(
            "Predecessor",
            "Predecessor summary",
            "product/acme",
            true,
            &receipt,
            "predecessor body\n",
        );
        let destination_content = valid_destination.replacen(
            PROVENANCE_PREFIX,
            &format!("{PROVENANCE_PREFIX}{{}}\n{PROVENANCE_PREFIX}"),
            1,
        );
        fs::write(&destination, &destination_content).unwrap();

        let error =
            crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), dry_run)
                .await
                .expect_err("duplicate destination receipt headers must fail closed");

        assert!(
            error.contains("duplicate reserved frontmatter field"),
            "{error}"
        );
        assert!(error.contains("tachi_model_invocation_v1"), "{error}");
        assert_eq!(fs::read_to_string(&source).unwrap(), source_content);
        assert_eq!(
            fs::read_to_string(&destination).unwrap(),
            destination_content
        );
        assert!(!docs.join("archive/duplicate-destination.md").exists());
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

    let result = crate::docs_ops::handle_wiki_organize(&server, docs.to_str().unwrap(), false)
        .await
        .expect("derived index failure remains warning-only");
    let result: Value = serde_json::from_str(&result).unwrap();

    assert!(
        result["log"].as_array().unwrap().iter().any(|entry| entry
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
