//! Source-level census: every trigger-DDL site in the repository must be
//! named in this file's inventory (tachi#1443).
//!
//! # The rule this defends
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
//! # What this scanner checks
//!
//! The unit of analysis is a **trigger-DDL site**: one occurrence of
//! `CREATE [TEMP] TRIGGER` / `DROP [TEMP] TRIGGER` in a file's code (comments
//! stripped, whitespace collapsed, uppercased, backslashes dropped), together
//! with the trigger name that follows it. Every site in `crates/` must be
//! covered by an [`Exemption`] entry below: an exact file path, the sorted set
//! of trigger names that file is allowed to mention, a count of sites whose
//! name is not statically readable, a [`ExemptionProof`] that the census
//! re-derives from the file itself, and a human reason.
//!
//! An uncovered site is a RED. There is no co-occurrence predicate left to
//! evade — in particular:
//!
//! * moving the SQL literal into a sibling module does not help: the literal
//!   is the anchor, and its new home has no entry;
//! * moving the `store.connection()` call into a helper does not help: the
//!   census never looked at where the connection call lives;
//! * an unguarded-route call somewhere in the file does not help: it is one of
//!   three *proofs*, and a proof only validates the names an entry already
//!   pins. It grants nothing on its own.
//!
//! This replaces an earlier file-scoped rule that ANDed three per-file
//! predicates (trigger DDL ∧ a store connection ∧ no unguarded route). That
//! rule caught both historical inputs — the two `census_flags_the_historical_*`
//! tests below still pin those exact shapes — but it was one refactor step from
//! blind on each of its three conjuncts, and its third conjunct handed 26 files
//! (including `crate::test_support`, the fixture hub) a pass that no later edit
//! could revoke.
//!
//! # What it costs, deliberately
//!
//! Adding any trigger to any Rust source in `crates/` now turns this test RED
//! until the trigger's name is written into the inventory below. That is the
//! intended price: the name lands in one central file, in a diff, next to the
//! doctrine explaining what the wall is — an author cannot silence this gate
//! without writing the fault-injection trigger's own name underneath a comment
//! that says trigger DDL on a store connection asserts nothing.
//!
//! # Honest limits
//!
//! * The inventory is still authored by the same person who writes the fixture.
//!   The pinned *names* bound the blast radius and the *proofs* are machine-
//!   re-derived, but the free-text `reason` is not checked, and nothing here
//!   can stop a determined author who understands the rule from typing one more
//!   name. The residual control is human review of a central, deliberately
//!   conspicuous list.
//! * `ExemptionProof::NoStoreDoorwayInFile` is the weakest of the three and is
//!   marked as such at its definition; it self-invalidates the moment its file
//!   gains a store doorway.
//! * This file is skipped by the walk (`CENSUS_RELATIVE_PATH`). Its own trigger
//!   DDL lives in inert `&str` fixtures that are handed to `trigger_sites`, not
//!   to a database, and it is not textually distinguishable from executed DDL.
//!   That is one documented hole, not a general escape hatch, and it is the
//!   only path in the walk that is skipped rather than inventoried.
//! * Site extraction is textual. Trigger names assembled at runtime cannot be
//!   read, so they are counted instead (`unnamed_sites`); the count is pinned
//!   per entry, so a new interpolated site is still a RED.
//! * Also textual, and not closed: DDL whose *keyword* is split so that
//!   `CREATE TRIGGER` never appears contiguously after normalization —
//!   `concat!("CREATE ", "TRIGGER …")`, `format!("{verb} TRIGGER …")`, a
//!   `/* … */` block comment (only `//` tails are stripped). Each is invisible
//!   here. None of them is a step a fixture author takes by accident, which is
//!   the failure this census exists to catch; a deliberate evader is out of
//!   scope for a source scanner and is the reason `memcore`'s runtime
//!   `validate_persistent_trigger_inventory` exists as an independent fence.
//!
//! Drift direction throughout: every unknown resolves to RED. A missing entry,
//! a stale entry, a name the file no longer mentions, a proof that stopped
//! holding, or a site count that moved all fail the suite. None of them can
//! produce a false green.

use std::path::{Path, PathBuf};

/// Path of this file, skipped so the scanner does not flag its own fixtures.
/// See the "Honest limits" note in the module header — this is the one skip.
const CENSUS_RELATIVE_PATH: &str =
    "crates/tachi-server/src/tests/docs_tests/store_trigger_ddl_census.rs";

/// Matched against whitespace-collapsed, uppercased, backslash-stripped code.
/// No needle may be a prefix of another (pinned by
/// `census_needles_are_mutually_non_prefixing`), so at any one index at most
/// one of them can match and site extraction is unambiguous.
const TRIGGER_DDL_NEEDLES: &[&str] = &[
    "CREATE TEMP TRIGGER",
    "CREATE TRIGGER",
    "DROP TEMP TRIGGER",
    "DROP TRIGGER",
];

/// Optional SQL noise between the DDL keyword and the trigger name.
const TRIGGER_NAME_PREFIXES: &[&str] = &["IF NOT EXISTS ", "IF EXISTS "];

