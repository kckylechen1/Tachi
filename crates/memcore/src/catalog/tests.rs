//! Catalog **type**-level discriminators (tachi#1681 D7 PR-B, item 1).
//!
//! Store-level behavior lives in `db::tests::model_catalog_ops`; these are the
//! properties that hold before any SQL is involved — chiefly discrimination
//! 10, "pricing snapshots are immutable by construction", which is a claim
//! about the *type*, not about a table constraint.

use super::*;
use serde_json::json;

fn prices(input: &str) -> Value {
    json!({
        "unit": "per_mtok",
        "input": input,
        "output": "0.28",
    })
}

// ─── discrimination 10: immutable by construction ────────────────────────────

#[test]
fn identical_prices_mint_the_same_snapshot_id_regardless_of_when_they_were_seen() {
    let first = PricingSnapshot::mint(
        "deepseek",
        prices("0.14"),
        Some(CatalogSource::ProviderApi),
        "2026-08-11T00:00:00.000Z",
    );
    let hours_later = PricingSnapshot::mint(
        "deepseek",
        prices("0.14"),
        Some(CatalogSource::ProviderApi),
        "2026-08-11T09:30:00.000Z",
    );

    assert_eq!(
        first.snapshot_id(),
        hours_later.snapshot_id(),
        "re-importing an unchanged price sheet must dedupe onto the same id — if the clock \
         entered the digest, every refresh would mint a new snapshot and the dedupe property \
         would be noise"
    );
    assert_ne!(
        first.fetched_at, hours_later.fetched_at,
        "the observation timestamps still differ; only the id is clock-independent"
    );
}

#[test]
fn any_price_change_mints_a_new_snapshot_id() {
    let cheap = PricingSnapshot::mint("deepseek", prices("0.14"), None, "2026-08-11T00:00:00.000Z");
    let dearer =
        PricingSnapshot::mint("deepseek", prices("0.15"), None, "2026-08-11T00:00:00.000Z");
    assert_ne!(cheap.snapshot_id(), dearer.snapshot_id());
}

#[test]
fn the_same_prices_under_a_different_provider_are_a_different_snapshot() {
    let deepseek =
        PricingSnapshot::mint("deepseek", prices("0.14"), None, "2026-08-11T00:00:00.000Z");
    let siliconflow = PricingSnapshot::mint(
        "siliconflow",
        prices("0.14"),
        None,
        "2026-08-11T00:00:00.000Z",
    );
    assert_ne!(
        deepseek.snapshot_id(),
        siliconflow.snapshot_id(),
        "provider_kind is part of the identity: two providers charging the same rate are not \
         one price sheet"
    );
}

#[test]
fn snapshot_ids_carry_the_versioned_scheme_prefix() {
    let snapshot =
        PricingSnapshot::mint("deepseek", prices("0.14"), None, "2026-08-11T00:00:00.000Z");
    assert!(
        snapshot
            .snapshot_id()
            .starts_with(&format!("{PRICING_SNAPSHOT_SCHEME}:")),
        "got {}",
        snapshot.snapshot_id()
    );
}

#[test]
fn a_snapshot_whose_prices_were_rewritten_under_its_id_is_refused_on_read() {
    let honest = PricingSnapshot::mint(
        "deepseek",
        prices("0.14"),
        Some(CatalogSource::ProviderApi),
        "2026-08-11T00:00:00.000Z",
    );

    // Exactly the corruption the content-addressed key exists to make
    // impossible: the id a historical outcome row points at, now carrying
    // different prices.
    let tampered = PricingSnapshot::from_stored(
        honest.snapshot_id(),
        "deepseek",
        prices("999.00"),
        Some(CatalogSource::ProviderApi),
        "2026-08-11T00:00:00.000Z",
        "2026-08-11T00:00:00.000Z",
    );

    let err = tampered.expect_err("rewritten prices under an existing id must be refused");
    let message = err.to_string();
    assert!(
        message.contains("rewritten in place"),
        "the refusal must name what happened, not just fail: {message}"
    );

    // And the honest round trip still works.
    PricingSnapshot::from_stored(
        honest.snapshot_id(),
        honest.provider_kind(),
        honest.pricing_data().clone(),
        honest.catalog_source,
        honest.fetched_at.as_str(),
        honest.created_at.as_str(),
    )
    .expect("an untampered snapshot round-trips");
}

