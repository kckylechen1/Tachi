use super::*;
use crate::capability_ops::recommend_capabilities_inner;
use serde_json::json;

/// #1140 fix direction B discriminator: definition paths must not trigger
/// host bonuses. Proves that path-like strings inside definitions do not leak
/// into host matching.
#[tokio::test]
async fn recommend_skill_host_bonus_ignores_filesystem_paths() {
    let server = make_server();
    let mut alpha = make_skill_capability(
        "skill:alpha",
        "host affinity fixture",
        "Fixture isolates host affinity from definition paths.",
        "listed",
    );
    let mut zeta = make_skill_capability(
        "skill:zeta",
        "host affinity fixture",
        "Fixture isolates host affinity from definition paths.",
        "listed",
    );
    alpha.definition = json!({
        "path": "/home/user/neutral-checkout/crates/skill",
    })
    .to_string();
    zeta.definition = json!({
        "path": "/home/user/codex-checkout/crates/skill",
    })
    .to_string();

    server
        .with_global_store(|store| {
            store.hub_register(&alpha).map_err(|e| e.to_string())?;
            store.hub_register(&zeta).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("register path-variant skills");

    let results = recommend_capabilities_inner(
        &server,
        "host affinity fixture",
        Some("codex"),
        Some("skill"),
        10,
        false,
        false,
    )
    .expect("recommend_capabilities_inner should succeed");

    let fixtures = results
        .iter()
        .filter(|rec| rec.id == "skill:alpha" || rec.id == "skill:zeta")
        .collect::<Vec<_>>();

    assert_eq!(fixtures.len(), 2, "both path-variant skills must rank");
    assert_eq!(
        fixtures[0].id, "skill:alpha",
        "definition paths must not give skill:zeta a codex host bonus"
    );
    assert_eq!(
        fixtures[0].score, fixtures[1].score,
        "the absolute checkout path is not host affinity"
    );
    assert!(
        fixtures
            .iter()
            .flat_map(|rec| rec.reasons.iter())
            .all(|reason| reason != "mentions host 'codex'"),
        "a host bonus must come from stable capability metadata, never an implementation path"
    );
}

/// #1140 fix direction B discriminator: a host token that only occurs in
/// free-text `content` must not earn the host bonus. This proves the
/// amplifier (the old `definition.contains(host)` substring scan) is gone.
#[tokio::test]
async fn recommend_skill_host_bonus_ignores_free_text_mentions() {
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

    let results = recommend_capabilities_inner(
        &server,
        "host affinity fixture",
        Some("codex"),
        Some("skill"),
        10,
        false,
        false,
    )
    .expect("recommend_capabilities_inner should succeed");

    let fixtures = results
        .iter()
        .filter(|rec| rec.id == "skill:alpha" || rec.id == "skill:zeta")
        .collect::<Vec<_>>();

    assert_eq!(fixtures.len(), 2, "both content-variant skills must rank");
    assert_eq!(
        fixtures[0].score, fixtures[1].score,
        "a host token embedded in free-text content is not declared host affinity"
    );
    assert!(
        fixtures
            .iter()
            .flat_map(|rec| rec.reasons.iter())
            .all(|reason| reason != "mentions host 'codex'"),
        "free-text prose must not earn the host bonus, only declared metadata"
    );
}

/// #1140 fix direction B: host affinity declared in capability metadata
/// (the `tags` field builtins already populate, or an explicit `host` /
/// `hosts` field) must still earn the bonus once the raw substring scan
/// over the whole definition blob is gone.
#[tokio::test]
async fn recommend_skill_host_bonus_matches_declared_metadata() {
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

    let results = recommend_capabilities_inner(
        &server,
        "host affinity fixture",
        Some("codex"),
        Some("skill"),
        10,
        false,
        false,
    )
    .expect("recommend_capabilities_inner should succeed");

    let fixtures = results
        .iter()
        .filter(|rec| rec.id == "skill:tagged" || rec.id == "skill:untagged")
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
        tagged
            .reasons
            .iter()
            .any(|reason| reason == "mentions host 'codex'"),
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
