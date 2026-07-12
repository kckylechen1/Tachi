//! Tests for the component governance read model (Issue #796).

use super::*;
use crate::tests::make_server;
use crate::tool_params::TachiComponentParams;
use serde_json::Value;

fn list_params() -> TachiComponentParams {
    TachiComponentParams {
        action: "list".to_string(),
        format: Some("json".to_string()),
        component_id: None,
        component_type: None,
        include_archived: None,
        limit: None,
        project: None,
        repo: None,
    }
}

fn show_params(component_id: &str) -> TachiComponentParams {
    TachiComponentParams {
        action: "show".to_string(),
        format: Some("json".to_string()),
        component_id: Some(component_id.to_string()),
        component_type: None,
        include_archived: None,
        limit: None,
        project: None,
        repo: None,
    }
}

#[tokio::test]
async fn seed_component_records_persists_five_records_under_components_v0() {
    let server = make_server();

    let seeded = seed_component_records(&server).expect("seed should succeed");
    assert!(seeded, "first seed call should report true (seeded)");

    // Five records must be queryable under /components/v0/.
    let count = server
        .with_global_store_read(|store| {
            let entries = store
                .list_by_path(COMPONENT_PATH_PREFIX, 100, false)
                .map_err(|e| format!("list: {e}"))?;
            Ok::<_, String>(entries.len())
        })
        .expect("list should succeed");
    assert_eq!(count, 5, "expected 5 component records, got {count}");

    // Each must carry a component_record metadata object.
    let with_record = server
        .with_global_store_read(|store| {
            let entries = store
                .list_by_path(COMPONENT_PATH_PREFIX, 100, false)
                .map_err(|e| format!("list: {e}"))?;
            let n = entries
                .iter()
                .filter(|e| extract_component_record(&e.metadata).is_some())
                .count();
            Ok::<_, String>(n)
        })
        .expect("metadata check");
    assert_eq!(
        with_record, 5,
        "all 5 records must carry metadata.component_record"
    );
}

#[tokio::test]
async fn seed_component_records_is_idempotent_across_calls() {
    let server = make_server();

    let first = seed_component_records(&server).expect("first seed");
    assert!(first, "first call seeds");
    let second = seed_component_records(&server).expect("second seed");
    assert!(!second, "second call must be a no-op (marker already set)");

    let count = server
        .with_global_store_read(|store| {
            store
                .list_by_path(COMPONENT_PATH_PREFIX, 100, false)
                .map(|e| e.len())
                .map_err(|e| format!("list: {e}"))
        })
        .expect("list");
    assert_eq!(
        count, 5,
        "idempotent seed must not duplicate rows: got {count}"
    );
}

#[tokio::test]
async fn component_list_returns_compact_records() {
    let server = make_server();
    seed_component_records(&server).expect("seed");

    let body = handle_tachi_component(&server, list_params())
        .await
        .expect("list action");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["status"], json!("completed"));
    let records = parsed["records"].as_array().expect("records array");
    assert_eq!(records.len(), 5, "list must return 5 compact records");
    // Each compact record carries the four required fields.
    for rec in records {
        assert!(rec.get("component_id").is_some(), "missing component_id");
        assert!(
            rec.get("component_type").is_some(),
            "missing component_type"
        );
        assert!(rec.get("owner_repo").is_some(), "missing owner_repo");
        assert!(rec.get("summary").is_some(), "missing summary");
    }
}

#[tokio::test]
async fn component_show_returns_full_record_and_edges() {
    let server = make_server();
    seed_component_records(&server).expect("seed");

    let body = handle_tachi_component(&server, show_params("tachi-memory-kernel"))
        .await
        .expect("show action");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["status"], json!("completed"));
    let record = &parsed["record"];
    assert_eq!(
        record["component_id"],
        json!("tachi-memory-kernel"),
        "show must return the requested record"
    );
    assert_eq!(
        record["component_type"],
        json!("kernel"),
        "kernel record type"
    );
    // The kernel record declares three known downstream consumers → owns edges.
    let edges = parsed["edges"].as_array().expect("edges array");
    let owns_count = edges
        .iter()
        .filter(|e| e["relation"].as_str() == Some("owns"))
        .count();
    assert_eq!(
        owns_count, 3,
        "kernel must own its 3 known downstream consumers: got {owns_count}"
    );
}

