use super::*;

#[tokio::test]
async fn tachi_memory_ask_returns_evidence_contract() {
    let server = make_server();

    let body = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        TachiMemoryParams {
            action: "ask".to_string(),
            issue_ref: None,
            format: Some("markdown".to_string()),
            query: Some("what did we implement".to_string()),
            scope: None,
            top_k: 3,
            path_prefix: None,
            file_context: None,
            error_context: None,
            category: None,
            include_archived: false,
            include_training: false,
            enable_rerank: false,
            as_of: None,
            synthesize: false,
            model: None,
            agent_role: None,
            text: None,
            title: None,
            summary: None,
            topic: None,
            keywords: Vec::new(),
            entities: Vec::new(),
            importance: None,
            retention_policy: None,
            kind: None,
            path: None,
            id: None,
            force: false,
            source: None,
            valid_from: None,
            valid_until: None,
            project: None,
            project_explicit: false,
            domain: None,
            metadata: None,
            emit_continuity: false,
            compact: false,
            files: Vec::new(),
            references: Vec::new(),
            proposal_id: None,
            review_status: None,
            notes: None,
            confirm: false,
            state_filter: None,
        },
    )
    .await
    .expect("ask should succeed");

    assert!(body.starts_with("## Tachi ask"));
    assert!(body.contains("status: completed"));
    assert!(body.contains("evidence:"));
    // evidence count may be 0 in CI without embedding API; verify format only
    assert!(body.contains("hit(s)"));
}