/// Schema qualifiers stripped off a trigger name so `temp.foo` and `foo` pin
/// as the same trigger.
const TRIGGER_SCHEMA_QUALIFIERS: &[&str] = &["TEMP.", "MAIN."];

/// Reaching a guarded `MemoryStore` connection. `connection`/`connection_mut`
/// are defined only on `MemoryStore` (`memcore/src/store/crud.rs:127` and
/// `:138`), so these two needles are specific to the guarded doorway.
const STORE_CONNECTION_NEEDLES: &[&str] = &[".connection()", ".connection_mut()"];

/// Evidence that a file opens its own unguarded connection. `Connection::open(`
/// carries the trailing paren on purpose: `Connection::open_in_memory()` opens
/// a *different* database and can never inject into a store, so it must not
/// count.
const UNGUARDED_ROUTE_NEEDLES: &[&str] =
    &["with_unrestricted_fixture_connection", "Connection::open("];

/// Arming the scoped schema-migration token. `authorize_schema_migration` is
/// `pub(crate)` to memcore (`memcore/src/db/open.rs:494`), which is why
/// [`ExemptionProof::MemcoreArmsTheMigrationToken`] is structurally
/// unreachable from `tachi-server` — the crate that produced all four #1411 /
/// #1431 fixtures.
const SCHEMA_MIGRATION_TOKEN_NEEDLE: &str = "authorize_schema_migration";

/// Path prefix of the crate that owns the wall.
const MEMCORE_PATH_PREFIX: &str = "crates/memcore/";

/// How an entry proves its exemption. Each variant is re-derived from the
/// exempted file on every run; a variant that stops holding **voids the whole
/// entry**, so every name it pinned goes RED. An exemption can never outlive
/// the condition it was granted under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExemptionProof {
    /// Checked: the file is under `crates/memcore/` **and** arms the scoped
    /// schema-migration token. The strongest of the three: the token is
    /// `pub(crate)` to memcore, so no `tachi-server` file can claim this
    /// variant without first being moved into another crate.
    MemcoreArmsTheMigrationToken,
    /// Checked: the file opens its own unguarded connection
    /// ([`UNGUARDED_ROUTE_NEEDLES`]). This is the sanctioned fault-injection
    /// route. Note what it is *not*: under the old file-scoped rule this needle
    /// alone silenced the whole file forever. Here it only makes an entry's
    /// pinned names admissible; an unpinned name in the same file is still RED
    /// (`an_unguarded_route_needle_alone_excuses_nothing`).
    FileOpensAnUnguardedConnection,
    /// Checked: the file names no store doorway at all
    /// ([`STORE_CONNECTION_NEEDLES`]), so nothing in it can reach a guarded
    /// connection from here.
    ///
    /// **Weakest variant — prefer either of the others.** It is the one that
    /// keys on the absence of something the file's own author controls. It is
    /// kept because canonical schema DDL has to live somewhere, and it is
    /// self-invalidating: the day the file gains a `.connection()` call the
    /// proof stops holding, the entry voids, and every name in it must be
    /// re-justified under a stronger variant.
    NoStoreDoorwayInFile,
}

impl ExemptionProof {
    fn holds(self, relative: &str, code: &str) -> bool {
        match self {
            Self::MemcoreArmsTheMigrationToken => {
                relative.starts_with(MEMCORE_PATH_PREFIX)
                    && code.contains(SCHEMA_MIGRATION_TOKEN_NEEDLE)
            }
            Self::FileOpensAnUnguardedConnection => contains_any(code, UNGUARDED_ROUTE_NEEDLES),
            Self::NoStoreDoorwayInFile => !contains_any(code, STORE_CONNECTION_NEEDLES),
        }
    }

    fn requirement(self) -> &'static str {
        match self {
            Self::MemcoreArmsTheMigrationToken => {
                "the file must live under crates/memcore/ and call authorize_schema_migration"
            }
            Self::FileOpensAnUnguardedConnection => {
                "the file must open an unguarded connection — \
                 with_unrestricted_fixture_connection, or Connection::open"
            }
            Self::NoStoreDoorwayInFile => {
                "the file must not mention .connection() or .connection_mut()"
            }
        }
    }
}

/// One inventoried file. `triggers` is the complete, sorted, deduplicated set
/// of trigger names the file may mention; `unnamed_sites` is the exact number
/// of sites in it whose name cannot be read statically.
#[derive(Debug, Clone, Copy)]
struct Exemption {
    /// Exact repo-relative path. Prefix entries are rejected by
    /// `census_inventory_is_sorted_exact_and_deduplicated`: a directory-wide
    /// exemption is an unbounded hole and this inventory does not have one.
    path: &'static str,
    proof: ExemptionProof,
    /// Uppercased trigger names, sorted, no duplicates.
    triggers: &'static [&'static str],
    /// Sites whose trigger name is interpolated at runtime, or where the string
    /// literal ends right after the keyword. Neither can be identified by name,
    /// so they are pinned by count.
    unnamed_sites: usize,
    /// Why these names are not fault injection through a store connection.
    /// NOT machine-checked — the pinned names and the proof are the bound; this
    /// is what a reviewer reads. State enclosing symbols, not adjectives, and
    /// say plainly whether the bodies were read.
    reason: &'static str,
}

