use super::*;
use crate::tool_params::TachiMemoryParams;
use rmcp::handler::server::wrapper::Parameters;
use sha2::{Digest, Sha256};

const REVIEW_NEEDLE: &str = "WikiReviewNeedle";

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

struct ManifestFixture {
    dir: tempfile::TempDir,
    snapshot_github: std::path::PathBuf,
    snapshot_spec: std::path::PathBuf,
}

impl ManifestFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("manifest temp dir");
        let snapshot_github = dir.path().join("issue-1072-archive.md");
        std::fs::write(
            &snapshot_github,
            b"operator-attested archived snapshot of the #1072 discussion\n",
        )
        .expect("write github snapshot");
        let snapshot_spec = dir.path().join("spec.md");
        std::fs::write(
            &snapshot_spec,
            b"referenced local spec: review-approval contract\n",
        )
        .expect("write local spec");
        Self {
            dir,
            snapshot_github,
            snapshot_spec,
        }
    }

    fn rows(&self) -> Vec<(String, std::path::PathBuf, String)> {
        let github_bytes = std::fs::read(&self.snapshot_github).expect("github snapshot bytes");
        let spec_bytes = std::fs::read(&self.snapshot_spec).expect("spec bytes");
        vec![
            (
                "kckylechen1/tachi#1072".to_string(),
                self.snapshot_github.clone(),
                sha256_hex(&github_bytes),
            ),
            (
                self.snapshot_spec.display().to_string(),
                self.snapshot_spec.clone(),
                sha256_hex(&spec_bytes),
            ),
        ]
    }

    fn references(&self) -> Vec<String> {
        self.rows()
            .iter()
            .map(|(target_ref, _, _)| target_ref.clone())
            .collect()
    }

    fn manifest_path(&self, rows: &[(String, std::path::PathBuf, String)]) -> std::path::PathBuf {
        let path = self
            .dir
            .path()
            .join(format!("manifest-{}.json", uuid::Uuid::new_v4()));
        let sources: Vec<Value> = rows
            .iter()
            .map(|(target_ref, snapshot, sha256)| {
                json!({
                    "ref": target_ref,
                    "snapshot": snapshot.display().to_string(),
                    "sha256": sha256,
                })
            })
            .collect();
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({"version": 1, "sources": sources})).expect("manifest json"),
        )
        .expect("write manifest");
        path
    }
}

fn review_write_params(path: &str, references: Vec<String>, metadata: Value) -> WikiWriteParams {
    WikiWriteParams {
        title: "Wiki review approval flow".to_string(),
        text: format!(
            "{REVIEW_NEEDLE} documents the operator approval flow for shared engineering knowledge."
        ),
        path: Some(path.to_string()),
        topic: Some("wiki-review-approval".to_string()),
        summary: Some("Operator approval flow summary".to_string()),
        category: "experience".to_string(),
        keywords: vec![],
        entities: vec![],
        importance: 0.7,
        scope: "global".to_string(),
        retention_policy: "permanent".to_string(),
        domain: None,
        project: Some("wiki".to_string()),
        metadata: Some(metadata),
        force: true,
        references,
        include_patterns: false,
        pattern_query: None,
        pattern_top_k: None,
    }
}

fn bounded_shared_metadata() -> Value {
    json!({
        "origin_projects": ["Sigil"],
        "applies_to": {"repos": ["kckylechen1/tachi"]},
        "operator_note": "ordinary metadata must survive approval",
    })
}

async fn write_pending_review_entry(
    server: &MemoryServer,
    path: &str,
    references: Vec<String>,
) -> String {
    let response = server
        .tachi_wiki_write(Parameters(review_write_params(
            path,
            references,
            bounded_shared_metadata(),
        )))
        .await
        .expect("pending wiki write");
    let parsed: Value = serde_json::from_str(&response).expect("wiki write JSON");
    parsed["id"].as_str().expect("id").to_string()
}

