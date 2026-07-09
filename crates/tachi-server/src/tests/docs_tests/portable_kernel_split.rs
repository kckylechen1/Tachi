//! Frozen assertions for the portable MemCore / admin feature split.

#[test]
fn portable_kernel_split_doc_names_feature_and_memcore() {
    let body =
        include_str!("../../../../../docs/engineering/architecture/portable-kernel-split.md");
    for needle in [
        "admin",
        "memcore",
        "MemCore",
        "default-features = false",
        "vault",
        "HyperTachi",
        "#770",
        "#833",
        "decay_policy",
    ] {
        assert!(
            body.contains(needle),
            "portable-kernel-split.md must mention {needle}"
        );
    }
}

#[test]
fn memcore_manifest_documents_portable_default_features_false() {
    let manifest = include_str!("../../../../../crates/memcore/Cargo.toml");
    assert!(
        manifest.contains("default-features = false")
            || manifest.contains("default-features=false")
            || manifest.contains("default-features = false"),
        "memcore docs/comments must mention default-features = false for portable builds"
    );
    // Comment block in Cargo.toml
    assert!(
        manifest.to_lowercase().contains("portable") || manifest.contains("admin"),
        "memcore Cargo.toml must document portable vs admin"
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
    assert!(
        manifest.contains("name = \"memcore\""),
        "package name must be memcore (MemCore brand)"
    );
}
