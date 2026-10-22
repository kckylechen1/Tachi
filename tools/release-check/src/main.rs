//! Standalone release version synchronization checker for Tachi.
//!
//! Replaces external python/tomllib reliance with a zero-new-dependency
//! Rust tool that validates all 16 workspace crates, 3 package.json files,
//! Cargo.lock, package-lock.json files, and release documentation.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub const RUST_MANIFEST_PATHS: &[&str] = &[
    "crates/memcore/Cargo.toml",
    "crates/memory-server-capture-gate/Cargo.toml",
    "crates/memory-server-hub-cli/Cargo.toml",
    "crates/memory-server-manifest-audit/Cargo.toml",
    "crates/tachi-server/Cargo.toml",
    "crates/tachi-params/Cargo.toml",
    "crates/memory-server-prompt-envelope/Cargo.toml",
    "crates/memory-server-rescue/Cargo.toml",
    "crates/memory-server-runtime/Cargo.toml",
    "crates/memory-node/Cargo.toml",
    "crates/tachi-bootstrap/Cargo.toml",
    "crates/tachi-dispatch/Cargo.toml",
    "crates/tachi-foundry/Cargo.toml",
    "crates/tachi-hub/Cargo.toml",
    "crates/tachi-llm/Cargo.toml",
];

pub const JS_PACKAGE_PATHS: &[&str] = &[
    "crates/memory-node/package.json",
    "integrations/openclaw/package.json",
    "packages/tachi-cli/package.json",
];

pub const PINNED_REFS: &[&str] = &[
    "README.md",
    "README.zh-CN.md",
    "README.classical.md",
    "scripts/install.sh",
    "docs/INSTALL.md",
    "integrations/openclaw/README.md",
];

pub fn package_lock_path(package_path: &str) -> String {
    let p = Path::new(package_path);
    if let Some(parent) = p.parent() {
        if parent.as_os_str().is_empty() {
            "package-lock.json".to_string()
        } else {
            parent
                .join("package-lock.json")
                .to_string_lossy()
                .replace('\\', "/")
        }
    } else {
        "package-lock.json".to_string()
    }
}

fn read_file(root: &Path, rel_path: &str) -> Result<String, String> {
    let full = root.join(rel_path);
    std::fs::read_to_string(&full).map_err(|e| format!("{}: failed to read: {e}", rel_path))
}

/// Parse [package] table name and version from a Cargo.toml file.
/// Adheres strictly to the contract:
/// - missing [package] -> error
/// - missing package.name -> error
/// - missing package.version -> error
pub fn cargo_package(root: &Path, rel_path: &str) -> Result<(String, String), String> {
    let content = read_file(root, rel_path)?;
    parse_cargo_package_content(&content, rel_path)
}

fn decode_toml_string(raw: &str, basic: bool, multiline: bool) -> Result<String, String> {
    let raw = if multiline {
        raw.strip_prefix("\r\n")
            .or_else(|| raw.strip_prefix('\n'))
            .unwrap_or(raw)
    } else {
        raw
    };
    if !basic {
        return Ok(raw.to_string());
    }
    let mut chars = raw.chars().peekable();
    let mut out = String::new();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        let escape = chars.next().ok_or("unterminated TOML escape")?;
        match escape {
            'b' => out.push('\u{0008}'),
            't' => out.push('\t'),
            'n' => out.push('\n'),
            'f' => out.push('\u{000c}'),
            'r' => out.push('\r'),
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            'u' | 'U' => {
                let size = if escape == 'u' { 4 } else { 8 };
                let mut digits = String::new();
                for _ in 0..size {
                    let digit = chars.next().ok_or("invalid TOML Unicode escape")?;
                    if !digit.is_ascii_hexdigit() {
                        return Err("invalid TOML Unicode escape".into());
                    }
                    digits.push(digit);
                }
                let value =
                    u32::from_str_radix(&digits, 16).map_err(|_| "invalid TOML Unicode escape")?;
                out.push(char::from_u32(value).ok_or("invalid TOML Unicode scalar")?);
            }
            ' ' | '\t' | '\r' | '\n' if multiline => {
                let mut newline = escape == '\n';
                while chars
                    .peek()
                    .is_some_and(|c| matches!(c, ' ' | '\t' | '\r' | '\n'))
                {
                    newline |= chars.next() == Some('\n');
                }
                if !newline {
                    return Err("TOML continuation requires a newline".into());
                }
            }
            _ => return Err("invalid TOML escape".into()),
        }
    }
    Ok(out)
}