#[tokio::test]
async fn component_show_unknown_returns_not_found() {
    let server = make_server();
    seed_component_records(&server).expect("seed");

    let body = handle_tachi_component(&server, show_params("nonexistent-component-xyz"))
        .await
        .expect("show action");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(
        parsed["status"],
        json!("not_found"),
        "unknown component must report not_found"
    );
}

// ─── Issue #797: read-only downstream classifier tests ───────────────────────

fn check_params(repo: &str) -> TachiComponentParams {
    TachiComponentParams {
        action: "check".to_string(),
        format: Some("json".to_string()),
        component_id: None,
        component_type: None,
        include_archived: None,
        limit: None,
        project: None,
        repo: Some(repo.to_string()),
    }
}

#[tokio::test]
async fn component_check_classifies_tachi_checkout_as_kernel_drift() {
    let server = make_server();
    seed_component_records(&server).expect("seed");
    // CARGO_MANIFEST_DIR is `<repo_root>/crates/tachi-server`, so the repo
    // root is two ancestors up (nth(0) = itself, nth(1) = `crates`,
    // nth(2) = repo root). Using nth(1) here previously landed on `crates/`,
    // which happened to still classify correctly ONLY because the git-remote
    // match (`git -C <path> remote get-url origin` searches upward for the
    // enclosing `.git`) papered over the wrong directory; the owner_path
    // existence check (e.g. `crates/memcore`) silently failed against the
    // wrong base, surfacing only when the checkout's origin remote is a
    // local-clone path instead of the GitHub URL (issue #997).
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap();
    let body = handle_tachi_component(&server, check_params(&repo_root.display().to_string()))
        .await
        .expect("check action");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["status"], json!("completed"));
    // The remote matches kckylechen1/tachi. Both tachi-memory-kernel and
    // tachi-event-projection-bridge share that owner_repo, so the classifier
    // must tie-break to the strongest match. Both are Remote-strength, so the
    // path_match_count tie-break decides between them: the kernel fixture
    // declares `crates/memcore; crates/tachi-server;
    // docs/.../kernel-surface-v1.fixture.json` — all 3 subpaths exist under
    // this checkout — while the bridge's owner_path
    // (`crates/tachi-server event projection and continuity paths`) is a
    // single prose phrase with no `;` separator and contains spaces, so it is
    // filtered out entirely by the path-match scan (0 matched subpaths).
    // 3 > 0 is not a fragile near-tie: on this fixture the kernel must win
    // deterministically, every time — asserting "kernel_drift OR bridge" would
    // silently tolerate the tie-break picking the wrong side (codex review,
    // PR #1006 CP5). A synthetic fixture-level test for the tie-break
    // mechanism itself (independent of this real checkout's fixture data)
    // lives in `classify_repo_prefers_higher_path_match_count_on_remote_tie`
    // below.
    let category = parsed["category"].as_str().expect("category");
    let matched = parsed["matched_component_id"].as_str().unwrap_or("(none)");
    assert_eq!(
        category, CATEGORY_KERNEL_DRIFT,
        "tachi checkout must classify as kernel_drift (count tie-break must favor the kernel's 3 matched owner_path subpaths over the bridge's 0), got {category}"
    );
    assert_eq!(
        matched, "tachi-memory-kernel",
        "must match tachi-memory-kernel, got {matched}"
    );
    // Evidence-gap expectation is derived from the checkout's ACTUAL git
    // origin remote rather than hardcoded, so the test is hermetic across
    // checkout shapes (issue #997): a normal clone/worktree with the real
    // GitHub remote is a confident Remote match (no gap), but a checkout
    // whose origin is a local-clone path (e.g. `git worktree add` off a
    // sibling checkout, or any dev clone with a filesystem-path origin, as
    // observed at /tmp/oz-test-969) legitimately falls back to a PathOnly
    // match and must carry the fork/drift evidence gap — that is the
    // classifier being honest about weaker evidence, not a bug.
    let empty = Vec::new();
    let gaps = parsed["evidence_gaps"].as_array().unwrap_or(&empty);
    let actual_remote = run_git_readonly(repo_root, &["remote", "get-url", "origin"]).ok();
    let remote_is_canonical = actual_remote
        .as_deref()
        .map(normalize_remote_to_owner_repo)
        .map(|rn| rn.eq_ignore_ascii_case("kckylechen1/tachi"))
        .unwrap_or(false);
    if remote_is_canonical {
        assert!(
            gaps.is_empty(),
            "remote-matched checkout must have no evidence gaps, got {gaps:?}"
        );
    } else {
        assert_eq!(
            gaps.len(),
            1,
            "non-canonical-remote checkout must carry exactly the path-only fork/drift gap, got {gaps:?}"
        );
        assert!(
            gaps[0]
                .as_str()
                .unwrap_or("")
                .contains("possible fork / drift"),
            "expected the fork/drift evidence gap, got {gaps:?}"
        );
    }
}