fn wiki_row(server: &MemoryServer, id: &str) -> memcore::MemoryEntry {
    server
        .with_named_project_store_read("wiki", |store| {
            store.get(id).map_err(|error| error.to_string())
        })
        .expect("read wiki row")
        .expect("wiki row exists")
}

async fn default_search_paths(server: &MemoryServer) -> Vec<String> {
    let params = WikiSearchParams {
        query: REVIEW_NEEDLE.to_string(),
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
        lifecycle: None,
    };
    let value = crate::wiki_ops::collect_wiki_search_value(server, params)
        .await
        .expect("wiki search");
    value["results"]
        .as_array()
        .expect("results array")
        .iter()
        .filter_map(|row| row["path"].as_str().map(str::to_string))
        .collect()
}

fn row_fingerprint(entry: &memcore::MemoryEntry) -> (i64, Value, bool) {
    (entry.revision, entry.metadata.clone(), entry.archived)
}

fn apply_request(
    project: &str,
    id: &str,
    manifest_path: &std::path::Path,
    preview: &Value,
) -> crate::wiki_ops::WikiReviewApply {
    crate::wiki_ops::WikiReviewApply {
        project: project.to_string(),
        id: id.to_string(),
        approver: "ops-lead".to_string(),
        expected_revision: preview["apply"]["expected_revision"]
            .as_i64()
            .expect("preview revision"),
        expected_review_digest: preview["apply"]["review_digest"]
            .as_str()
            .expect("preview digest")
            .to_string(),
        manifest_path: manifest_path.to_path_buf(),
    }
}

fn preview_entry(
    project: &str,
    id: &str,
    manifest_path: &std::path::Path,
) -> Result<Value, String> {
    crate::wiki_ops::preview_wiki_review(project, id, manifest_path)
}