fn toml_key(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    for quote in ['"', '\''] {
        if let Some(inner) = raw.strip_prefix(quote).and_then(|s| s.strip_suffix(quote)) {
            return decode_toml_string(inner, quote == '"', false);
        }
    }
    Ok(raw.to_string())
}

fn parse_cargo_package_content(content: &str, rel_path: &str) -> Result<(String, String), String> {
    let chars: Vec<char> = content.chars().collect();
    let length = chars.len();
    let mut pos = 0;

    let mut current_section: Option<String> = None;
    let mut name = None;
    let mut version = None;
    let mut found_package_header = false;

    while pos < length {
        let ch = chars[pos];
        if ch.is_whitespace() {
            pos += 1;
            continue;
        }
        if ch == '#' {
            while pos < length && chars[pos] != '\n' {
                pos += 1;
            }
            continue;
        }

        // Table header: [section] or [[section]]
        if ch == '[' {
            let is_double = pos + 1 < length && chars[pos + 1] == '[';
            let offset = if is_double { 2 } else { 1 };
            let mut end = pos + offset;
            let mut found_end = false;
            while end < length && chars[end] != '\n' {
                let matches_end = if is_double {
                    end + 1 < length && chars[end] == ']' && chars[end + 1] == ']'
                } else {
                    chars[end] == ']'
                };
                if matches_end {
                    found_end = true;
                    break;
                }
                end += 1;
            }

            if found_end {
                let sec_name: String = chars[pos + offset..end].iter().collect();
                let trimmed_sec = toml_key(&sec_name)?;
                if trimmed_sec == "package" && !is_double {
                    if found_package_header {
                        return Err(format!("{rel_path}: duplicate [package] table"));
                    }
                    found_package_header = true;
                }
                current_section = (!is_double).then_some(trimmed_sec);
                pos = end + offset;
                while pos < length && chars[pos] != '\n' {
                    if chars[pos] == '#' {
                        while pos < length && chars[pos] != '\n' {
                            pos += 1;
                        }
                        break;
                    }
                    pos += 1;
                }
                continue;
            }
        }

        // Key = Value
        let key_start = pos;
        while pos < length
            && !chars[pos].is_whitespace()
            && chars[pos] != '='
            && chars[pos] != '#'
            && chars[pos] != '['
        {
            pos += 1;
        }
        let key: String = chars[key_start..pos].iter().collect();
        let key = toml_key(&key)?;
        if current_section.as_deref() == Some("package")
            && ((key == "name" && name.is_some()) || (key == "version" && version.is_some()))
        {
            return Err(format!("{rel_path}: duplicate package.{key}"));
        }

        while pos < length && (chars[pos] == ' ' || chars[pos] == '\t') {
            pos += 1;
        }

        if pos < length && chars[pos] == '=' {
            pos += 1; // skip '='
            while pos < length && (chars[pos] == ' ' || chars[pos] == '\t') {
                pos += 1;
            }
            if pos >= length {
                break;
            }

            let remaining = &chars[pos..];
            let is_triple_double = remaining.len() >= 3
                && remaining[0] == '"'
                && remaining[1] == '"'
                && remaining[2] == '"';
            let is_triple_single = remaining.len() >= 3
                && remaining[0] == '\''
                && remaining[1] == '\''
                && remaining[2] == '\'';

            if is_triple_double || is_triple_single {
                let delim = if is_triple_double { '"' } else { '\'' };
                pos += 3;
                let val_start = pos;
                let mut found_end = false;
                while pos + 2 < length {
                    if is_triple_double && chars[pos] == '\\' && pos + 1 < length {
                        pos += 2;
                        continue;
                    }
                    if chars[pos] == delim && chars[pos + 1] == delim && chars[pos + 2] == delim {
                        found_end = true;
                        break;
                    }
                    pos += 1;
                }
                let val_str: String = chars[val_start..pos].iter().collect();
                if found_end {
                    pos += 3;
                } else {
                    return Err(format!("{rel_path}: unterminated TOML multiline string"));
                }
                let val_str = decode_toml_string(&val_str, is_triple_double, true)?;
                if current_section.as_deref() == Some("package") {
                    if key == "name" {
                        name = Some(val_str);
                    } else if key == "version" {
                        version = Some(val_str);
                    }
                }
            } else if chars[pos] == '"' || chars[pos] == '\'' {
                let delim = chars[pos];
                pos += 1;
                let mut val_chars = Vec::new();
                let mut closed = false;
                while pos < length {
                    let c = chars[pos];
                    if delim == '"' && c == '\\' && pos + 1 < length {
                        val_chars.push(c);
                        pos += 1;
                        val_chars.push(chars[pos]);
                        pos += 1;
                    } else if c == delim {
                        pos += 1;
                        closed = true;
                        break;
                    } else {
                        val_chars.push(c);
                        pos += 1;
                    }
                }
                if !closed {
                    return Err(format!("{rel_path}: unterminated TOML string"));
                }
                let val_str: String = val_chars.into_iter().collect();
                let val_str = decode_toml_string(&val_str, delim == '"', false)?;
                if current_section.as_deref() == Some("package") {
                    if key == "name" {
                        name = Some(val_str);
                    } else if key == "version" {
                        version = Some(val_str);
                    }
                }
            } else if chars[pos] == '[' || chars[pos] == '{' {
                let open_ch = chars[pos];
                let close_ch = if open_ch == '[' { ']' } else { '}' };
                let mut depth = 0;
                while pos < length {
                    let c = chars[pos];
                    if c == '#' {
                        while pos < length && chars[pos] != '\n' {
                            pos += 1;
                        }
                        continue;
                    }
                    if c == open_ch {
                        depth += 1;
                    } else if c == close_ch {
                        depth -= 1;
                        if depth == 0 {
                            pos += 1;
                            break;
                        }
                    } else if pos + 2 < length
                        && ((chars[pos] == '"' && chars[pos + 1] == '"' && chars[pos + 2] == '"')
                            || (chars[pos] == '\''
                                && chars[pos + 1] == '\''
                                && chars[pos + 2] == '\''))
                    {
                        let delim = chars[pos];
                        let is_basic = delim == '"';
                        pos += 3;
                        while pos + 2 < length {
                            if is_basic && chars[pos] == '\\' && pos + 1 < length {
                                pos += 2;
                                continue;
                            }
                            if chars[pos] == delim
                                && chars[pos + 1] == delim
                                && chars[pos + 2] == delim
                            {
                                pos += 2;
                                break;
                            }
                            pos += 1;
                        }
                    } else if c == '"' || c == '\'' {
                        let delim = c;
                        pos += 1;
                        while pos < length && chars[pos] != delim {
                            if delim == '"' && chars[pos] == '\\' && pos + 1 < length {
                                pos += 1;
                            }
                            pos += 1;
                        }
                    }
                    pos += 1;
                }
            } else {
                while pos < length && chars[pos] != '\r' && chars[pos] != '\n' && chars[pos] != '#'
                {
                    pos += 1;
                }
            }
        } else {
            pos += 1;
        }
    }

    if !found_package_header {
        return Err(format!("{rel_path}: missing [package]"));
    }
    let name = match name {
        Some(n) if !n.trim().is_empty() => n.trim().to_string(),
        _ => return Err(format!("{rel_path}: missing package.name")),
    };
    let version = match version {
        Some(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => return Err(format!("{rel_path}: missing package.version")),
    };

    Ok((name, version))
}

fn extract_toml_str(line: &str, key: &str) -> Option<String> {
    let prefix = format!("{key} =");
    let prefix_space = format!("{key}=");
    let rest = if let Some(r) = line.strip_prefix(&prefix) {
        r.trim()
    } else {
        let r = line.strip_prefix(&prefix_space)?;
        r.trim()
    };

    if let Some(stripped) = rest.strip_prefix('"') {
        if let Some(end) = stripped.find('"') {
            return Some(stripped[..end].trim().to_string());
        }
    } else if let Some(stripped) = rest.strip_prefix('\'') {
        if let Some(end) = stripped.find('\'') {
            return Some(stripped[..end].trim().to_string());
        }
    }
    None
}

pub fn package_json_version(root: &Path, rel_path: &str) -> Result<String, String> {
    let content = read_file(root, rel_path)?;
    let v: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("{rel_path}: invalid JSON: {e}"))?;
    v.get("version")
        .and_then(|val| val.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("{rel_path}: missing version field"))
}

