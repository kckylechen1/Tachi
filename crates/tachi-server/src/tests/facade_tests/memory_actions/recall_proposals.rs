use super::*;
use sha2::{Digest, Sha256};

#[cfg(unix)]
fn make_server_with_temp_home() -> (crate::MemoryServer, crate::tests::TempHomeGuard) {
    crate::tests::ensure_test_env();
    let mut temp_home = crate::tests::TempHomeGuard::new();
    temp_home.temp_home = temp_home
        .temp_home
        .canonicalize()
        .expect("canonicalize recall test home");
    std::env::set_var("HOME", &temp_home.temp_home);
    std::env::set_var("TACHI_HOME", temp_home.temp_home.join(".tachi"));
    std::env::set_var("TACHI_RUN_ROOT", temp_home.temp_home.join(".tachi/runs"));

    let global_db = temp_home.temp_home.join(".tachi/global/memory.db");
    std::fs::create_dir_all(global_db.parent().expect("global db parent"))
        .expect("create global db dir");
    crate::tests::copy_template_db(&global_db);
    let server = crate::MemoryServer::new(global_db, None).expect("failed to create test server");
    (server, temp_home)
}

#[tokio::test]
async fn tachi_memory_recall_proposals_review_and_apply_config_env() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env.parent().expect("config env parent"))
        .expect("create config env parent");
    std::fs::write(
        &config_env,
        "VOYAGE_API_KEY=vault:VOYAGE_API_KEY\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\n",
    )
    .expect("seed config.env");

    server
        .with_global_store(|store| {
            let mut all_terms = make_entry("recall-proposal-all-terms");
            all_terms.path = "/scratch/tachi/recall-proposal-all-terms".to_string();
            all_terms.summary = "Recall proposal all terms".to_string();
            all_terms.text = "cleanup preview safe deployment note".to_string();
            all_terms.keywords = vec![
                "cleanup".to_string(),
                "preview".to_string(),
                "safe".to_string(),
            ];
            store.upsert(&all_terms).map_err(|e| e.to_string())?;

            let mut partial = make_entry("recall-proposal-partial-term");
            partial.path = "/scratch/tachi/recall-proposal-partial-term".to_string();
            partial.summary = "Recall proposal partial term".to_string();
            partial.text = "cleanup preview deletes stale artifacts".to_string();
            partial.keywords = Vec::new();
            store.upsert(&partial).map_err(|e| e.to_string())
        })
        .expect("seed recall proposal entries");

    let mut proposals = tachi_memory_params("recall_proposals");
    proposals.format = Some("json".to_string());
    proposals.scope = Some("memory".to_string());
    proposals.top_k = 3;
    proposals.force = true;
    proposals.metadata = Some(json!({
        "cases": [
            {
                "name": "partial-cleanup",
                "query": "cleanup preview safe",
                "expected_id": "recall-proposal-partial-term"
            }
        ],
        "variants": [
            {
                "name": "or-fallback-0.6",
                "recall_config": {
                    "or_fallback_fts_score_factor": 0.6,
                    "or_fallback_fts_max_terms": 4
                }
            }
        ]
    }));

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, proposals)
        .await
        .expect("recall proposals should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("proposal response JSON");
    let proposal = parsed["proposals"]
        .as_array()
        .expect("proposal list")
        .iter()
        .find(|proposal| proposal["variant"] == json!("or-fallback-0.6"))
        .unwrap_or_else(|| panic!("expected recall proposal in response: {parsed}"));
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();
    assert_eq!(proposal["kind"], json!("recall_config"));
    assert_eq!(
        proposal["config_env"]["TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR"],
        json!("0.6")
    );
    assert_eq!(
        proposal["config_env"]["TACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS"],
        json!("4")
    );

    let mut review = tachi_memory_params("review_recall_proposal");
    review.format = Some("json".to_string());
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    review.notes = Some("fixture approval".to_string());
    let review_body = crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("review should succeed");
    let review_json: Value = serde_json::from_str(&review_body).expect("review JSON");
    assert_eq!(review_json["proposal"]["status"], json!("approved"));
    assert!(
        review_json["proposal"]["expires_at"].is_null(),
        "#1342 follow-up: an approved-but-not-yet-applied proposal must stay \
         TTL-less until its own terminal (applied) write: {review_json}"
    );

    let mut missing_confirm = tachi_memory_params("apply_recall_proposals");
    missing_confirm.proposal_id = Some(proposal_id.clone());
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, missing_confirm)
        .await
        .expect_err("apply should require confirm=true");
    assert!(err.contains("confirm=true"));

    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.format = Some("json".to_string());
    apply.proposal_id = Some(proposal_id);
    apply.confirm = true;
    let apply_body = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect("apply should succeed");
    let apply_json: Value = serde_json::from_str(&apply_body).expect("apply JSON");
    assert_eq!(apply_json["restart_required"], json!(true));
    assert!(apply_json["updated_keys"]
        .as_array()
        .expect("updated keys")
        .contains(&json!("TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR")));
    // #1342 follow-up: `applied` is terminal, so this write must carry a TTL.
    let expires_at = apply_json["proposal"]["expires_at"]
        .as_str()
        .expect("applied recall config proposal must carry expires_at");
    assert!(
        chrono::DateTime::parse_from_rfc3339(expires_at).is_ok(),
        "expires_at must be a valid RFC3339 timestamp: {expires_at}"
    );

    let config_body = std::fs::read_to_string(&config_env).expect("read config.env");
    assert!(config_body.contains("VOYAGE_API_KEY=vault:VOYAGE_API_KEY"));
    assert!(config_body.contains("TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.6"));
    assert!(config_body.contains("TACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=4"));
}

/// #1342 follow-up: a rejected recall-config proposal is terminal (it will
/// never be applied), so its review write must carry a 30-day TTL
/// immediately — unlike `approved`, which must wait for the apply action's
/// own terminal write.
#[tokio::test]
async fn recall_proposal_reject_stamps_a_ttl_immediately() {
    let (server, _temp_home) = make_server_with_temp_home();

    server
        .with_global_store(|store| {
            let mut all_terms = make_entry("recall-reject-all-terms");
            all_terms.path = "/scratch/tachi/recall-reject-all-terms".to_string();
            all_terms.summary = "Recall reject all terms".to_string();
            all_terms.text = "cleanup preview safe deployment note".to_string();
            all_terms.keywords = vec![
                "cleanup".to_string(),
                "preview".to_string(),
                "safe".to_string(),
            ];
            store.upsert(&all_terms).map_err(|e| e.to_string())?;

            let mut partial = make_entry("recall-reject-partial-term");
            partial.path = "/scratch/tachi/recall-reject-partial-term".to_string();
            partial.summary = "Recall reject partial term".to_string();
            partial.text = "cleanup preview deletes stale artifacts".to_string();
            partial.keywords = Vec::new();
            store.upsert(&partial).map_err(|e| e.to_string())
        })
        .expect("seed recall proposal entries");

    let metadata = json!({
        "cases": [
            {
                "name": "partial-cleanup",
                "query": "cleanup preview safe",
                "expected_id": "recall-reject-partial-term"
            }
        ],
        "variants": [
            {
                "name": "or-fallback-0.6",
                "recall_config": {
                    "or_fallback_fts_score_factor": 0.6,
                    "or_fallback_fts_max_terms": 4
                }
            }
        ]
    });

    let mut proposals = tachi_memory_params("recall_proposals");
    proposals.format = Some("json".to_string());
    proposals.scope = Some("memory".to_string());
    proposals.top_k = 3;
    proposals.force = true;
    proposals.metadata = Some(metadata.clone());
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, proposals)
        .await
        .expect("recall proposals should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("proposal response JSON");
    let proposal = parsed["proposals"]
        .as_array()
        .expect("proposal list")
        .iter()
        .find(|proposal| proposal["variant"] == json!("or-fallback-0.6"))
        .unwrap_or_else(|| panic!("expected recall proposal in response: {parsed}"));
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();

    let mut review = tachi_memory_params("review_recall_proposal");
    review.format = Some("json".to_string());
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("rejected".to_string());
    let review_body = crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("review should succeed");
    let review_json: Value = serde_json::from_str(&review_body).expect("review JSON");
    assert_eq!(review_json["proposal"]["status"], json!("rejected"));

    let expires_at = review_json["proposal"]["expires_at"]
        .as_str()
        .expect("a rejected (terminal) recall proposal must carry expires_at immediately")
        .to_string();
    assert!(
        chrono::DateTime::parse_from_rfc3339(&expires_at).is_ok(),
        "expires_at must be a valid RFC3339 timestamp: {expires_at}"
    );

    // #1342 follow-up BUG (cross-vendor review): re-generating proposals
    // (the same eval-input path a repeat `tachi_memory(action='recall_proposals',
    // ...)` call takes) used to rewrite this now-terminal row from scratch,
    // silently erasing its `expires_at` — which the next maintenance tick's
    // idempotent backfill would then re-stamp with a brand-new `now+30d`.
    // Refreshing before the original TTL elapsed meant the row's expiry
    // never actually arrived. The proposal_id is deterministic (hash of
    // variant name + config_env), so re-submitting identical metadata must
    // hit the SAME id and must NOT change its `expires_at` at all.
    let mut regenerate = tachi_memory_params("recall_proposals");
    regenerate.format = Some("json".to_string());
    regenerate.scope = Some("memory".to_string());
    regenerate.top_k = 3;
    regenerate.force = true;
    regenerate.metadata = Some(metadata);
    let regen_body = crate::facade_memory_ops::handle_tachi_memory(&server, regenerate)
        .await
        .expect("recall proposals regenerate should succeed");
    let regen_parsed: Value = serde_json::from_str(&regen_body).expect("regen response JSON");
    let regen_proposal = regen_parsed["proposals"]
        .as_array()
        .expect("proposal list")
        .iter()
        .find(|proposal| proposal["proposal_id"] == json!(proposal_id))
        .unwrap_or_else(|| {
            panic!("expected the same proposal_id after regenerate: {regen_parsed}")
        });
    assert_eq!(
        regen_proposal["status"],
        json!("rejected"),
        "the rejected status itself must also survive the regenerate refresh"
    );
    assert_eq!(
        regen_proposal["expires_at"].as_str(),
        Some(expires_at.as_str()),
        "expires_at must be the ORIGINAL value verbatim after a regenerate refresh, not merely \
         present and not a freshly recomputed timestamp: {regen_proposal}"
    );
}

/// Local mirror of `recall_proposal_ops::compute_recall_digest`. The recovery
/// path fingerprints the complete config.env source and compares it to the
/// applying_receipt's before/after digests, so the receipt we stamp in these
/// tests must use the identical algorithm. Kept local (rather than reaching
/// into the production module) to avoid broadening this PR's file scope to
/// facade_memory_ops/mod.rs.
fn test_recall_digest_of(path: &std::path::Path) -> String {
    let body = match std::fs::read_to_string(path) {
        Ok(body) => body,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => panic!("read config.env {}: {err}", path.display()),
    };
    test_config_env_digest(&body)
}

