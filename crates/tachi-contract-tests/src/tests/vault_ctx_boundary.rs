//! Non-test Vault code reaches the server only through `VaultCtx`.

use std::fs;
use std::path::{Path, PathBuf};

fn vault_sources() -> Vec<PathBuf> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tachi-server/src");
    let mut files = vec![src.join("vault_ops.rs")];
    let mut dirs = vec![src.join("vault_ops")];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).expect("read vault_ops dir") {
            let path = entry.expect("dir entry").path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if path.is_dir() {
                if name != "tests" {
                    dirs.push(path);
                }
            } else if name.ends_with(".rs") && name != "tests.rs" {
                files.push(path);
            }
        }
    }
    files
}

/// The part of a file before its first top-level `#[cfg(test)] mod`.
fn non_test_part(source: &str) -> Vec<(usize, &str)> {
    let lines: Vec<&str> = source.lines().collect();
    let end = lines
        .windows(2)
        .position(|pair| {
            let next = pair[1]
                .trim_start_matches("pub(crate) ")
                .trim_start_matches("pub(super) ");
            pair[0] == "#[cfg(test)]" && (next.starts_with("mod ") || next.starts_with("pub mod "))
        })
        .unwrap_or(lines.len());
    lines[..end].iter().copied().enumerate().collect()
}

fn names_memory_server(line: &str) -> bool {
    line.match_indices("MemoryServer").any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + "MemoryServer".len()..].chars().next();
        let ident = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        !ident(before) && !ident(after)
    })
}

#[test]
fn vault_ops_does_not_name_the_concrete_server() {
    let files = vault_sources();
    assert!(files.len() > 20, "vault_ops sources not found: {files:?}");
    let mut hits = Vec::new();
    for path in files {
        let source = fs::read_to_string(&path).expect("read vault source");
        for (index, line) in non_test_part(&source) {
            let glob = line.contains("server_state::*") || line.contains("crate::*");
            if names_memory_server(line) || glob {
                hits.push(format!("{}:{}: {}", path.display(), index + 1, line.trim()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "non-test vault_ops code must use `VaultCtx`, not the concrete server:\n{}",
        hits.join("\n")
    );
}