#[tokio::test]
async fn pending_hidden_approve_default_visible_ordinary_edit_hidden() {
    let (server, _home) = seed_wiki_project_entries(vec![]);
    let fixture = ManifestFixture::new();
    let manifest_path = fixture.manifest_path(&fixture.rows());
    let path = "/wiki/engineering/review-cli/approval-flow";
    let id = write_pending_review_entry(&server, path, fixture.references()).await;

    // Pending row is hidden from default search and read.
    assert!(
        !default_search_paths(&server)
            .await
            .contains(&path.to_string()),
        "a pending_review wiki entry must stay hidden from default search"
    );
    let pending_read =
        crate::wiki_ops::collect_wiki_read_value(&server, path, "wiki").expect("read pending row");
    assert_eq!(
        pending_read["status"],
        json!("not_found"),
        "default read gates pending rows: {pending_read:?}"
    );

    // Preview: read-only, reports the exact binding an apply must carry.
    let preview = preview_entry("wiki", &id, &manifest_path).expect("preview");
    assert_eq!(preview["mode"], json!("preview"));
    assert_eq!(preview["read_only"], json!(true));
    assert_eq!(preview["entry"]["lifecycle"], json!("pending_review"));
    assert_eq!(preview["entry"]["id"], json!(id));
    let expected_revision = preview["apply"]["expected_revision"]
        .as_i64()
        .expect("revision");
    let preview_digest = preview["apply"]["review_digest"]
        .as_str()
        .expect("digest")
        .to_string();
    assert_eq!(preview["review_digest"], json!(preview_digest));
    assert_eq!(
        preview["source_bundle_hash"].as_str().map(str::len),
        Some(64)
    );
    let notes = preview["notes"].as_array().expect("notes");
    assert!(
        notes.iter().any(|note| note
            .as_str()
            .unwrap_or_default()
            .contains("operator-attested")),
        "preview must say URL/GitHub mappings are operator-attested snapshot verification, not \
         live upstream verification: {notes:?}"
    );
    let verifications: Vec<&str> = preview["references"]
        .as_array()
        .expect("reference rows")
        .iter()
        .map(|row| row["verification"].as_str().expect("verification"))
        .collect();
    assert!(verifications.contains(&"operator_attested_snapshot"));
    assert!(verifications.contains(&"referenced_file"));

    // Apply with values bound to the preview.
    let before = wiki_row(&server, &id);
    let receipt = crate::wiki_ops::apply_wiki_review(
        &server,
        &apply_request("wiki", &id, &manifest_path, &preview),
    )
    .expect("apply approval");
    assert_eq!(receipt["mode"], json!("apply"));
    assert_eq!(receipt["approved"], json!(true));
    assert_eq!(receipt["approver"], json!("ops-lead"));
    assert_eq!(receipt["previous_revision"], json!(expected_revision));
    assert_eq!(receipt["revision"], json!(expected_revision + 1));
    assert_eq!(receipt["review_digest"], json!(preview_digest));

    // Row-level invariants: approved metadata, revision bump, body/summary/
    // refs/unrelated metadata preserved.
    let approved = wiki_row(&server, &id);
    assert_eq!(approved.revision, before.revision + 1);
    assert_eq!(approved.text, before.text);
    assert_eq!(approved.summary, before.summary);
    assert_eq!(approved.metadata["lifecycle"], json!("active"));
    assert_eq!(approved.metadata["status"], json!("active"));
    assert_eq!(approved.metadata["review_status"], json!("approved"));
    assert_eq!(approved.metadata["authority"], json!("advisory"));
    assert_eq!(
        approved.metadata["source_bundle_hash"],
        preview["source_bundle_hash"]
    );
    let receipt_json = &approved.metadata["review_receipt"];
    assert_eq!(receipt_json["approver"], json!("ops-lead"));
    assert_eq!(receipt_json["decision"], json!("approved"));
    assert_eq!(receipt_json["review_digest"], json!(preview_digest));
    assert_eq!(receipt_json["expected_revision"], json!(expected_revision));
    assert_eq!(receipt_json["store"], json!("named:wiki"));
    assert!(
        chrono::DateTime::parse_from_rfc3339(
            receipt_json["decided_at"].as_str().expect("decided_at")
        )
        .is_ok(),
        "decided_at must be RFC3339"
    );
    assert_eq!(
        approved.metadata["operator_note"],
        json!("ordinary metadata must survive approval")
    );
    let stored_refs = approved.metadata["evidence_refs_v1"]
        .as_array()
        .expect("typed refs preserved")
        .iter()
        .map(|row| row["ref"].as_str().expect("ref").to_string())
        .collect::<Vec<_>>();
    assert!(stored_refs.contains(&"kckylechen1/tachi#1072".to_string()));
    assert!(stored_refs.contains(&fixture.snapshot_spec.display().to_string()));

    // Approved row is now default-visible in search and read.
    assert!(
        default_search_paths(&server)
            .await
            .contains(&path.to_string()),
        "the approved active entry must reach default search"
    );
    let active_read =
        crate::wiki_ops::collect_wiki_read_value(&server, path, "wiki").expect("read approved row");
    assert_eq!(active_read["status"], json!("found"));
    assert_eq!(active_read["entry"]["lifecycle"], json!("active"));
    assert_eq!(active_read["entry"]["authority"], json!("advisory"));
    assert_eq!(
        active_read["entry"]["review_receipt"]["review_digest"],
        json!(preview_digest)
    );

    // Ordinary public edit through the real wiki facade: the update resets
    // the candidate lifecycle to pending and the row is hidden again.
    let mut edit = review_write_params(path, fixture.references(), bounded_shared_metadata());
    edit.text = format!("{REVIEW_NEEDLE} revised body after operator approval.");
    let edit_response = server
        .tachi_wiki_write(Parameters(edit))
        .await
        .expect("edit");
    let edit_json: Value = serde_json::from_str(&edit_response).expect("edit JSON");
    assert_eq!(edit_json["wiki_write_mode"], json!("updated"));
    let edited = wiki_row(&server, &id);
    assert_eq!(
        edited.metadata["lifecycle"],
        json!("pending_review"),
        "an ordinary edit must return the row to pending: {edited:?}"
    );
    assert!(
        !default_search_paths(&server)
            .await
            .contains(&path.to_string()),
        "the edited pending row must be hidden from default search again"
    );
    assert_eq!(
        crate::wiki_ops::collect_wiki_read_value(&server, path, "wiki").expect("read edited row")
            ["status"],
        json!("not_found"),
        "default read must hide the edited pending row again"
    );

    // The ordinary `tachi_memory action=save` route additionally strips the
    // inherited receipt and hash: public saves cannot keep review authority.
    let memory_params: TachiMemoryParams = serde_json::from_value(json!({
        "action": "save",
        "kind": "memory",
        "id": id,
        "project": "wiki",
        "__tachi_project_explicit": true,
        "text": format!("{REVIEW_NEEDLE} memory-facade replacement body."),
        "path": path,
        "scope": "global",
        "force": true,
    }))
    .expect("memory facade params");
    crate::facade_memory_ops::handle_tachi_memory(&server, memory_params)
        .await
        .expect("memory facade edit");
    let stripped = wiki_row(&server, &id);
    assert_eq!(stripped.metadata["lifecycle"], json!("pending_review"));
    assert!(
        stripped.metadata.get("review_receipt").is_none(),
        "ordinary public edits must strip the review receipt: {}",
        stripped.metadata
    );
    assert!(
        stripped.metadata.get("source_bundle_hash").is_none(),
        "ordinary public edits must strip the source bundle hash: {}",
        stripped.metadata
    );
    assert!(
        !default_search_paths(&server)
            .await
            .contains(&path.to_string()),
        "the stripped pending row must stay hidden from default search"
    );
}

