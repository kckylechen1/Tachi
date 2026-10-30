//! One-shot golden generator for the v40 schema inventory.
//!
//! Run explicitly with: cargo test -p memcore --test golden_gen -- --nocapture
//! Not a regression gate: it prints the current classifier's fresh-store
//! inventory so maintainers can regenerate goldens after a catalogue change.

#[test]
fn generate_golden_v40_from_fresh_fixture() {
    for (profile, name) in [
        (memcore::db::StoreProfile::TachiFull, "TachiFull"),
        (memcore::db::StoreProfile::PortableKernel, "PortableKernel"),
    ] {
        let dir = tempfile::tempdir().expect("owned fixture directory");
        let path = dir.path().join("tachi-memory.db");
        let store = memcore::MemoryStore::open_with_context(
            path.to_str().unwrap(),
            &memcore::DbOpenContext::create_fresh().with_exact_profile(profile),
        )
        .expect("create fresh fixture");
        drop(store);
        let conn = rusqlite::Connection::open(&path).unwrap();
        let classified = memcore::db::schema_inventory::classified_inventory(&conn);
        let golden = serde_json::json!({
            "required": classified.required,
            "classification": classified.classes,
        });
        eprintln!("GOLDEN-BEGIN-{name}");
        eprintln!("{}", serde_json::to_string_pretty(&golden).unwrap());
        eprintln!("GOLDEN-END-{name}");
    }
}