/// Local mirror of `recall_proposal_ops::digest_config_env_source`. Used by
/// the recovery test to compute an `after_digest` without mutating the config
/// file, so the receipt covers the same complete source that production will
/// replace.
fn test_config_env_digest(body: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());
    let bytes = hasher.finalize();
    let mut out = String::with_capacity(2 * bytes.len());
    for byte in bytes {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

// ─── v3 proposal-safety discrimination tests ─────────────────────────────────
//
// These tests pin the recall-config side of the v3 content-addressed proposal
// safety contract. They are written but NOT run by this lane — the leader
// runs the full suite in the delivery worktree. Each names the production
// path it bites and the red->green property it discriminates.

/// Shared seed helper for the recall discrimination tests: drops the two
/// partial/all-terms entries the simulation matches against.
fn seed_recall_pair(server: &crate::MemoryServer, suffix: &str) {
    server
        .with_global_store(|store| {
            let mut all_terms = make_entry(&format!("recall-{suffix}-all-terms"));
            all_terms.path = format!("/scratch/tachi/recall-{suffix}-all-terms");
            all_terms.summary = format!("Recall {suffix} all terms");
            all_terms.text = "cleanup preview safe deployment note".to_string();
            all_terms.keywords = vec![
                "cleanup".to_string(),
                "preview".to_string(),
                "safe".to_string(),
            ];
            store.upsert(&all_terms).map_err(|e| e.to_string())?;

            let mut partial = make_entry(&format!("recall-{suffix}-partial-term"));
            partial.path = format!("/scratch/tachi/recall-{suffix}-partial-term");
            partial.summary = format!("Recall {suffix} partial term");
            partial.text = "cleanup preview deletes stale artifacts".to_string();
            partial.keywords = Vec::new();
            store.upsert(&partial).map_err(|e| e.to_string())
        })
        .expect("seed recall pair");
}

/// Discrimination: a recall-config proposal whose reviewed evidence changes
/// (here: different `case_count` / `cases` fed to the simulation) must NOT
/// inherit a prior approval, even if the proposed `config_env` is otherwise
/// identical. The v3 identity binds `evidence_review`, so a different
/// evidence snapshot rotates the SHA-256 id and starts a fresh pending row.
///
/// Production path: `build_proposals_from_simulation` ->
/// `recall_config_v3_identity_payload` binds the simulation evidence
/// (case_count / top_k / variant_cases / metrics) alongside the config_env.
/// Pre-fix red: the proposal id was `recall_config:<variant>:<fnv(config_env)>`,
/// which depended only on the config_env bytes — so an approval stamped
/// against evidence A silently carried over to a regenerated proposal that
/// the human had reviewed against a different evidence B.
/// Post-fix green: the v3 id rotates when the evidence rotates, and the
/// regenerated proposal at the new id starts pending.
#[tokio::test]
async fn recall_regen_with_changed_evidence_does_not_inherit_approval() {
    let (server, _temp_home) = make_server_with_temp_home();
    seed_recall_pair(&server, "evidence");

    // Phase A: one case in the simulation.
    let metadata_a = json!({
        "cases": [
            {
                "name": "partial-cleanup",
                "query": "cleanup preview safe",
                "expected_id": "recall-evidence-partial-term"
            }
        ],
        "variants": [
            {
                "name": "or-fallback-0.6",
                "recall_config": {
                    "or_fallback_fts_score_factor": 0.6,
                    "or_fallback_fts_max_terms": 4
                }
            }
        ]
    });

    let mut proposals_a = tachi_memory_params("recall_proposals");
    proposals_a.format = Some("json".to_string());
    proposals_a.scope = Some("memory".to_string());
    proposals_a.top_k = 3;
    proposals_a.force = true;
    proposals_a.metadata = Some(metadata_a);
    let body_a = crate::facade_memory_ops::handle_tachi_memory(&server, proposals_a)
        .await
        .expect("recall proposals A");
    let parsed_a: Value = serde_json::from_str(&body_a).expect("proposal response A JSON");
    let proposal_a = parsed_a["proposals"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|p| p["variant"] == json!("or-fallback-0.6"))
        })
        .expect("phase A proposal");
    let id_a = proposal_a["proposal_id"]
        .as_str()
        .expect("id A")
        .to_string();
    assert!(
        id_a.starts_with("recall_config:v3:"),
        "v3 id format expected, got: {id_a}"
    );
    assert_eq!(proposal_a["schema_version"], json!(3));

    // Approve proposal A.
    let mut review_a = tachi_memory_params("review_recall_proposal");
    review_a.format = Some("json".to_string());
    review_a.proposal_id = Some(id_a.clone());
    review_a.review_status = Some("approved".to_string());
    let _ = crate::facade_memory_ops::handle_tachi_memory(&server, review_a)
        .await
        .expect("approve A");

    // Phase B: TWO cases in the simulation (different evidence). Same variant
    // and same config_env, so the apply payload is identical — only the
    // reviewed evidence rotates.
    let metadata_b = json!({
        "cases": [
            {
                "name": "partial-cleanup",
                "query": "cleanup preview safe",
                "expected_id": "recall-evidence-partial-term"
            },
            {
                "name": "all-terms",
                "query": "cleanup preview safe",
                "expected_id": "recall-evidence-all-terms"
            }
        ],
        "variants": [
            {
                "name": "or-fallback-0.6",
                "recall_config": {
                    "or_fallback_fts_score_factor": 0.6,
                    "or_fallback_fts_max_terms": 4
                }
            }
        ]
    });

    let mut proposals_b = tachi_memory_params("recall_proposals");
    proposals_b.format = Some("json".to_string());
    proposals_b.scope = Some("memory".to_string());
    proposals_b.top_k = 3;
    proposals_b.force = true;
    proposals_b.metadata = Some(metadata_b);
    let body_b = crate::facade_memory_ops::handle_tachi_memory(&server, proposals_b)
        .await
        .expect("recall proposals B");
    let parsed_b: Value = serde_json::from_str(&body_b).expect("proposal response B JSON");
    let proposal_b = parsed_b["proposals"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|p| p["variant"] == json!("or-fallback-0.6"))
        })
        .expect("phase B proposal");
    let id_b = proposal_b["proposal_id"]
        .as_str()
        .expect("id B")
        .to_string();

    assert_ne!(
        id_a, id_b,
        "the v3 id must rotate when the reviewed evidence changes, even if the          config_env is otherwise identical"
    );
    assert_eq!(
        proposal_b["status"],
        json!("pending"),
        "the regenerated proposal at the new v3 id must NOT inherit the prior approval"
    );
    assert_eq!(proposal_b["schema_version"], json!(3));
}

/// Discrimination: a recall-config apply that crashed between the descriptor
/// append and the finalize CAS must recover to `applied` exactly once when
/// the apply is re-driven. The recovery path reads the live config.env
/// digest, observes it equals the receipt's `after_digest`, and finalizes
/// idempotently — never re-writing the file, never refusing.
///
/// Production path: `drive_recall_apply_state_machine` recovery branch
/// (observed == after_digest -> FinalizedExisting).
/// Pre-fix red: there was no `applying` state at all — the old apply wrote
/// the config.env first and then the proposal row, so a crash between the
/// two left the config mutated and the row still `approved`, which a re-apply
/// would happily do again (double write, double stamp).
/// Post-fix green: the recovery reads the receipt, sees the file is already
/// at the after state, and finalizes once.
#[tokio::test]
async fn recall_apply_crash_after_rename_recovers_to_applied_once() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env_path = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env_path.parent().expect("parent"))
        .expect("create config env parent");
    std::fs::write(
        &config_env_path,
        "VOYAGE_API_KEY=vault:VOYAGE_API_KEY
TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1
",
    )
    .expect("seed config.env");

    seed_recall_pair(&server, "crash");

    let metadata = json!({
        "cases": [
            {
                "name": "partial-cleanup",
                "query": "cleanup preview safe",
                "expected_id": "recall-crash-partial-term"
            }
        ],
        "variants": [
            {
                "name": "or-fallback-0.6",
                "recall_config": {
                    "or_fallback_fts_score_factor": 0.6,
                    "or_fallback_fts_max_terms": 4
                }
            }
        ]
    });

    let mut proposals = tachi_memory_params("recall_proposals");
    proposals.format = Some("json".to_string());
    proposals.scope = Some("memory".to_string());
    proposals.top_k = 3;
    proposals.force = true;
    proposals.metadata = Some(metadata);
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, proposals)
        .await
        .expect("recall proposals");
    let parsed: Value = serde_json::from_str(&body).expect("proposal JSON");
    let proposal = parsed["proposals"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|p| p["variant"] == json!("or-fallback-0.6"))
        })
        .expect("proposal");
    let proposal_id = proposal["proposal_id"].as_str().expect("id").to_string();

    // Approve.
    let mut review = tachi_memory_params("review_recall_proposal");
    review.format = Some("json".to_string());
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    let _ = crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("approve");

    // Simulate the crash mid-apply: the proposal row is in `applying` with a
    // receipt, and the config file has already been appended to the after
    // state. We compute the same digests the production code would, then
    // hand-stamp the row.
    let patch: std::collections::BTreeMap<String, String> = proposal["config_env"]
        .as_object()
        .expect("config_env object")
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().expect("string").to_string()))
        .collect();
    let before_digest = test_recall_digest_of(&config_env_path);
    let before_len =
        "VOYAGE_API_KEY=vault:VOYAGE_API_KEY\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\n"
            .len();
    let append_payload =
        "TACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=4\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.6\n";
    // Append the patch by hand so we can capture the after_digest.
    std::fs::write(
        &config_env_path,
        "VOYAGE_API_KEY=vault:VOYAGE_API_KEY
TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1
TACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=4
TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.6
",
    )
    .expect("write simulated appended state");
    let after_digest = test_recall_digest_of(&config_env_path);

    server
        .with_global_store(|store| {
            let (raw, version) = store
                .get_state_kv("recall_config_proposals", &proposal_id)
                .map_err(|e| e.to_string())?
                .expect("row");
            let mut value: Value = serde_json::from_str(&raw).expect("json");
            value["status"] = json!("applying");
            value["applying_receipt"] = json!({
                "attempt_id": "crash-recovery-fixture",
                "before_digest": before_digest,
                "after_digest": after_digest,
                "before_len": before_len,
                "append_payload": append_payload,
                "updated_keys": patch.keys().cloned().collect::<Vec<_>>(),
                "started_at": "2026-07-25T00:00:00Z",
            });
            let next = serde_json::to_string(&value).expect("serialize");
            store
                .set_state("recall_config_proposals", &proposal_id, &next)
                .map_err(|e| e.to_string())?;
            // silence unused-version warning while keeping the snapshot for
            // future assertions on version stability.
            let _ = version;
            Ok::<_, String>(())
        })
        .expect("stamp applying row");

    // Re-drive the apply: recovery must observe after_digest and finalize.
    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.format = Some("json".to_string());
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect("recovery apply should finalize");
    let applied: Value = serde_json::from_str(&body).expect("apply JSON");
    assert_eq!(applied["proposal"]["status"], json!("applied"));
    assert_eq!(
        applied["apply_outcome"],
        json!("applied_finalized_existing"),
        "recovery against an already-after file must report idempotent finalize"
    );
    assert_eq!(
        applied["attempt_id"],
        json!("crash-recovery-fixture"),
        "the terminal receipt carries the original attempt id, not a new one"
    );
}