#[tokio::test]
async fn stale_revision_digest_or_applicability_refuse_with_no_db_change() {
    let (server, _home) = seed_wiki_project_entries(vec![]);
    let fixture = ManifestFixture::new();
    let manifest_path = fixture.manifest_path(&fixture.rows());
    let path = "/wiki/engineering/review-cli/stale-guard";
    let id = write_pending_review_entry(&server, path, fixture.references()).await;

    let preview = preview_entry("wiki", &id, &manifest_path).expect("preview");

    // Stale revision: an ordinary edit bumped the revision after preview.
    let mut edit = review_write_params(path, fixture.references(), bounded_shared_metadata());
    edit.text = format!("{REVIEW_NEEDLE} body drift after preview.");
    server
        .tachi_wiki_write(Parameters(edit))
        .await
        .expect("edit");
    let drifted = wiki_row(&server, &id);
    let fingerprint = row_fingerprint(&drifted);

    let stale_revision = crate::wiki_ops::WikiReviewApply {
        expected_review_digest: preview["apply"]["review_digest"]
            .as_str()
            .expect("preview digest")
            .to_string(),
        expected_revision: preview["apply"]["expected_revision"]
            .as_i64()
            .expect("preview revision"),
        ..apply_base("wiki", &id, &manifest_path)
    };
    let error = crate::wiki_ops::apply_wiki_review(&server, &stale_revision)
        .expect_err("stale revision must refuse");
    assert!(error.contains("stale revision"), "{error}");
    assert_eq!(row_fingerprint(&wiki_row(&server, &id)), fingerprint);

    // Fresh preview: current revision, but a digest from the older body.
    let fresh = preview_entry("wiki", &id, &manifest_path).expect("fresh preview");
    assert_eq!(
        fresh["apply"]["expected_revision"].as_i64(),
        Some(drifted.revision)
    );
    let wrong_digest = crate::wiki_ops::WikiReviewApply {
        expected_review_digest: preview["apply"]["review_digest"]
            .as_str()
            .expect("old digest")
            .to_string(),
        ..apply_request("wiki", &id, &manifest_path, &fresh)
    };
    let error = crate::wiki_ops::apply_wiki_review(&server, &wrong_digest)
        .expect_err("wrong digest must refuse");
    assert!(error.contains("digest mismatch"), "{error}");
    assert_eq!(row_fingerprint(&wiki_row(&server, &id)), fingerprint);

    // Applicability drift: an ordinary edit that drops the bounded
    // applicability makes the row shared-unbounded while keeping its
    // references covered by the manifest; approval must refuse BEFORE any
    // mutation because the approved row would never reach default
    // read/search.
    let mut unbounded = review_write_params(path, fixture.references(), json!({}));
    unbounded.text = format!("{REVIEW_NEEDLE} unbounded shared body needs bounded applicability.");
    server
        .tachi_wiki_write(Parameters(unbounded))
        .await
        .expect("unbounded edit");
    let unbounded_preview = preview_entry("wiki", &id, &manifest_path)
        .expect("preview of the unbounded row still reports");
    assert_eq!(
        unbounded_preview["applicability"]["applicability_status"],
        json!("unspecified"),
        "the drifted row must read as unbounded: {unbounded_preview:?}"
    );
    let error = crate::wiki_ops::apply_wiki_review(
        &server,
        &apply_request("wiki", &id, &manifest_path, &unbounded_preview),
    )
    .expect_err("unbounded shared approval must refuse");
    assert!(
        error.contains("not default-retrievable") || error.contains("bounded applicability"),
        "{error}"
    );
    let after = wiki_row(&server, &id);
    assert!(
        after.metadata.get("review_receipt").is_none(),
        "refused approval must not write a receipt: {}",
        after.metadata
    );
    assert!(after.metadata.get("source_bundle_hash").is_none());
    assert_eq!(
        after.metadata["lifecycle"],
        json!("pending_review"),
        "the refused row must stay pending: {}",
        after.metadata
    );
}

