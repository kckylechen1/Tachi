use super::*;

// #1690 C3 slice A: `recommend_skill`'s ranking semantics are re-anchored onto
// the surviving scoring core `recommend_capabilities_inner` (kept because
// `handle_prepare_capability_bundle` — tachi_skill bundle/loadout — still calls
// it). Every assertion below maps 1:1 from the retired route's JSON shape onto
// the live `CapabilityRecommendation` rows; nothing here tests the deleted MCP
// surface.

#[tokio::test]
async fn recommend_capabilities_inner_prefers_matching_skill() {
    let server = make_server();
    let excel = make_skill_capability(
        "skill:excel-automation",
        "excel-automation",
        "Build spreadsheet workflows and Excel reports from CSV data.",
        "listed",
    );
    let web = make_skill_capability(
        "skill:web-research",
        "web-research",
        "Browse websites and summarize online sources.",
        "listed",
    );

    server
        .with_global_store(|store| {
            store.hub_register(&excel).map_err(|e| e.to_string())?;
            store.hub_register(&web).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("register skills");

    let results = crate::capability_ops::recommend_capabilities_inner(
        &server,
        "make an excel spreadsheet report from csv exports",
        Some("codex"),
        Some("skill"),
        3,
        false,
        false,
    )
    .expect("recommend capabilities should succeed");
    assert!(
        !results.is_empty(),
        "expected at least one skill recommendation"
    );
    assert_eq!(results[0].id, "skill:excel-automation");
    assert_eq!(
        results[0].suggested_tool_name.as_deref(),
        Some("tachi_skill_excel_automation")
    );
}

#[tokio::test]
async fn recommend_capabilities_inner_prefers_review_for_code_review_queries() {
    let server = make_server();
    let review = make_skill_capability(
        "skill:review",
        "review",
        "Inspect diffs and catch correctness, security, and maintainability risks before merge.",
        "listed",
    );
    let baoyu_markdown = make_skill_capability(
        "skill:baoyu-markdown-to-html",
        "baoyu-markdown-to-html",
        "Convert markdown docs to HTML, preserve code blocks, review formatting, and publish documentation.",
        "listed",
    );

    server
        .with_global_store(|store| {
            store.hub_register(&review).map_err(|e| e.to_string())?;
            store
                .hub_register(&baoyu_markdown)
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("register skills");

    let results = crate::capability_ops::recommend_capabilities_inner(
        &server,
        "code review",
        Some("codex"),
        Some("skill"),
        3,
        false,
        false,
    )
    .expect("recommend capabilities should succeed");
    let top_id = results[0].id.as_str();
    assert!(
        matches!(
            top_id,
            "skill:review" | "skill:waza-check" | "skill:superpowers-requesting-code-review"
        ),
        "expected a review workflow skill to win, got {results:?}"
    );
    assert_ne!(top_id, "skill:baoyu-markdown-to-html");
}

#[tokio::test]
async fn recommend_capabilities_inner_prefers_investigate_for_debug_500_error_queries() {
    let server = make_server();
    let investigate = make_skill_capability(
        "skill:investigate",
        "investigate",
        "Debug 500 errors by tracing requests, logs, and failing handlers.",
        "listed",
    );
    let feishu_docs = make_skill_capability(
        "skill:feishu-doc-reader",
        "feishu-doc-reader",
        "Read Feishu docs, error guides, and debugging notes for API integrations.",
        "listed",
    );

    server
        .with_global_store(|store| {
            store
                .hub_register(&investigate)
                .map_err(|e| e.to_string())?;
            store
                .hub_register(&feishu_docs)
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("register skills");

    let results = crate::capability_ops::recommend_capabilities_inner(
        &server,
        "debug 500 error",
        Some("codex"),
        Some("skill"),
        3,
        false,
        false,
    )
    .expect("recommend capabilities should succeed");
    assert_eq!(results[0].id, "skill:investigate");
}

#[tokio::test]
async fn recommend_capabilities_inner_prefers_ship_for_create_pr_queries() {
    let server = make_server();
    let ship = make_skill_capability(
        "skill:ship",
        "ship",
        "Ship code, prepare pull requests, and land changes safely.",
        "listed",
    );
    let review = make_skill_capability(
        "skill:review",
        "review",
        "Review code changes and summarize risks before merge.",
        "listed",
    );

    server
        .with_global_store(|store| {
            store.hub_register(&ship).map_err(|e| e.to_string())?;
            store.hub_register(&review).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("register skills");

    let results = crate::capability_ops::recommend_capabilities_inner(
        &server,
        "ship this code, create a PR",
        Some("codex"),
        Some("skill"),
        3,
        false,
        false,
    )
    .expect("recommend capabilities should succeed");
    assert_eq!(results[0].id, "skill:ship");
}

#[tokio::test]
async fn recommend_capabilities_inner_uses_active_patterns_as_ranking_context() {
    let server = make_server();
    let closure = make_skill_capability(
        "skill:marmalade-closure",
        "marmalade-closure",
        "Write marmalade closure notes and durable project completion records.",
        "listed",
    );
    let spreadsheet = make_skill_capability(
        "skill:spreadsheet",
        "spreadsheet",
        "Build spreadsheet reports from CSV exports.",
        "listed",
    );

    server
        .with_global_store(|store| {
            store.hub_register(&closure).map_err(|e| e.to_string())?;
            store
                .hub_register(&spreadsheet)
                .map_err(|e| e.to_string())?;
            let mut pattern = make_entry("pattern-alignment-bridge-closure");
            pattern.path = "/user/patterns/agent_os/alignment-bridge".to_string();
            pattern.summary = "Zephyr alignment bridge closes through marmalade closure".to_string();
            pattern.text =
                "When the user asks about the zephyr alignment bridge, use marmalade closure to write durable completion records."
                    .to_string();
            pattern.metadata = json!({
                "projection_kind": "pattern",
                "projection_key": "alignment-bridge-closure",
                "source_event_id": "pattern-event-recommend-skill",
                "counters": {"seen": 4, "hit": 2}
            });
            store.upsert(&pattern).map_err(|e| e.to_string())
        })
        .expect("seed skills and pattern");

    let results = crate::capability_ops::recommend_capabilities_inner(
        &server,
        "zephyr",
        Some("codex"),
        Some("skill"),
        3,
        false,
        false,
    )
    .expect("recommend capabilities should succeed");
    let top = &results[0];
    assert_eq!(
        top.id,
        "skill:marmalade-closure",
        "expected pattern-bridged marmalade skill to rank first, got {results:?}"
    );
    assert_eq!(
        top.pattern_refs[0]["projection_key"],
        json!("alignment-bridge-closure")
    );
    assert!(
        top.reasons
            .iter()
            .any(|reason| reason.contains("active pattern 'alignment-bridge-closure'")),
        "expected active pattern reason in {results:?}"
    );
}

#[tokio::test]
async fn recommend_capabilities_inner_host_bonus_ignores_definition_paths() {
    let server = make_server();
    let mut alpha = make_skill_capability(
        "skill:alpha",
        "host affinity fixture",
        "Fixture isolates host affinity from filesystem paths.",
        "listed",
    );
    let mut zeta = make_skill_capability(
        "skill:zeta",
        "host affinity fixture",
        "Fixture isolates host affinity from filesystem paths.",
        "listed",
    );
    alpha.definition = json!({
        "content": "host affinity fixture",
        "resolved_path": "/work/sigil/skills/fixture/SKILL.md"
    })
    .to_string();
    zeta.definition = json!({
        "content": "host affinity fixture",
        "resolved_path": "/work/codex-issue/skills/fixture/SKILL.md"
    })
    .to_string();

    server
        .with_global_store(|store| {
            store.hub_register(&alpha).map_err(|e| e.to_string())?;
            store.hub_register(&zeta).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("register path-variant skills");

    let results = crate::capability_ops::recommend_capabilities_inner(
        &server,
        "host affinity fixture",
        Some("codex"),
        Some("skill"),
        10,
        false,
        false,
    )
    .expect("recommend capabilities should succeed");
    let fixtures = results
        .iter()
        .filter(|rec| matches!(rec.id.as_str(), "skill:alpha" | "skill:zeta"))
        .collect::<Vec<_>>();

    assert_eq!(fixtures.len(), 2, "both path-variant skills must rank");
    assert_eq!(
        fixtures[0].id,
        "skill:alpha",
        "definition paths must not give skill:zeta a codex host bonus"
    );
    assert_eq!(
        fixtures[0].score, fixtures[1].score,
        "the absolute checkout path is not host affinity"
    );
    assert!(
        fixtures
            .iter()
            .flat_map(|rec| &rec.reasons)
            .all(|reason| reason != "mentions host 'codex'"),
        "a host bonus must come from stable capability metadata, never an implementation path"
    );
}

/// #1140 fix direction B discriminator: a host token that only occurs in
/// free-text `content` must not earn the host bonus. This proves the
/// amplifier (the old `definition.contains(host)` substring scan) is gone —
/// it fails against pre-#1166 `main`, where the same fixture flips
/// `skill:zeta` ahead of `skill:alpha`.
///
/// The two `content` strings are word-for-word identical except at one
/// position, where alpha carries a neutral filler token (`plumbob`) and
/// zeta carries the host token (`codex`). Neither word is a query token, so
/// this keeps token count, query-intersection size, and union size exactly
/// equal between the two fixtures — meaning `definition_overlap` (and thus
/// `capability_score`'s overall total) is provably identical by construction,
/// isolating the host bonus as the *only* variable the `assert_eq!` below can
/// be sensitive to. An earlier version of this fixture used content strings
/// with different token counts (8 vs 11), which made `definition_overlap`
/// diverge for reasons unrelated to host affinity and made the assertion
/// flaky/wrong independent of the fix under test.
#[tokio::test]
async fn recommend_capabilities_inner_host_bonus_ignores_free_text_mentions() {
    let server = make_server();
    let mut alpha = make_skill_capability(
        "skill:alpha",
        "host affinity fixture",
        "Fixture isolates host affinity from free-text content.",
        "listed",
    );
    let mut zeta = make_skill_capability(
        "skill:zeta",
        "host affinity fixture",
        "Fixture isolates host affinity from free-text content.",
        "listed",
    );
    alpha.definition = json!({
        "content": "host affinity fixture that happens to mention plumbob in its prose",
    })
    .to_string();
    zeta.definition = json!({
        "content": "host affinity fixture that happens to mention codex in its prose",
    })
    .to_string();

    server
        .with_global_store(|store| {
            store.hub_register(&alpha).map_err(|e| e.to_string())?;
            store.hub_register(&zeta).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("register content-variant skills");

    let results = crate::capability_ops::recommend_capabilities_inner(
        &server,
        "host affinity fixture",
        Some("codex"),
        Some("skill"),
        10,
        false,
        false,
    )
    .expect("recommend capabilities should succeed");
    let fixtures = results
        .iter()
        .filter(|rec| matches!(rec.id.as_str(), "skill:alpha" | "skill:zeta"))
        .collect::<Vec<_>>();

    assert_eq!(fixtures.len(), 2, "both content-variant skills must rank");
    assert_eq!(
        fixtures[0].score, fixtures[1].score,
        "a host token embedded in free-text content is not declared host affinity"
    );
    assert!(
        fixtures
            .iter()
            .flat_map(|rec| &rec.reasons)
            .all(|reason| reason != "mentions host 'codex'"),
        "free-text prose must not earn the host bonus, only declared metadata"
    );
}

/// #1140 fix direction B: host affinity declared in capability metadata
/// (the `tags` field builtins already populate, or an explicit `host` /
/// `hosts` field) must still earn the bonus once the raw substring scan
/// over the whole definition blob is gone. This fails against #1166 as
/// submitted, which deleted `definition.contains(host)` outright with no
/// scoped replacement.
#[tokio::test]
async fn recommend_capabilities_inner_host_bonus_matches_declared_metadata() {
    let server = make_server();
    let mut tagged = make_skill_capability(
        "skill:tagged",
        "host affinity fixture",
        "Fixture proves declared host metadata still earns the bonus.",
        "listed",
    );
    let mut untagged = make_skill_capability(
        "skill:untagged",
        "host affinity fixture",
        "Fixture proves declared host metadata still earns the bonus.",
        "listed",
    );
    tagged.definition = json!({
        "content": "neutral content with no host word anywhere in the prose",
        "tags": ["preset", "codex"],
    })
    .to_string();
    untagged.definition = json!({
        "content": "neutral content with no host word anywhere in the prose",
        "tags": ["preset"],
    })
    .to_string();

    server
        .with_global_store(|store| {
            store.hub_register(&tagged).map_err(|e| e.to_string())?;
            store.hub_register(&untagged).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("register tag-variant skills");

    let results = crate::capability_ops::recommend_capabilities_inner(
        &server,
        "host affinity fixture",
        Some("codex"),
        Some("skill"),
        10,
        false,
        false,
    )
    .expect("recommend capabilities should succeed");
    let fixtures = results
        .iter()
        .filter(|rec| matches!(rec.id.as_str(), "skill:tagged" | "skill:untagged"))
        .collect::<Vec<_>>();

    assert_eq!(fixtures.len(), 2, "both tag-variant skills must rank");
    let tagged = fixtures
        .iter()
        .find(|rec| rec.id == "skill:tagged")
        .expect("skill:tagged in results");
    let untagged = fixtures
        .iter()
        .find(|rec| rec.id == "skill:untagged")
        .expect("skill:untagged in results");
    assert!(
        tagged.score > untagged.score,
        "a host declared in `tags` must score strictly higher than a fixture without it \
         (tagged={}, untagged={})",
        tagged.score,
        untagged.score
    );
    assert!(
        tagged.reasons.iter().any(|reason| reason == "mentions host 'codex'"),
        "the bonus reason must still fire when host affinity is declared metadata"
    );
    assert!(
        untagged
            .reasons
            .iter()
            .all(|reason| reason != "mentions host 'codex'"),
        "the fixture without declared host metadata must not earn the bonus"
    );
}