/// A persisted receipt is not authority to bless arbitrary bytes. Even when
/// its forged after_digest equals the live third-party source, recovery must
/// reject a receipt suffix that omits the separator required by the approved
/// no-final-newline source.
#[tokio::test]
async fn recall_apply_forged_after_digest_cannot_finalize_third_party_source() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env_path = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env_path.parent().expect("config parent"))
        .expect("create config parent");
    let source = "VOYAGE_API_KEY=vault:provider\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1";
    std::fs::write(&config_env_path, source).expect("seed approved source");
    seed_recall_pair(&server, "forged-after-digest");

    let proposal = generate_recall_source_proposal(&server, "forged-after-digest").await;
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();
    let mut review = tachi_memory_params("review_recall_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("approve proposal");

    let append_payload =
        "TACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=4\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.6\n";
    let third_party = format!("{source}{append_payload}");
    std::fs::write(&config_env_path, &third_party).expect("write third-party source");
    server
        .with_global_store(|store| {
            let (raw, _version) = store
                .get_state_kv("recall_config_proposals", &proposal_id)
                .map_err(|err| err.to_string())?
                .expect("proposal row");
            let mut value: Value = serde_json::from_str(&raw).expect("proposal JSON");
            value["status"] = json!("applying");
            value["applying_receipt"] = json!({
                "attempt_id": "forged-after-digest-fixture",
                "before_digest": test_config_env_digest(source),
                "after_digest": test_config_env_digest(&third_party),
                "before_len": source.len(),
                "append_payload": append_payload,
                "updated_keys": [
                    "TACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS",
                    "TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR",
                ],
                "started_at": "2026-07-25T00:00:00Z",
            });
            store
                .set_state(
                    "recall_config_proposals",
                    &proposal_id,
                    &serde_json::to_string(&value).expect("serialize forged receipt"),
                )
                .map_err(|err| err.to_string())
        })
        .expect("persist forged receipt");
    let applying_row = read_recall_row(&server, &proposal_id);

    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect_err("forged after_digest must not authorize third-party bytes");
    assert!(
        err.contains("invalid_applying_receipt"),
        "unexpected forged-receipt refusal: {err}"
    );
    assert_eq!(
        read_recall_row(&server, &proposal_id),
        applying_row,
        "refusal must leave the applying receipt recoverable"
    );
    assert_eq!(
        std::fs::read_to_string(&config_env_path).expect("read third-party source"),
        third_party,
        "recovery refusal must not rewrite third-party bytes"
    );
}

/// Discrimination: a recall-config apply that crashed BEFORE the rename
/// recovers by redoing the file write against the known-clean before state
/// (safe retry); a recall-config apply whose file drifted to anything other
/// than the receipt's before or after digest refuses loudly with
/// `third_party_drift`.
///
/// Production path: `drive_recall_apply_state_machine` recovery branches
/// (observed == before -> Retried; else -> third_party_drift refusal).
/// Pre-fix red: no applying state and no digest check, so a half-applied row
/// would silently re-do the file write even if a third party had changed the
/// recall keys between the crash and the retry.
/// Post-fix green: the digest check distinguishes retry-safe from drifted.
#[tokio::test]
async fn recall_apply_before_digest_retries_and_third_party_drift_refuses() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env_path = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env_path.parent().expect("parent"))
        .expect("create config env parent");
    std::fs::write(
        &config_env_path,
        "VOYAGE_API_KEY=vault:VOYAGE_API_KEY
TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1
",
    )
    .expect("seed config.env");

    seed_recall_pair(&server, "drift");

    let metadata = json!({
        "cases": [
            {
                "name": "partial-cleanup",
                "query": "cleanup preview safe",
                "expected_id": "recall-drift-partial-term"
            }
        ],
        "variants": [
            {
                "name": "or-fallback-0.6",
                "recall_config": {
                    "or_fallback_fts_score_factor": 0.6,
                    "or_fallback_fts_max_terms": 4
                }
            }
        ]
    });

    let mut proposals = tachi_memory_params("recall_proposals");
    proposals.format = Some("json".to_string());
    proposals.scope = Some("memory".to_string());
    proposals.top_k = 3;
    proposals.force = true;
    proposals.metadata = Some(metadata);
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, proposals)
        .await
        .expect("recall proposals");
    let parsed: Value = serde_json::from_str(&body).expect("proposal JSON");
    let proposal = parsed["proposals"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|p| p["variant"] == json!("or-fallback-0.6"))
        })
        .expect("proposal");
    let proposal_id = proposal["proposal_id"].as_str().expect("id").to_string();

    // Approve.
    let mut review = tachi_memory_params("review_recall_proposal");
    review.format = Some("json".to_string());
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    let _ = crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("approve");

    let before_digest = test_recall_digest_of(&config_env_path);
    let before_len =
        "VOYAGE_API_KEY=vault:VOYAGE_API_KEY\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\n"
            .len();
    let append_payload =
        "TACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=4\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.6\n";
    // Compute the projected after_digest WITHOUT mutating the file. The
    // append protocol preserves the source and adds the approved assignments
    // in deterministic key order.
    let projected = test_config_env_digest(
        "VOYAGE_API_KEY=vault:VOYAGE_API_KEY\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\nTACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=4\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.6\n",
    );

    // (a) before-digest retry: file still at before state, row at applying
    // with receipt. Apply should redo the write and finalize.
    server
        .with_global_store(|store| {
            let (raw, _version) = store
                .get_state_kv("recall_config_proposals", &proposal_id)
                .map_err(|e| e.to_string())?
                .expect("row");
            let mut value: Value = serde_json::from_str(&raw).expect("json");
            value["status"] = json!("applying");
            value["applying_receipt"] = json!({
                "attempt_id": "retry-fixture",
                "before_digest": before_digest,
                "after_digest": projected,
                "before_len": before_len,
                "append_payload": append_payload,
                "updated_keys": [
                    "TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR",
                    "TACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS",
                ],
                "started_at": "2026-07-25T00:00:00Z",
            });
            let next = serde_json::to_string(&value).expect("serialize");
            store
                .set_state("recall_config_proposals", &proposal_id, &next)
                .map_err(|e| e.to_string())
        })
        .expect("stamp applying row");

    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.format = Some("json".to_string());
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect("retry apply should succeed");
    let applied: Value = serde_json::from_str(&body).expect("apply JSON");
    assert_eq!(applied["proposal"]["status"], json!("applied"));
    assert_eq!(
        applied["apply_outcome"],
        json!("applied_retried"),
        "observed==before_digest must surface as a retried apply, not a fresh one"
    );
    assert_eq!(applied["attempt_id"], json!("retry-fixture"));

    // (b) third-party drift: now move the file to a state that matches
    // NEITHER the before nor the after digest, re-stamp the row at applying,
    // and verify the next apply refuses with third_party_drift.
    std::fs::write(
        &config_env_path,
        "VOYAGE_API_KEY=vault:VOYAGE_API_KEY
TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.424242
TACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=99
",
    )
    .expect("write drifted config");

    server
        .with_global_store(|store| {
            let (raw, _version) = store
                .get_state_kv("recall_config_proposals", &proposal_id)
                .map_err(|e| e.to_string())?
                .expect("row");
            let mut value: Value = serde_json::from_str(&raw).expect("json");
            value["status"] = json!("applying");
            value["applying_receipt"] = json!({
                "attempt_id": "drift-fixture",
                "before_digest": before_digest,
                "after_digest": projected,
                "before_len": before_len,
                "append_payload": append_payload,
                "updated_keys": [
                    "TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR",
                    "TACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS",
                ],
                "started_at": "2026-07-25T00:00:00Z",
            });
            // Drop the expires_at the terminal write stamped in (a) so the
            // maintenance reaper does not delete the row before the next
            // assertion, and reset status back to applying for the drift
            // scenario.
            let next = serde_json::to_string(&value).expect("serialize");
            store
                .set_state("recall_config_proposals", &proposal_id, &next)
                .map_err(|e| e.to_string())
        })
        .expect("re-stamp applying row for drift");

    let mut drift_apply = tachi_memory_params("apply_recall_proposals");
    drift_apply.format = Some("json".to_string());
    drift_apply.proposal_id = Some(proposal_id.clone());
    drift_apply.confirm = true;
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, drift_apply)
        .await
        .expect_err("a drifted config file must refuse to finalize");
    assert!(
        err.contains("third_party_drift"),
        "expected a third_party_drift refusal, got: {err}"
    );
}

/// Discrimination: a legacy (pre-v3) recall-config proposal — even one that
/// was approved under the old schema — must be refused at apply with
/// `legacy_unbound_proposal`, never silently inheriting the old approval.
///
/// Production path: `drive_recall_apply_state_machine` -> schema_version guard.
/// Pre-fix red: apply only checked `status == "approved"`, so a legacy
/// approved row applied without any content-addressed binding.
/// Post-fix green: the schema_version guard refuses the legacy row loudly.
#[tokio::test]
async fn legacy_approved_recall_proposal_cannot_be_applied() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env_path = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env_path.parent().expect("parent"))
        .expect("create config env parent");
    std::fs::write(
        &config_env_path,
        "VOYAGE_API_KEY=vault:VOYAGE_API_KEY
TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1
",
    )
    .expect("seed config.env");

    let proposal_id = "recall_config:or-fallback-0.6:deadbeefdeadbeef";
    let legacy = serde_json::json!({
        "proposal_id": proposal_id,
        "kind": "recall_config",
        "status": "approved",
        "review": {"status": "approved"},
        "variant": "or-fallback-0.6",
        "config_env": {
            "TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR": "0.6",
            "TACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS": "4",
        },
        "evidence": {"source": "legacy"},
    });
    server
        .with_global_store(|store| {
            store
                .set_state("recall_config_proposals", proposal_id, &legacy.to_string())
                .map_err(|e| e.to_string())
        })
        .expect("seed legacy approved recall proposal");

    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.proposal_id = Some(proposal_id.to_string());
    apply.confirm = true;
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect_err("legacy approved recall proposal must refuse to apply");
    assert!(
        err.contains("legacy_unbound_proposal"),
        "expected legacy_unbound_proposal refusal, got: {err}"
    );

    let config_body = std::fs::read_to_string(&config_env_path).expect("read config.env");
    assert!(
        config_body.contains("TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1"),
        "the legacy refusal must not have mutated config.env"
    );
}