/// Fixture-level, checkout-independent proof of the count tie-break itself
/// (codex review, PR #1006 CP5): two synthetic records tied at Remote
/// strength (same owner_repo, real git origin configured to match), one
/// declaring 2 owner_path subpaths that exist under the checkout and the
/// other declaring only 1. The classifier must select the higher-count
/// record — this does not depend on the real tachi-memory-kernel /
/// tachi-event-projection-bridge fixture data, so it stays discriminating
/// even if that fixture's path counts ever change.
#[test]
fn classify_repo_prefers_higher_path_match_count_on_remote_tie() {
    let temp = tempfile::tempdir().expect("temp dir");
    let repo_path = temp.path();
    run_git_readonly(repo_path, &["init"]).expect("git init");
    run_git_readonly(
        repo_path,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/kckylechen1/tie-break-fixture.git",
        ],
    )
    .expect("git remote add");

    // two_path_dirs/{a,b} exist; one_path_dir/a exists but one_path_dir/b does not.
    std::fs::create_dir_all(repo_path.join("two_path_dirs/a")).expect("mkdir a");
    std::fs::create_dir_all(repo_path.join("two_path_dirs/b")).expect("mkdir b");
    std::fs::create_dir_all(repo_path.join("one_path_dir/a")).expect("mkdir c");

    let records = vec![
        json!({
            "component_id": "fixture-two-path-matches",
            "component_type": "kernel",
            "owner_repo": "kckylechen1/tie-break-fixture",
            "owner_path": "two_path_dirs/a; two_path_dirs/b",
        }),
        json!({
            "component_id": "fixture-one-path-match",
            "component_type": "workflow_bridge",
            "owner_repo": "kckylechen1/tie-break-fixture",
            "owner_path": "one_path_dir/a; one_path_dir/does-not-exist",
        }),
    ];

    let (category, matched, gaps) = classify_repo(&records, repo_path, None);
    assert_eq!(
        category, CATEGORY_KERNEL_DRIFT,
        "the record with more matched owner_path subpaths (2) must win the \
         Remote-strength tie over the record with fewer (1)"
    );
    assert_eq!(matched.as_deref(), Some("fixture-two-path-matches"));
    assert!(
        gaps.is_empty(),
        "a Remote-strength match must carry no evidence gaps, got {gaps:?}"
    );
}

