use super::*;

fn listing_test_server() -> MemoryServer {
    let db_path = std::env::temp_dir().join(format!(
        "dispatch-profiles-listing-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    MemoryServer::new(db_path, None).expect("test memory server")
}

/// tachi#1173 item 2 discriminator: on origin/main (pre-#1173)
/// `dispatch_profiles_json_for_server` always returned the full mbit_card
/// (stats/guidance/moves/personality/skill_loadout/evidence_contract) for
/// every profile row, so this assertion is RED before the fix (mbit_card is
/// always present) and GREEN after (absent by default; present at
/// verbose=true). The default row must still carry the four fields the issue
/// names: name, backend, model, role.
#[test]
fn dispatch_profiles_listing_default_is_slim_rows() {
    let server = listing_test_server();

    let slim = dispatch_profiles_json_for_server(&server, false).expect("slim profiles listing");
    let rows = slim["dispatch_profiles"]
        .as_array()
        .expect("dispatch_profiles array");
    assert!(!rows.is_empty(), "{slim}");
    assert_eq!(slim["verbose"], json!(false), "{slim}");

    for row in rows {
        assert!(row["name"].is_string(), "{row}");
        assert!(row["backend"].is_string(), "{row}");
        assert!(row["role"].is_string(), "{row}");
        // `model` may be null for a profile without a resolved model, but the
        // key itself must be present (a slim row, not a missing field).
        assert!(row.get("model").is_some(), "{row}");
        assert!(
            row.get("mbit_card").is_none(),
            "slim profile row must not carry the full mbit_card: {row}"
        );
        assert!(
            row.get("skill_loadout").is_none(),
            "slim profile row must not carry skill_loadout: {row}"
        );
        assert!(
            row.get("evidence_contract").is_none(),
            "slim profile row must not carry evidence_contract: {row}"
        );
        // The whole point is fewer keys, not the same keys with null values —
        // exactly the 4 named fields on a slim row.
        assert_eq!(
            row.as_object().map(|obj| obj.len()),
            Some(4),
            "slim row should carry exactly name/backend/model/role: {row}"
        );
    }
    let raw = serde_json::to_string(&slim).expect("serialize");
    assert!(!raw.contains("mbit_card"), "{raw}");
}

/// tachi#1173 item 2 discriminator (verbose escape hatch): verbose=true must
/// restore the exact pre-#1173 full-card shape so no information is lost.
#[test]
fn dispatch_profiles_listing_verbose_true_restores_full_cards() {
    let server = listing_test_server();

    let full = dispatch_profiles_json_for_server(&server, true).expect("verbose profiles listing");
    let rows = full["dispatch_profiles"]
        .as_array()
        .expect("dispatch_profiles array");
    assert!(!rows.is_empty(), "{full}");
    assert_eq!(full["verbose"], json!(true), "{full}");

    for row in rows {
        assert!(
            row["mbit_card"].is_object(),
            "verbose=true row must carry the full mbit_card: {row}"
        );
        assert!(
            row["skill_loadout"].is_object() || row["skill_loadout"].is_array(),
            "verbose=true row must carry skill_loadout: {row}"
        );
    }
}
