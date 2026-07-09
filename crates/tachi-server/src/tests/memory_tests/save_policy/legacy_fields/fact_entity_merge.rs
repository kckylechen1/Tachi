use super::*;

#[test]
fn fact_to_entry_merges_legacy_persons_into_entities() {
    let fact = json!({
        "text": "Kyle migrated Sigil search completely and successfully.",
        "topic": "migration",
        "keywords": ["sigil", "search"],
        "persons": ["Kyle", ""],
        "entities": ["Sigil", "tachi-server"],
        "scope": "project",
        "importance": 0.9
    });

    let entry = crate::tool_params::fact_to_entry(&fact, "extraction", json!({}))
        .expect("fact_to_entry should build an entry");
    assert!(entry.persons.is_empty());
    assert_eq!(
        entry.entities,
        vec![
            "Sigil".to_string(),
            "tachi-server".to_string(),
            "Kyle".to_string(),
            "user".to_string()
        ]
    );
}
