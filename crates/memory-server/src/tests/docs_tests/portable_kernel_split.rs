//! Frozen assertions for the portable kernel / admin feature split.

#[test]
fn portable_kernel_split_doc_names_feature_and_facade() {
    let body =
        include_str!("../../../../../docs/engineering/architecture/portable-kernel-split.md");
    for needle in [
        "admin",
        "portable-kernel",
        "memory-core",
        "default-features = false",
        "vault",
        "HyperTachi",
        "#770",
        "#833",
    ] {
        assert!(
            body.contains(needle),
            "portable-kernel-split.md must mention {needle}"
        );
    }
}

#[test]
fn portable_kernel_crate_manifest_disables_admin() {
    let manifest = include_str!("../../../../../crates/portable-kernel/Cargo.toml");
    assert!(
        manifest.contains("default-features = false"),
        "portable-kernel must depend on memory-core with admin off"
    );
    assert!(
        manifest.contains("memory-core"),
        "portable-kernel must depend on memory-core"
    );
}

#[test]
fn memory_core_manifest_declares_admin_feature() {
    let manifest = include_str!("../../../../../crates/memory-core/Cargo.toml");
    assert!(
        manifest.contains("default = [\"admin\"]"),
        "memory-core default features must enable admin for Tachi product"
    );
    assert!(
        manifest.contains("admin = []"),
        "memory-core must declare empty admin feature gate"
    );
}