/// Controlled structural proof that `classify_repo` resolves the true
/// checkout root before evaluating owner_path evidence, adapted (rebase of
/// PR #1006 onto main, #987/#997) for a checkout-root fix that landed on
/// `main` independently of, and via a different mechanism than, this
/// branch's own `nth(1)` -> `nth(2)` call-site fix. `main`'s `classify_repo`
/// runs `git rev-parse --show-toplevel` on whatever `repo_path` the caller
/// passes (`checkout_root` above) and walks UP to the enclosing `.git` — so
/// a caller-supplied subdirectory INSIDE a git tree (this branch's original
/// `nth(1)` bug shape, landing one level short of the checkout root) is
/// transparently self-corrected regardless of the call site's ancestor-count
/// choice. #1006's original fixture proved this by nesting the short root
/// inside a real git repo and asserting failure at the short root — that no
/// longer discriminates on `main`, since `checkout_root` corrects it. This
/// version keeps the git-nested case as the PRIMARY proof (it must now
/// SUCCEED via the walk-up — the regression this guards against is
/// `checkout_root`'s resolution being removed or broken), and additionally
/// proves the one shape that still cannot be corrected: a `repo_path` with
/// NO enclosing `.git` at all, where `--show-toplevel` fails and
/// `checkout_root` falls back to `repo_path` unresolved.
#[test]
fn classify_repo_path_evidence_requires_correct_checkout_root_depth() {
    let temp = tempfile::tempdir().expect("temp dir");
    let repo_root = temp.path();
    run_git_readonly(repo_root, &["init"]).expect("git init");

    // The kernel's real owner_path subpath, present only at the true root.
    std::fs::create_dir_all(repo_root.join("crates/memcore")).expect("mkdir memcore");
    // Simulate `<repo_root>/crates/tachi-server` (2 levels deep) as the
    // caller-supplied CARGO_MANIFEST_DIR-shaped path — the original `nth(1)`
    // bug's one-level-short resolution, still INSIDE the same git tree.
    let nested_in_git = repo_root.join("crates").join("tachi-server");
    std::fs::create_dir_all(&nested_in_git).expect("mkdir nested");
    assert_ne!(
        nested_in_git, repo_root,
        "sanity: the simulated short-root path must differ from the true root"
    );

    let records = vec![json!({
        "component_id": "fixture-kernel",
        "component_type": "kernel",
        "owner_repo": "does-not-matter-for-this-fixture/no-origin",
        "owner_path": "crates/memcore",
    })];

    // PRIMARY proof: a subdirectory one level short of the true root, but
    // still inside the same git tree, must classify correctly — `main`'s
    // `checkout_root` resolves it back to `repo_root` via
    // `git rev-parse --show-toplevel` before evaluating owner_path evidence.
    // This is the regression guard: if that walk-up were removed or broken,
    // `nested_in_git.join("crates/memcore")` would not exist and this would
    // report CATEGORY_UNKNOWN instead.
    let (nested_category, nested_matched, nested_gaps) =
        classify_repo(&records, &nested_in_git, None);
    assert_eq!(
        nested_category, CATEGORY_KERNEL_DRIFT,
        "a caller-supplied subdirectory one level short of the true root, but still \
         inside the same git tree, must classify via checkout_root's walk-up to the \
         real root; got matched={nested_matched:?} gaps={nested_gaps:?}"
    );
    assert_eq!(nested_matched.as_deref(), Some("fixture-kernel"));
    assert!(
        nested_gaps
            .iter()
            .any(|g| g.contains("git origin remote is unavailable")),
        "a PathOnly match with no origin remote at all must surface the \
         remote-unavailable fork/drift evidence gap, got {nested_gaps:?}"
    );

    // SECONDARY proof: a caller-supplied subdirectory with NO enclosing git
    // tree at all is the one shape `checkout_root`'s walk-up cannot correct
    // — `--show-toplevel` fails there and falls back to the path unresolved.
    let no_git_temp = tempfile::tempdir().expect("temp dir (no git)");
    let no_git_root = no_git_temp.path();
    std::fs::create_dir_all(no_git_root.join("crates/memcore")).expect("mkdir memcore");
    let no_git_nested = no_git_root.join("crates").join("tachi-server");
    std::fs::create_dir_all(&no_git_nested).expect("mkdir nested");

    let (short_root_category, short_root_matched, short_root_gaps) =
        classify_repo(&records, &no_git_nested, None);
    assert_eq!(
        short_root_category, CATEGORY_UNKNOWN,
        "a caller-supplied subdirectory with no enclosing git tree must fail to \
         classify — evidence lives at the true repo root, not at `crates/tachi-server`; \
         got matched={short_root_matched:?} gaps={short_root_gaps:?}"
    );
    assert!(short_root_matched.is_none());
}

#[tokio::test]
async fn component_check_unknown_repo_returns_unknown_with_evidence_gaps() {
    let server = make_server();
    seed_component_records(&server).expect("seed");
    let temp = tempfile::tempdir().expect("temp dir");
    let body = handle_tachi_component(&server, check_params(&temp.path().display().to_string()))
        .await
        .expect("check action");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(parsed["category"], json!(CATEGORY_UNKNOWN));
    let gaps = parsed["evidence_gaps"].as_array().expect("evidence_gaps");
    assert!(!gaps.is_empty(), "unknown must carry evidence gaps");
}