#[test]
fn serialized_snapshot_exposes_its_id_and_prices_but_offers_no_way_back_in() {
    let snapshot =
        PricingSnapshot::mint("deepseek", prices("0.14"), None, "2026-08-11T00:00:00.000Z");
    let encoded = serde_json::to_value(&snapshot).expect("snapshots serialize for status surfaces");
    assert_eq!(encoded["snapshot_id"], json!(snapshot.snapshot_id()));
    assert_eq!(encoded["pricing_data"], *snapshot.pricing_data());

    // The compile-time half of this property (no `Deserialize`, no setters)
    // is asserted by the doctest in the type's documentation; this test pins
    // the runtime half: what a status surface can see.
    assert!(encoded.get("snapshot_id").is_some());
}

// ─── deployment content digest ───────────────────────────────────────────────

fn env_deployment() -> NewModelDeployment {
    NewModelDeployment::observed(
        "env:extract",
        "env:api.siliconflow.cn",
        ProtocolKind::OpenAiChatCompletions,
        "Qwen/Qwen3.5-27B",
        CatalogSource::Env,
        "2026-08-11T00:00:00.000Z",
    )
    .with_endpoint_ref("https://api.siliconflow.cn/v1/chat/completions")
    .with_capabilities(DeploymentCapabilities {
        chat: true,
        ..DeploymentCapabilities::default()
    })
    .with_source_refs(vec![
        "env_api_key:EXTRACT_API_KEY".to_string(),
        "env_api_key:SILICONFLOW_API_KEY".to_string(),
    ])
}

#[test]
fn re_observing_the_same_deployment_later_does_not_change_its_content_digest() {
    let first = env_deployment();
    let mut second = env_deployment();
    second.fetched_at = "2026-08-12T04:05:06.000Z".to_string();

    assert_eq!(
        first.content_digest(),
        second.content_digest(),
        "fetched_at is an observation timestamp, not catalog content — letting it into the \
         digest would turn every process start into a revision bump"
    );
}

#[test]
fn a_model_change_changes_the_content_digest() {
    let before = env_deployment();
    let mut after = env_deployment();
    after.provider_model_id = "Qwen/Qwen3.5-72B".to_string();
    assert_ne!(before.content_digest(), after.content_digest());
}

#[test]
fn a_capability_change_changes_the_content_digest() {
    let before = env_deployment();
    let after = env_deployment().with_capabilities(DeploymentCapabilities {
        chat: true,
        embeddings: Some(EmbeddingsCapability { dimension: 1024 }),
        ..DeploymentCapabilities::default()
    });
    assert_ne!(before.content_digest(), after.content_digest());
}

#[test]
fn an_embedding_dimension_change_changes_the_content_digest() {
    let at_1024 = env_deployment().with_capabilities(DeploymentCapabilities {
        embeddings: Some(EmbeddingsCapability { dimension: 1024 }),
        ..DeploymentCapabilities::default()
    });
    let at_2048 = env_deployment().with_capabilities(DeploymentCapabilities {
        embeddings: Some(EmbeddingsCapability { dimension: 2048 }),
        ..DeploymentCapabilities::default()
    });
    assert_ne!(
        at_1024.content_digest(),
        at_2048.content_digest(),
        "a silent dimension change is the exact corruption the escape hatch gates; it must not \
         look like an unchanged deployment"
    );
}

// ─── closed vocabularies ─────────────────────────────────────────────────────

#[test]
fn catalog_source_round_trips_and_refuses_unknown_values() {
    for source in [
        CatalogSource::Env,
        CatalogSource::ProviderApi,
        CatalogSource::Manual,
    ] {
        assert_eq!(CatalogSource::parse(source.as_str()), Some(source));
    }
    assert_eq!(CatalogSource::parse("ENV"), None);
    assert_eq!(CatalogSource::parse("guessed"), None);
}

#[test]
fn protocol_kind_round_trips_and_refuses_unknown_values() {
    for protocol in [
        ProtocolKind::OpenAiChatCompletions,
        ProtocolKind::VoyageEmbeddings,
    ] {
        assert_eq!(ProtocolKind::parse(protocol.as_str()), Some(protocol));
    }
    assert_eq!(ProtocolKind::parse("anthropic_messages"), None);
}