/// Read a raw recall-config proposal row (JSON string, state_version) for
/// direct post-condition assertions the facade response does not surface
/// (byte-identical row, exact version stability).
fn read_recall_row(server: &crate::MemoryServer, proposal_id: &str) -> (String, u32) {
    server
        .with_global_store_read(|store| {
            store
                .get_state_kv("recall_config_proposals", proposal_id)
                .map_err(|e| e.to_string())
        })
        .expect("read recall proposal row")
        .expect("recall proposal row present")
}

async fn generate_recall_source_proposal(
    server: &crate::MemoryServer,
    suffix: &str,
) -> serde_json::Value {
    let proposals = recall_source_proposal_params(suffix);
    let body = crate::facade_memory_ops::handle_tachi_memory(server, proposals)
        .await
        .expect("generate recall proposal");
    let parsed: Value = serde_json::from_str(&body).expect("recall proposal JSON");
    parsed["proposals"]
        .as_array()
        .and_then(|proposals| {
            proposals
                .iter()
                .find(|proposal| proposal["variant"] == json!("or-fallback-0.6"))
        })
        .cloned()
        .expect("recall source proposal")
}

fn recall_source_proposal_params(suffix: &str) -> crate::tool_params::TachiMemoryParams {
    let mut proposals = tachi_memory_params("recall_proposals");
    proposals.format = Some("json".to_string());
    proposals.scope = Some("memory".to_string());
    proposals.top_k = 3;
    proposals.force = true;
    proposals.metadata = Some(json!({
        "cases": [{
            "name": "partial-cleanup",
            "query": "cleanup preview safe",
            "expected_id": format!("recall-{suffix}-partial-term"),
        }],
        "variants": [{
            "name": "or-fallback-0.6",
            "recall_config": {
                "or_fallback_fts_score_factor": 0.6,
                "or_fallback_fts_max_terms": 4,
            },
        }],
    }));
    proposals
}

fn test_recall_content_digest(identity_payload: &Value) -> String {
    let canonical = tachi_dispatch::policy::canonical_json(identity_payload).to_string();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Discrimination: once a recall-config proposal is in a terminal state
/// (`rejected`), a second review call attempting to move it to `approved`
/// must be refused, and the refusal must not mutate the row at all — not the
/// status, not the state_version.
///
/// Production path: `handle_recall_config_review` -> `current_status !=
/// "pending"` guard, which runs before the `set_state_if_version` write.
/// Pre-fix red (this specific gap, not the guard's existence): the guard
/// itself was untested — nothing in this file drove a proposal into
/// `rejected` and then attempted a second review, so a regression that
/// dropped or weakened the guard would not have been caught here even though
/// route_policy's equivalent guard has direct test coverage.
/// Post-fix green: the second review is refused with a terminal-state error
/// and the stored row (including state_version) is byte-identical before and
/// after the refused attempt.
#[tokio::test]
async fn recall_terminal_state_cannot_be_rereviewed() {
    let (server, _temp_home) = make_server_with_temp_home();
    seed_recall_pair(&server, "terminal");

    let metadata = json!({
        "cases": [
            {
                "name": "partial-cleanup",
                "query": "cleanup preview safe",
                "expected_id": "recall-terminal-partial-term"
            }
        ],
        "variants": [
            {
                "name": "or-fallback-0.6",
                "recall_config": {
                    "or_fallback_fts_score_factor": 0.6,
                    "or_fallback_fts_max_terms": 4
                }
            }
        ]
    });

    let mut proposals = tachi_memory_params("recall_proposals");
    proposals.format = Some("json".to_string());
    proposals.scope = Some("memory".to_string());
    proposals.top_k = 3;
    proposals.force = true;
    proposals.metadata = Some(metadata);
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, proposals)
        .await
        .expect("recall proposals");
    let parsed: Value = serde_json::from_str(&body).expect("proposal JSON");
    let proposal = parsed["proposals"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|p| p["variant"] == json!("or-fallback-0.6"))
        })
        .expect("proposal");
    let proposal_id = proposal["proposal_id"].as_str().expect("id").to_string();

    // Move the proposal to terminal "rejected".
    let mut reject = tachi_memory_params("review_recall_proposal");
    reject.proposal_id = Some(proposal_id.clone());
    reject.review_status = Some("rejected".to_string());
    let _ = crate::facade_memory_ops::handle_tachi_memory(&server, reject)
        .await
        .expect("reject");

    let (before_raw, before_version) = read_recall_row(&server, &proposal_id);

    // Re-review the rejected row: must refuse with a terminal-state error.
    let mut re_approve = tachi_memory_params("review_recall_proposal");
    re_approve.proposal_id = Some(proposal_id.clone());
    re_approve.review_status = Some("approved".to_string());
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, re_approve)
        .await
        .expect_err("a rejected recall proposal must not be re-reviewable");
    assert!(
        err.contains("terminal state"),
        "expected a terminal-state refusal, got: {err}"
    );

    // Post-invariant: the refusal must not have mutated the row at all.
    let (after_raw, after_version) = read_recall_row(&server, &proposal_id);
    assert_eq!(
        before_version, after_version,
        "a refused re-review must not bump the row's state_version"
    );
    assert_eq!(
        before_raw, after_raw,
        "a refused re-review must not mutate the stored row"
    );
    let after_value: Value = serde_json::from_str(&after_raw).expect("row json");
    assert_eq!(
        after_value["status"],
        json!("rejected"),
        "status must remain rejected, not silently become approved"
    );
}

/// Discrimination: applying an already-`applied` recall-config proposal a
/// second time must be refused terminally-idempotent — no second file
/// mutation, no second `applied_at` stamp, no second terminal receipt.
/// `drive_recall_apply_state_machine`'s `match status` has arms for
/// `"approved"` (fresh apply) and `"applying"` (crash recovery) only; a row
/// already at `"applied"` falls into the `other` catch-all and is refused.
///
/// Production path: `drive_recall_apply_state_machine` `other => Err(...)`
/// arm, reached because the first apply's `finalize_recall_apply` already
/// moved `status` to `"applied"`.
/// Pre-fix red (this specific gap): nothing in this file drove two applies of
/// the same proposal back-to-back, so a regression that let a second apply
/// silently repeat the file write (or re-stamp `applied_at`/`attempt_id`)
/// would not have been caught even though route_policy's equivalent
/// double-apply path has direct test coverage.
/// Post-fix green: the second apply is refused, config.env is byte-identical
/// before/after the second call, and the row's `applied_at` /
/// `apply_result.attempt_id` remain the first apply's values.
#[tokio::test]
async fn recall_two_applies_yield_one_terminal_receipt() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env_path = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env_path.parent().expect("parent"))
        .expect("create config env parent");
    std::fs::write(
        &config_env_path,
        "VOYAGE_API_KEY=vault:VOYAGE_API_KEY\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\n",
    )
    .expect("seed config.env");

    seed_recall_pair(&server, "two-apply");

    let metadata = json!({
        "cases": [
            {
                "name": "partial-cleanup",
                "query": "cleanup preview safe",
                "expected_id": "recall-two-apply-partial-term"
            }
        ],
        "variants": [
            {
                "name": "or-fallback-0.6",
                "recall_config": {
                    "or_fallback_fts_score_factor": 0.6,
                    "or_fallback_fts_max_terms": 4
                }
            }
        ]
    });

    let mut proposals = tachi_memory_params("recall_proposals");
    proposals.format = Some("json".to_string());
    proposals.scope = Some("memory".to_string());
    proposals.top_k = 3;
    proposals.force = true;
    proposals.metadata = Some(metadata);
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, proposals)
        .await
        .expect("recall proposals");
    let parsed: Value = serde_json::from_str(&body).expect("proposal JSON");
    let proposal = parsed["proposals"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|p| p["variant"] == json!("or-fallback-0.6"))
        })
        .expect("proposal");
    let proposal_id = proposal["proposal_id"].as_str().expect("id").to_string();

    let mut review = tachi_memory_params("review_recall_proposal");
    review.format = Some("json".to_string());
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    let _ = crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("approve");

    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.format = Some("json".to_string());
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let first_body = crate::facade_memory_ops::handle_tachi_memory(&server, apply.clone())
        .await
        .expect("apply #1");
    let first: Value = serde_json::from_str(&first_body).expect("apply #1 JSON");
    assert_eq!(first["proposal"]["status"], json!("applied"));
    let first_applied_at = first["proposal"]["applied_at"]
        .as_str()
        .expect("applied_at present")
        .to_string();
    let first_attempt_id = first["attempt_id"]
        .as_str()
        .expect("attempt_id present")
        .to_string();

    let config_after_first = std::fs::read_to_string(&config_env_path).expect("read config.env");

    // Second apply of the same (now-applied) proposal must be refused.
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect_err("second apply of an applied proposal must be refused");
    assert!(
        err.contains("cannot be applied from status 'applied'"),
        "expected an 'applied' terminal-status refusal, got: {err}"
    );

    // Post-invariant: no second file mutation, no second terminal receipt.
    let config_after_second = std::fs::read_to_string(&config_env_path).expect("read config.env");
    assert_eq!(
        config_after_first, config_after_second,
        "a refused second apply must not mutate config.env again"
    );
    let (raw, _version) = read_recall_row(&server, &proposal_id);
    let row: Value = serde_json::from_str(&raw).expect("row json");
    assert_eq!(row["status"], json!("applied"));
    assert_eq!(
        row["applied_at"].as_str(),
        Some(first_applied_at.as_str()),
        "applied_at must remain the first apply's timestamp, not a second stamp"
    );
    assert_eq!(
        row["apply_result"]["attempt_id"].as_str(),
        Some(first_attempt_id.as_str()),
        "exactly one terminal receipt: apply_result.attempt_id must still be the first attempt"
    );
}