#[tokio::test]
async fn component_check_nonexistent_path_returns_unknown() {
    let server = make_server();
    seed_component_records(&server).expect("seed");
    let body = handle_tachi_component(
        &server,
        check_params("/tmp/nonexistent-component-check-path-xyz-797"),
    )
    .await
    .expect("check action");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["category"], json!(CATEGORY_UNKNOWN));
    let gaps = parsed["evidence_gaps"].as_array().expect("gaps");
    assert!(
        gaps.iter().any(|g| g
            .as_str()
            .map(|s| s.contains("does not exist"))
            .unwrap_or(false)),
        "nonexistent path must report the missing-path gap: {gaps:?}"
    );
}

#[test]
fn normalize_remote_handles_https_and_ssh_forms() {
    assert_eq!(
        normalize_remote_to_owner_repo("https://github.com/kckylechen1/tachi.git"),
        "kckylechen1/tachi"
    );
    assert_eq!(
        normalize_remote_to_owner_repo("git@github.com:kckylechen1/Quant_Analyzer_2026.git"),
        "kckylechen1/Quant_Analyzer_2026"
    );
    assert_eq!(
        normalize_remote_to_owner_repo("https://github.com/kckylechen1/RomanBath"),
        "kckylechen1/RomanBath"
    );
    // trailing slash must not survive normalization (would cause silent no-match)
    assert_eq!(
        normalize_remote_to_owner_repo("https://github.com/kckylechen1/tachi/"),
        "kckylechen1/tachi"
    );
    assert_eq!(
        normalize_remote_to_owner_repo("https://github.com/kckylechen1/tachi.git/"),
        "kckylechen1/tachi"
    );
}

// ─── Issue #798: cutover planner ─────────────────────────────────────────────

fn plan_params(from: &str, to: &str) -> TachiComponentParams {
    TachiComponentParams {
        action: "plan".to_string(),
        format: Some("json".to_string()),
        component_id: Some(from.to_string()),
        component_type: None,
        include_archived: None,
        limit: None,
        project: None,
        repo: Some(to.to_string()),
    }
}