#[test]
fn deployment_event_kind_round_trips_and_refuses_unknown_values() {
    for kind in [
        DeploymentEventKind::DeploymentImported,
        DeploymentEventKind::DeploymentUpdated,
        DeploymentEventKind::DeploymentRetired,
        DeploymentEventKind::HealthServed,
        DeploymentEventKind::HealthCooldown,
        DeploymentEventKind::HealthError,
    ] {
        assert_eq!(DeploymentEventKind::parse(kind.as_str()), Some(kind));
    }
    assert_eq!(DeploymentEventKind::parse("deployment_deleted"), None);
}

#[test]
fn the_two_authorities_sharing_the_event_log_stay_distinguishable() {
    // One append-only table carries both catalog metadata transitions and
    // health observations (#1681 D1/D4). A reader that could not tell them
    // apart would let "the deployment was throttled at 09:04" read as a change
    // to what the deployment *is* — which is the merge the four-authority rule
    // exists to prevent.
    for kind in [
        DeploymentEventKind::HealthServed,
        DeploymentEventKind::HealthCooldown,
        DeploymentEventKind::HealthError,
    ] {
        assert!(kind.is_health());
    }
    for kind in [
        DeploymentEventKind::DeploymentImported,
        DeploymentEventKind::DeploymentUpdated,
        DeploymentEventKind::DeploymentRetired,
    ] {
        assert!(!kind.is_health());
    }
}

#[test]
fn capabilities_round_trip_through_the_json_column_shape() {
    let capabilities = DeploymentCapabilities {
        chat: true,
        tools: true,
        streaming: true,
        structured_output: false,
        media: false,
        rerank: false,
        embeddings: Some(EmbeddingsCapability { dimension: 1024 }),
    };
    let encoded = serde_json::to_string(&capabilities).expect("capabilities encode");
    let decoded: DeploymentCapabilities = serde_json::from_str(&encoded).expect("and decode");
    assert_eq!(capabilities, decoded);

    // A row written before a field existed still reads: the column default is
    // `'{}'`, and every field is `#[serde(default)]`.
    let legacy: DeploymentCapabilities = serde_json::from_str("{}").expect("empty object decodes");
    assert_eq!(legacy, DeploymentCapabilities::default());
    assert!(legacy.embeddings.is_none());
}

#[test]
fn attachment_bounds_round_trip_from_the_empty_column_default() {
    let empty: AttachmentBounds = serde_json::from_str("{}").expect("empty object decodes");
    assert_eq!(empty, AttachmentBounds::default());
    let encoded = serde_json::to_string(&empty).expect("bounds encode");
    let decoded: AttachmentBounds = serde_json::from_str(&encoded).expect("and decode");
    assert_eq!(empty, decoded);
}

// ─── staleness (tachi#1681 D7 PR-B, item 5) ──────────────────────────────────

fn stored(expires_at: Option<&str>, status: &str) -> ModelDeployment {
    ModelDeployment {
        deployment_id: "env:extract".to_string(),
        provider_account_id: "env:api.siliconflow.cn".to_string(),
        endpoint_ref: Some("https://api.siliconflow.cn/v1/chat/completions".to_string()),
        protocol_kind: ProtocolKind::OpenAiChatCompletions,
        provider_model_id: "Qwen/Qwen3.5-27B".to_string(),
        effective_version: None,
        capabilities: DeploymentCapabilities {
            chat: true,
            ..DeploymentCapabilities::default()
        },
        context_window: None,
        max_output: None,
        attachment_bounds: AttachmentBounds::default(),
        region: None,
        data_policy: None,
        pricing_snapshot_ref: None,
        catalog_source: CatalogSource::ProviderApi,
        fetched_at: "2026-08-10T00:00:00.000Z".to_string(),
        effective_at: "2026-08-10T00:00:00.000Z".to_string(),
        expires_at: expires_at.map(str::to_string),
        status: status.to_string(),
        revision: 1,
        source_refs: Vec::new(),
        created_at: "2026-08-10T00:00:00.000Z".to_string(),
        updated_at: "2026-08-10T00:00:00.000Z".to_string(),
    }
}

#[test]
fn a_row_with_no_declared_expiry_is_always_fresh() {
    let row = stored(None, DEPLOYMENT_STATUS_ACTIVE);
    assert_eq!(
        row.freshness_at("2099-01-01T00:00:00.000Z")
            .expect("no expiry parses"),
        CatalogFreshness::Fresh
    );
    assert!(row
        .into_authoritative_at("2099-01-01T00:00:00.000Z")
        .is_ok());
}