/// Discrimination: a recall-config proposal row whose `identity_payload` was
/// mutated after review (a hand-edit, a partial write) without recomputing
/// `content_digest` must refuse to apply with `content_digest_mismatch`,
/// never silently applying content the human never actually reviewed.
///
/// Production path: `drive_recall_apply_state_machine` -> recompute
/// `content_digest_hex(identity_payload)` and compare against the stored
/// `content_digest` before touching config.env.
/// Pre-fix red (this specific gap): the digest-recheck code at apply existed
/// but had zero test coverage in this file — a regression that dropped the
/// comparison (or always treated it as a match) would not have been caught.
/// Post-fix green: the mismatch is refused and config.env is untouched.
#[tokio::test]
async fn recall_apply_content_digest_mismatch_refuses() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env_path = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env_path.parent().expect("parent"))
        .expect("create config env parent");
    std::fs::write(
        &config_env_path,
        "VOYAGE_API_KEY=vault:VOYAGE_API_KEY\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\n",
    )
    .expect("seed config.env");

    seed_recall_pair(&server, "digest-mismatch");

    let metadata = json!({
        "cases": [
            {
                "name": "partial-cleanup",
                "query": "cleanup preview safe",
                "expected_id": "recall-digest-mismatch-partial-term"
            }
        ],
        "variants": [
            {
                "name": "or-fallback-0.6",
                "recall_config": {
                    "or_fallback_fts_score_factor": 0.6,
                    "or_fallback_fts_max_terms": 4
                }
            }
        ]
    });

    let mut proposals = tachi_memory_params("recall_proposals");
    proposals.format = Some("json".to_string());
    proposals.scope = Some("memory".to_string());
    proposals.top_k = 3;
    proposals.force = true;
    proposals.metadata = Some(metadata);
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, proposals)
        .await
        .expect("recall proposals");
    let parsed: Value = serde_json::from_str(&body).expect("proposal JSON");
    let proposal = parsed["proposals"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|p| p["variant"] == json!("or-fallback-0.6"))
        })
        .expect("proposal");
    let proposal_id = proposal["proposal_id"].as_str().expect("id").to_string();

    // Approve while the row is still clean (digest matches).
    let mut review = tachi_memory_params("review_recall_proposal");
    review.format = Some("json".to_string());
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    let _ = crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("approve");

    // Tamper the row AFTER review: mutate identity_payload without
    // recomputing content_digest, simulating a hand-edit / partial write that
    // landed between review and apply.
    server
        .with_global_store(|store| {
            let (raw, _version) = store
                .get_state_kv("recall_config_proposals", &proposal_id)
                .map_err(|e| e.to_string())?
                .expect("row");
            let mut value: Value = serde_json::from_str(&raw).expect("json");
            value["identity_payload"]["tampered"] = json!(true);
            let next = serde_json::to_string(&value).expect("serialize");
            store
                .set_state("recall_config_proposals", &proposal_id, &next)
                .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .expect("tamper row");

    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.format = Some("json".to_string());
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect_err("a tampered identity_payload must refuse to apply");
    assert!(
        err.contains("content_digest_mismatch"),
        "expected content_digest_mismatch refusal, got: {err}"
    );

    let config_body = std::fs::read_to_string(&config_env_path).expect("read config.env");
    assert!(
        config_body.contains("TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1"),
        "the digest-mismatch refusal must not have mutated config.env"
    );
}

/// Discrimination: the top-level `config_env` field on a recall-config
/// proposal row is a DISPLAY copy — the exact field
/// `handle_recall_config_proposals`'s response shows a human reviewer —
/// distinct from the digest-bound `identity_payload.apply_payload.config_env`
/// copy. `content_digest` only proves `identity_payload` is internally
/// self-consistent; it says nothing about whether the display copy still
/// matches it. A row whose display copy was tampered *without* touching
/// `identity_payload`/`content_digest` still passes the digest check, so a
/// human approving based on the (tampered) display copy has not actually
/// approved the bound content — refusing loudly is the only sound response;
/// silently applying the bound (correct) value would mean this code decided,
/// on the human's behalf, that they "really meant" the untampered version.
///
/// Production path: `drive_recall_apply_state_machine` ->
/// `recall_config_display_drifted` (canonical_json_eq against
/// `identity_payload.apply_payload.config_env`), checked immediately after
/// the content_digest re-validation and before `parse_config_env_patch`.
/// Pre-fix red (cross-vendor review, #1424/#1425): tampering ONLY the
/// top-level `config_env` field (leaving `identity_payload`/`content_digest`
/// byte-for-byte untouched) passed the digest check with no further
/// cross-check, so apply proceeded — under the interim (bound-copy-read-only)
/// fix it proceeded silently against the ORIGINAL value; under the fully
/// unfixed pre-#1424 code it would have proceeded against the ATTACKER's
/// value.
/// Post-fix green: apply is REFUSED with `display_copy_drift`; config.env
/// and the proposal row are byte-identical before and after the refused call.
#[tokio::test]
async fn recall_apply_refuses_tampered_unbound_top_level_config_env() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env_path = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env_path.parent().expect("parent"))
        .expect("create config env parent");
    std::fs::write(
        &config_env_path,
        "VOYAGE_API_KEY=vault:VOYAGE_API_KEY\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\n",
    )
    .expect("seed config.env");

    seed_recall_pair(&server, "unbound-tamper");

    let metadata = json!({
        "cases": [
            {
                "name": "partial-cleanup",
                "query": "cleanup preview safe",
                "expected_id": "recall-unbound-tamper-partial-term"
            }
        ],
        "variants": [
            {
                "name": "or-fallback-0.6",
                "recall_config": {
                    "or_fallback_fts_score_factor": 0.6,
                    "or_fallback_fts_max_terms": 4
                }
            }
        ]
    });

    let mut proposals = tachi_memory_params("recall_proposals");
    proposals.format = Some("json".to_string());
    proposals.scope = Some("memory".to_string());
    proposals.top_k = 3;
    proposals.force = true;
    proposals.metadata = Some(metadata);
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, proposals)
        .await
        .expect("recall proposals");
    let parsed: Value = serde_json::from_str(&body).expect("proposal JSON");
    let proposal = parsed["proposals"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|p| p["variant"] == json!("or-fallback-0.6"))
        })
        .expect("proposal");
    let proposal_id = proposal["proposal_id"].as_str().expect("id").to_string();

    // Approve while the row is still clean (digest matches, display matches
    // bound).
    let mut review = tachi_memory_params("review_recall_proposal");
    review.format = Some("json".to_string());
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    let _ = crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("approve");

    // Tamper ONLY the unbound top-level `config_env` display field —
    // identity_payload / content_digest are left byte-for-byte untouched, so
    // the digest check at apply must still pass; the NEW display-drift check
    // is what must catch this.
    server
        .with_global_store(|store| {
            let (raw, _version) = store
                .get_state_kv("recall_config_proposals", &proposal_id)
                .map_err(|e| e.to_string())?
                .expect("row");
            let mut value: Value = serde_json::from_str(&raw).expect("json");
            value["config_env"]["TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR"] = json!("9.9");
            let next = serde_json::to_string(&value).expect("serialize");
            store
                .set_state("recall_config_proposals", &proposal_id, &next)
                .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .expect("tamper unbound top-level config_env");

    let (before_raw, before_version) = read_recall_row(&server, &proposal_id);

    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.format = Some("json".to_string());
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect_err("a drifted display copy must refuse to apply, even though the digest matches");
    assert!(
        err.contains("display_copy_drift"),
        "expected display_copy_drift refusal, got: {err}"
    );

    let config_body = std::fs::read_to_string(&config_env_path).expect("read config.env");
    assert!(
        config_body.contains("TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1"),
        "the display-drift refusal must not have mutated config.env: {config_body}"
    );
    assert!(
        !config_body.contains("9.9") && !config_body.contains("0.6"),
        "neither the attacker's value nor the originally-approved value may reach \
         config.env from a refused apply: {config_body}"
    );

    let (after_raw, after_version) = read_recall_row(&server, &proposal_id);
    assert_eq!(
        before_version, after_version,
        "a refused apply must not bump the row's state_version"
    );
    assert_eq!(
        before_raw, after_raw,
        "a refused apply must not mutate the stored row"
    );
}

/// Discrimination: the same display/bound drift must also be caught at
/// REVIEW time, not just apply — a human recording an approve/reject
/// decision is reading the display copy (`handle_recall_config_review`'s
/// response echoes the row's top-level fields), so if it drifted before
/// review, the decision being recorded does not cover the bound content
/// either.
///
/// Production path: `handle_recall_config_review` ->
/// `recall_config_display_drifted`, checked immediately after the
/// content_digest re-validation and before the status/review mutation.
/// Pre-fix red: review only re-validated `content_digest` (identity_payload
/// self-consistency); a display copy tampered before review — even one an
/// attacker softened specifically to slip past a human who would have
/// rejected the real payload — passed straight through to a recorded
/// approval.
/// Post-fix green: review is REFUSED with `display_copy_drift` and the row
/// (including state_version and status) is byte-identical before and after.
#[tokio::test]
async fn recall_review_refuses_tampered_unbound_top_level_config_env() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env_path = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env_path.parent().expect("parent"))
        .expect("create config env parent");
    std::fs::write(
        &config_env_path,
        "VOYAGE_API_KEY=vault:VOYAGE_API_KEY\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\n",
    )
    .expect("seed config.env");

    seed_recall_pair(&server, "review-unbound-tamper");

    let metadata = json!({
        "cases": [
            {
                "name": "partial-cleanup",
                "query": "cleanup preview safe",
                "expected_id": "recall-review-unbound-tamper-partial-term"
            }
        ],
        "variants": [
            {
                "name": "or-fallback-0.6",
                "recall_config": {
                    "or_fallback_fts_score_factor": 0.6,
                    "or_fallback_fts_max_terms": 4
                }
            }
        ]
    });

    let mut proposals = tachi_memory_params("recall_proposals");
    proposals.format = Some("json".to_string());
    proposals.scope = Some("memory".to_string());
    proposals.top_k = 3;
    proposals.force = true;
    proposals.metadata = Some(metadata);
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, proposals)
        .await
        .expect("recall proposals");
    let parsed: Value = serde_json::from_str(&body).expect("proposal JSON");
    let proposal = parsed["proposals"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|p| p["variant"] == json!("or-fallback-0.6"))
        })
        .expect("proposal");
    let proposal_id = proposal["proposal_id"].as_str().expect("id").to_string();

    // Tamper the display copy BEFORE any review — while the row is still
    // pending — leaving identity_payload/content_digest untouched.
    server
        .with_global_store(|store| {
            let (raw, _version) = store
                .get_state_kv("recall_config_proposals", &proposal_id)
                .map_err(|e| e.to_string())?
                .expect("row");
            let mut value: Value = serde_json::from_str(&raw).expect("json");
            value["config_env"]["TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR"] = json!("0.05");
            let next = serde_json::to_string(&value).expect("serialize");
            store
                .set_state("recall_config_proposals", &proposal_id, &next)
                .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .expect("tamper unbound top-level config_env before review");

    let (before_raw, before_version) = read_recall_row(&server, &proposal_id);

    let mut review = tachi_memory_params("review_recall_proposal");
    review.format = Some("json".to_string());
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect_err(
            "review of a proposal whose display copy drifted from its bound copy must refuse",
        );
    assert!(
        err.contains("display_copy_drift"),
        "expected display_copy_drift refusal, got: {err}"
    );

    let (after_raw, after_version) = read_recall_row(&server, &proposal_id);
    assert_eq!(
        before_version, after_version,
        "a refused review must not bump the row's state_version"
    );
    assert_eq!(
        before_raw, after_raw,
        "a refused review must not mutate the stored row"
    );
    let after_value: Value = serde_json::from_str(&after_raw).expect("row json");
    assert_eq!(
        after_value["status"],
        json!("pending"),
        "status must remain pending; the refused review must not record a decision"
    );
}

/// Discrimination: recall proposal identity binds the live TACHI_RECALL_*
/// source digest. Config drift must refuse review and approved apply without
/// changing proposal state, while regeneration mints a distinct pending id.
#[tokio::test]
async fn recall_source_config_drift_refuses_review_apply_and_rotates_pending_identity() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env_path = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env_path.parent().expect("config parent"))
        .expect("create config parent");
    std::fs::write(
        &config_env_path,
        "TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\nTACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=2\n",
    )
    .expect("seed config");
    seed_recall_pair(&server, "source-drift");

    let first = generate_recall_source_proposal(&server, "source-drift").await;
    let first_id = first["proposal_id"].as_str().expect("first id").to_string();
    std::fs::write(
        &config_env_path,
        "TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.2\nTACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=2\n",
    )
    .expect("third-party config edit before review");

    let (before_review, before_review_version) = read_recall_row(&server, &first_id);
    let mut review = tachi_memory_params("review_recall_proposal");
    review.proposal_id = Some(first_id.clone());
    review.review_status = Some("approved".to_string());
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect_err("review after config drift must refuse");
    assert!(
        err.contains("source_state_drift"),
        "unexpected review error: {err}"
    );
    assert_eq!(
        read_recall_row(&server, &first_id),
        (before_review, before_review_version),
        "review refusal must leave the pending row untouched"
    );

    let regenerated = generate_recall_source_proposal(&server, "source-drift").await;
    let regenerated_id = regenerated["proposal_id"]
        .as_str()
        .expect("regenerated id")
        .to_string();
    assert_ne!(
        first_id, regenerated_id,
        "source revision must rotate the id"
    );
    assert_eq!(regenerated["status"], json!("pending"));

    let mut approve = tachi_memory_params("review_recall_proposal");
    approve.proposal_id = Some(regenerated_id.clone());
    approve.review_status = Some("approved".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, approve)
        .await
        .expect("approve regenerated proposal");
    std::fs::write(
        &config_env_path,
        "TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.3\nTACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=2\n",
    )
    .expect("third-party config edit after approval");

    let (before_apply, before_apply_version) = read_recall_row(&server, &regenerated_id);
    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.proposal_id = Some(regenerated_id.clone());
    apply.confirm = true;
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect_err("approved proposal must refuse after config drift");
    assert!(
        err.contains("source_state_drift"),
        "unexpected apply error: {err}"
    );
    assert_eq!(
        read_recall_row(&server, &regenerated_id),
        (before_apply, before_apply_version),
        "apply refusal must leave the approved row untouched"
    );
    assert_eq!(
        std::fs::read_to_string(&config_env_path).expect("read config after refusal"),
        "TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.3\nTACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=2\n",
        "refused apply must not overwrite third-party config"
    );
}