pub fn package_lock_versions(root: &Path, rel_path: &str) -> Result<Vec<String>, String> {
    let content = read_file(root, rel_path)?;
    let v: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("{rel_path}: invalid JSON: {e}"))?;
    let mut versions = Vec::new();
    if let Some(ver) = v.get("version").and_then(|val| val.as_str()) {
        versions.push(ver.to_string());
    } else {
        return Err(format!("{rel_path}: missing root version"));
    }
    if let Some(packages) = v.get("packages").and_then(|p| p.get("")) {
        if let Some(ver) = packages.get("version").and_then(|val| val.as_str()) {
            versions.push(ver.to_string());
        }
    }
    Ok(versions)
}

pub fn cargo_lock_versions(
    root: &Path,
    names: &HashSet<String>,
) -> Result<HashMap<String, String>, String> {
    let content = read_file(root, "Cargo.lock")?;
    let mut versions = HashMap::new();
    for block in content.split("[[package]]").skip(1) {
        let mut name = None;
        let mut version = None;
        let mut external = false;
        for line in block.lines().map(str::trim) {
            if let Some((key, _)) = line.split_once('=') {
                external |= key.trim() == "source";
            }
            if let Some(value) = extract_toml_str(line, "name") {
                name = Some(value);
            }
            if let Some(value) = extract_toml_str(line, "version") {
                version = Some(value);
            }
        }
        if let Some(name) = name.filter(|name| names.contains(name) && !external) {
            if versions
                .insert(name.clone(), version.unwrap_or_default())
                .is_some()
            {
                return Err(format!("Cargo.lock: duplicate workspace package {name}"));
            }
        }
    }
    Ok(versions)
}

