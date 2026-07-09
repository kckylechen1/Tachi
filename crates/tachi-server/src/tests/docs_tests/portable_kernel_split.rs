//! Frozen assertions for the portable kernel / admin feature split.

#[test]
fn portable_kernel_split_doc_names_feature_and_facade() {
    let body =
        include_str!("../../../../../docs/engineering/architecture/portable-kernel-split.md");
    for needle in [
        "admin",
        "portable-kernel",
        "memcore",
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
        "portable-kernel must depend on memcore with admin off"
    );
    assert!(
        manifest.contains("memcore"),
        "portable-kernel must depend on memcore"
    );
}

#[test]
fn memcore_manifest_declares_admin_feature() {
    let manifest = include_str!("../../../../../crates/memcore/Cargo.toml");
    assert!(
        manifest.contains("default = [\"admin\"]"),
        "memcore default features must enable admin for Tachi product"
    );
    assert!(
        manifest.contains("admin = []"),
        "memcore must declare empty admin feature gate"
    );
}
