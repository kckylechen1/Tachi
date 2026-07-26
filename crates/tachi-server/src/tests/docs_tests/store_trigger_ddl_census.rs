//! Source-level census: trigger DDL and a `MemoryStore` connection must not
//! appear in the same file without an unguarded fixture connection (tachi#1443).
//!
//! # The rule
//!
//! Every `MemoryStore` connection carries an authorizer that denies *all*
//! schema mutation — see `memcore::db::open`'s module header for the exact
//! allowlist. A fixture that installs a failure trigger through
//! `store.connection()` therefore dies at prepare time with SQLite's generic
//! `not authorized`, before it ever reaches the code under test. The test
//! still compiles, still runs, and asserts nothing.
//!
//! That happened four times in two PRs on one day (kckylechen1/tachi#1411 x1,
//! #1431 x3), by the same author, independently. It is not a typo class; it is
//! what a correct-looking doorway plus an unwritten constraint produces. The
//! constraint's readable form lives on `memcore::MemoryStore::connection` and
//! on `crate::test_support::with_unrestricted_fixture_connection`. This file
//! is its executable form: prose fails open, a failing test does not.
//!
//! # The sanctioned route
//!
//! A second connection opened directly on the store's database file carries no
//! authorizer. Install the `RAISE(ABORT, …)` trigger there, then drive the
//! code under test through the store:
//!
//! * `tachi-server`: `crate::test_support::with_unrestricted_fixture_connection`
//! * `memcore`: `rusqlite::Connection::open(&path)`
//!
//! # What this scanner checks, and what it deliberately does not
//!
//! A file is flagged when all three hold:
//!
//! 1. its code (comments stripped) contains trigger DDL;
//! 2. its code reaches a store connection via `.connection()`/`.connection_mut()`;
//! 3. its code contains no unguarded route at all.
//!
//! File granularity rather than statement granularity is deliberate. #1431's
//! three instances routed their SQL literals through a one-line helper
//! (`install_loadout_trigger(store, sql)`), so the literal and the
//! `.connection()` call sat in different functions — a statement-scoped or
//! function-scoped scanner misses three of the four historical instances. The
//! cost is a coarser rule: a file that already demonstrates knowledge of the
//! unguarded route gets the benefit of the doubt on a later mistake. That is
//! the right side to be coarse on, because the failure being prevented is
//! *not knowing the route exists*.
//!
//! Drift direction: an over-broad match produces a visible RED that a human
//! resolves with an allowlist entry. It cannot produce a false green.

use std::path::{Path, PathBuf};

/// Path of this file, skipped so the scanner does not flag its own fixtures.
const CENSUS_RELATIVE_PATH: &str =
    "crates/tachi-server/src/tests/docs_tests/store_trigger_ddl_census.rs";

/// Matched against whitespace-collapsed, uppercased, backslash-stripped code.
const TRIGGER_DDL_NEEDLES: &[&str] = &[
    "CREATE TRIGGER",
    "CREATE TEMP TRIGGER",
    "DROP TRIGGER",
    "DROP TEMP TRIGGER",
];

/// Reaching a guarded `MemoryStore` connection.
const STORE_CONNECTION_NEEDLES: &[&str] = &[".connection()", ".connection_mut()"];

/// Evidence that the file knows about the unguarded route. `Connection::open(`
/// carries the trailing paren on purpose: `Connection::open_in_memory()` opens
/// a *different* database and can never inject into a store, so it must not
/// count as knowledge of the route.
const UNGUARDED_ROUTE_NEEDLES: &[&str] =
    &["with_unrestricted_fixture_connection", "Connection::open("];

#[derive(Debug, Clone, Copy)]
struct AllowEntry {
    /// Repo-relative path, or a prefix when it ends in `/`.
    path: &'static str,
    reason: &'static str,
}

/// Keep sorted by path. Every entry must explain why the file's trigger DDL is
/// not fault injection through a store connection.
const ALLOWLIST: &[AllowEntry] = &[
    AllowEntry {
        path: "crates/memcore/src/db/search_generation.rs",
        reason: "canonical search-generation trigger DDL run under an armed \
                 authorize_schema_migration token; the authorizer admits these \
                 two byte-exact shapes and nothing else, so this file cannot \
                 express fault injection even deliberately",
    },
    AllowEntry {
        path: "crates/memcore/src/db/tests/search_generation.rs",
        reason: "same: drops/recreates the canonical generation triggers under \
                 an armed migration token to exercise the migration path",
    },
];

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