/// Discrimination: recall proposal identity binds the complete config.env
/// source it can later rewrite. A provider/Vault-only edit must refuse review
/// and approved apply without changing proposal state or overwriting that
/// provider edit, while regeneration mints a distinct pending id.
#[tokio::test]
async fn recall_provider_config_drift_refuses_review_apply_and_rotates_pending_identity() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env_path = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env_path.parent().expect("config parent"))
        .expect("create config parent");
    std::fs::write(
        &config_env_path,
        "VOYAGE_API_KEY=vault:provider-at-generation\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\nTACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=2\n",
    )
    .expect("seed config");
    seed_recall_pair(&server, "source-drift");

    let first = generate_recall_source_proposal(&server, "source-drift").await;
    let first_id = first["proposal_id"].as_str().expect("first id").to_string();
    std::fs::write(
        &config_env_path,
        "VOYAGE_API_KEY=vault:provider-before-review\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\nTACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=2\n",
    )
    .expect("third-party config edit before review");

    let (before_review, before_review_version) = read_recall_row(&server, &first_id);
    let mut review = tachi_memory_params("review_recall_proposal");
    review.proposal_id = Some(first_id.clone());
    review.review_status = Some("approved".to_string());
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect_err("review after config drift must refuse");
    assert!(
        err.contains("source_state_drift"),
        "unexpected review error: {err}"
    );
    assert_eq!(
        read_recall_row(&server, &first_id),
        (before_review, before_review_version),
        "review refusal must leave the pending row untouched"
    );

    let regenerated = generate_recall_source_proposal(&server, "source-drift").await;
    let regenerated_id = regenerated["proposal_id"]
        .as_str()
        .expect("regenerated id")
        .to_string();
    assert_ne!(
        first_id, regenerated_id,
        "source revision must rotate the id"
    );
    assert_eq!(regenerated["status"], json!("pending"));

    let mut approve = tachi_memory_params("review_recall_proposal");
    approve.proposal_id = Some(regenerated_id.clone());
    approve.review_status = Some("approved".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, approve)
        .await
        .expect("approve regenerated proposal");
    std::fs::write(
        &config_env_path,
        "VOYAGE_API_KEY=vault:provider-before-apply\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\nTACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=2\n",
    )
    .expect("third-party config edit after approval");

    let (before_apply, before_apply_version) = read_recall_row(&server, &regenerated_id);
    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.proposal_id = Some(regenerated_id.clone());
    apply.confirm = true;
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect_err("approved proposal must refuse after config drift");
    assert!(
        err.contains("source_state_drift"),
        "unexpected apply error: {err}"
    );
    assert_eq!(
        read_recall_row(&server, &regenerated_id),
        (before_apply, before_apply_version),
        "apply refusal must leave the approved row untouched"
    );
    assert_eq!(
        std::fs::read_to_string(&config_env_path).expect("read config after refusal"),
        "VOYAGE_API_KEY=vault:provider-before-apply\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\nTACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=2\n",
        "refused apply must not overwrite the concurrent provider/Vault edit"
    );
}

/// Discrimination: an atomic config replacement must retain restrictive
/// existing permissions. The old `File::create` temp path honored the process
/// umask and commonly replaced a private 0600 config with mode 0644.
#[cfg(unix)]
#[tokio::test]
async fn recall_apply_preserves_private_config_mode() {
    use std::os::unix::fs::PermissionsExt;

    let (server, temp_home) = make_server_with_temp_home();
    let config_env_path = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env_path.parent().expect("config parent"))
        .expect("create config parent");
    std::fs::write(
        &config_env_path,
        "VOYAGE_API_KEY=vault:provider\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\n",
    )
    .expect("seed private config");
    std::fs::set_permissions(&config_env_path, std::fs::Permissions::from_mode(0o600))
        .expect("restrict config.env to owner-only mode");
    seed_recall_pair(&server, "private-mode");

    let proposal = generate_recall_source_proposal(&server, "private-mode").await;
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();
    let mut review = tachi_memory_params("review_recall_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("approve private-config proposal");

    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.proposal_id = Some(proposal_id);
    apply.confirm = true;
    crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect("apply private-config proposal");

    assert_eq!(
        std::fs::metadata(&config_env_path)
            .expect("read rewritten config mode")
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "atomic replacement must not downgrade a private config.env to 0644"
    );
}

/// Discrimination: a provider replacement that lands after the apply receipt
/// is stamped but immediately before the config mutation must not be replaced
/// by stale whole-file bytes. The hook atomically renames a new inode over the
/// final path; apply must refuse identity drift and preserve it verbatim.
#[tokio::test]
async fn recall_apply_provider_edit_in_final_window_is_not_overwritten() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env_path = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env_path.parent().expect("config parent"))
        .expect("create config parent");
    std::fs::write(
        &config_env_path,
        "VOYAGE_API_KEY=vault:provider-before\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\n",
    )
    .expect("seed config");
    seed_recall_pair(&server, "provider-final-window");

    let proposal = generate_recall_source_proposal(&server, "provider-final-window").await;
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();
    let mut review = tachi_memory_params("review_recall_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("approve proposal");

    let raced_body =
        "VOYAGE_API_KEY=vault:provider-raced\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\n";
    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    apply.metadata = Some(json!({
        "test_config_env_before_recall_append": raced_body,
    }));
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect_err("provider edit in the final mutation window must refuse");
    assert!(
        err.contains("source_identity_drift"),
        "unexpected final-window error: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(&config_env_path).expect("read raced config"),
        raced_body,
        "identity refusal must preserve the replacement inode's bytes verbatim"
    );
    let (raw, _) = read_recall_row(&server, &proposal_id);
    let row: Value = serde_json::from_str(&raw).expect("applying row JSON");
    assert_eq!(row["status"], json!("applying"));
    assert!(
        row["applying_receipt"].is_object(),
        "the refusal must retain a recoverable applying receipt: {row}"
    );
}

/// A config.env symlink does not provide a stable object identity for the
/// proposal/apply protocol. Refuse before following it, leave the target bytes
/// untouched, and keep the approved proposal row byte-identical.
#[cfg(unix)]
#[tokio::test]
async fn recall_apply_refuses_symlinked_config_env_loudly() {
    use std::os::unix::fs::symlink;

    let (server, temp_home) = make_server_with_temp_home();
    let config_env_path = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env_path.parent().expect("config parent"))
        .expect("create config parent");
    let source = "VOYAGE_API_KEY=vault:provider\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\n";
    std::fs::write(&config_env_path, source).expect("seed config");
    seed_recall_pair(&server, "symlink-config");

    let proposal = generate_recall_source_proposal(&server, "symlink-config").await;
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();
    let mut review = tachi_memory_params("review_recall_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("approve proposal");
    let before_row = read_recall_row(&server, &proposal_id);

    let target = config_env_path.with_extension("env.real");
    std::fs::rename(&config_env_path, &target).expect("move config to symlink target");
    symlink(&target, &config_env_path).expect("symlink config.env");

    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect_err("symlinked config.env must refuse");
    assert!(
        err.contains("symlink_config_refused"),
        "unexpected symlink refusal: {err}"
    );
    assert!(
        std::fs::symlink_metadata(&config_env_path)
            .expect("symlink metadata")
            .file_type()
            .is_symlink(),
        "refusal must not replace the symlink"
    );
    assert_eq!(
        std::fs::read_to_string(&target).expect("read symlink target"),
        source,
        "refusal must not mutate the symlink target"
    );
    assert_eq!(
        read_recall_row(&server, &proposal_id),
        before_row,
        "symlink refusal must not mutate the approved proposal row"
    );
}

