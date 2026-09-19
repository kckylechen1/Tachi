use super::*;
use axum::{response::IntoResponse, routing::post, Json, Router};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tachi_llm::{
    ChatLaneConfig, LlmClient, ProviderRuntimeConfig, ProviderSecret, RerankConfig,
    RerankProviderKind,
};

const PROVENANCE_PREFIX: &str = "tachi_model_invocation_v1: ";

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
    async fn start(content: &str, finish_reason: &str) -> Self {
        let content = content.to_string();
        let finish_reason = finish_reason.to_string();
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
                    Json(json!({
                        "choices": [{
                            "message": {"role": "assistant", "content": content},
                            "finish_reason": finish_reason,
                        }],
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

fn assert_model_document(content: &str, title: &str, summary: &str, category: &str) {
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
        "stop",
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
    for content in [&moved, &updated] {
        assert_model_document(
            content,
            "Model Deployment Guide",
            "Deployment steps selected by the model",
            "engineering/devops",
        );
    }
    assert_eq!(classifier.request_count(), 2);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn heuristic_reclassification_replaces_fields_and_removes_stale_model_provenance() {
    let server = make_server();
    let workspace = DocsWorktree::new();
    let docs = workspace.docs_path();
    let source = docs.join("debug-fix.md");
    fs::write(
        &source,
        "---\ntitle: \"Stale model title\"\nsummary: \"Stale model summary\"\norganize: true\ntachi_model_invocation_v1: {\"schema\":\"model-invocation-v1\",\"effective_model\":\"stale-model\"}\n---\n# Heuristic body\n",
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
    assert!(!content.contains("stale-model"), "{content}");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn invalid_truncated_and_unsafe_model_outputs_never_create_clean_provenance() {
    for (label, response, finish_reason) in [
        ("invalid-json", "not-json".to_string(), "stop"),
        (
            "unsafe-category",
            model_response(
                "docs/../../outside",
                "Unsafe model title",
                "Unsafe model summary",
            ),
            "stop",
        ),
        (
            "truncated",
            model_response(
                "docs/product/unsafe",
                "Truncated model title",
                "Truncated model summary",
            ),
            "length",
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
        "stop",
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
        "stop",
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
        "stop",
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
        "stop",
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
        "stop",
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
    );
}