#[test]
fn a_row_past_its_expiry_is_stale_and_not_authoritative() {
    let row = stored(Some("2026-08-11T00:00:00.000Z"), DEPLOYMENT_STATUS_ACTIVE);
    assert_eq!(
        row.freshness_at("2026-08-11T00:00:01.000Z")
            .expect("parses"),
        CatalogFreshness::Stale
    );

    let refusal = row
        .into_authoritative_at("2026-08-11T00:00:01.000Z")
        .expect_err("a stale row must not be usable as present-tense truth");
    assert!(matches!(refusal, NotAuthoritative::Expired { .. }));
    assert!(refusal.to_string().contains("expired"), "{refusal}");
}

#[test]
fn expiry_is_compared_as_an_instant_not_as_a_string() {
    // The discriminator that catches a lexical comparison. `+08:00` renders a
    // moment that is EARLIER than the `Z` timestamp it sorts after:
    //   expires_at 2026-08-11T08:00:00+08:00  ==  2026-08-11T00:00:00Z
    //   now        2026-08-11T00:30:00.000Z
    // So the row IS expired — but string-compared, "00:30…Z" < "08:00…+08:00"
    // and a lexical implementation would call it fresh and keep routing to a
    // deployment the provider has withdrawn.
    let row = stored(Some("2026-08-11T08:00:00+08:00"), DEPLOYMENT_STATUS_ACTIVE);
    let now = "2026-08-11T00:30:00.000Z";

    assert!(
        now < "2026-08-11T08:00:00+08:00",
        "precondition: the two timestamps sort the wrong way as strings"
    );
    assert_eq!(
        row.freshness_at(now).expect("both parse"),
        CatalogFreshness::Stale,
        "expiry must be decided on parsed instants; a string comparison gets this backwards"
    );
}

#[test]
fn the_instant_exactly_at_expiry_is_already_stale() {
    let row = stored(Some("2026-08-11T00:00:00.000Z"), DEPLOYMENT_STATUS_ACTIVE);
    assert_eq!(
        row.freshness_at("2026-08-11T00:00:00.000Z")
            .expect("parses"),
        CatalogFreshness::Stale,
        "`expires_at` is the first instant the row no longer speaks for"
    );
}

#[test]
fn a_retired_row_is_not_authoritative_even_while_fresh() {
    let row = stored(None, DEPLOYMENT_STATUS_RETIRED);
    let refusal = row
        .into_authoritative_at("2026-08-11T00:00:00.000Z")
        .expect_err("a retired deployment is not a candidate");
    assert!(matches!(refusal, NotAuthoritative::Status { .. }));
}

#[test]
fn an_unparseable_expiry_is_a_refusal_in_both_directions() {
    let row = stored(Some("next tuesday"), DEPLOYMENT_STATUS_ACTIVE);
    assert!(
        row.freshness_at("2026-08-11T00:00:00.000Z").is_err(),
        "unreadable freshness must not resolve to Fresh — that is fail-open — nor be silently \
         treated as Stale, which would hide a corrupt row behind an ordinary-looking expiry"
    );
    let refusal = row
        .into_authoritative_at("2026-08-11T00:00:00.000Z")
        .expect_err("and it must not become authoritative");
    assert!(matches!(
        refusal,
        NotAuthoritative::UnreadableTimestamp { .. }
    ));
}

#[test]
fn partitioning_reports_both_what_survived_and_why_the_rest_did_not() {
    let mut fresh = stored(None, DEPLOYMENT_STATUS_ACTIVE);
    fresh.deployment_id = "catalog:fresh".to_string();
    let mut expired = stored(Some("2026-08-01T00:00:00.000Z"), DEPLOYMENT_STATUS_ACTIVE);
    expired.deployment_id = "catalog:expired".to_string();
    let mut retired = stored(None, DEPLOYMENT_STATUS_RETIRED);
    retired.deployment_id = "catalog:retired".to_string();

    let (admitted, excluded) =
        partition_authoritative_at(vec![fresh, expired, retired], "2026-08-11T00:00:00.000Z");

    assert_eq!(admitted.len(), 1);
    assert_eq!(admitted[0].get().deployment_id, "catalog:fresh");
    assert_eq!(
        excluded
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        vec!["catalog:expired", "catalog:retired"],
        "a resolver that only sees survivors cannot explain an abstain (#1681 D5)"
    );
    assert!(matches!(excluded[0].1, NotAuthoritative::Expired { .. }));
    assert!(matches!(excluded[1].1, NotAuthoritative::Status { .. }));
}