/// Every parent component is part of the config identity boundary. Even when
/// an outside config has byte-identical approved content, a symlinked `.tachi`
/// parent must refuse before the approved proposal or outside file changes.
#[cfg(unix)]
#[tokio::test]
async fn recall_apply_refuses_symlinked_config_parent_before_mutation() {
    use std::os::unix::fs::symlink;

    let (server, temp_home) = make_server_with_temp_home();
    let config_parent = temp_home.temp_home.join(".tachi");
    let config_env_path = config_parent.join("config.env");
    std::fs::create_dir_all(&config_parent).expect("create config parent");
    let source = "VOYAGE_API_KEY=vault:provider\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\n";
    std::fs::write(&config_env_path, source).expect("seed config");
    seed_recall_pair(&server, "symlink-parent");

    let proposal = generate_recall_source_proposal(&server, "symlink-parent").await;
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();
    let mut review = tachi_memory_params("review_recall_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("approve proposal");
    let before_row = read_recall_row(&server, &proposal_id);

    let displaced_parent = temp_home.temp_home.join(".tachi.displaced");
    std::fs::rename(&config_parent, &displaced_parent).expect("displace real config parent");
    let outside = tempfile::tempdir().expect("outside config dir");
    let outside_config = outside.path().join("config.env");
    std::fs::write(&outside_config, source).expect("seed byte-identical outside config");
    symlink(outside.path(), &config_parent).expect("symlink config parent outside");

    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect_err("symlinked config parent must refuse before apply mutation");
    assert!(
        err.contains("symlink_config_parent_refused"),
        "unexpected parent-symlink refusal: {err}"
    );
    assert_eq!(
        read_recall_row(&server, &proposal_id),
        before_row,
        "parent-symlink refusal must leave the approved row byte-identical"
    );
    assert_eq!(
        std::fs::read_to_string(&outside_config).expect("read outside config"),
        source,
        "parent traversal refusal must not append to the outside config"
    );
    assert_eq!(
        std::fs::read_to_string(displaced_parent.join("config.env"))
            .expect("read displaced config"),
        source,
        "parent traversal refusal must not mutate the displaced real config"
    );
}

/// If the verified parent is replaced by a symlink after the applying receipt
/// is stamped, re-opening the component chain must refuse before descriptor
/// append. The outside target survives and the row remains recoverable.
#[cfg(unix)]
#[tokio::test]
async fn recall_apply_parent_replacement_race_refuses_without_outside_write() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_parent = temp_home.temp_home.join(".tachi");
    let config_env_path = config_parent.join("config.env");
    std::fs::create_dir_all(&config_parent).expect("create config parent");
    let source = "VOYAGE_API_KEY=vault:provider\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\n";
    std::fs::write(&config_env_path, source).expect("seed config");
    seed_recall_pair(&server, "parent-replacement-race");

    let proposal = generate_recall_source_proposal(&server, "parent-replacement-race").await;
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();
    let mut review = tachi_memory_params("review_recall_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("approve proposal");

    let outside = tempfile::tempdir().expect("outside config dir");
    let outside_config = outside.path().join("config.env");
    std::fs::write(&outside_config, source).expect("seed byte-identical outside config");
    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    apply.metadata = Some(json!({
        "test_config_parent_symlink_before_recall_append": outside.path().display().to_string(),
    }));
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect_err("parent replacement race must refuse before append");
    assert!(
        err.contains("symlink_config_parent_refused"),
        "unexpected parent replacement refusal: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(&outside_config).expect("read outside config"),
        source,
        "replacement race must not append through the new parent symlink"
    );
    let (raw, _) = read_recall_row(&server, &proposal_id);
    let row: Value = serde_json::from_str(&raw).expect("applying row JSON");
    assert_eq!(row["status"], json!("applying"));
    assert!(row["applying_receipt"].is_object());
}

/// Non-Unix has no equivalent to the descriptor/no-follow identity protocol.
/// Refusal must happen before the approved row can be stamped applying.
#[cfg(not(unix))]
#[tokio::test]
async fn recall_apply_non_unix_refuses_before_applying_stamp() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env_path = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env_path.parent().expect("config parent"))
        .expect("create config parent");
    let source = "TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\n";
    std::fs::write(&config_env_path, source).expect("seed config");
    seed_recall_pair(&server, "non-unix-refusal");

    let proposal = generate_recall_source_proposal(&server, "non-unix-refusal").await;
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();
    let mut review = tachi_memory_params("review_recall_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("approve proposal");
    let before_row = read_recall_row(&server, &proposal_id);

    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect_err("non-Unix apply must refuse before stamping");
    assert!(
        err.contains("unsupported_platform"),
        "unexpected platform refusal: {err}"
    );
    assert_eq!(
        read_recall_row(&server, &proposal_id),
        before_row,
        "platform refusal must leave the approved row byte-identical"
    );
    assert_eq!(
        std::fs::read_to_string(&config_env_path).expect("read config"),
        source,
        "platform refusal must not mutate config.env"
    );
}

/// The complete config source is bounded before hashing or proposal creation.
/// The exact boundary remains readable; one additional byte refuses loudly.
#[tokio::test]
async fn recall_config_env_read_limit_accepts_boundary_and_refuses_over_limit() {
    let max_config_bytes = memcore::recall_config::MAX_RECALL_CONFIG_ENV_BYTES;

    let (server, temp_home) = make_server_with_temp_home();
    let config_env_path = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env_path.parent().expect("config parent"))
        .expect("create config parent");
    let boundary = format!("#{}\n", "x".repeat(max_config_bytes - 2));
    assert_eq!(boundary.len(), max_config_bytes);
    std::fs::write(&config_env_path, &boundary).expect("write boundary config");
    seed_recall_pair(&server, "config-size-boundary");
    let _ = generate_recall_source_proposal(&server, "config-size-boundary").await;

    let over_limit = format!("{boundary}x");
    std::fs::write(&config_env_path, &over_limit).expect("write oversized config");
    seed_recall_pair(&server, "config-size-over");
    let err = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        recall_source_proposal_params("config-size-over"),
    )
    .await
    .expect_err("oversized config.env must refuse before proposal persistence");
    assert!(
        err.contains("config_env_too_large"),
        "unexpected oversized-config refusal: {err}"
    );
    assert_eq!(
        std::fs::metadata(&config_env_path)
            .expect("oversized config metadata")
            .len() as usize,
        max_config_bytes + 1,
        "refusal must not rewrite the oversized source"
    );
}

/// Appending recall assignments must handle a source without a final newline
/// without joining keys, preserve every original byte, and rely on the same
/// last-declaration-wins parser used by production RecallConfig loading.
#[tokio::test]
async fn recall_apply_appends_after_missing_final_newline_with_last_value_winning() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env_path = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env_path.parent().expect("config parent"))
        .expect("create config parent");
    let source = "VOYAGE_API_KEY=vault:provider\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1";
    std::fs::write(&config_env_path, source).expect("seed config without final newline");
    seed_recall_pair(&server, "no-final-newline");

    let proposal = generate_recall_source_proposal(&server, "no-final-newline").await;
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();
    let mut review = tachi_memory_params("review_recall_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("approve proposal");
    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.proposal_id = Some(proposal_id);
    apply.confirm = true;
    crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect("apply appended config");

    let body = std::fs::read_to_string(&config_env_path).expect("read appended config");
    assert_eq!(
        body,
        format!(
            "{source}\nTACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=4\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.6\n"
        ),
        "append protocol must preserve the source byte prefix and delimit the new assignments"
    );
    let effective = memcore::RecallConfig::from_config_env_source(&body);
    assert_eq!(
        effective.or_fallback_fts_score_factor, 0.6,
        "the appended declaration must be the effective production RecallConfig value"
    );
    assert_eq!(effective.or_fallback_fts_max_terms, 4);
}

/// Missing config.env is a valid empty source revision. The append protocol
/// creates it privately and writes only the approved recall assignments.
#[tokio::test]
async fn recall_apply_creates_missing_config_env_privately() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env_path = temp_home.temp_home.join(".tachi/config.env");
    assert!(
        !config_env_path.exists(),
        "fixture requires a missing config.env"
    );
    seed_recall_pair(&server, "missing-config");

    let proposal = generate_recall_source_proposal(&server, "missing-config").await;
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();
    let mut review = tachi_memory_params("review_recall_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("approve proposal");
    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.proposal_id = Some(proposal_id);
    apply.confirm = true;
    crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect("apply missing config");

    let effective: std::collections::HashMap<String, String> =
        dotenvy::from_path_iter(&config_env_path)
            .expect("open created config with shipped dotenv parser")
            .collect::<Result<Vec<_>, _>>()
            .expect("parse created config")
            .into_iter()
            .collect();
    assert_eq!(
        effective.len(),
        2,
        "only approved recall keys may be created"
    );
    assert_eq!(
        effective.get("TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR"),
        Some(&"0.6".to_string())
    );
    assert_eq!(
        effective.get("TACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS"),
        Some(&"4".to_string())
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&config_env_path)
                .expect("created config metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "new config.env must be owner-only"
        );
    }
}

/// If another writer creates config.env after the approved missing-file check
/// but before our exclusive create, its bytes and permissions are outside the
/// approved lifecycle. Losing O_EXCL must refuse instead of reopening it.
#[cfg(unix)]
#[tokio::test]
async fn recall_apply_missing_file_create_race_refuses_competitor_file() {
    use std::os::unix::fs::PermissionsExt;

    let (server, temp_home) = make_server_with_temp_home();
    let config_env_path = temp_home.temp_home.join(".tachi/config.env");
    assert!(
        !config_env_path.exists(),
        "fixture requires missing config.env"
    );
    seed_recall_pair(&server, "missing-create-race");

    let proposal = generate_recall_source_proposal(&server, "missing-create-race").await;
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();
    let mut review = tachi_memory_params("review_recall_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("approve missing-file proposal");

    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    apply.metadata = Some(json!({
        "test_create_config_env_before_exclusive_create": true,
    }));
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect_err("losing exclusive create must refuse the competitor file");
    assert!(
        err.contains("third_party_drift"),
        "unexpected create-race refusal: {err}"
    );
    assert_eq!(
        std::fs::read(&config_env_path).expect("read competitor file"),
        b"",
        "competitor-created bytes must remain untouched"
    );
    assert_eq!(
        std::fs::metadata(&config_env_path)
            .expect("competitor metadata")
            .permissions()
            .mode()
            & 0o777,
        0o644,
        "apply must not normalize or replace competitor permissions"
    );
    let (raw, _) = read_recall_row(&server, &proposal_id);
    let row: Value = serde_json::from_str(&raw).expect("applying row JSON");
    assert_eq!(row["status"], json!("applying"));
    assert!(row["applying_receipt"].is_object());
}

/// A crash can leave only a prefix of the receipt-bound append on disk. The
/// recovery path may resume exactly that suffix only after proving the
/// original source prefix and every already-written append byte.
#[tokio::test]
async fn recall_apply_partial_append_recovers_without_replacing_source() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env_path = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env_path.parent().expect("config parent"))
        .expect("create config parent");
    let source = "VOYAGE_API_KEY=vault:provider\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\n";
    std::fs::write(&config_env_path, source).expect("seed config");
    seed_recall_pair(&server, "partial-append-recovery");

    let proposal = generate_recall_source_proposal(&server, "partial-append-recovery").await;
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();
    let mut review = tachi_memory_params("review_recall_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("approve proposal");

    let append_payload =
        "TACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=4\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.6\n";
    let after = format!("{source}{append_payload}");
    let partial_len = append_payload
        .find("SCORE_FACTOR")
        .expect("partial boundary");
    std::fs::write(
        &config_env_path,
        format!("{source}{}", &append_payload[..partial_len]),
    )
    .expect("write crash-partial append");

    server
        .with_global_store(|store| {
            let (raw, _version) = store
                .get_state_kv("recall_config_proposals", &proposal_id)
                .map_err(|e| e.to_string())?
                .expect("proposal row");
            let mut value: Value = serde_json::from_str(&raw).expect("proposal JSON");
            value["status"] = json!("applying");
            value["applying_receipt"] = json!({
                "attempt_id": "partial-append-fixture",
                "before_digest": test_config_env_digest(source),
                "after_digest": test_config_env_digest(&after),
                "before_len": source.len(),
                "append_payload": append_payload,
                "updated_keys": [
                    "TACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS",
                    "TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR",
                ],
                "started_at": "2026-07-25T00:00:00Z",
            });
            let next = serde_json::to_string(&value).expect("serialize applying row");
            store
                .set_state("recall_config_proposals", &proposal_id, &next)
                .map_err(|e| e.to_string())
        })
        .expect("stamp partial applying receipt");

    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.format = Some("json".to_string());
    apply.proposal_id = Some(proposal_id);
    apply.confirm = true;
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect("resume receipt-bound partial append");
    let applied: Value = serde_json::from_str(&body).expect("apply JSON");
    assert_eq!(applied["apply_outcome"], json!("applied_retried"));
    assert_eq!(
        std::fs::read_to_string(&config_env_path).expect("read recovered config"),
        after,
        "recovery must preserve the original source and append only the missing receipt-bound suffix"
    );
}