fn allow_entry(relative: &str) -> Option<&'static AllowEntry> {
    ALLOWLIST.iter().find(|entry| {
        if entry.path.ends_with('/') {
            relative.starts_with(entry.path)
        } else {
            relative == entry.path
        }
    })
}

fn rust_sources(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read source directory") {
        let entry = entry.expect("read directory entry");
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "target" || name == "node_modules" || name.starts_with('.') {
                continue;
            }
            rust_sources(&path, files);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            files.push(path);
        }
    }
}

/// Drop each line's `//`-comment tail, quote-aware so a `//` inside a string
/// literal survives. This is what keeps documentation *about* the rule — which
/// necessarily quotes the forbidden SQL — from tripping the rule.
fn strip_line_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    for line in source.lines() {
        out.push_str(code_before_line_comment(line));
        out.push('\n');
    }
    out
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

/// Collapse whitespace, uppercase, and drop backslashes so SQL split across
/// Rust string-continuation lines still reads as one statement.
fn normalize_sql_text(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    let mut pending_space = false;
    for character in code.chars() {
        if character == '\\' {
            continue;
        }
        if character.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.extend(character.to_uppercase());
    }
    out
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

/// The whole rule, over one file's source text. Factored out so it can be
/// exercised on fixtures rather than only on the live tree.
fn violates(source: &str) -> bool {
    let code = strip_line_comments(source);
    contains_any(&normalize_sql_text(&code), TRIGGER_DDL_NEEDLES)
        && contains_any(&code, STORE_CONNECTION_NEEDLES)
        && !contains_any(&code, UNGUARDED_ROUTE_NEEDLES)
}

fn violating_files(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    rust_sources(&root.join("crates"), &mut files);
    files.sort();

    let mut scanned = 0_usize;
    let mut violations = Vec::new();
    for path in &files {
        let relative = relative_path(root, path);
        if relative == CENSUS_RELATIVE_PATH {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        scanned += 1;
        if violates(&source) && allow_entry(&relative).is_none() {
            violations.push(relative);
        }
    }

    assert!(
        scanned > 100,
        "census scanned only {scanned} Rust sources — the walker is broken, \
         not the tree"
    );
    violations
}

#[test]
fn store_connection_trigger_ddl_census() {
    let violations = violating_files(&repo_root());
    if violations.is_empty() {
        return;
    }

    let mut message = String::from(
        "trigger DDL in a file that reaches a MemoryStore connection and has \
         no unguarded fixture connection:\n",
    );
    for relative in &violations {
        message.push_str("  ");
        message.push_str(relative);
        message.push('\n');
    }
    message.push_str(
        "\nA MemoryStore connection denies ALL schema DDL (memcore::db::open's \
         authorizer allowlist is two byte-exact internal shapes). A failure \
         trigger installed through store.connection() dies on `not authorized` \
         before reaching the code under test, so the test asserts nothing — \
         kckylechen1/tachi#1443, four such fixtures in two PRs in one day.\n\n\
         To fail a store write mid-transaction, install the RAISE(ABORT, ...) \
         trigger on a SECOND, unguarded connection to the same database file:\n\
         \x20 tachi-server: crate::test_support::with_unrestricted_fixture_connection\n\
         \x20 memcore:      rusqlite::Connection::open(&path)\n\
         The store must be file-backed (open_in_memory has no path to reopen), \
         and a persistent injected trigger must be dropped before anything \
         reopens the file or validate_persistent_trigger_inventory will refuse \
         the open. Full rule: memcore::MemoryStore::connection's rustdoc.\n\n\
         When the failure need not be injected, prefer a deterministic no-DDL \
         failure (missing row, violated constraint).\n\n\
         If this file's trigger DDL is genuinely not fault injection through a \
         store connection, add an ALLOWLIST entry with a reason.\n",
    );
    panic!("{message}");
}

#[test]
fn census_flags_the_historical_inline_fixture_shape() {
    // kckylechen1/tachi#1411, crates/tachi-server/src/tests/vault_tests/rotation/access_count.rs
    let source = r#"
        fn install_failure_trigger(store: &memcore::MemoryStore) {
            store
                .connection()
                .execute_batch(
                    "CREATE TEMP TRIGGER fail_second_access_count_touch \
                     BEFORE UPDATE OF access_count ON vault_entries \
                     BEGIN SELECT RAISE(ABORT, 'boom'); END;",
                )
                .expect("install fixture failure trigger");
        }
    "#;
    assert!(
        violates(source),
        "the #1411 inline shape must be flagged; the scanner has stopped working"
    );
}

#[test]
fn census_flags_the_historical_indirected_helper_shape() {
    // kckylechen1/tachi#1431: SQL literal and `.connection()` in different
    // functions. This is the case that forces file-granularity.
    let source = r#"
        fn install_loadout_trigger(store: &memcore::MemoryStore, sql: &str) {
            store
                .connection()
                .execute_batch(sql)
                .expect("install loadout transaction trigger");
        }

        fn case_one(store: &memcore::MemoryStore) {
            install_loadout_trigger(
                store,
                "CREATE TEMP TRIGGER loadout_force_overlay_insert_failure \
                 BEFORE INSERT ON hard_state \
                 BEGIN SELECT RAISE(ABORT, 'boom'); END;",
            );
        }
    "#;
    assert!(
        violates(source),
        "the #1431 indirected shape must be flagged; file granularity is the \
         only reason this case is reachable at all"
    );
}

#[test]
fn census_accepts_the_unguarded_fixture_route() {
    let source = r#"
        fn inject(server: &crate::MemoryServer) {
            crate::test_support::with_unrestricted_fixture_connection(
                &server.global_db_path_buf(),
                |connection| {
                    connection.execute_batch(
                        "CREATE TRIGGER fail_ingest_success_audit \
                         BEFORE INSERT ON audit_log \
                         BEGIN SELECT RAISE(FAIL, 'boom'); END;",
                    )
                },
            )
            .expect("inject failure");
            let _ = server.store().connection();
        }
    "#;
    assert!(
        !violates(source),
        "the sanctioned route must not be flagged, or the fence teaches the \
         wrong lesson"
    );
}

#[test]
fn census_accepts_a_raw_memcore_fixture_connection() {
    let source = r#"
        fn inject(path: &std::path::Path, store: &memcore::MemoryStore) {
            let offline = rusqlite::Connection::open(path).expect("offline fixture");
            offline
                .execute_batch(
                    "CREATE TRIGGER fail_pool_rotation \
                     BEFORE INSERT ON vault_key_rotations \
                     BEGIN SELECT RAISE(ABORT, 'boom'); END;",
                )
                .expect("install failure trigger");
            drop(offline);
            let _ = store.connection();
        }
    "#;
    assert!(
        !violates(source),
        "memcore's raw second-connection idiom must not be flagged"
    );
}

#[test]
fn census_does_not_count_an_in_memory_connection_as_an_unguarded_route() {
    // `Connection::open_in_memory()` opens a different database, so it can
    // never inject into the store under test. The trailing paren in
    // UNGUARDED_ROUTE_NEEDLES is what keeps it from granting a free pass.
    let source = r#"
        fn bogus(store: &memcore::MemoryStore) {
            let _elsewhere = rusqlite::Connection::open_in_memory().unwrap();
            store
                .connection()
                .execute_batch("CREATE TEMP TRIGGER t BEFORE UPDATE ON memories BEGIN SELECT 1; END;")
                .unwrap();
        }
    "#;
    assert!(
        violates(source),
        "open_in_memory must not satisfy the unguarded-route check"
    );
}

#[test]
fn census_ignores_documentation_about_the_rule() {
    // Every doc comment written for tachi#1443 quotes the forbidden SQL. If
    // comment stripping regresses, the fix documents itself into a RED.
    let source = r#"
        /// Do not write `CREATE TEMP TRIGGER` against `store.connection()`.
        //! DROP TRIGGER is denied here too.
        fn documented(store: &memcore::MemoryStore) {
            let _ = store.connection();
        }
    "#;
    assert!(
        !violates(source),
        "documentation quoting the rule must not trip the rule"
    );
}

#[test]
fn census_allowlist_entries_still_exist_and_still_need_the_exemption() {
    let root = repo_root();
    for entry in ALLOWLIST {
        assert!(
            !entry.reason.trim().is_empty(),
            "allowlist entry {} has no reason",
            entry.path
        );
        if entry.path.ends_with('/') {
            assert!(
                root.join(entry.path).is_dir(),
                "allowlist prefix {} no longer exists — drop the entry",
                entry.path
            );
            continue;
        }
        let path = root.join(entry.path);
        assert!(
            path.is_file(),
            "allowlist entry {} no longer exists — drop the entry",
            entry.path
        );
        let source = std::fs::read_to_string(&path).expect("read allowlisted source");
        assert!(
            violates(&source),
            "allowlist entry {} would no longer be flagged — drop the entry so \
             the exemption cannot outlive its reason",
            entry.path
        );
    }
}