fn require_match(label: &str, got: &str, expected: &str, errors: &mut Vec<String>) {
    if got != expected {
        errors.push(format!("{label}: expected {expected}, got {got}"));
    }
}

fn require_contains(label: &str, text: &str, needle: &str, errors: &mut Vec<String>) {
    if !text.contains(needle) {
        errors.push(format!("{label}: missing {needle}"));
    }
}

/// Runs the complete release version verification against the given repository root.
pub fn check_release_versions(root: &Path) -> Result<String, Vec<String>> {
    let mut errors = Vec::new();
    let mut cargo_packages = Vec::new();

    for &path in RUST_MANIFEST_PATHS {
        match cargo_package(root, path) {
            Ok((name, version)) => cargo_packages.push((path, name, version)),
            Err(err) => errors.push(err),
        }
    }

    let tachi_server = cargo_packages
        .iter()
        .find(|(path, _, _)| *path == "crates/tachi-server/Cargo.toml")
        .map(|(_, _, version)| version.clone())
        .unwrap_or_default();

    if tachi_server.is_empty() {
        errors.push("crates/tachi-server/Cargo.toml: missing release version".to_string());
    }
    let expected = tachi_server;
    let tag = format!("v{expected}");

    for (path, _, version) in &cargo_packages {
        require_match(path, version, &expected, &mut errors);
    }

    let mut cargo_names: Vec<String> = cargo_packages
        .iter()
        .map(|(_, name, _)| name.clone())
        .collect();
    cargo_names.sort();
    let mut duplicate_names = HashSet::new();
    for i in 1..cargo_names.len() {
        if cargo_names[i] == cargo_names[i - 1] {
            duplicate_names.insert(cargo_names[i].clone());
        }
    }
    if !duplicate_names.is_empty() {
        let mut sorted_dups: Vec<String> = duplicate_names.into_iter().collect();
        sorted_dups.sort();
        errors.push(format!(
            "Cargo manifests: duplicate package name(s): {}",
            sorted_dups.join(", ")
        ));
    }

    let unique_names: HashSet<String> = cargo_packages
        .iter()
        .map(|(_, name, _)| name.clone())
        .collect();
    match cargo_lock_versions(root, &unique_names) {
        Ok(lock_versions) => {
            for (_, name, _) in &cargo_packages {
                let lock_ver = lock_versions.get(name).map(|s| s.as_str()).unwrap_or("");
                require_match(
                    &format!("Cargo.lock {name}"),
                    lock_ver,
                    &expected,
                    &mut errors,
                );
            }
        }
        Err(err) => errors.push(err),
    }

    for &path in JS_PACKAGE_PATHS {
        match package_json_version(root, path) {
            Ok(ver) => require_match(path, &ver, &expected, &mut errors),
            Err(err) => errors.push(err),
        }
        let lock_path = package_lock_path(path);
        match package_lock_versions(root, &lock_path) {
            Ok(versions) => {
                for (idx, ver) in versions.iter().enumerate() {
                    require_match(
                        &format!("{lock_path} version[{idx}]"),
                        ver,
                        &expected,
                        &mut errors,
                    );
                }
            }
            Err(err) => errors.push(err),
        }
    }

    match read_file(root, "docs/current-state.agent.yaml") {
        Ok(content) => {
            let mut found_release = None;
            for line in content.lines() {
                if let Some(rest) = line.strip_prefix("release:") {
                    let rest = rest.trim();
                    let rel = if rest.starts_with(['"', '\'']) {
                        extract_toml_str(&format!("release = {rest}"), "release")
                            .unwrap_or_default()
                    } else {
                        rest.split('#').next().unwrap_or("").trim().to_string()
                    };
                    if !rel.is_empty() {
                        found_release = Some(rel);
                        break;
                    }
                }
            }
            require_match(
                "docs/current-state.agent.yaml release",
                found_release.as_deref().unwrap_or(""),
                &tag,
                &mut errors,
            );
        }
        Err(err) => errors.push(err),
    }

    let needle = format!("/{tag}/scripts/install");
    for &path in PINNED_REFS {
        match read_file(root, path) {
            Ok(content) => require_contains(path, &content, &needle, &mut errors),
            Err(err) => errors.push(err),
        }
    }

    if errors.is_empty() {
        Ok(expected)
    } else {
        Err(errors)
    }
}

