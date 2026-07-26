//! Source-level census: every production `memories` table writer must either
//! sync both `memories_fts` and `memories_symbolic_fts`, or sit on an explicit
//! allowlist.
//!
//! Exemptions documented here:
//! 1. `crates/memcore/src/db/anchor.rs` — synthetic anchor rows, zero index
//!    (R1 rebuild backstop).
//! 2. `crates/memory-server-rescue/` — disaster-recovery writes, zero index
//!    (R1 rebuild backstop).
//!
//! Metadata-only UPDATEs (retention/archived/tier/recall_count/…) are listed
//! as `index_irrelevant` (allowlist or auto-classified from SET columns) so
//! they cannot silently hide a new indexed writer.
//!
//! Schema/migration path/keyword rewrites that historically rely on R1 FTS
//! rebuild rather than per-row sync are allowlisted as `r1_rebuild_backstop`.
//!
//! Print observed writers with `TACHI_PRINT_MEMORIES_WRITER_CENSUS=1`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const INDEXED_COLUMNS: &[&str] = &["path", "summary", "text", "keywords", "entities", "topic"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AllowClass {
    /// Intentionally unindexed (R1 rebuild is the backstop).
    ZeroIndex,
    /// UPDATE does not touch lexical/symbolic indexed columns.
    /// (Also auto-classified from SET columns; variant kept for explicit rows.)
    #[allow(dead_code)]
    IndexIrrelevant,
    /// One-time schema/migration rewrite; R1 rebuild is the backstop.
    R1RebuildBackstop,
}

#[derive(Debug, Clone, Copy)]
struct AllowEntry {
    /// Relative path prefix or exact file path under the repo root.
    path: &'static str,
    /// Optional enclosing function name; `None` matches the whole path prefix.
    function: Option<&'static str>,
    class: AllowClass,
    reason: &'static str,
}

/// Explicit allowlist. Prefer path+function when a file mixes synced and
/// deferred writers. Keep sorted by path then function for review stability.
const ALLOWLIST: &[AllowEntry] = &[
    AllowEntry {
        path: "crates/memcore/src/db/anchor.rs",
        function: None,
        class: AllowClass::ZeroIndex,
        reason: "synthetic anchor rows, zero index (R1 rebuild backstop)",
    },
    AllowEntry {
        path: "crates/memcore/src/db/migrations/basic.rs",
        function: Some("migrate_v1_path_normalize"),
        class: AllowClass::R1RebuildBackstop,
        reason: "path normalize migration; FTS rebuilt by R1 / later init steps",
    },
    AllowEntry {
        path: "crates/memcore/src/db/migrations/basic.rs",
        function: Some("migrate_v3_handoff_standardize"),
        class: AllowClass::R1RebuildBackstop,
        reason: "bulk path rewrite /handoff → /handoff/unknown; R1 backstop",
    },
    AllowEntry {
        path: "crates/memcore/src/db/migrations/legacy_columns.rs",
        function: Some("migrate_v6_fold_persons_into_entities"),
        class: AllowClass::R1RebuildBackstop,
        reason: "entities fold from persons; R1 rebuild backstop",
    },
    AllowEntry {
        path: "crates/memcore/src/db/migrations/legacy_columns.rs",
        function: Some("migrate_v7_reconcile_legacy_memory_columns"),
        class: AllowClass::R1RebuildBackstop,
        reason: "keywords/path/domain legacy column bridge; R1 rebuild backstop",
    },
    AllowEntry {
        path: "crates/memcore/src/db/migrations/legacy_columns.rs",
        function: Some("relocate_location_rows"),
        class: AllowClass::R1RebuildBackstop,
        reason: "path rewrite from location column; R1 rebuild backstop",
    },
    AllowEntry {
        path: "crates/memcore/src/db/schema.rs",
        function: Some("bridge_hypertachi_memory_columns"),
        class: AllowClass::R1RebuildBackstop,
        reason: "keywords bridge from indexed_tags; R1 rebuild backstop",
    },
    AllowEntry {
        path: "crates/memory-server-rescue/",
        function: None,
        class: AllowClass::ZeroIndex,
        reason: "disaster-recovery writes, zero index (R1 rebuild backstop)",
    },
];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct WriterSite {
    relative: String,
    line: usize,
    kind: &'static str,
    function: String,
    classification: String,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("tachi-server lives under <repo>/crates")
        .to_path_buf()
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .expect("source path is inside repository")
        .to_string_lossy()
        .replace('\\', "/")
}