/// Keep sorted by path.
const EXEMPTIONS: &[Exemption] = &[
    Exemption {
        path: "crates/memcore/src/db/memory_crud.rs",
        proof: ExemptionProof::MemcoreArmsTheMigrationToken,
        triggers: &[
            "AUTHORIZATION_LEAK",
            "MALICIOUS_ACCESS_HISTORY_INSERT",
            "MALICIOUS_MEMORY_UPDATE",
            "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
            "MEMORY_SEARCH_GENERATION_AFTER_UPDATE",
            "MIGRATION_DDL_BYPASS",
            "MIXEDCASEAUXTRIGGER",
            "TEMPAUXTRIGGER",
            "TYPED_DML_DDL_BYPASS",
            "TYPED_SCOPE_ERROR",
        ],
        unnamed_sites: 0,
        reason: "memcore's own authorizer tests, bodies read 2026-07-26. Each \
                 name is either pushed at a store connection and asserted to be \
                 REFUSED (raw_connection_cannot_disable_reserved_reference_guards, \
                 raw_triggers_cannot_launder_typed_write_authority, \
                 raw_auxiliary_trigger_cannot_chain_into_typed_search_write, \
                 schema_migration_scope_allows_only_canonical_trigger_ddl, \
                 private_authorization_scopes_reset_after_database_errors), \
                 planted on a second connection before the authorizer is \
                 installed (raw_trigger_drop_is_denied_for_arbitrary_main_and_temp_triggers), \
                 or the canonical search-generation trigger dropped under an \
                 armed migration token. These tests are the wall's proof, not \
                 users of it.",
    },
    Exemption {
        path: "crates/memcore/src/db/migrations.rs",
        proof: ExemptionProof::FileOpensAnUnguardedConnection,
        triggers: &[
            "MEMORIES_RESERVED_REFS_INSERT_GUARD",
            "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
            "MEMORY_SEARCH_GENERATION_AFTER_UPDATE",
        ],
        unnamed_sites: 0,
        reason: "v22->v23 migration tests; the three names are the migration's \
                 own canonical DDL. Enclosing tests: \
                 v22_to_v23_installs_reserved_reference_guards_and_stamps, \
                 v23_guard_install_failure_rolls_back_triggers_sentinel_and_stamp, \
                 stamped_v23_with_missing_guards_is_refused_even_with_migration_authority, \
                 stamped_v23_with_missing_search_generation_trigger_is_refused_without_repair. \
                 Enclosing symbols extracted mechanically; bodies NOT read — the \
                 pinned names are the bound.",
    },
    Exemption {
        path: "crates/memcore/src/db/open.rs",
        proof: ExemptionProof::MemcoreArmsTheMigrationToken,
        triggers: &[
            "INGEST_STABLE_OWNER_FENCE",
            "WF1443_FAIL_STATE_WRITE",
            "WF1443_FAULT_PROBE",
        ],
        unnamed_sites: 0,
        reason: "the wall's own module. INGEST_STABLE_OWNER_FENCE is the \
                 byte-exact temp-trigger shape the authorizer admits under the \
                 owner-fence token (install_ingest_stable_owner_fence, \
                 remove_ingest_stable_owner_fence, and the two scoped_owner_fence \
                 tests). WF1443_FAULT_PROBE is asserted DENIED on a store \
                 connection and WF1443_FAIL_STATE_WRITE is installed on an \
                 unguarded second connection and asserted to fail a real store \
                 write — those two tests are tachi#1443's executable pins.",
    },
    Exemption {
        path: "crates/memcore/src/db/schema/ddl.rs",
        proof: ExemptionProof::NoStoreDoorwayInFile,
        triggers: &[
            "MEMORIES_RESERVED_REFS_INSERT_GUARD",
            "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
        ],
        unnamed_sites: 0,
        reason: "the canonical reserved-reference guard triggers as const DDL \
                 text; this file executes nothing. Weakest proof in the enum on \
                 purpose — it holds only while this file names no store doorway, \
                 and voids itself the day one appears.",
    },
    Exemption {
        path: "crates/memcore/src/db/schema/migration_backup_tests.rs",
        proof: ExemptionProof::FileOpensAnUnguardedConnection,
        triggers: &[
            "MEMORIES_RESERVED_REFS_INSERT_GUARD",
            "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
        ],
        unnamed_sites: 0,
        reason: "seed_pre_v23_fixture drops the canonical guards on a fixture \
                 database to manufacture a pre-v23 shape. Enclosing symbol \
                 extracted mechanically; body NOT read.",
    },
    Exemption {
        path: "crates/memcore/src/db/search_generation.rs",
        proof: ExemptionProof::MemcoreArmsTheMigrationToken,
        triggers: &[
            "MEMORY_ACCESS_SEARCH_GENERATION_AFTER_DELETE",
            "MEMORY_ACCESS_SEARCH_GENERATION_AFTER_INSERT",
            "MEMORY_ACCESS_SEARCH_GENERATION_AFTER_UPDATE",
            "MEMORY_EDGE_SEARCH_GENERATION_AFTER_DELETE",
            "MEMORY_EDGE_SEARCH_GENERATION_AFTER_INSERT",
            "MEMORY_EDGE_SEARCH_GENERATION_AFTER_UPDATE",
            "MEMORY_SEARCH_GENERATION_AFTER_DELETE",
            "MEMORY_SEARCH_GENERATION_AFTER_INSERT",
            "MEMORY_SEARCH_GENERATION_AFTER_UPDATE",
        ],
        unnamed_sites: 2,
        reason: "the canonical search-generation triggers themselves, and the \
                 migration that replaces a drifted one, all under an armed \
                 migration token. The two unnamed sites are trigger_sql's \
                 `CREATE TRIGGER {name}` builder and the literal \
                 `.expect(\"drop trigger\")` message in \
                 missing_or_drifted_trigger_refuses_generation_read, which \
                 uppercases into a keyword with no name after it. Both read \
                 2026-07-26.",
    },
    Exemption {
        path: "crates/memcore/src/db/tests/search_generation.rs",
        proof: ExemptionProof::MemcoreArmsTheMigrationToken,
        triggers: &[
            "MEMORY_EDGE_SEARCH_GENERATION_AFTER_UPDATE",
            "MEMORY_SEARCH_GENERATION_AFTER_UPDATE",
        ],
        unnamed_sites: 1,
        reason: "drops and recreates the canonical generation triggers under an \
                 armed migration token to exercise the migration path \
                 (missing_trigger_refuses_search_generation, \
                 known_previous_memory_update_trigger_migrates_to_all_column_coverage). \
                 The unnamed site is the same `.expect(\"drop trigger\")` message \
                 shape; read 2026-07-26.",
    },
    Exemption {
        path: "crates/memcore/src/store/memory_lifecycle.rs",
        proof: ExemptionProof::FileOpensAnUnguardedConnection,
        triggers: &["ABORT_LIFECYCLE_PROPOSAL_STAMP"],
        unnamed_sites: 0,
        reason: "apply_rolls_back_memory_when_proposal_stamp_aborts injects a \
                 rollback fault on memcore's raw second connection. Enclosing \
                 symbol extracted mechanically; body NOT read.",
    },
    Exemption {
        path: "crates/memcore/src/store/open.rs",
        proof: ExemptionProof::MemcoreArmsTheMigrationToken,
        triggers: &[
            "MALICIOUS_ACCESS_CHAIN",
            "MEMORIES_RESERVED_REFS_INSERT_GUARD",
            "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
        ],
        unnamed_sites: 0,
        reason: "open-path tests over the canonical guard inventory \
                 (open_refuses_unknown_or_spoofed_persistent_triggers, \
                 authorized_v22_migration_installs_missing_reference_guards, \
                 existing_opens_refuse_missing_reserved_reference_guards_before_repair, \
                 stamp_v22_without_v23_guards, \
                 read_only_existing_schema_compat_opens_stamped_older_without_write_authority). \
                 Enclosing symbols extracted mechanically; bodies NOT read.",
    },
    Exemption {
        path: "crates/memcore/src/store/vault.rs",
        proof: ExemptionProof::FileOpensAnUnguardedConnection,
        triggers: &["FAIL_POOL_ROTATION"],
        unnamed_sites: 0,
        reason: "vault_replace_api_key_pool_rolls_back_when_rotation_write_fails \
                 — memcore's raw second-connection idiom, the shape #1443's \
                 doorway doc points fixture authors at.",
    },
    Exemption {
        path: "crates/tachi-server/src/mcp_pool/proxy.rs",
        proof: ExemptionProof::FileOpensAnUnguardedConnection,
        triggers: &["FAIL_PROXY_AUTO_INGEST_ROW", "FAIL_PROXY_AUTO_INGEST_STAGE"],
        unnamed_sites: 0,
        reason: "both installed inside with_unrestricted_fixture_connection on \
                 the server's global DB path; bodies read 2026-07-26 \
                 (failed_auto_ingest_does_not_replace_successful_mcp_result, \
                 failed_auto_ingest_staging_is_visible_in_pipeline_status). This \
                 is the sanctioned shape.",
    },
    Exemption {
        path: "crates/tachi-server/src/pipeline_ops/audit.rs",
        proof: ExemptionProof::FileOpensAnUnguardedConnection,
        triggers: &["FAIL_INGEST_HEARTBEAT", "MARK_HEARTBEAT_AFTER_JOIN"],
        unnamed_sites: 0,
        reason: "both installed through with_offline_global_fixture_connection \
                 (a local wrapper over the unguarded route) inside \
                 lost_heartbeat_fences_stale_graph_observation_after_takeover, \
                 and dropped again in the same test; body read 2026-07-26.",
    },
    Exemption {
        path: "crates/tachi-server/src/repair/tests/plan_c_restore.rs",
        proof: ExemptionProof::FileOpensAnUnguardedConnection,
        triggers: &[
            "MEMORIES_FTS_FAIL_DELETE",
            "MEMORIES_FTS_FAIL_UPDATE",
            "MEMORIES_SYMBOLIC_FTS_FAIL_DELETE",
        ],
        unnamed_sites: 0,
        reason: "FTS restore fault injection on an unguarded fixture connection; \
                 this file never mentions a store doorway. Enclosing symbols \
                 extracted mechanically; bodies NOT read.",
    },
    Exemption {
        path: "crates/tachi-server/src/repair/tests/quarantine_jobs_integrity.rs",
        proof: ExemptionProof::FileOpensAnUnguardedConnection,
        triggers: &["QUARANTINE_PURGE_REQUIRES_DEFAULT_DENY_GUARD"],
        unnamed_sites: 0,
        reason: "installs the quarantine default-deny guard on an unguarded \
                 fixture connection; this file never mentions a store doorway. \
                 Body NOT read.",
    },
    Exemption {
        path: "crates/tachi-server/src/tests/memory_tests/save_policy/recall_cache_invalidation.rs",
        proof: ExemptionProof::FileOpensAnUnguardedConnection,
        triggers: &["MEMORY_SEARCH_GENERATION_AFTER_INSERT"],
        unnamed_sites: 0,
        reason: "missing_generation_trigger_bypasses_a_warm_cache_instead_of_serving_stale_rows \
                 drops the canonical insert trigger through \
                 with_unrestricted_fixture_connection to simulate drift; body \
                 read 2026-07-26.",
    },
    Exemption {
        path: "crates/tachi-server/src/tests/mod.rs",
        proof: ExemptionProof::FileOpensAnUnguardedConnection,
        triggers: &[
            "MEMORIES_RESERVED_REFS_INSERT_GUARD",
            "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
        ],
        unnamed_sites: 0,
        reason: "seed_pre_v23_wiki_reference_metadata drops the canonical guards \
                 on a fixture database to manufacture a pre-v23 shape. Enclosing \
                 symbol extracted mechanically; body NOT read.",
    },
    Exemption {
        path: "crates/tachi-server/src/tests/skill_tests/builtin_ingest/ingest_source.rs",
        proof: ExemptionProof::FileOpensAnUnguardedConnection,
        triggers: &["FAIL_INGEST_SUCCESS_AUDIT"],
        unnamed_sites: 0,
        reason: "source_success_audit_failure_is_loud_retryable_and_idempotent \
                 installs and then drops the audit fault inside \
                 with_unrestricted_fixture_connection; body read 2026-07-26. \
                 This is the sanctioned shape.",
    },
    Exemption {
        path: "crates/tachi-server/src/tests/wiki_tests/write/facade_routing/guide_metadata.rs",
        proof: ExemptionProof::FileOpensAnUnguardedConnection,
        triggers: &[
            "MEMORIES_RESERVED_REFS_INSERT_GUARD",
            "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
        ],
        unnamed_sites: 0,
        reason: "pre-v23 wiki fixture seeding on an unguarded connection; this \
                 file never mentions a store doorway. Enclosing symbol extracted \
                 mechanically; body NOT read.",
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

fn exemption(relative: &str) -> Option<&'static Exemption> {
    EXEMPTIONS.iter().find(|entry| entry.path == relative)
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
/// Rust string-continuation lines still reads as one statement. Returned as
/// `Vec<char>` so the scanner can index without worrying about UTF-8
/// boundaries — uppercasing non-ASCII source text can widen a character.
fn normalize_sql_chars(code: &str) -> Vec<char> {
    let mut out: Vec<char> = Vec::with_capacity(code.len());
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

fn matches_at(text: &[char], index: usize, needle: &str) -> bool {
    let mut cursor = index;
    for expected in needle.chars() {
        match text.get(cursor) {
            Some(actual) if *actual == expected => cursor += 1,
            _ => return false,
        }
    }
    true
}

fn is_trigger_name_char(character: char) -> bool {
    character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        || character == '.'
}

/// One trigger-DDL occurrence. `Named` carries the uppercased trigger name;
/// `Unnamed` is a site whose name cannot be read statically — a runtime
/// interpolation such as `format!("CREATE TRIGGER {name} …")`, or a string
/// literal that ends right after the keyword.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TriggerSite {
    Named(String),
    Unnamed,
}

/// Every trigger-DDL site in one file's comment-stripped code.
fn trigger_sites(code: &str) -> Vec<TriggerSite> {
    let text = normalize_sql_chars(code);
    let mut sites = Vec::new();
    let mut index = 0;
    while index < text.len() {
        let mut matched: Option<&str> = None;
        for needle in TRIGGER_DDL_NEEDLES {
            if matches_at(&text, index, needle) {
                matched = Some(*needle);
                break;
            }
        }
        let Some(needle) = matched else {
            index += 1;
            continue;
        };
        let mut cursor = index + needle.chars().count();
        if text.get(cursor) == Some(&' ') {
            cursor += 1;
        }
        for prefix in TRIGGER_NAME_PREFIXES {
            if matches_at(&text, cursor, prefix) {
                cursor += prefix.chars().count();
                break;
            }
        }
        let start = cursor;
        while text.get(cursor).is_some_and(|c| is_trigger_name_char(*c)) {
            cursor += 1;
        }
        let mut name: String = text[start..cursor].iter().collect();
        for qualifier in TRIGGER_SCHEMA_QUALIFIERS {
            if name.starts_with(qualifier) {
                name.drain(..qualifier.len());
                break;
            }
        }
        sites.push(if name.is_empty() {
            TriggerSite::Unnamed
        } else {
            TriggerSite::Named(name)
        });
        index += needle.chars().count();
    }
    sites
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

/// The whole rule, over one file. Factored out so it can be exercised on
/// fixtures — including fixtures paired with a hand-built [`Exemption`] — rather
/// than only on the live tree.
///
/// Returns one finding per uncovered site. Empty means every trigger-DDL site
/// in `source` is accounted for. Every failure path here adds findings; none
/// removes them.
fn uncovered_sites(relative: &str, source: &str, entry: Option<&Exemption>) -> Vec<String> {
    let code = strip_line_comments(source);
    let sites = trigger_sites(&code);
    if sites.is_empty() {
        return Vec::new();
    }

    let mut named: Vec<String> = sites
        .iter()
        .filter_map(|site| match site {
            TriggerSite::Named(name) => Some(name.clone()),
            TriggerSite::Unnamed => None,
        })
        .collect();
    named.sort();
    named.dedup();
    let unnamed = sites
        .iter()
        .filter(|site| **site == TriggerSite::Unnamed)
        .count();

    let mut findings = Vec::new();
    let Some(entry) = entry else {
        for name in &named {
            findings.push(format!("{relative}: trigger `{name}` is not inventoried"));
        }
        if unnamed > 0 {
            findings.push(format!(
                "{relative}: {unnamed} trigger-DDL site(s) with an unreadable name, none inventoried"
            ));
        }
        return findings;
    };

    if !entry.proof.holds(relative, &code) {
        // Fail closed: a proof that stopped holding voids the entry outright
        // rather than degrading it, so every name it pinned is uncovered.
        findings.push(format!(
            "{relative}: inventory proof {:?} no longer holds ({}) — the entry is void",
            entry.proof,
            entry.proof.requirement()
        ));
        for name in &named {
            findings.push(format!(
                "{relative}: trigger `{name}` is covered only by the voided entry"
            ));
        }
        if unnamed > 0 {
            findings.push(format!(
                "{relative}: {unnamed} unnamed site(s) covered only by the voided entry"
            ));
        }
        return findings;
    }

    for name in &named {
        if !entry.triggers.contains(&name.as_str()) {
            findings.push(format!(
                "{relative}: trigger `{name}` is not in this file's inventory entry"
            ));
        }
    }
    if unnamed != entry.unnamed_sites {
        findings.push(format!(
            "{relative}: {unnamed} trigger-DDL site(s) with an unreadable name, \
             inventory pins {}",
            entry.unnamed_sites
        ));
    }
    findings
}

fn census_findings(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    rust_sources(&root.join("crates"), &mut files);
    files.sort();

    let mut scanned = 0_usize;
    let mut findings = Vec::new();
    for path in &files {
        let relative = relative_path(root, path);
        if relative == CENSUS_RELATIVE_PATH {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        scanned += 1;
        findings.extend(uncovered_sites(&relative, &source, exemption(&relative)));
    }

    assert!(
        scanned > 100,
        "census scanned only {scanned} Rust sources — the walker is broken, \
         not the tree"
    );
    findings
}

#[test]
fn store_connection_trigger_ddl_census() {
    let findings = census_findings(&repo_root());
    if findings.is_empty() {
        return;
    }

    let mut message = String::from("uninventoried trigger DDL:\n");
    for finding in &findings {
        message.push_str("  ");
        message.push_str(finding);
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
         Once the trigger really is installed on an unguarded connection, add \
         its NAME to this file's EXEMPTIONS entry (or add the entry) with a \
         proof and a reason naming the enclosing symbol. Writing a name here is \
         deliberately conspicuous: it is the record that someone decided this \
         particular trigger is not the #1443 mistake.\n",
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
    let findings = uncovered_sites("crates/tachi-server/src/fixture.rs", source, None);
    assert_eq!(
        findings.len(),
        1,
        "the #1411 inline shape must be flagged exactly once: {findings:?}"
    );
    assert!(
        findings[0].contains("FAIL_SECOND_ACCESS_COUNT_TOUCH"),
        "the finding must name the trigger so the inventory edit is explicit: {findings:?}"
    );
}

#[test]
fn census_flags_the_historical_indirected_helper_shape() {
    // kckylechen1/tachi#1431: SQL literal and `.connection()` in different
    // functions. The old file-scoped rule needed file granularity to see this
    // at all; site granularity never looked at the `.connection()` call.
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
    let findings = uncovered_sites("crates/tachi-server/src/fixture.rs", source, None);
    assert_eq!(
        findings.len(),
        1,
        "the #1431 indirected shape must be flagged: {findings:?}"
    );
    assert!(
        findings[0].contains("LOADOUT_FORCE_OVERLAY_INSERT_FAILURE"),
        "the finding must name the trigger: {findings:?}"
    );
}

#[test]
fn census_flags_a_sql_literal_split_into_its_own_module() {
    // Gap the file-scoped rule had: move the literal one module over and the
    // per-file conjunction "trigger DDL AND a store connection" goes false on
    // both halves. The site is the unit now, so the half holding the SQL is
    // still flagged and the half holding the connection was never needed.
    let sql_half = r#"
        pub(crate) const LOADOUT_FAILURE_TRIGGER: &str =
            "CREATE TEMP TRIGGER loadout_force_overlay_insert_failure \
             BEFORE INSERT ON hard_state \
             BEGIN SELECT RAISE(ABORT, 'boom'); END;";
    "#;
    let findings = uncovered_sites("crates/tachi-server/src/fixture_sql.rs", sql_half, None);
    assert_eq!(
        findings.len(),
        1,
        "a lone SQL literal with no store connection in sight must still be \
         flagged, or the split-file refactor buys silence: {findings:?}"
    );
}

#[test]
fn an_unguarded_route_needle_alone_excuses_nothing() {
    // Gap the file-scoped rule had: one `with_unrestricted_fixture_connection`
    // anywhere in a file made every trigger in it invisible forever, across 26
    // files including crate::test_support. The needle is now only a proof that
    // an entry's *pinned* names are admissible.
    let source = r#"
        fn sanctioned(server: &crate::MemoryServer) {
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
    let findings = uncovered_sites("crates/tachi-server/src/fixture.rs", source, None);
    assert_eq!(
        findings.len(),
        1,
        "an unguarded-route call must not excuse an uninventoried trigger: {findings:?}"
    );

    let entry = Exemption {
        path: "crates/tachi-server/src/fixture.rs",
        proof: ExemptionProof::FileOpensAnUnguardedConnection,
        triggers: &["FAIL_INGEST_SUCCESS_AUDIT"],
        unnamed_sites: 0,
        reason: "fixture",
    };
    assert!(
        uncovered_sites("crates/tachi-server/src/fixture.rs", source, Some(&entry)).is_empty(),
        "the sanctioned route must pass once its trigger is inventoried, or the \
         fence teaches the wrong lesson"
    );
}

#[test]
fn a_voided_proof_covers_nothing_it_previously_covered() {
    // An entry is not a skip. If its proof stops holding, everything it pinned
    // goes back to uncovered instead of quietly staying exempt.
    let source = r#"
        fn injects(store: &memcore::MemoryStore) {
            store
                .connection()
                .execute_batch(
                    "CREATE TEMP TRIGGER fail_second_access_count_touch \
                     BEFORE UPDATE OF access_count ON vault_entries \
                     BEGIN SELECT RAISE(ABORT, 'boom'); END;",
                )
                .unwrap();
        }
    "#;
    let entry = Exemption {
        path: "crates/tachi-server/src/fixture.rs",
        proof: ExemptionProof::FileOpensAnUnguardedConnection,
        triggers: &["FAIL_SECOND_ACCESS_COUNT_TOUCH"],
        unnamed_sites: 0,
        reason: "claims a route this source does not open",
    };
    let findings = uncovered_sites("crates/tachi-server/src/fixture.rs", source, Some(&entry));
    assert!(
        findings.iter().any(|f| f.contains("is void")),
        "a failed proof must void the entry loudly: {findings:?}"
    );
    assert!(
        findings
            .iter()
            .any(|f| f.contains("FAIL_SECOND_ACCESS_COUNT_TOUCH")),
        "a voided entry must stop covering its pinned names: {findings:?}"
    );
}

#[test]
fn memcore_only_proof_is_unreachable_from_tachi_server() {
    // authorize_schema_migration is pub(crate) to memcore. A tachi-server
    // fixture cannot arm the token, so it cannot claim this proof even by
    // writing the identifier into a comment-stripped line.
    let code = "let _ = crate::db::authorize_schema_migration(&flag);";
    assert!(
        ExemptionProof::MemcoreArmsTheMigrationToken
            .holds("crates/memcore/src/db/tests/x.rs", code),
        "memcore must be able to claim the strongest proof"
    );
    assert!(
        !ExemptionProof::MemcoreArmsTheMigrationToken
            .holds("crates/tachi-server/src/tests/x.rs", code),
        "the crate that produced every #1411/#1431 fixture must never reach the \
         migration-token proof"
    );
}

#[test]
fn no_store_doorway_proof_self_invalidates_when_a_doorway_appears() {
    assert!(
        ExemptionProof::NoStoreDoorwayInFile.holds(
            "crates/memcore/src/db/schema/ddl.rs",
            "const GUARD: &str = \"CREATE TRIGGER x AFTER UPDATE ON t BEGIN SELECT 1; END;\";"
        ),
        "const DDL text with no doorway must satisfy the weakest proof"
    );
    assert!(
        !ExemptionProof::NoStoreDoorwayInFile.holds(
            "crates/memcore/src/db/schema/ddl.rs",
            "let _ = store.connection();"
        ),
        "the weakest proof must stop holding the moment the file gains a doorway"
    );
}

#[test]
fn census_counts_a_runtime_interpolated_trigger_name() {
    // A name assembled at runtime cannot be pinned by name, so it is pinned by
    // count. One more of them is still a RED.
    let source = r#"
        fn build(name: &str) -> String {
            format!("CREATE TRIGGER {name} AFTER UPDATE ON memories BEGIN SELECT 1; END;")
        }
    "#;
    let entry = Exemption {
        path: "crates/tachi-server/src/fixture.rs",
        proof: ExemptionProof::NoStoreDoorwayInFile,
        triggers: &[],
        unnamed_sites: 1,
        reason: "fixture",
    };
    assert!(
        uncovered_sites("crates/tachi-server/src/fixture.rs", source, Some(&entry)).is_empty(),
        "a pinned unnamed-site count must cover the interpolated builder"
    );

    let doubled = format!("{source}{source}");
    let findings = uncovered_sites("crates/tachi-server/src/fixture.rs", &doubled, Some(&entry));
    assert_eq!(
        findings.len(),
        1,
        "a second interpolated site must break the pinned count: {findings:?}"
    );
}

#[test]
fn census_reads_names_through_if_exists_and_schema_qualifiers() {
    let source = r#"
        fn drop_all(connection: &rusqlite::Connection) {
            connection
                .execute_batch(
                    "DROP TRIGGER IF EXISTS memories_reserved_refs_insert_guard; \
                     DROP TRIGGER IF EXISTS temp.ingest_stable_owner_fence;",
                )
                .unwrap();
        }
    "#;
    let findings = uncovered_sites("crates/tachi-server/src/fixture.rs", source, None);
    assert_eq!(findings.len(), 2, "both drops must be seen: {findings:?}");
    assert!(
        findings
            .iter()
            .any(|f| f.contains("`MEMORIES_RESERVED_REFS_INSERT_GUARD`")),
        "IF EXISTS must not be read as the trigger name: {findings:?}"
    );
    assert!(
        findings
            .iter()
            .any(|f| f.contains("`INGEST_STABLE_OWNER_FENCE`")),
        "a temp. qualifier must not be read as the trigger name: {findings:?}"
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
        uncovered_sites("crates/tachi-server/src/fixture.rs", source, None).is_empty(),
        "documentation quoting the rule must not trip the rule"
    );
}

#[test]
fn census_needles_are_mutually_non_prefixing() {
    // Site extraction advances by the matched needle's length, so an ambiguous
    // needle set would silently change how many sites a file has.
    for outer in TRIGGER_DDL_NEEDLES {
        for inner in TRIGGER_DDL_NEEDLES {
            if outer == inner {
                continue;
            }
            assert!(
                !outer.starts_with(inner),
                "needle {inner:?} is a prefix of {outer:?}"
            );
        }
    }
}

#[test]
fn census_inventory_is_sorted_exact_and_deduplicated() {
    let mut previous: Option<&str> = None;
    for entry in EXEMPTIONS {
        assert!(
            !entry.path.ends_with('/'),
            "inventory entry {} is a directory prefix — a subtree-wide exemption \
             is an unbounded hole; list the files",
            entry.path
        );
        assert!(
            entry.path.starts_with("crates/") && entry.path.ends_with(".rs"),
            "inventory entry {} is not a repo-relative Rust source path",
            entry.path
        );
        if let Some(previous) = previous {
            assert!(
                previous < entry.path,
                "inventory is not sorted / has a duplicate: {previous} then {}",
                entry.path
            );
        }
        previous = Some(entry.path);

        assert!(
            !entry.reason.trim().is_empty(),
            "inventory entry {} has no reason",
            entry.path
        );
        let mut previous_trigger: Option<&str> = None;
        for trigger in entry.triggers {
            assert!(
                !trigger.is_empty() && trigger.chars().all(is_trigger_name_char),
                "inventory entry {} pins {trigger:?}, which is not an uppercased \
                 trigger name in the shape trigger_sites yields",
                entry.path
            );
            if let Some(previous_trigger) = previous_trigger {
                assert!(
                    previous_trigger < *trigger,
                    "inventory entry {} pins triggers out of order or twice: \
                     {previous_trigger} then {trigger}",
                    entry.path
                );
            }
            previous_trigger = Some(trigger);
        }
        assert!(
            !entry.triggers.is_empty() || entry.unnamed_sites > 0,
            "inventory entry {} pins nothing — drop it",
            entry.path
        );
    }
}

#[test]
fn census_inventory_entries_still_exist_and_are_still_load_bearing() {
    let root = repo_root();
    for entry in EXEMPTIONS {
        let path = root.join(entry.path);
        assert!(
            path.is_file(),
            "inventory entry {} no longer exists — drop the entry",
            entry.path
        );
        let source = std::fs::read_to_string(&path).expect("read inventoried source");
        let code = strip_line_comments(&source);
        assert!(
            entry.proof.holds(entry.path, &code),
            "inventory entry {}'s proof {:?} no longer holds ({})",
            entry.path,
            entry.proof,
            entry.proof.requirement()
        );

        let sites = trigger_sites(&code);
        let found: Vec<String> = sites
            .iter()
            .filter_map(|site| match site {
                TriggerSite::Named(name) => Some(name.clone()),
                TriggerSite::Unnamed => None,
            })
            .collect();
        for trigger in entry.triggers {
            assert!(
                found.iter().any(|name| name == trigger),
                "inventory entry {} still pins {trigger}, which the file no \
                 longer mentions — drop the name so an exemption cannot outlive \
                 its reason",
                entry.path
            );
        }
        let unnamed = sites
            .iter()
            .filter(|site| **site == TriggerSite::Unnamed)
            .count();
        assert_eq!(
            unnamed, entry.unnamed_sites,
            "inventory entry {} pins {} unnamed site(s), the file has {unnamed}",
            entry.path, entry.unnamed_sites
        );
    }
}