fn apply_base(
    project: &str,
    id: &str,
    manifest_path: &std::path::Path,
) -> crate::wiki_ops::WikiReviewApply {
    crate::wiki_ops::WikiReviewApply {
        project: project.to_string(),
        id: id.to_string(),
        approver: "ops-lead".to_string(),
        expected_revision: 1,
        expected_review_digest: "0".repeat(64),
        manifest_path: manifest_path.to_path_buf(),
    }
}

#[tokio::test]
async fn source_manifest_coverage_and_snapshot_refusals() {
    let (server, _home) = seed_wiki_project_entries(vec![]);
    let fixture = ManifestFixture::new();
    let rows = fixture.rows();
    let path = "/wiki/engineering/review-cli/manifest-guards";
    let id = write_pending_review_entry(&server, path, fixture.references()).await;

    // Missing coverage: drop one manifest row.
    let missing = fixture.manifest_path(&rows[1..2]);
    let error = preview_entry("wiki", &id, &missing).expect_err("missing ref must refuse");
    assert!(error.contains("missing rows"), "{error}");

    // Unmatched extra row.
    let mut extra = rows.clone();
    extra.push((
        "kckylechen1/tachi#9999".to_string(),
        fixture.snapshot_github.clone(),
        sha256_hex(b"unmatched"),
    ));
    let error = preview_entry("wiki", &id, &fixture.manifest_path(&extra))
        .expect_err("unmatched row must refuse");
    assert!(error.contains("match no entry reference"), "{error}");

    // Duplicate rows.
    let mut duplicate = rows.clone();
    duplicate.push(rows[0].clone());
    let error = preview_entry("wiki", &id, &fixture.manifest_path(&duplicate))
        .expect_err("duplicate row must refuse");
    assert!(error.contains("duplicate"), "{error}");

    // Bad expected hash.
    let mut bad_hash = rows.clone();
    bad_hash[0].2 = sha256_hex(b"not the snapshot bytes");
    let error = preview_entry("wiki", &id, &fixture.manifest_path(&bad_hash))
        .expect_err("bad hash must refuse");
    assert!(
        error.contains("hashes to") && error.contains("expects"),
        "{error}"
    );

    // Local absolute-path reference mapped to an unrelated snapshot.
    let unrelated = fixture.dir.path().join("unrelated-substitute.md");
    std::fs::write(&unrelated, b"a different file than the referenced spec")
        .expect("write substitute");
    let mut substituted = rows.clone();
    substituted[1].1 = unrelated;
    let error = preview_entry("wiki", &id, &fixture.manifest_path(&substituted))
        .expect_err("snapshot substitution must refuse");
    assert!(
        error.contains("must hash the referenced file itself"),
        "{error}"
    );

    #[cfg(unix)]
    {
        // A symlink snapshot is a nonregular file and must refuse.
        let symlinked = fixture.dir.path().join("symlinked-spec.md");
        std::os::unix::fs::symlink(&fixture.snapshot_spec, &symlinked).expect("symlink snapshot");
        let mut symlink_rows = rows.clone();
        symlink_rows[1].1 = symlinked.clone();
        let error = preview_entry("wiki", &id, &fixture.manifest_path(&symlink_rows))
            .expect_err("symlink snapshot must refuse");
        assert!(error.contains("regular non-symlink file"), "{error}");
    }

    // Snapshot drift between preview and apply: the bytes change after the
    // preview, so the apply's re-verification must refuse with no write.
    let manifest_path = fixture.manifest_path(&rows);
    let preview = preview_entry("wiki", &id, &manifest_path).expect("preview before drift");
    std::fs::write(
        &fixture.snapshot_github,
        b"operator-attested archived snapshot of the #1072 discussion, tampered\n",
    )
    .expect("tamper snapshot");
    let error = crate::wiki_ops::apply_wiki_review(
        &server,
        &apply_request("wiki", &id, &manifest_path, &preview),
    )
    .expect_err("drifted snapshot must refuse apply");
    assert!(
        error.contains("hashes to") && error.contains("expects"),
        "{error}"
    );
    let after = wiki_row(&server, &id);
    assert!(
        after.metadata.get("review_receipt").is_none(),
        "a refused apply must leave no receipt: {}",
        after.metadata
    );

    // An entry with no references has nothing to cover: refusal.
    let empty_id = write_pending_review_entry(
        &server,
        "/wiki/engineering/review-cli/no-references",
        vec![],
    )
    .await;
    let error =
        preview_entry("wiki", &empty_id, &manifest_path).expect_err("empty references must refuse");
    assert!(error.contains("no references"), "{error}");
}