fn outcome_actions(parsed: &Value, outcome: &str) -> Vec<String> {
    parsed["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| i["outcome"].as_str() == Some(outcome))
        .filter_map(|i| i["action"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn component_plan_hypermem_covers_aliases_direct_reader_trading_policy() {
    let server = make_server();
    seed_component_records(&server).expect("seed");

    let body = handle_tachi_component(
        &server,
        plan_params("tachi-memory-kernel", "hypermemory-trading-adapter"),
    )
    .await
    .expect("plan action");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(
        parsed["to"]["matched_component_id"],
        json!("hypermemory-trading-adapter")
    );
    assert_eq!(
        parsed["to"]["category"],
        json!(CATEGORY_ALLOWED_ADAPTER_POLICY)
    );

    let actions = outcome_actions(&parsed, OUTCOME_PULL);
    assert!(
        actions.iter().any(|a| a == "gate_aliases"),
        "hypermem plan must cover aliases gate: {actions:?}"
    );
    assert!(
        actions.iter().any(|a| a == "gate_direct_reader"),
        "hypermem plan must cover direct-reader gate: {actions:?}"
    );
    let adapt = outcome_actions(&parsed, OUTCOME_ADAPT);
    assert!(
        adapt.iter().any(|a| a == "gate_trading_policy"),
        "hypermem plan must cover trading policy as adapt: {adapt:?}"
    );

    // Outcomes group must distinguish pull/adapt/backflow/delete_retire.
    let outcomes: Vec<&str> = parsed["outcomes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| o["outcome"].as_str())
        .collect();
    for required in [
        OUTCOME_PULL,
        OUTCOME_ADAPT,
        OUTCOME_BACKFLOW,
        OUTCOME_DELETE_RETIRE,
    ] {
        assert!(
            outcomes.contains(&required),
            "plan must include outcome `{required}`: {outcomes:?}"
        );
    }
}

#[tokio::test]
async fn component_plan_zeroclaw_covers_chat_agent_and_event_projection_gates() {
    let server = make_server();
    seed_component_records(&server).expect("seed");

    let body = handle_tachi_component(
        &server,
        plan_params("tachi-memory-kernel", "zeroclaw-chat-memory-adapter"),
    )
    .await
    .expect("plan action");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["status"], json!("completed"));
    let actions = outcome_actions(&parsed, OUTCOME_PULL);
    assert!(
        actions.iter().any(|a| a == "gate_chat_agent_adapter"),
        "zeroclaw plan must cover chat-agent adapter gate: {actions:?}"
    );
    assert!(
        actions.iter().any(|a| a == "gate_event_projection"),
        "zeroclaw plan must cover event projection gate: {actions:?}"
    );
}

#[tokio::test]
async fn component_plan_romanbath_defaults_to_frontend_shell() {
    let server = make_server();
    seed_component_records(&server).expect("seed");

    let body = handle_tachi_component(
        &server,
        plan_params("tachi-memory-kernel", "romanbath-frontend-app-shell"),
    )
    .await
    .expect("plan action");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["status"], json!("completed"));
    assert_eq!(parsed["to"]["category"], json!(CATEGORY_FRONTEND_SHELL));
    let note = parsed["romanbath_note"].as_str().unwrap_or("");
    assert!(
        note.to_ascii_lowercase().contains("frontend")
            || note.to_ascii_lowercase().contains("shell"),
        "romanbath note must treat it as frontend shell: {note}"
    );
    // Must not claim product-owned memory policy without evidence.
    assert!(
        !note
            .to_ascii_lowercase()
            .contains("product-owned memory policy only"),
        "without product-memory evidence, note must default to shell"
    );
}

#[tokio::test]
async fn component_plan_unknown_source_returns_not_found() {
    let server = make_server();
    seed_component_records(&server).expect("seed");
    let body = handle_tachi_component(
        &server,
        plan_params("no-such-component", "hypermemory-trading-adapter"),
    )
    .await
    .expect("plan action");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["status"], json!("not_found"));
}

// ─── Issue #799: briefing/status context ─────────────────────────────────────

#[tokio::test]
async fn component_governance_context_matches_tachi_checkout() {
    let server = make_server();
    seed_component_records(&server).expect("seed");
    // See the nth(2) note on component_check_classifies_tachi_checkout_as_kernel_drift
    // above — CARGO_MANIFEST_DIR is `<repo_root>/crates/tachi-server`.
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap();

    let ctx =
        component_governance_context(&server, Some("tachi"), Some(repo_root)).expect("context");
    assert_eq!(ctx["status"], json!("completed"));
    assert_eq!(ctx["authority"], json!("governance_registry"));
    let matches = ctx["matches"].as_array().expect("matches");
    assert!(
        !matches.is_empty(),
        "tachi checkout must match at least one governance record"
    );
    // Freshness must be labeled (current/stale/unknown) — never silent.
    for m in matches {
        let state = m["freshness"]["state"].as_str().unwrap_or("");
        assert!(
            matches!(state, "current" | "stale" | "unknown"),
            "freshness state required, got {state}"
        );
        // Registry note must not claim memory truth.
        let label = m["freshness"]["label"].as_str().unwrap_or("");
        assert!(
            !label.is_empty(),
            "freshness label required for {}",
            m["component_id"]
        );
    }

    let warnings = component_governance_warning_lines(&ctx);
    // blocked_fork on zeroclaw may appear if project-name also matches; for
    // tachi path we at least expect no panic and a Vec.
    let _ = warnings;
}

#[tokio::test]
async fn component_governance_context_project_hint_matches_hypermem() {
    let server = make_server();
    seed_component_records(&server).expect("seed");
    // No real Quant path — project name alone must surface the trading adapter.
    let ctx =
        component_governance_context(&server, Some("Quant_Analyzer_2026"), None).expect("context");
    let matches = ctx["matches"].as_array().expect("matches");
    assert!(
        matches
            .iter()
            .any(|m| { m["component_id"].as_str() == Some("hypermemory-trading-adapter") }),
        "project Quant_Analyzer_2026 must match hypermemory adapter: {matches:?}"
    );
    let hm = matches
        .iter()
        .find(|m| m["component_id"].as_str() == Some("hypermemory-trading-adapter"))
        .expect("hypermem match");
    // Drift/backflow must be surfaced for cutover awareness.
    assert!(
        hm["known_drift"].as_array().is_some_and(|d| !d.is_empty()),
        "hypermem match must surface known_drift"
    );
    assert!(
        hm["backflow_candidates"]
            .as_array()
            .is_some_and(|b| !b.is_empty()),
        "hypermem match must surface backflow_candidates"
    );
}