fn is_testish_path(relative: &str) -> bool {
    relative.contains("/tests/")
        || relative.ends_with("/tests.rs")
        || relative.contains("/examples/")
        || relative.ends_with("migration_tests.rs")
        || relative
            .rsplit('/')
            .next()
            .is_some_and(|name| name.contains("test") && name.ends_with(".rs"))
}

fn production_rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read crates directory") {
        let entry = entry.expect("read entry");
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "tests" || name == "examples" || name == "target" {
                continue;
            }
            production_rust_files(&path, files);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            files.push(path);
        }
    }
}

fn code_before_line_comment(line: &str) -> &str {
    let mut quoted = false;
    let mut escaped = false;
    let bytes = line.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let character = bytes[index];
        if escaped {
            escaped = false;
        } else if character == b'\\' && quoted {
            escaped = true;
        } else if character == b'"' {
            quoted = !quoted;
        } else if !quoted
            && character == b'/'
            && bytes.get(index + 1).is_some_and(|next| *next == b'/')
        {
            return &line[..index];
        }
        index += 1;
    }
    line
}

fn brace_delta(line: &str) -> i64 {
    let mut delta = 0_i64;
    let mut quoted = false;
    let mut escaped = false;
    for character in code_before_line_comment(line).chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quoted {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else if !quoted && character == '{' {
            delta += 1;
        } else if !quoted && character == '}' {
            delta -= 1;
        }
    }
    delta
}

fn function_name(line: &str) -> Option<String> {
    let line = code_before_line_comment(line);
    let marker = line.find("fn ")?;
    let before = &line[..marker];
    if before
        .chars()
        .last()
        .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return None;
    }
    let name = line[marker + 3..]
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
        .collect::<String>();
    (!name.is_empty()).then_some(name)
}

/// Skip `#[cfg(test)] mod … { … }` blocks; keep production code.
fn skip_cfg_test_module(lines: &[&str], mut index: usize) -> Option<usize> {
    let trimmed = lines[index].trim_start();
    if !trimmed.starts_with("#[cfg(test)]") {
        return None;
    }
    let mut module_line = index + 1;
    while module_line < lines.len()
        && (lines[module_line].trim().is_empty()
            || lines[module_line].trim_start().starts_with("#["))
    {
        module_line += 1;
    }
    if module_line >= lines.len() || !lines[module_line].trim_start().starts_with("mod ") {
        return None;
    }
    index = module_line;
    let mut depth = 0_i64;
    let mut opened = false;
    while index < lines.len() {
        let change = brace_delta(lines[index]);
        opened |= code_before_line_comment(lines[index]).contains('{');
        depth += change;
        index += 1;
        if opened && depth == 0 {
            return Some(index);
        }
    }
    Some(index)
}

#[derive(Clone)]
struct FunctionSpan {
    name: String,
    start_line: usize, // 1-based
    end_line: usize,   // exclusive, 1-based
    source: String,
}

fn functions_in_source(source: &str) -> Vec<FunctionSpan> {
    let lines = source.lines().collect::<Vec<_>>();
    let mut functions = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if let Some(next) = skip_cfg_test_module(&lines, index) {
            index = next;
            continue;
        }
        // Skip #[test] / #[tokio::test] functions even outside cfg(test) mods.
        let mut lookback = index;
        let mut is_test_fn = false;
        while lookback > 0 {
            lookback -= 1;
            let t = lines[lookback].trim_start();
            if t.is_empty() || t.starts_with("//") {
                continue;
            }
            if t.starts_with("#[") {
                if t.contains("test") {
                    is_test_fn = true;
                }
                continue;
            }
            break;
        }
        let Some(name) = function_name(lines[index]) else {
            index += 1;
            continue;
        };
        let start = index;
        let mut depth = 0_i64;
        let mut opened = false;
        while index < lines.len() {
            let change = brace_delta(lines[index]);
            opened |= code_before_line_comment(lines[index]).contains('{');
            depth += change;
            index += 1;
            if opened && depth == 0 {
                break;
            }
        }
        if !is_test_fn {
            functions.push(FunctionSpan {
                name,
                start_line: start + 1,
                end_line: index + 1,
                source: lines[start..index].join("\n"),
            });
        }
    }
    functions
}