#[tokio::test]
async fn wrong_archived_and_nonwiki_targets_refuse() {
    // One seeded fixture: it creates the named wiki store (so the wiki
    // write below can target it) and carries a plain non-wiki row.
    let mut ordinary = make_entry("wiki-review-ordinary-row");
    ordinary.path = "/ordinary/not-wiki".to_string();
    ordinary.text = format!("{REVIEW_NEEDLE} plain memory row living in the wiki store.");
    let (server, _home) = seed_wiki_project_entries(vec![ordinary]);
    let fixture = ManifestFixture::new();
    let manifest_path = fixture.manifest_path(&fixture.rows());

    // Unknown id.
    let error = preview_entry("wiki", "does-not-exist", &manifest_path)
        .expect_err("unknown id must refuse");
    assert!(error.contains("not found"), "{error}");

    // Missing named store: preview refuses and creates nothing.
    let home = std::path::PathBuf::from(
        std::env::var("TACHI_HOME").expect("TACHI_HOME is set by the temp-home guard"),
    );
    let absent_store_dir = home.join("projects/absent-wiki");
    let error = preview_entry("absent-wiki", "any-id", &manifest_path)
        .expect_err("absent store must refuse");
    assert!(
        error.contains("not found") || error.contains("Project"),
        "{error}"
    );
    assert!(
        !absent_store_dir.exists(),
        "preview must not create the named wiki store"
    );

    // Non-wiki row inside the wiki store.
    let error = preview_entry("wiki", "wiki-review-ordinary-row", &manifest_path)
        .expect_err("non-wiki row must refuse");
    assert!(
        error.contains("not a Wiki/guide knowledge artifact"),
        "{error}"
    );

    // Archived wiki row.
    let path = "/wiki/engineering/review-cli/archived-target";
    let id = write_pending_review_entry(&server, path, fixture.references()).await;
    let revision = wiki_row(&server, &id).revision;
    server
        .with_named_project_store("wiki", |store| {
            store
                .archive_memory_if_revision(&id, revision)
                .map_err(|error| error.to_string())
        })
        .expect("archive row")
        .then_some(())
        .expect("archive must succeed");
    let error = preview_entry("wiki", &id, &manifest_path).expect_err("archived must refuse");
    assert!(error.contains("archived"), "{error}");
}