fn explicit_root(args: &[String]) -> Result<Option<PathBuf>, String> {
    match args {
        [] => Ok(None),
        [flag, path] if flag == "--root" && !path.is_empty() && !path.starts_with("--") => {
            Ok(Some(PathBuf::from(path)))
        }
        _ => Err("usage: tachi-release-check [--root <path>]".to_string()),
    }
}

fn locate_repo_root(args: &[String]) -> Result<PathBuf, String> {
    if let Some(root) = explicit_root(args)? {
        return Ok(root);
    }

    // 2. Walk up from current working directory
    let cwd = std::env::current_dir().map_err(|e| format!("cannot determine current dir: {e}"))?;
    let mut candidate = cwd.as_path();
    loop {
        if candidate.join("Cargo.lock").exists() && candidate.join("crates").is_dir() {
            return Ok(candidate.to_path_buf());
        }
        match candidate.parent() {
            Some(parent) => candidate = parent,
            None => break,
        }
    }

    // 3. Fallback to CARGO_MANIFEST_DIR relative if compiling/running within the repo
    if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        let p = Path::new(&manifest_dir).join("../..");
        if p.join("Cargo.lock").exists() {
            return Ok(p.canonicalize().unwrap_or(p));
        }
    }

    Err("Could not locate repository root (expected Cargo.lock and crates/)".to_string())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = match locate_repo_root(&args) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("release version check failed:\n  - {e}");
            return ExitCode::FAILURE;
        }
    };

    match check_release_versions(&root) {
        Ok(version) => {
            println!("release version check passed: {version}");
            ExitCode::SUCCESS
        }
        Err(errors) => {
            eprintln!("release version check failed:");
            for err in errors {
                eprintln!("  - {err}");
            }
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_package_lock_path_is_derived_from_its_package_path() {
        assert_eq!(
            package_lock_path("nested/tool/package.json"),
            "nested/tool/package-lock.json"
        );
        assert_eq!(package_lock_path("package.json"), "package-lock.json");
    }

    #[test]
    fn test_live_repo_version_sync_passes() {
        let root = locate_repo_root(&[]).expect("locate repo root");
        let expected_server_version = cargo_package(&root, "crates/tachi-server/Cargo.toml")
            .expect("read tachi-server manifest")
            .1;
        let result = check_release_versions(&root);
        assert!(
            result.is_ok(),
            "Live repo failed version check: {:?}",
            result.err()
        );
        assert_eq!(result.unwrap(), expected_server_version);
    }

    #[test]
    fn test_duplicate_or_missing_cargo_names_fail_loudly() {
        let missing_name = "[package]\nversion = \"1.9.0\"\n";
        let err = parse_cargo_package_content(missing_name, "test/Cargo.toml").unwrap_err();
        assert_eq!(err, "test/Cargo.toml: missing package.name");

        let missing_pkg = "name = \"test\"\nversion = \"1.9.0\"\n";
        let err = parse_cargo_package_content(missing_pkg, "test/Cargo.toml").unwrap_err();
        assert_eq!(err, "test/Cargo.toml: missing [package]");

        let missing_ver = "[package]\nname = \"test\"\n";
        let err = parse_cargo_package_content(missing_ver, "test/Cargo.toml").unwrap_err();
        assert_eq!(err, "test/Cargo.toml: missing package.version");
    }

    #[test]
    fn test_toml_comments_single_quotes_and_multiline_strings() {
        let inline_comment =
            "[package] # release package\nname = \"memcore\"\nversion = \"1.9.0\"\n";
        let (n, v) = parse_cargo_package_content(inline_comment, "test/Cargo.toml").unwrap();
        assert_eq!((n.as_str(), v.as_str()), ("memcore", "1.9.0"));

        let single_quotes = "[package]\nname = 'memcore'\nversion = '1.9.0'\n";
        let (n, v) = parse_cargo_package_content(single_quotes, "test/Cargo.toml").unwrap();
        assert_eq!((n.as_str(), v.as_str()), ("memcore", "1.9.0"));

        let multiline_literal = "[package]\nname = \"memcore\"\nversion = \"9.9.9\"\ndescription = '''\nversion = \"1.9.0\"\n'''\n";
        let (n, v) = parse_cargo_package_content(multiline_literal, "test/Cargo.toml").unwrap();
        assert_eq!((n.as_str(), v.as_str()), ("memcore", "9.9.9"));

        let multiline_basic = "[package]\nname = \"memcore\"\nversion = \"9.9.9\"\ndescription = \"\"\"\nversion = \"1.9.0\"\n\"\"\"\n";
        let (n, v) = parse_cargo_package_content(multiline_basic, "test/Cargo.toml").unwrap();
        assert_eq!((n.as_str(), v.as_str()), ("memcore", "9.9.9"));

        let comment_quote = "[package]\nname = \"memcore\"\nkeywords = [ # \"unclosed in comment\n    \"database\",\n]\nversion = \"2.0.0\"\n";
        let (n, v) = parse_cargo_package_content(comment_quote, "test/Cargo.toml").unwrap();
        assert_eq!((n.as_str(), v.as_str()), ("memcore", "2.0.0"));

        let comment_apostrophe = "[package]\nname = \"memcore\"\nkeywords = [ # don't reorder\n    \"database\",\n]\nversion = \"2.0.0\"\n";
        let (n, v) = parse_cargo_package_content(comment_apostrophe, "test/Cargo.toml").unwrap();
        assert_eq!((n.as_str(), v.as_str()), ("memcore", "2.0.0"));

        let escaped_multiline_delim = "[package]\nname = \"memcore\"\ndescription = \"\"\"Example: \\\"\"\" [example]\ntext\n\"\"\"\nversion = \"2.0.0\"\n";
        let (n, v) =
            parse_cargo_package_content(escaped_multiline_delim, "test/Cargo.toml").unwrap();
        assert_eq!((n.as_str(), v.as_str()), ("memcore", "2.0.0"));

        let escaped_multiline_mismatch = "[package]\nname = \"memcore\"\nversion = \"9.9.9\"\ndescription = \"\"\"Example: \\\"\"\" version = \"2.0.0\"\n\"\"\"\n";
        let (n, v) =
            parse_cargo_package_content(escaped_multiline_mismatch, "test/Cargo.toml").unwrap();
        assert_eq!((n.as_str(), v.as_str()), ("memcore", "9.9.9"));
    }

    fn release_fixture(version: &str, lock_version: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = tmp.path();

        for &path in RUST_MANIFEST_PATHS {
            let p = root.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            let name = p.parent().unwrap().file_name().unwrap().to_str().unwrap();
            let name = if path == "crates/tachi-server/Cargo.toml" {
                "tachi-server"
            } else {
                name
            };
            std::fs::write(
                &p,
                format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\n"),
            )
            .unwrap();
        }

        let mut lock_content = String::new();
        for &path in RUST_MANIFEST_PATHS {
            let name = if path == "crates/tachi-server/Cargo.toml" {
                "tachi-server"
            } else {
                Path::new(path)
                    .parent()
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
            };
            lock_content.push_str(&format!(
                "[[package]]\nname = \"{name}\"\nversion = \"{lock_version}\"\n"
            ));
        }
        std::fs::write(root.join("Cargo.lock"), lock_content).unwrap();

        for &path in JS_PACKAGE_PATHS {
            let p = root.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, format!("{{\"version\": \"{version}\"}}")).unwrap();
            let lp = p.parent().unwrap().join("package-lock.json");
            std::fs::write(
                &lp,
                format!("{{\"version\": \"{version}\", \"packages\": {{\"\": {{\"version\": \"{version}\"}}}}}}"),
            )
            .unwrap();
        }

        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(
            root.join("docs/current-state.agent.yaml"),
            format!("release: v{version}\n"),
        )
        .unwrap();

        for &path in PINNED_REFS {
            let p = root.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, format!("/v{version}/scripts/install\n")).unwrap();
        }

        tmp
    }

    #[test]
    fn yaml_release_comments_preserve_scalar_and_mismatches_stay_loud() {
        let tmp = release_fixture("1.9.0", "1.9.0");
        for release in [
            "v1.9.0 # current release",
            "\"v1.9.0\" # current release",
            "'v1.9.0' # current release",
        ] {
            std::fs::write(
                tmp.path().join("docs/current-state.agent.yaml"),
                format!("release: {release}\n"),
            )
            .unwrap();
            assert!(check_release_versions(tmp.path()).is_ok(), "{release}");
        }
        std::fs::write(
            tmp.path().join("docs/current-state.agent.yaml"),
            "release: v9.9.9 # mismatched\n",
        )
        .unwrap();
        assert!(check_release_versions(tmp.path())
            .unwrap_err()
            .iter()
            .any(|e| e.contains("expected v1.9.0, got v9.9.9")));
    }

    #[test]
    fn test_synthetic_fixture_version_mismatch() {
        let tmp = release_fixture("1.9.0", "9.9.9");
        let root = tmp.path();
        let errors = check_release_versions(root).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("Cargo.lock memcore: expected 1.9.0, got 9.9.9")),
            "Expected Cargo.lock mismatch error, got: {:?}",
            errors
        );
    }

    #[test]
    fn quoted_package_keys_and_unicode_escapes_preserve_version() {
        for header in [r#"["package"]"#, "['package']", r#"["pack\u0061ge"]"#] {
            let text = format!("{header}\n\"name\" = \"memcore\"\n\"version\" = \"\\u0031.9.0\"\n");
            assert_eq!(
                parse_cargo_package_content(&text, "fixture").unwrap(),
                ("memcore".into(), "1.9.0".into())
            );
        }
        for text in [
            "[package]\nname=\"memcore\"\nversion=\"\\uD800\"\n",
            "[package]\nname=\"memcore\"\nversion=\"\\q\"\n",
            "[package]\nname=\"memcore\"\nversion=\"1.9.0\"\nversion=\"9.9.9\"\n",
            "[package]\nname=\"memcore\"\nversion=\"1.9.0",
        ] {
            assert!(
                parse_cargo_package_content(text, "fixture").is_err(),
                "{text}"
            );
        }
    }

    #[test]
    fn lock_selects_workspace_entry_independently_of_external_order() {
        let tmp = tempfile::tempdir().unwrap();
        let names = HashSet::from(["memcore".to_string()]);
        let local = "[[package]]\nname=\"memcore\"\nversion=\"1.9.0\"\n";
        let external = "[[package]]\nname=\"memcore\"\nversion=\"9.9.9\"\nsource=\"registry+https://example.invalid\"\n";
        for body in [format!("{local}{external}"), format!("{external}{local}")] {
            std::fs::write(tmp.path().join("Cargo.lock"), body).unwrap();
            assert_eq!(
                cargo_lock_versions(tmp.path(), &names).unwrap()["memcore"],
                "1.9.0"
            );
        }
        std::fs::write(tmp.path().join("Cargo.lock"), format!("{local}{local}")).unwrap();
        assert!(cargo_lock_versions(tmp.path(), &names).is_err());
    }

    #[test]
    fn malformed_explicit_root_never_falls_back() {
        for args in [
            vec!["--root"],
            vec!["--root", ""],
            vec!["--root", "--help"],
            vec!["--unknown"],
        ] {
            let args = args.into_iter().map(str::to_string).collect::<Vec<_>>();
            assert!(explicit_root(&args).is_err());
        }
        assert_eq!(explicit_root(&[]).unwrap(), None);
        assert_eq!(
            explicit_root(&["--root".into(), "/fixture".into()]).unwrap(),
            Some(PathBuf::from("/fixture"))
        );
    }
}
