//! `tachi-runtime-core` dependency allowlist (#2077 PR C).
//!
//! The crate may depend only on `memcore`, `tachi-params`, `tachi-llm`,
//! `vault-kit`, and third-party crates already in the workspace lock.
//! A dependency on `tachi-server` or on any other workspace crate must
//! RED here — that would break the "leaf utilities only" contract.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const ALLOWED_WORKSPACE_DEPS: &[&str] = &["memcore", "tachi-params", "tachi-llm", "vault-kit"];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Package names of every workspace member, parsed from the root
/// manifest's `members` list and each member's own `Cargo.toml`.
fn workspace_package_names(root: &Path) -> BTreeSet<String> {
    let text =
        std::fs::read_to_string(root.join("Cargo.toml")).expect("workspace Cargo.toml readable");
    let mut names = BTreeSet::new();
    for line in text.lines() {
        let line = line.trim().trim_end_matches(',');
        let Some(member) = line.strip_prefix('"').and_then(|l| l.strip_suffix('"')) else {
            continue;
        };
        if member.is_empty() {
            continue;
        }
        let Ok(member_text) = std::fs::read_to_string(root.join(member).join("Cargo.toml")) else {
            continue;
        };
        if let Some(name_line) = member_text.lines().find(|l| l.trim().starts_with("name =")) {
            let name = name_line
                .split('=')
                .nth(1)
                .map(|s| s.trim().trim_matches('"').to_string())
                .unwrap_or_default();
            if !name.is_empty() {
                names.insert(name);
            }
        }
    }
    names
}

/// Dependency names from every `*dependencies*` table of a manifest.
///
/// Covers inline tables (`name = { .. }`), dotted table headers
/// (`[dependencies.name]`), and `package = "..."` renames — the rename
/// target is the crate actually linked, so it is checked as a dependency.
fn dependency_names(manifest_text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_section = false;
    for line in manifest_text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if let Some(close) = trimmed.rfind(']') {
                let inner = &trimmed[1..close];
                // `[dependencies.x]`, `[dev-dependencies.x]` and
                // `[target.'cfg(..)'.dependencies.x]` all name the crate after
                // the last `dependencies.` segment.
                if let Some(idx) = inner.rfind("dependencies.") {
                    let name = inner[idx + "dependencies.".len()..]
                        .trim()
                        .trim_matches('"');
                    if !name.is_empty() {
                        names.push(name.to_string());
                        in_section = false;
                        continue;
                    }
                }
                in_section = inner.contains("dependencies");
                continue;
            }
        }
        if !in_section || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(name) = trimmed.split('=').next() {
            let name = name.trim();
            if !name.is_empty() {
                names.push(name.to_string());
            }
        }
        if let Some(idx) = trimmed.find("package = \"") {
            let rest = &trimmed[idx + "package = \"".len()..];
            if let Some(end) = rest.find('"') {
                names.push(rest[..end].to_string());
            }
        }
    }
    names
}

#[test]
fn runtime_core_dependencies_stay_within_allowlist() {
    let root = workspace_root();
    let manifest = root.join("crates/tachi-runtime-core/Cargo.toml");
    let text = std::fs::read_to_string(&manifest).expect("tachi-runtime-core/Cargo.toml readable");
    let workspace_names = workspace_package_names(&root);
    assert!(
        !workspace_names.is_empty(),
        "workspace member parse produced no package names; the test's parser is broken"
    );
    for dep in dependency_names(&text) {
        assert!(
            dep != "tachi-runtime-core",
            "tachi-runtime-core declares a self-dependency: {dep}"
        );
        if workspace_names.contains(&dep) {
            assert!(
                ALLOWED_WORKSPACE_DEPS.contains(&dep.as_str()),
                "tachi-runtime-core depends on workspace crate '{dep}', outside the \
                 #2077 allowlist {ALLOWED_WORKSPACE_DEPS:?}"
            );
        }
    }
}

#[test]
fn dependency_parser_sees_every_table_form() {
    let manifest = r#"
[dependencies]
memcore = { path = "../memcore" }
linked = { package = "tachi-server", path = "../tachi-server" }

[dependencies.plain-dotted]
path = "../a"

[dev-dependencies.dev-dotted]
path = "../b"

[build-dependencies.build-dotted]
path = "../c"

[target.'cfg(unix)'.dependencies.target-dotted]
path = "../d"

[target.'cfg(unix)'.dev-dependencies]
target-inline = { path = "../e" }
"#;
    let names = dependency_names(manifest);
    for expected in [
        "memcore",
        "tachi-server",
        "plain-dotted",
        "dev-dotted",
        "build-dotted",
        "target-dotted",
        "target-inline",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "parser missed dependency '{expected}'; saw {names:?}"
        );
    }
}