fn is_memories_table_sql(code: &str) -> Option<&'static str> {
    // Prefer the most specific verb; exclude index-only tables.
    // Include OR IGNORE / OR REPLACE forms used by anchor inserts.
    let lower = code.to_ascii_lowercase();
    let kind = if lower.contains("insert into memories")
        || lower.contains("insert or ignore into memories")
        || lower.contains("insert or replace into memories")
    {
        "INSERT"
    } else if lower.contains("delete from memories") {
        "DELETE"
    } else if lower.contains("update memories") {
        "UPDATE"
    } else {
        return None;
    };
    // False positives: memories_fts / memories_vec / memories_symbolic* /
    // memories_new — accept only when the matched table token is bare
    // `memories` (word boundary after).
    for needle in [
        "insert or ignore into memories",
        "insert or replace into memories",
        "insert into memories",
        "delete from memories",
        "update memories",
    ] {
        if let Some(pos) = lower.find(needle) {
            let after = &lower[pos + needle.len()..];
            let next = after.chars().next();
            let ok = match next {
                None => true,
                Some(c) if c.is_ascii_whitespace() || matches!(c, '(' | '\n' | '\r') => true,
                // `UPDATE memories SET` / `UPDATE memories\n`
                Some(_)
                    if kind == "UPDATE"
                        && (after.starts_with(" set") || after.starts_with('\n')) =>
                {
                    true
                }
                _ => false,
            };
            // Reject memories_fts / memories_symbolic / memories_vec / memories_new
            if after.starts_with('_') || after.starts_with("fts") || after.starts_with("vec") {
                continue;
            }
            if ok {
                return Some(kind);
            }
        }
    }
    None
}

fn sets_indexed_column(update_sql: &str) -> bool {
    let lower = update_sql.to_ascii_lowercase();
    let Some(set_pos) = lower.find(" set ") else {
        // multiline `UPDATE memories\n SET …`
        if let Some(set_pos) = lower
            .find("\n         set ")
            .or_else(|| lower.find(" set\n"))
        {
            let after = &lower[set_pos..];
            return INDEXED_COLUMNS.iter().any(|col| {
                after.contains(&format!(" {col} "))
                    || after.contains(&format!(" {col}="))
                    || after.contains(&format!("\n{col} "))
                    || after.contains(&format!("{col} ="))
            });
        }
        return true; // fail closed: treat unknown UPDATE shape as indexed
    };
    let after = &lower[set_pos..];
    // Truncate at WHERE if present to avoid matching column names in WHERE.
    let after = after.split(" where ").next().unwrap_or(after);
    INDEXED_COLUMNS.iter().any(|col| {
        after.contains(&format!(" {col} "))
            || after.contains(&format!(" {col}="))
            || after.contains(&format!(",{col} "))
            || after.contains(&format!(", {col} "))
            || after.contains(&format!("set {col} "))
            || after.contains(&format!("set {col}="))
            || after.contains(&format!("{col} ="))
    })
}

fn function_syncs_both_fts(fn_source: &str) -> bool {
    // `sync_memories_fts` itself refreshes lexical then calls
    // `sync_memories_symbolic_fts` — one call covers both projections.
    // Match the bare helper name carefully so
    // `sync_memories_symbolic_fts` does not count as the dual helper.
    let has_dual_helper = fn_source.contains("sync_memories_fts(")
        || fn_source.lines().any(|line| {
            let code = code_before_line_comment(line);
            code.contains("sync_memories_fts")
                && !code.contains("sync_memories_symbolic_fts")
                && !code.contains("fn sync_memories_fts")
        });
    if has_dual_helper {
        return true;
    }
    let has_lexical = fn_source.contains("DELETE FROM memories_fts")
        || fn_source.contains("INSERT INTO memories_fts")
        || fn_source.contains("UPDATE memories_fts");
    let has_symbolic = fn_source.contains("sync_memories_symbolic_fts")
        || fn_source.contains("delete_memories_symbolic_fts")
        || fn_source.contains("DELETE FROM memories_symbolic_fts")
        || fn_source.contains("UPDATE memories_symbolic_fts")
        || fn_source.contains("INSERT INTO memories_symbolic_fts");
    has_lexical && has_symbolic
}

fn allow_match(relative: &str, function: &str) -> Option<&'static AllowEntry> {
    ALLOWLIST.iter().find(|entry| {
        let path_ok = if entry.path.ends_with('/') {
            relative.starts_with(entry.path)
        } else {
            relative == entry.path
        };
        path_ok && entry.function.is_none_or(|want| want == function)
    })
}

fn collect_sql_fragment(lines: &[&str], start: usize) -> String {
    // Gather a small window so multiline SQL string literals are classified.
    let end = (start + 12).min(lines.len());
    lines[start..end]
        .iter()
        .map(|l| code_before_line_comment(l))
        .collect::<Vec<_>>()
        .join("\n")
}