#[tokio::test]
async fn wiki_save_response_carries_deterministic_read_locator() {
    // Seeded fixture: the named wiki store exists, so an explicit
    // project="wiki" write can land there.
    let (server, _home) = seed_wiki_project_entries(vec![]);
    let fixture = ManifestFixture::new();

    // Named-project write: the locator names the resolved store.
    let response = server
        .tachi_wiki_write(Parameters(review_write_params(
            "/wiki/engineering/review-cli/read-locator",
            fixture.references(),
            bounded_shared_metadata(),
        )))
        .await
        .expect("wiki write");
    let parsed: Value = serde_json::from_str(&response).expect("write JSON");
    let id = parsed["id"].as_str().expect("id").to_string();
    assert_eq!(
        parsed["read"],
        json!({
            "action": "get",
            "id": id,
            "project": "wiki",
        }),
        "named-project writes must carry the resolved project locator: {parsed:?}"
    );

    // The locator actually resolves through the real get facade.
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: id.clone(),
            include_archived: false,
            project: Some("wiki".to_string()),
        }))
        .await
        .expect("get via locator project");
    let entry: Value = serde_json::from_str(&fetched).expect("get JSON");
    assert_eq!(entry["id"], json!(id));

    // The memory facade's compact save receipt preserves the locator.
    // On this server the default wiki route also lands in the named wiki
    // store, so the receipt's locator names it.
    let memory_params: TachiMemoryParams = serde_json::from_value(json!({
        "action": "save",
        "format": "json",
        "kind": "wiki",
        "title": "Memory facade locator preservation",
        "text": "The compact save receipt keeps the wiki route's read locator whole.",
        "path": "/wiki/engineering/review-cli/facade-receipt-locator",
        "scope": "global",
        "force": true,
    }))
    .expect("memory facade params");
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, memory_params)
        .await
        .expect("memory facade wiki save");
    let receipt: Value = serde_json::from_str(&body).expect("receipt JSON");
    assert_eq!(
        receipt["read"]["action"],
        json!("get"),
        "the memory facade save receipt must preserve the read locator: {receipt:?}"
    );
    assert_eq!(receipt["read"]["id"], receipt["id"]);
    assert_eq!(receipt["read"]["project"], json!("wiki"));

    // Default-routed write on a global-only server (no named wiki store,
    // no bound project): the locator omits `project` because the row
    // landed in the global store.
    drop(server);
    drop(_home);
    let (global_server, _global_home) = crate::tests::make_server_with_temp_home();
    let mut default_routed = review_write_params(
        "/wiki/engineering/review-cli/read-locator-global",
        vec![],
        json!({}),
    );
    default_routed.project = None;
    let response = global_server
        .tachi_wiki_write(Parameters(default_routed))
        .await
        .expect("default-routed wiki write");
    let parsed: Value = serde_json::from_str(&response).expect("write JSON");
    assert_eq!(parsed["read"]["action"], json!("get"));
    assert_eq!(parsed["read"]["id"], parsed["id"]);
    assert!(
        parsed["read"].get("project").is_none(),
        "a global landing must not claim a project: {parsed:?}"
    );
}
