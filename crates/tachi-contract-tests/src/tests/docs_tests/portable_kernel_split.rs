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

/// #1319-D2 deleted the `tachi_arena` facade; this is the guard that would
/// have caught the D2 doc lag (README still advertising the retired tool).
#[test]
fn readme_does_not_reference_retired_tachi_arena() {
    let readme = include_str!("../../../../../README.md");
    assert!(
        !readme.contains("tachi_arena"),
        "README.md must not reference the retired tachi_arena facade (#1319-D2)"
    );
}

#[test]
fn zeroclaw_direction_is_not_documented_as_current_wiring() {
    let surfaces = [
        ("README", include_str!("../../../../../README.md")),
        (
            "portable-kernel manifest",
            include_str!("../../../../../crates/portable-kernel/Cargo.toml"),
        ),
        (
            "portable-kernel crate docs",
            include_str!("../../../../../crates/portable-kernel/src/lib.rs"),
        ),
        (
            "portable-server manifest",
            include_str!("../../../../../crates/portable-server/Cargo.toml"),
        ),
        (
            "tachi-server crate docs",
            include_str!("../../../../../crates/tachi-server/src/lib.rs"),
        ),
        (
            "portable-kernel split doc",
            include_str!("../../../../../docs/engineering/architecture/portable-kernel-split.md"),
        ),
        (
            "downstream sync doc",
            include_str!("../../../../../docs/engineering/architecture/downstream-sync-surface.md"),
        ),
    ];

    for (name, body) in surfaces {
        let normalized = body
            .replace("//!", " ")
            .replace('#', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase();
        assert!(
            !normalized.contains("zeroclaw links directly"),
            "{name} must not present the future ZeroClaw direction as current wiring"
        );
        assert!(
            normalized.contains("future") && normalized.contains("zeroclaw"),
            "{name} must label the ZeroClaw direction as future work"
        );
        assert!(
            normalized.contains("no direct zeroclaw")
                || normalized.contains("does not currently depend on this crate"),
            "{name} must state that direct ZeroClaw integration is not current"
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
