//! Frozen assertions for downstream sync / cratesplit law (#770/#833).

#[test]
fn downstream_sync_surface_names_three_products_and_whitelist() {
    let body =
        include_str!("../../../../../docs/engineering/architecture/downstream-sync-surface.md");
    for needle in [
        "Quant",
        "HyperMemory",
        "zeroclaw",
        "RomanBath",
        "zeroclaw-memory-sigil",
        "memcore",
        "memcore",
        "Whitelist",
        "Blacklist",
        "Hyperion-HyperTachi",
        "#833",
        "#770",
    ] {
        assert!(
            body.contains(needle),
            "downstream-sync-surface.md must mention {needle}"
        );
    }
    assert!(
        body.contains("Do not") || body.contains("Anti-patterns"),
        "must record anti-patterns for naive rsync"
    );
}