/// Persisted recovery receipts are untrusted state. Oversized append payloads
/// and impossible before lengths must refuse before another file or row write.
#[tokio::test]
async fn recall_apply_refuses_oversized_persisted_receipt_without_mutation() {
    const EXPECTED_MAX_RECALL_APPEND_PAYLOAD_BYTES: usize = 64 * 1024;
    let max_config_bytes = memcore::recall_config::MAX_RECALL_CONFIG_ENV_BYTES;

    let (server, temp_home) = make_server_with_temp_home();
    let config_env_path = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env_path.parent().expect("config parent"))
        .expect("create config parent");
    let source = "TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\n";
    std::fs::write(&config_env_path, source).expect("seed config");
    seed_recall_pair(&server, "oversized-receipt");
    let proposal = generate_recall_source_proposal(&server, "oversized-receipt").await;
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();
    let mut review = tachi_memory_params("review_recall_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("approve proposal");

    let oversized_payload = "x".repeat(EXPECTED_MAX_RECALL_APPEND_PAYLOAD_BYTES + 1);
    server
        .with_global_store(|store| {
            let (raw, _version) = store
                .get_state_kv("recall_config_proposals", &proposal_id)
                .map_err(|e| e.to_string())?
                .expect("proposal row");
            let mut value: Value = serde_json::from_str(&raw).expect("proposal JSON");
            value["status"] = json!("applying");
            value["applying_receipt"] = json!({
                "attempt_id": "oversized-payload-fixture",
                "before_digest": test_config_env_digest(source),
                "after_digest": "0".repeat(64),
                "before_len": source.len(),
                "append_payload": oversized_payload,
                "updated_keys": [],
                "started_at": "2026-07-25T00:00:00Z",
            });
            store
                .set_state(
                    "recall_config_proposals",
                    &proposal_id,
                    &serde_json::to_string(&value).expect("serialize oversized receipt"),
                )
                .map_err(|e| e.to_string())
        })
        .expect("persist oversized receipt");
    let oversized_row = read_recall_row(&server, &proposal_id);

    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect_err("oversized append payload must refuse");
    assert!(
        err.contains("applying_receipt_too_large"),
        "unexpected oversized-receipt refusal: {err}"
    );
    assert_eq!(read_recall_row(&server, &proposal_id), oversized_row);
    assert_eq!(
        std::fs::read_to_string(&config_env_path).expect("read unchanged config"),
        source
    );

    server
        .with_global_store(|store| {
            let (raw, _version) = store
                .get_state_kv("recall_config_proposals", &proposal_id)
                .map_err(|e| e.to_string())?
                .expect("proposal row");
            let mut value: Value = serde_json::from_str(&raw).expect("proposal JSON");
            value["applying_receipt"]["append_payload"] = json!(
                "TACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=4\nTACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.6\n"
            );
            value["applying_receipt"]["before_len"] = json!(max_config_bytes + 1);
            store
                .set_state(
                    "recall_config_proposals",
                    &proposal_id,
                    &serde_json::to_string(&value).expect("serialize oversized length"),
                )
                .map_err(|e| e.to_string())
        })
        .expect("persist oversized before_len");
    let oversized_len_row = read_recall_row(&server, &proposal_id);
    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect_err("oversized before_len must refuse");
    assert!(
        err.contains("applying_receipt_too_large"),
        "unexpected oversized-before_len refusal: {err}"
    );
    assert_eq!(read_recall_row(&server, &proposal_id), oversized_len_row);
    assert_eq!(
        std::fs::read_to_string(&config_env_path).expect("read unchanged config"),
        source
    );

    server
        .with_global_store(|store| {
            let (raw, _version) = store
                .get_state_kv("recall_config_proposals", &proposal_id)
                .map_err(|e| e.to_string())?
                .expect("proposal row");
            let mut value: Value = serde_json::from_str(&raw).expect("proposal JSON");
            value["applying_receipt"]["before_len"] = json!("not-a-number");
            store
                .set_state(
                    "recall_config_proposals",
                    &proposal_id,
                    &serde_json::to_string(&value).expect("serialize malformed length"),
                )
                .map_err(|e| e.to_string())
        })
        .expect("persist malformed before_len");
    let malformed_row = read_recall_row(&server, &proposal_id);
    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect_err("malformed before_len must refuse");
    assert!(
        err.contains("malformed_applying_receipt"),
        "unexpected malformed-receipt refusal: {err}"
    );
    assert_eq!(read_recall_row(&server, &proposal_id), malformed_row);
    assert_eq!(
        std::fs::read_to_string(&config_env_path).expect("read unchanged config"),
        source
    );
}

/// Deterministic review-race discrimination: edit config.env after the
/// pending -> approved CAS has executed but while its SQLite transaction is
/// still uncommitted. The post-CAS digest check must detect the edit and roll
/// the transaction back, preserving the exact pending bytes and state version.
#[tokio::test]
async fn recall_review_racing_config_edit_rolls_back_exact_pending_row() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env_path = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env_path.parent().expect("config parent"))
        .expect("create config parent");
    std::fs::write(
        &config_env_path,
        "TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\nTACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=2\n",
    )
    .expect("seed config");
    seed_recall_pair(&server, "review-race");

    let proposal = generate_recall_source_proposal(&server, "review-race").await;
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();
    let before = read_recall_row(&server, &proposal_id);

    let raced_body =
        "TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.9\nTACHI_RECALL_OR_FALLBACK_FTS_MAX_TERMS=2\n";
    let mut review = tachi_memory_params("review_recall_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    review.metadata = Some(json!({
        "test_config_env_after_review_cas": raced_body,
    }));
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect_err("a config edit racing review persistence must roll review back");
    assert!(
        err.contains("review_source_drift"),
        "unexpected review-race error: {err}"
    );
    assert_eq!(
        read_recall_row(&server, &proposal_id),
        before,
        "rollback must preserve the exact pre-review pending row and state_version"
    );
    assert_eq!(
        std::fs::read_to_string(&config_env_path).expect("read raced config"),
        raced_body,
        "review refusal must not overwrite the external writer's config edit"
    );
}

/// Discrimination: recomputing a row's digest after changing only its stored
/// policy version does not make it current. Both review and apply must reject
/// the stale policy before mutating lifecycle state.
#[tokio::test]
async fn recall_stale_current_policy_with_recomputed_digest_refuses_review_and_apply() {
    let (server, temp_home) = make_server_with_temp_home();
    let config_env_path = temp_home.temp_home.join(".tachi/config.env");
    std::fs::create_dir_all(config_env_path.parent().expect("config parent"))
        .expect("create config parent");
    std::fs::write(
        &config_env_path,
        "TACHI_RECALL_OR_FALLBACK_FTS_SCORE_FACTOR=0.1\n",
    )
    .expect("seed config");
    seed_recall_pair(&server, "stale-policy");
    let proposal = generate_recall_source_proposal(&server, "stale-policy").await;
    let proposal_id = proposal["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();

    let mutate_policy = |status: Option<&str>| {
        server
            .with_global_store(|store| {
                let (raw, _version) = store
                    .get_state_kv("recall_config_proposals", &proposal_id)
                    .map_err(|e| e.to_string())?
                    .expect("proposal row");
                let mut value: Value = serde_json::from_str(&raw).expect("row JSON");
                value["policy_version"] = json!("retired-recall-policy");
                value["identity_payload"]["policy_version"] = json!("retired-recall-policy");
                value["content_digest"] =
                    json!(test_recall_content_digest(&value["identity_payload"]));
                if let Some(status) = status {
                    value["status"] = json!(status);
                }
                store
                    .set_state(
                        "recall_config_proposals",
                        &proposal_id,
                        &serde_json::to_string(&value).expect("serialize stale row"),
                    )
                    .map_err(|e| e.to_string())
            })
            .expect("store stale but self-consistent proposal");
    };

    mutate_policy(None);
    let (before_review, before_review_version) = read_recall_row(&server, &proposal_id);
    let mut review = tachi_memory_params("review_recall_proposal");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect_err("stale current policy must refuse review");
    assert!(
        err.contains("current_policy_mismatch"),
        "unexpected review error: {err}"
    );
    assert_eq!(
        read_recall_row(&server, &proposal_id),
        (before_review, before_review_version),
        "review refusal must not mutate a self-consistent stale row"
    );

    let regenerated = generate_recall_source_proposal(&server, "stale-policy").await;
    assert_eq!(regenerated["status"], json!("pending"));
    let mut approve = tachi_memory_params("review_recall_proposal");
    approve.proposal_id = Some(proposal_id.clone());
    approve.review_status = Some("approved".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, approve)
        .await
        .expect("approve restored proposal");
    mutate_policy(Some("approved"));
    let (before_apply, before_apply_version) = read_recall_row(&server, &proposal_id);
    let mut apply = tachi_memory_params("apply_recall_proposals");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect_err("stale current policy must refuse apply");
    assert!(
        err.contains("current_policy_mismatch"),
        "unexpected apply error: {err}"
    );
    assert_eq!(
        read_recall_row(&server, &proposal_id),
        (before_apply, before_apply_version),
        "apply refusal must not mutate or terminalize a self-consistent stale row"
    );
}