fn observe_writers(root: &Path) -> Vec<WriterSite> {
    let mut files = Vec::new();
    production_rust_files(&root.join("crates"), &mut files);
    files.sort();
    let mut sites = Vec::new();

    for path in files {
        let relative = relative_path(root, &path);
        if is_testish_path(&relative) {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!("failed to read {}: {e}", path.display());
        });
        let functions = functions_in_source(&source);
        let lines: Vec<&str> = source.lines().collect();

        for (idx, line) in lines.iter().enumerate() {
            let code = code_before_line_comment(line);
            let fragment = collect_sql_fragment(&lines, idx);
            let Some(kind) = is_memories_table_sql(code).or_else(|| {
                // Opening quote may be on this line with SQL continuing.
                if code.contains("INSERT INTO memories")
                    || code.contains("UPDATE memories")
                    || code.contains("DELETE FROM memories")
                {
                    is_memories_table_sql(&fragment)
                } else {
                    None
                }
            }) else {
                continue;
            };
            // Re-validate fragment to drop memories_fts false positives where
            // the verb line alone looked ambiguous.
            if is_memories_table_sql(&fragment).is_none() && is_memories_table_sql(code).is_none() {
                continue;
            }
            let line_no = idx + 1;
            // Only count SQL inside production function spans. Lines in
            // `#[cfg(test)]` modules (skipped during function discovery) and
            // bare `#[test]` helpers therefore never enter the census.
            let Some(function) = functions
                .iter()
                .find(|f| line_no >= f.start_line && line_no < f.end_line)
                .cloned()
            else {
                continue;
            };
            let fn_name = function.name.clone();
            let fn_source = function.source.as_str();

            let classification = if let Some(entry) = allow_match(&relative, &fn_name) {
                match entry.class {
                    AllowClass::ZeroIndex => format!("exempt_zero_index: {}", entry.reason),
                    AllowClass::IndexIrrelevant => {
                        format!("index_irrelevant: {}", entry.reason)
                    }
                    AllowClass::R1RebuildBackstop => {
                        format!("r1_rebuild_backstop: {}", entry.reason)
                    }
                }
            } else if kind == "UPDATE" && !sets_indexed_column(&fragment) {
                "index_irrelevant: auto (SET columns exclude path/summary/text/keywords/entities/topic)"
                    .to_string()
            } else if function_syncs_both_fts(fn_source) {
                "synced".to_string()
            } else {
                "UNCLASSIFIED".to_string()
            };

            sites.push(WriterSite {
                relative: relative.clone(),
                line: line_no,
                kind,
                function: fn_name,
                classification,
            });
        }
    }

    sites.sort();
    sites
}

#[test]
fn memories_writer_census_synced_or_allowlisted() {
    let root = repo_root();
    let sites = observe_writers(&root);
    assert!(
        !sites.is_empty(),
        "census found zero memories writers — scanner likely broken"
    );

    if std::env::var_os("TACHI_PRINT_MEMORIES_WRITER_CENSUS").is_some() {
        for site in &sites {
            eprintln!(
                "{}:{} {} {} :: {}",
                site.relative, site.line, site.kind, site.function, site.classification
            );
        }
        eprintln!("total writers: {}", sites.len());
    }

    let bad: Vec<&WriterSite> = sites
        .iter()
        .filter(|s| s.classification == "UNCLASSIFIED")
        .collect();
    if !bad.is_empty() {
        let mut msg =
            String::from("memories writers missing dual FTS sync and not on allowlist:\n");
        for site in &bad {
            msg.push_str(&format!(
                "  {}:{} {} in {}()\n",
                site.relative, site.line, site.kind, site.function
            ));
        }
        msg.push_str(
            "\nEither sync both memories_fts + memories_symbolic_fts in the \
             enclosing function, or add an explicit ALLOWLIST entry with reason.\n",
        );
        panic!("{msg}");
    }

    // Stable presence checks for the two documented zero-index exemptions.
    let relatives: BTreeSet<&str> = sites.iter().map(|s| s.relative.as_str()).collect();
    assert!(
        relatives.iter().any(|p| p.ends_with("db/anchor.rs")),
        "expected anchor.rs writer in census"
    );
    assert!(
        relatives
            .iter()
            .any(|p| p.starts_with("crates/memory-server-rescue/")),
        "expected memory-server-rescue writer in census"
    );
}