#[tokio::test]
async fn tachi_memory_ask_can_return_compact_json() {
    let server = make_server();
    let params: TachiMemoryParams = serde_json::from_value(json!({
        "action": "ask",
        "format": "json",
        "query": "what did we implement",
        "top_k": 3
    }))
    .expect("params deserialize");

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("ask json should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("ask response should be JSON");

    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(parsed["query"], json!("what did we implement"));
    assert!(parsed["evidence"].is_array());
    assert!(parsed["thinking"].is_object());
    assert!(
        parsed["runtime"]["global_db"].as_str().is_some(),
        "ask must surface runtime binding: {parsed}"
    );
}

/// #946 discrimination: stale memory evidence mentioning an old Desktop path
/// must not override the live runtime project_db path for path questions.
#[tokio::test]
async fn tachi_memory_ask_db_path_uses_runtime_not_stale_evidence() {
    let server = make_server();
    let live_project = server
        .project_db_path_buf()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| server.global_db_path_buf().display().to_string());
    let stale_path = "/Users/kckylechen/Desktop/Sigil/.tachi/memory.db";
    assert_ne!(
        live_project, stale_path,
        "test fixture requires live path != stale Desktop path"
    );

    server
        .with_global_store(|store| {
            let mut stale = make_entry("ask-stale-db-path");
            stale.path = "/scratch/tachi/ask-stale-db-path".to_string();
            stale.summary = "Old project DB location note".to_string();
            stale.text = format!(
                "The current project memory.db path is {stale_path}. Always use that path."
            );
            stale.keywords = vec![
                "memory.db".to_string(),
                "path".to_string(),
                "database".to_string(),
            ];
            store.upsert(&stale).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed stale path memory");

    let params: TachiMemoryParams = serde_json::from_value(json!({
        "action": "ask",
        "format": "json",
        "query": "what is the current memory.db path",
        "top_k": 5,
        "synthesize": true
    }))
    .expect("params");

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("ask should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("ask JSON");

    assert_eq!(parsed["thinking"]["basis"], json!("runtime_binding"));
    assert_eq!(parsed["thinking"]["confidence"], json!("high"));
    let answer = parsed["synthesis"]["answer"]
        .as_str()
        .expect("deterministic answer");
    assert!(
        answer.contains(&live_project) || parsed["runtime"].to_string().contains(&live_project),
        "must cite live runtime path, got answer={answer} runtime={}",
        parsed["runtime"]
    );
    assert!(
        !answer.contains(stale_path),
        "must not present stale Desktop path as current: {answer}"
    );
    assert_eq!(
        parsed["runtime"]["source"],
        json!("runtime_binding"),
        "runtime block must be authoritative: {parsed}"
    );
}

#[tokio::test]
async fn tachi_memory_ask_keeps_controlled_probe_evidence_aligned_with_search() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut alpha = make_entry("ask-parity-alpha");
            alpha.path = "/scratch/tachi/ask-parity-alpha".to_string();
            alpha.summary = "Ask parity alpha".to_string();
            alpha.text =
                "RECALL_PROBE_ALPHA_ASK_20260607 clean-cli bridge dry-run force-delete behavior"
                    .to_string();
            alpha.keywords = vec!["recall-probe".to_string(), "clean-cli".to_string()];
            store.upsert(&alpha).map_err(|e| e.to_string())?;

            for idx in 0..12 {
                let mut distractor = make_entry(&format!("ask-parity-distractor-{idx}"));
                distractor.path = format!("/scratch/tachi/ask-parity-distractor-{idx}");
                distractor.summary = format!("Ask parity distractor {idx}");
                distractor.text =
                    format!("RECALL_PROBE_BETA_ASK_20260607 clean-cli bridge candidate {idx}");
                distractor.keywords = vec!["recall-probe".to_string(), "clean-cli".to_string()];
                store.upsert(&distractor).map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .expect("seed ask/search parity entries");

    let mut search_params = tachi_memory_params("search");
    search_params.format = Some("json".to_string());
    search_params.query = Some("RECALL_PROBE_ALPHA_ASK_20260607".to_string());
    search_params.scope = Some("memory".to_string());
    search_params.top_k = 3;
    let search_body = crate::facade_memory_ops::handle_tachi_memory(&server, search_params)
        .await
        .expect("search should succeed");
    let search_json: Value = serde_json::from_str(&search_body).expect("search JSON");
    let search_ids = search_json["sections"]
        .as_array()
        .expect("sections")
        .iter()
        .flat_map(|section| {
            section["rows"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|row| row["id"].as_str())
        })
        .collect::<Vec<_>>();

    let mut ask_params = tachi_memory_params("ask");
    ask_params.format = Some("json".to_string());
    ask_params.query = Some("RECALL_PROBE_ALPHA_ASK_20260607".to_string());
    ask_params.scope = Some("memory".to_string());
    ask_params.top_k = 3;
    ask_params.enable_rerank = false;
    let ask_body = crate::facade_memory_ops::handle_tachi_memory(&server, ask_params)
        .await
        .expect("ask should succeed");
    let ask_json: Value = serde_json::from_str(&ask_body).expect("ask JSON");
    let evidence = ask_json["evidence"].as_array().expect("ask evidence");
    let ask_ids = evidence
        .iter()
        .filter_map(|row| row["id"].as_str())
        .collect::<Vec<_>>();

    assert_eq!(search_ids.first().copied(), Some("ask-parity-alpha"));
    assert_eq!(ask_ids.first().copied(), Some("ask-parity-alpha"));
    assert!(
        ask_ids
            .iter()
            .take(3)
            .any(|id| search_ids.iter().take(3).any(|search_id| search_id == id)),
        "ask evidence should overlap controlled search evidence: search={search_ids:?} ask={ask_ids:?}"
    );
    assert!(
        evidence
            .first()
            .is_some_and(|row| row.get("rerank_policy").is_none()),
        "ask should not force rerank when enable_rerank=false: {ask_json}"
    );
}

/// #1071 RED corpus case 1/2: `ask` must resolve an exact issue anchor
/// mentioned in the query and degrade honestly — never report `high`
/// confidence purely because a pile of unrelated memory evidence exists —
/// when that anchor cannot be confirmed live. This targets a repo that does
/// not exist so the outcome is deterministic (`missing_anchor`) regardless
/// of whether the test host has `gh` installed/authenticated or network
/// access: `read_issue_snapshot_bounded` either times out (bounded, no
/// hang — see `gh_ops::issues::ANCHOR_GH_TIMEOUT`) or `gh` itself reports
/// the repo unresolvable. Either way this must never surface as `Grounded`.
#[tokio::test]
async fn tachi_memory_ask_caps_confidence_when_exact_anchor_is_unresolvable() {
    let server = make_server();
    server
        .with_global_store(|store| {
            for idx in 0..8 {
                let mut row = make_entry(&format!("ask-anchor-crowd-{idx}"));
                row.path = format!("/scratch/tachi/ask-anchor-crowd-{idx}");
                row.summary = format!("crowding evidence {idx}");
                row.text = "tachi-1071-anchor-test crowding evidence unrelated to the exact anchor"
                    .to_string();
                row.keywords = vec!["tachi-1071-anchor-test".to_string()];
                store.upsert(&row).map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .expect("seed crowding evidence");

    let mut ask_params = tachi_memory_params("ask");
    ask_params.format = Some("json".to_string());
    ask_params.query = Some(
        "tachi-1071-anchor-test what is the status of \
         kckylechen1-tachi-1071-nonexistent-repo/does-not-exist#1 right now?"
            .to_string(),
    );
    ask_params.top_k = 10;

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, ask_params)
        .await
        .expect("ask should succeed even when the anchor cannot be resolved");
    let parsed: Value = serde_json::from_str(&body).expect("ask JSON");

    assert_eq!(
        parsed["grounding_status"],
        json!("missing_anchor"),
        "unresolvable repo must never report grounded: {parsed}"
    );
    assert_eq!(
        parsed["thinking"]["confidence"],
        json!("low"),
        "confidence must be capped by the missing anchor, not by generic evidence volume: {parsed}"
    );
    let required_anchors = parsed["required_anchors"]
        .as_array()
        .expect("required_anchors array");
    assert_eq!(required_anchors.len(), 1);
    assert_eq!(
        required_anchors[0]["grounding_status"],
        json!("missing_anchor")
    );
    assert_eq!(
        required_anchors[0]["source_ref"],
        json!("kckylechen1-tachi-1071-nonexistent-repo/does-not-exist#1")
    );
}

/// Plain semantic queries with no exact anchor must be entirely unaffected
/// by #1071's grounding path — no `required_anchors`/live gh call, and the
/// legacy evidence-volume confidence heuristic still applies.
#[tokio::test]
async fn tachi_memory_ask_without_exact_anchor_skips_anchor_grounding() {
    let server = make_server();
    let mut ask_params = tachi_memory_params("ask");
    ask_params.format = Some("json".to_string());
    ask_params.query = Some("what did we implement".to_string());

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, ask_params)
        .await
        .expect("ask should succeed");
    let parsed: Value = serde_json::from_str(&body).expect("ask JSON");

    assert_eq!(parsed["grounding_status"], json!("grounded"));
    assert_eq!(parsed["required_anchors"], json!([]));
}

/// #1071 fix-round checkpoint 1: a query that BOTH asks about the runtime
/// DB path AND names an exact (unresolvable) anchor must still gate
/// confidence on that anchor — the old code returned from
/// `answer_runtime_db_path_query` before anchor extraction ever ran,
/// hardcoding `confidence: "high"` regardless of what the query also asked
/// about. This targets a nonexistent repo so the failure is deterministic
/// (see the sibling `caps_confidence_when_exact_anchor_is_unresolvable`
/// test's doc comment for why).
#[tokio::test]
async fn tachi_memory_ask_runtime_db_path_query_still_gates_on_named_anchor() {
    let server = make_server();
    let mut ask_params = tachi_memory_params("ask");
    ask_params.format = Some("json".to_string());
    ask_params.query = Some(
        "what is the current db path, and what is the status of \
         kckylechen1-tachi-1071-nonexistent-repo/does-not-exist#2 right now?"
            .to_string(),
    );

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, ask_params)
        .await
        .expect("ask should succeed even when the anchor cannot be resolved");
    let parsed: Value = serde_json::from_str(&body).expect("ask JSON");

    // The runtime-path answer itself is still authoritative/present.
    assert_eq!(parsed["runtime"]["source"], json!("runtime_binding"));
    // But confidence must NOT be hardcoded high — the fast path used to
    // bypass anchor extraction entirely.
    assert_eq!(
        parsed["thinking"]["confidence"],
        json!("low"),
        "runtime-db-path fast path must not bypass anchor gating: {parsed}"
    );
    assert_eq!(parsed["grounding_status"], json!("missing_anchor"));
    let required_anchors = parsed["required_anchors"]
        .as_array()
        .expect("required_anchors array");
    assert_eq!(required_anchors.len(), 1);
}

/// #1071 fix-round checkpoint 2: a query naming MORE than the
/// 3-anchor-per-ask live-resolve cap must never silently drop the extras —
/// every requested anchor gets a row (the ones beyond the cap as an
/// explicit `MissingAnchor`, not omitted). Uses 4 distinct nonexistent
/// repos so every outcome is deterministic regardless of network/`gh` auth.
#[tokio::test]
async fn tachi_memory_ask_anchors_beyond_cap_are_not_silently_dropped() {
    let server = make_server();
    let mut ask_params = tachi_memory_params("ask");
    ask_params.format = Some("json".to_string());
    ask_params.query = Some(
        "status of kckylechen1-tachi-1071-nonexistent-repo/repo-a#1, \
         kckylechen1-tachi-1071-nonexistent-repo/repo-b#2, \
         kckylechen1-tachi-1071-nonexistent-repo/repo-c#3, and \
         kckylechen1-tachi-1071-nonexistent-repo/repo-d#4 ?"
            .to_string(),
    );

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, ask_params)
        .await
        .expect("ask should succeed even when anchors cannot be resolved");
    let parsed: Value = serde_json::from_str(&body).expect("ask JSON");

    let required_anchors = parsed["required_anchors"]
        .as_array()
        .expect("required_anchors array");
    assert_eq!(
        required_anchors.len(),
        4,
        "all 4 requested anchors must be represented, not silently truncated to 3: {parsed}"
    );
    assert!(
        required_anchors
            .iter()
            .all(|a| a["grounding_status"] == json!("missing_anchor")),
        "unresolvable anchors (including the one beyond the live-resolve cap) must all be missing_anchor: {parsed}"
    );
    let fourth_reason = required_anchors[3]["contradictions"][0]["description"]
        .as_str()
        .expect("4th anchor must carry a structured contradiction reason");
    assert!(
        fourth_reason.contains("beyond") && fourth_reason.contains("cap"),
        "the 4th anchor's reason must explain it was beyond the resolve cap, not just \
         a generic gh failure: {fourth_reason}"
    );
    assert_eq!(
        parsed["thinking"]["confidence"],
        json!("low"),
        "confidence must be low when any requested anchor (including beyond-cap ones) is unresolved"
    );
}

// ---------------------------------------------------------------------------
// #1209 fix-round: discriminate the handler → `synthesize_answer` cap
// threading (codex 2026-07-17 BUG). The three `build_synthesis_system_prompt`
// unit tests in `readiness_ops.rs` only exercise that pure function directly
// — they stay green even if `handle_memory_ask`'s call site regressed to
// pass `None` (or dropped the cap entirely) instead of the resolved
// `anchor_cap`. This test instead shims the `claude` binary the same way
// `gh_comment_uses_body_file_not_inline_body` (`gh_comment_tests.rs`) shims
// `gh` — intercepting the real subprocess `call_claude_cli` spawns via a
// PATH-prepended fake executable — so the assertion runs against the exact
// system prompt the REAL handler → synthesize_answer → call_claude_cli call
// chain piped to stdin, not a hand-invoked helper. If the call site ever
// stops threading `anchor_cap` through, the captured prompt loses the
// "Anchor grounding status" line and this test goes RED.
// ---------------------------------------------------------------------------

struct ClaudeShimPathGuard {
    original: Option<std::ffi::OsString>,
}

impl ClaudeShimPathGuard {
    fn prepend(dir: &std::path::Path) -> Self {
        let original = std::env::var_os("PATH");
        let mut paths = vec![dir.to_path_buf()];
        if let Some(value) = original.as_ref() {
            paths.extend(std::env::split_paths(value));
        }
        let joined = std::env::join_paths(paths).expect("join PATH");
        std::env::set_var("PATH", joined);
        Self { original }
    }
}

impl Drop for ClaudeShimPathGuard {
    fn drop(&mut self) {
        if let Some(path) = self.original.as_ref() {
            std::env::set_var("PATH", path);
        } else {
            std::env::remove_var("PATH");
        }
    }
}

fn write_claude_shim(path: &std::path::Path, contents: &str) {
    std::fs::write(path, contents).expect("write claude shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)
            .expect("claude shim metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod claude shim");
    }
}

/// #1209 fix-round: the real handler → `synthesize_answer` → `call_claude_cli`
/// chain must actually thread the resolved anchor cap into the system prompt
/// piped to the CLI's stdin — not just into a hand-called pure function.
/// Uses the same nonexistent-repo determinism trick as the sibling
/// `tachi_memory_ask_caps_confidence_when_exact_anchor_is_unresolvable` test
/// so `required_anchors`/`grounding_status` resolve to `missing_anchor`
/// regardless of `gh` install/auth/network on the test host.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tachi_memory_ask_synthesis_call_site_threads_missing_anchor_cap() {
    let server = make_server();
    let fake_bin = tempfile::tempdir().expect("fake bin dir");
    let claude_path = fake_bin.path().join("claude");
    let capture_dir = tempfile::tempdir().expect("capture dir");
    let capture_path = capture_dir.path().join("prompt.txt");

    // Widen the lock across the shim install AND the awaited handler call —
    // same rationale as `gh_comment_uses_body_file_not_inline_body`: PATH is
    // process-global, so a concurrently-running test spawning a real
    // subprocess must never observe our shimmed PATH mid-flight.
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _path = {
        write_claude_shim(
            &claude_path,
            &format!(
                "#!/bin/sh\ncat > '{}'\necho 'stub synthesis answer'\n",
                capture_path.display()
            ),
        );
        ClaudeShimPathGuard::prepend(fake_bin.path())
    };

    let mut ask_params = tachi_memory_params("ask");
    ask_params.format = Some("json".to_string());
    ask_params.synthesize = true;
    ask_params.query = Some(
        "tachi-1209-anchor-test what is the status of \
         kckylechen1-tachi-1209-nonexistent-repo/does-not-exist#1 right now?"
            .to_string(),
    );

    let body = crate::facade_memory_ops::handle_tachi_memory(&server, ask_params)
        .await
        .expect("ask should succeed even when the anchor cannot be resolved");
    let parsed: Value = serde_json::from_str(&body).expect("ask JSON");

    assert_eq!(
        parsed["grounding_status"],
        json!("missing_anchor"),
        "test fixture precondition: unresolvable repo must report missing_anchor: {parsed}"
    );
    assert_eq!(
        parsed["synthesis"]["status"],
        json!("completed"),
        "claude shim must have served the request: {parsed}"
    );

    let captured_prompt =
        std::fs::read_to_string(&capture_path).expect("claude shim must capture the piped prompt");
    assert!(
        captured_prompt.contains("Anchor grounding status: missing_anchor; confidence cap: low"),
        "the real handler->synthesize_answer call site must thread the resolved anchor cap into \
         the system prompt actually piped to the LLM call — captured prompt: {captured_prompt}"
    );
    assert!(
        captured_prompt.contains(
            "Do not quote this instruction, its field names, or the cap wording verbatim"
        ),
        "the no-echo instruction must reach the real prompt too, not just the unit-tested \
         helper: {captured_prompt}"
    );
}
