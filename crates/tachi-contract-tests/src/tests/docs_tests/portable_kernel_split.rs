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

/// The invariant behind the portable-kernel split is that a portable build
/// (`default-features = false`) resolves **none** of the product-only
/// dependencies — not that memcore's `admin` gate is textually empty. Empty
/// was merely the shape the gate happened to have when this guard was first
/// written, so freezing the literal `admin = []` froze the *means* instead of
/// the *end*: it made any legitimate admin-gated dependency impossible to add,
/// while still failing to catch the case that actually breaks portability (a
/// **non-optional** dependency, which a portable build resolves no matter what
/// the gate says).
///
/// tachi#1680's keyed credential fingerprints need a MAC-capable hash
/// (`blake2`, since `sha2` cannot MAC without also pulling in `hmac`), and
/// `optional = true` + `admin = ["dep:blake2"]` is precisely the correct use of
/// this invariant — the portable kernel stays on one hash family and resolves
/// no `blake2` at all. So the assertion is restated against the end.
///
/// **What this test is and is not.** Measured, not assumed: Cargo itself
/// refuses to load a manifest where a feature gate names a non-optional
/// dependency (`feature 'admin' includes 'dep:blake2', but 'blake2' is not an
/// optional dependency`), and equally refuses a `default` that names a missing
/// feature. So the per-dependency loop below cannot fire on any manifest that
/// builds at all — it is a defense-in-depth restatement of intent for a human
/// reader, not the primary enforcement. The primary enforcement for portable
/// purity is the portable build gate itself (`cargo test -p portable-kernel
/// --features portable-contract-test`), which compiles the crate with `admin`
/// off and is what actually fails if product-only code or deps leak in.
///
/// Neither this test nor the `admin = []` literal it replaces catches the one
/// genuinely uncaught manifest-level leak — a product-only dependency added as
/// plain non-optional, referenced by no feature at all. That gap is the
/// portable build gate's job, and is called out here so nobody mistakes a green
/// run of this test for proof of portable purity.
///
/// The third leg — portable-kernel depending on memcore with
/// `default-features = false` — is already pinned by
/// [`portable_kernel_crate_manifest_disables_admin`] above, and is deliberately
/// not duplicated here.
#[test]
fn memcore_admin_gate_deps_are_optional_and_excluded_from_portable_builds() {
    let manifest = include_str!("../../../../../crates/memcore/Cargo.toml");
    assert!(
        manifest.contains("default = [\"admin\"]"),
        "memcore default features must enable admin for Tachi product"
    );

    let features = manifest_section(manifest, "features")
        .expect("memcore manifest must declare a [features] section");
    // Not `contains("admin = [")`: the gate must parse as an array so the
    // per-dependency check below cannot pass vacuously by failing to find it.
    let admin_gate = toml_array_value(&features, "admin")
        .expect("memcore must declare an `admin` feature gate as an array");
    let dependencies = manifest_section(manifest, "dependencies")
        .expect("memcore manifest must declare a [dependencies] section");

    for entry in admin_gate {
        // `dep:foo` is the explicit spelling, but a bare `foo` naming a
        // dependency enables that optional dependency just the same — check
        // both so the bare form is not a silent bypass. An entry that matches
        // no dependency names another feature, and has nothing to enforce.
        let name = entry.strip_prefix("dep:").unwrap_or(entry.as_str());
        let Some(declaration) = toml_entry_value(&dependencies, name) else {
            continue;
        };
        assert!(
            declaration.contains("optional = true"),
            "memcore's admin gate enables `{name}`, so `{name}` must be declared \
             `optional = true` in [dependencies]; otherwise a portable build \
             (default-features = false) still resolves it and the split leaks"
        );
    }
}

/// Body of a top-level `[name]` table, up to the next top-level header.
fn manifest_section(manifest: &str, name: &str) -> Option<String> {
    let header = format!("[{name}]");
    let mut lines = manifest.lines().skip_while(|line| line.trim() != header);
    // Consumes the header itself; `None` here means it was never found.
    lines.next()?;
    Some(
        lines
            .take_while(|line| !line.trim_start().starts_with('['))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Raw value text for `key = ...` in a section body, following an inline table
/// or array across lines until its brackets balance.
fn toml_entry_value(section: &str, key: &str) -> Option<String> {
    let mut lines = section.lines();
    let first = lines.find(|line| {
        line.trim_start()
            .strip_prefix(key)
            .is_some_and(|rest| rest.trim_start().starts_with('='))
    })?;
    let mut value = first.split_once('=')?.1.trim().to_string();
    while !brackets_balanced(&value) {
        value.push('\n');
        value.push_str(lines.next()?.trim());
    }
    Some(value)
}

fn toml_array_value(section: &str, key: &str) -> Option<Vec<String>> {
    let raw = toml_entry_value(section, key)?;
    let inner = raw.trim().strip_prefix('[')?.rsplit_once(']')?.0;
    Some(
        inner
            .split(',')
            .map(|item| item.trim().trim_matches('"').to_string())
            .filter(|item| !item.is_empty())
            .collect(),
    )
}

fn brackets_balanced(value: &str) -> bool {
    let balanced = |open: char, close: char| {
        value.chars().filter(|c| *c == open).count()
            == value.chars().filter(|c| *c == close).count()
    };
    balanced('[', ']') && balanced('{', '}')
}
