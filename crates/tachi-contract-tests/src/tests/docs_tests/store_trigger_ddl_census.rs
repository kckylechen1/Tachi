//! Source-level census: every trigger-DDL **site** in the workspace must be
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
//! The unit of analysis is a **trigger-DDL site**, identified by three things:
//!
//! 1. the file it lives in,
//! 2. the enclosing `fn` / `const` / `static` it lives under, and
//! 3. a digest of the DDL statement text itself.
//!
//! Every site in every workspace member (see [`scan_roots`]) must be pinned by
//! an [`Exemption`] entry below, and the entry pins how many times that exact
//! site occurs. A site the inventory does not pin is a RED; so is a site the
//! inventory pins that the file no longer contains.
//!
//! Keying on the site rather than on the trigger *name* is tachi#1443's third
//! gap: while entries pinned names, a second copy of an already-listed name —
//! installed in the same file through `store.connection()` — produced zero
//! findings, because the name was already allowed. A second copy now differs in
//! symbol, in digest, or in occurrence count, and surfaces as a new site.
//!
//! There is no co-occurrence predicate left to evade — in particular:
//!
//! * moving the SQL literal into a sibling module does not help: the literal is
//!   the anchor, and its new home has no entry;
//! * moving the `store.connection()` call into a helper does not help: the
//!   census never looked at where the connection call lives;
//! * writing one line of dead `Connection::open(…)` does not help: that used to
//!   be an accepted "proof" and is exactly the hole this revision closed.
//!
//! # Proof versus declaration
//!
//! An entry's [`ExemptionBasis`] is one of two visibly different things.
//!
//! * [`ExemptionBasis::Proven`] carries a [`MachineProof`] that the census
//!   re-derives from the exempted file on every run. A proof that stops holding
//!   **voids the whole entry**, so every site it pinned goes RED.
//! * [`ExemptionBasis::DeclaredByReviewerNotProven`] carries a signature and
//!   nothing else. It is an assertion by a person. It is checked by no machine
//!   and it never voids.
//!
//! The two must not wear the same clothes. Before tachi#1443's third round the
//! enum was called `ExemptionProof` and one of its variants,
//! `FileOpensAnUnguardedConnection`, was satisfied by the mere presence of the
//! text `Connection::open(` anywhere in the file. Any author could mint that
//! "proof" with one line of dead code, which makes it worse than no exemption
//! at all: it launders a declaration into something a reader takes for a proof.
//! It was deleted, and the twelve entries that rested on it were re-sorted into
//! the two categories above — eight of them into the declared one. That drop in
//! "proven" entries is not a regression; it is the hole becoming visible.
//!
//! # Honest limits
//!
//! * Eight entries below rest on a declaration, not a proof, and the declared
//!   `signed_by` strings record who asserted what and whether the enclosing test
//!   bodies were actually read. Four of the eight say `bodies NOT read`. A
//!   declaration is worth exactly what its signer is worth; that is the point of
//!   naming the variant after its weakness.
//! * [`MachineProof::NoStoreDoorwayInFile`] is the weaker of the two proofs and
//!   is minted by an *absence*. It cannot be minted the way its deleted
//!   predecessor could — a file that installs a trigger on `store.connection()`
//!   necessarily contains the doorway text, so the proof fails on exactly the
//!   #1411 shape — but a deliberate #1431-style split, with the doorway moved to
//!   a sibling module, does mint it. That residual is why it is documented as
//!   the weaker proof rather than removed: canonical schema DDL has to live
//!   somewhere, and it self-invalidates the day its file gains a doorway.
//! * The free-text `reason` is not machine-checked. The digests and the
//!   occurrence counts are the bound; the reason is what a reviewer reads.
//! * This file is skipped by the walk (`CENSUS_RELATIVE_PATH`). Its own trigger
//!   DDL lives in inert `&str` fixtures that are handed to [`observed_sites`],
//!   not to a database, and it is not textually distinguishable from executed
//!   DDL. That is one documented hole, not a general escape hatch, and it is the
//!   only path in the walk that is skipped rather than inventoried.
//! * Site extraction is textual. Trigger names assembled at runtime cannot be
//!   read, so such a site is pinned with an empty `trigger` and identified by
//!   its digest alone.
//! * Also textual, and not closed: DDL whose *keyword* is split so that
//!   `CREATE TRIGGER` never appears contiguously after normalization —
//!   `concat!("CREATE ", "TRIGGER …")`, `format!("{verb} TRIGGER …")`, a
//!   `/* … */` block comment (only `//` tails are stripped). Each is invisible
//!   here. Closing that class needs a real Rust parser in the test, which is a
//!   separate decision and deliberately not taken here. None of them is a step a
//!   fixture author takes by accident, which is the failure this census exists
//!   to catch; a deliberate evader is out of scope for a source scanner and is
//!   the reason `memcore`'s runtime `validate_persistent_trigger_inventory`
//!   exists as an independent fence.
//!
//! # What the digest deliberately ignores
//!
//! A digest that moved on unrelated edits would turn every commit into a census
//! failure, and a gate that cries wolf gets disabled. Before hashing, the
//! statement text has already been put through the same normalization the
//! scanner uses:
//!
//! * `//` comment tails are removed (quote-aware), so rewording a comment next
//!   to — or inside — the statement changes nothing;
//! * every run of whitespace collapses to a single space, so reindenting or
//!   reflowing the SQL changes nothing;
//! * backslashes are dropped, so re-splitting a Rust string literal across
//!   continuation lines changes nothing;
//! * the text is uppercased, so SQL keyword and identifier casing changes
//!   nothing.
//!
//! What it does *not* ignore: the statement's own tokens. Changing a table,
//! a `WHEN` clause, or a `RAISE` message changes the digest, and that is the
//! intent — the entry stops describing the site, and the census says so and
//! prints the replacement entry to paste.
//!
//! The hashed extent starts at the DDL keyword and stops at the first `;`
//! outside a `BEGIN … END` body, or at the first `"` (the end of the enclosing
//! Rust literal), whichever comes first, capped at
//! [`MAX_DDL_SLICE_CHARS`]. That is a deterministic extent, not a parse: it can
//! stop early on a statement that quotes the word `END` or a `"` character. Both
//! ends compute the same extent, so the digest stays stable; the cost of an
//! early stop is only that the digest covers less text.
//!
//! Drift direction throughout: every unknown resolves to RED. A missing entry,
//! a stale entry, a site the file no longer contains, a proof that stopped
//! holding, or an occurrence count that moved all fail the suite. None of them
//! can produce a false green.

use std::path::{Path, PathBuf};

/// Path of this file, skipped so the scanner does not flag its own fixtures.
/// See the "Honest limits" note in the module header — this is the one skip.
const CENSUS_RELATIVE_PATH: &str =
    "crates/tachi-contract-tests/src/tests/docs_tests/store_trigger_ddl_census.rs";

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

/// Arming the scoped schema-migration token. `authorize_schema_migration` is
/// `pub(crate)` to memcore (`memcore/src/db/open.rs:494`), which is why
/// [`MachineProof::MemcoreArmsTheMigrationToken`] is structurally unreachable
/// from `tachi-server` — the crate that produced all four #1411 / #1431
/// fixtures.
const SCHEMA_MIGRATION_TOKEN_NEEDLE: &str = "authorize_schema_migration";

/// Path prefix of the crate that owns the wall.
const MEMCORE_PATH_PREFIX: &str = "crates/memcore/";

/// Reported as a site's symbol when no enclosing item declaration precedes it.
const MODULE_SCOPE_SYMBOL: &str = "<module scope>";

/// Upper bound on the normalized characters a single site's digest covers.
/// Reached only if a statement has no terminator, which would mean the scan is
/// looking at something that is not a statement; the cap keeps the digest a
/// bounded function of the file instead of a function of everything after it.
const MAX_DDL_SLICE_CHARS: usize = 4096;

/// Workspace members deliberately kept out of the walk, each with the reason.
/// Empty is the correct state: silence about a member is what tachi#1443's
/// second gap was (`crates/` was hardcoded, so `tools/cleaner` — a member that
/// depends on memcore — was never scanned and nothing said so). Anything
/// dropped from the walk has to be dropped out loud, here.
/// `census_excluded_members_are_real_and_explained` refuses stale entries.
const EXCLUDED_WORKSPACE_MEMBERS: &[(&str, &str)] = &[];

/// A fact about the exempted file that the census re-derives from that file on
/// every run. The author of the exempted file cannot assert one of these into
/// being by writing prose; they either hold or they do not, and one that stops
/// holding voids its whole entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MachineProof {
    /// Checked: the file is under `crates/memcore/` **and** arms the scoped
    /// schema-migration token. The strongest lever in the file, because the
    /// token is `pub(crate)` to memcore: a `tachi-server` fixture cannot arm it
    /// without first being moved into another crate, which is not something an
    /// author does by accident and not something a reviewer misses.
    MemcoreArmsTheMigrationToken,
    /// Checked: the file names no store doorway at all
    /// ([`STORE_CONNECTION_NEEDLES`]), so no site in it reaches a guarded
    /// connection from this file.
    ///
    /// **The weaker proof — prefer the other one.** It keys on an *absence*,
    /// which the file's own author controls. What it still cannot be talked
    /// into: the #1411 shape, where the fixture installs its trigger on
    /// `store.connection()` in the same file, necessarily writes the doorway
    /// text and so fails this proof. What it can be talked into: the #1431
    /// shape, where the doorway sits in a sibling module. It is kept because
    /// canonical schema DDL has to live somewhere, and because it is
    /// self-invalidating — the day the file gains a `.connection()` call the
    /// proof stops holding, the entry voids, and every site in it must be
    /// re-justified.
    NoStoreDoorwayInFile,
}

impl MachineProof {
    fn holds(self, relative: &str, code: &str) -> bool {
        match self {
            Self::MemcoreArmsTheMigrationToken => {
                relative.starts_with(MEMCORE_PATH_PREFIX)
                    && code.contains(SCHEMA_MIGRATION_TOKEN_NEEDLE)
            }
            Self::NoStoreDoorwayInFile => !contains_any(code, STORE_CONNECTION_NEEDLES),
        }
    }

    fn requirement(self) -> &'static str {
        match self {
            Self::MemcoreArmsTheMigrationToken => {
                "the file must live under crates/memcore/ and call authorize_schema_migration"
            }
            Self::NoStoreDoorwayInFile => {
                "the file must not mention .connection() or .connection_mut()"
            }
        }
    }
}

/// Why an entry's sites are allowed. The variant names are load-bearing: a
/// reader must be able to tell, without following any indirection, whether
/// something was established or merely asserted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExemptionBasis {
    /// Established by the census itself, every run, from the exempted file.
    /// See [`MachineProof`].
    Proven(MachineProof),
    /// **Not a proof. An assertion.**
    ///
    /// Nothing here is checked by any machine. This variant says: a person
    /// looked at these sites and decided they are not the tachi#1443 mistake,
    /// and their name is on it. It is worth exactly what that person's reading
    /// was worth, and no more. Unlike [`ExemptionBasis::Proven`] it can never
    /// stop holding, because there is nothing to hold — it will keep excusing
    /// its sites until a human deletes it.
    ///
    /// Use it when no [`MachineProof`] applies. Do not invent a proof to avoid
    /// it; an honest declaration is the correct outcome, and it is deliberately
    /// spelled out at every use site so the weakness is visible in the diff.
    ///
    /// `signed_by` is free text ending in an ISO date
    /// (`census_declared_allowances_are_signed_and_dated`). State who is
    /// asserting this and, plainly, whether the enclosing test bodies were read.
    DeclaredByReviewerNotProven { signed_by: &'static str },
}

impl ExemptionBasis {
    /// A declaration always "holds" — there is nothing to check. That is the
    /// weakness, stated in code rather than in a comment.
    fn holds(self, relative: &str, code: &str) -> bool {
        match self {
            Self::Proven(proof) => proof.holds(relative, code),
            Self::DeclaredByReviewerNotProven { .. } => true,
        }
    }

    fn requirement(self) -> &'static str {
        match self {
            Self::Proven(proof) => proof.requirement(),
            Self::DeclaredByReviewerNotProven { .. } => {
                "a declaration is not machine-checkable and never voids"
            }
        }
    }

    fn signature(self) -> Option<&'static str> {
        match self {
            Self::Proven(_) => None,
            Self::DeclaredByReviewerNotProven { signed_by } => Some(signed_by),
        }
    }
}

/// One pinned trigger-DDL site inside an inventoried file.
///
/// The triple `(symbol, trigger, ddl)` is the key. `occurrences` pins how many
/// byte-identical copies of that site the file holds, so an exact duplicate is
/// still a new finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Site<'a> {
    /// Enclosing `fn` / `const` / `static` name, or [`MODULE_SCOPE_SYMBOL`].
    symbol: &'a str,
    /// Uppercased trigger name, or `""` when the name is assembled at runtime
    /// and cannot be read statically.
    trigger: &'a str,
    /// Digest of the normalized DDL statement — see the module header's
    /// "What the digest deliberately ignores".
    ddl: &'a str,
    /// How many identical sites the file holds under this key. Always >= 1.
    occurrences: usize,
}

impl Site<'_> {
    fn matches(&self, key: &SiteKey) -> bool {
        self.symbol == key.symbol && self.trigger == key.trigger && self.ddl == key.ddl
    }
}

/// One inventoried file.
#[derive(Debug, Clone, Copy)]
struct Exemption<'a> {
    /// Exact repo-relative path. Prefix entries are rejected by
    /// `census_inventory_is_sorted_exact_and_deduplicated`: a directory-wide
    /// exemption is an unbounded hole and this inventory does not have one.
    path: &'a str,
    basis: ExemptionBasis,
    /// Every site in the file, sorted by `(symbol, trigger, ddl)`, no
    /// duplicate keys.
    sites: &'a [Site<'a>],
    /// Why these sites are not fault injection through a store connection.
    /// NOT machine-checked — the digests and the counts are the bound; this is
    /// what a reviewer reads. State enclosing symbols, not adjectives, and say
    /// plainly whether the bodies were read.
    reason: &'a str,
}

/// Keep sorted by path.
const EXEMPTIONS: &[Exemption<'static>] = &[
    Exemption {
        path: "crates/memcore/src/db/memory_crud.rs",
        basis: ExemptionBasis::Proven(MachineProof::MemcoreArmsTheMigrationToken),
        sites: &[
            Site {
                symbol: "private_authorization_scopes_reset_after_database_errors",
                trigger: "AUTHORIZATION_LEAK",
                ddl: "3fc0f106e730ea0a",
                occurrences: 1,
            },
            Site {
                symbol: "private_authorization_scopes_reset_after_database_errors",
                trigger: "TYPED_SCOPE_ERROR",
                ddl: "798f6231f938e88f",
                occurrences: 1,
            },
            Site {
                symbol: "raw_auxiliary_trigger_cannot_chain_into_typed_search_write",
                trigger: "MALICIOUS_ACCESS_HISTORY_INSERT",
                ddl: "fee8cf3c70f389a2",
                occurrences: 1,
            },
            Site {
                symbol: "raw_connection_cannot_disable_reserved_reference_guards",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "2aec808021c3d3f0",
                occurrences: 1,
            },
            Site {
                symbol: "raw_connection_cannot_disable_reserved_reference_guards",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "47b624650f017587",
                occurrences: 2,
            },
            Site {
                symbol: "raw_connection_cannot_disable_reserved_reference_guards",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "d232a9fb5db0971e",
                occurrences: 2,
            },
            Site {
                symbol: "raw_trigger_drop_is_denied_for_arbitrary_main_and_temp_triggers",
                trigger: "MIXEDCASEAUXTRIGGER",
                ddl: "1935d8f3df73ed82",
                occurrences: 1,
            },
            Site {
                symbol: "raw_trigger_drop_is_denied_for_arbitrary_main_and_temp_triggers",
                trigger: "MIXEDCASEAUXTRIGGER",
                ddl: "ddbacc979109044d",
                occurrences: 1,
            },
            Site {
                symbol: "raw_trigger_drop_is_denied_for_arbitrary_main_and_temp_triggers",
                trigger: "TEMPAUXTRIGGER",
                ddl: "b3fb7a4857f54094",
                occurrences: 1,
            },
            Site {
                symbol: "raw_trigger_drop_is_denied_for_arbitrary_main_and_temp_triggers",
                trigger: "TEMPAUXTRIGGER",
                ddl: "fa414be6d72ac150",
                occurrences: 1,
            },
            Site {
                symbol: "raw_triggers_cannot_launder_typed_write_authority",
                trigger: "MALICIOUS_MEMORY_UPDATE",
                ddl: "07820a364a9417ef",
                occurrences: 1,
            },
            Site {
                symbol: "raw_triggers_cannot_launder_typed_write_authority",
                trigger: "MALICIOUS_MEMORY_UPDATE",
                ddl: "8507aa5b586fc0b9",
                occurrences: 1,
            },
            Site {
                symbol: "schema_migration_scope_allows_only_canonical_trigger_ddl",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "d232a9fb5db0971e",
                occurrences: 1,
            },
            Site {
                symbol: "schema_migration_scope_allows_only_canonical_trigger_ddl",
                trigger: "MIGRATION_DDL_BYPASS",
                ddl: "5bef73c316bf372f",
                occurrences: 1,
            },
            Site {
                symbol: "schema_migration_scope_allows_only_canonical_trigger_ddl",
                trigger: "TYPED_DML_DDL_BYPASS",
                ddl: "ecb413ba204a18cd",
                occurrences: 1,
            },
        ],
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
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "tachi#1443 census lane (agent), bodies NOT read 2026-07-26",
        },
        sites: &[
            Site {
                symbol:
                    "stamped_current_with_missing_search_generation_trigger_is_refused_without_repair",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "d232a9fb5db0971e",
                occurrences: 1,
            },
            Site {
                symbol: "stamped_v23_with_missing_guards_is_refused_even_with_migration_authority",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "bb3d35f8dd48927f",
                occurrences: 1,
            },
            Site {
                symbol: "stamped_v23_with_missing_guards_is_refused_even_with_migration_authority",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "07bdacf9fb6a3723",
                occurrences: 1,
            },
            Site {
                symbol: "v22_to_current_installs_reserved_reference_guards_and_scored_count",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "bb3d35f8dd48927f",
                occurrences: 1,
            },
            Site {
                symbol: "v22_to_current_installs_reserved_reference_guards_and_scored_count",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "07bdacf9fb6a3723",
                occurrences: 1,
            },
            Site {
                symbol: "v23_guard_install_failure_rolls_back_triggers_sentinel_and_stamp",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "bb3d35f8dd48927f",
                occurrences: 1,
            },
            Site {
                symbol: "v23_guard_install_failure_rolls_back_triggers_sentinel_and_stamp",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "07bdacf9fb6a3723",
                occurrences: 1,
            },
        ],
        reason: "v22-to-current migration tests; the pinned digests are the migration's \
                 own canonical DDL. Enclosing symbols: \
                 v22_to_current_installs_reserved_reference_guards_and_scored_count, \
                 v23_guard_install_failure_rolls_back_triggers_sentinel_and_stamp, \
                 stamped_v23_with_missing_guards_is_refused_even_with_migration_authority, \
                 stamped_current_with_missing_search_generation_trigger_is_refused_without_repair. \
                 Enclosing symbols extracted mechanically; bodies NOT read. This \
                 file names a store doorway and does not arm the migration \
                 token, so nothing here is machine-provable — it is a \
                 declaration.",
    },
    Exemption {
        path: "crates/memcore/src/db/open.rs",
        basis: ExemptionBasis::Proven(MachineProof::MemcoreArmsTheMigrationToken),
        sites: &[
            Site {
                symbol: "FAULT_TRIGGER_PERSISTENT",
                trigger: "WF1443_FAULT_PROBE",
                ddl: "423086a5aa70e980",
                occurrences: 1,
            },
            Site {
                symbol: "FAULT_TRIGGER_TEMP",
                trigger: "WF1443_FAULT_PROBE",
                ddl: "537e5a3b68ddb80c",
                occurrences: 1,
            },
            Site {
                symbol: "an_unguarded_second_connection_can_fail_a_real_store_write",
                trigger: "WF1443_FAIL_STATE_WRITE",
                ddl: "2dbc54ad46e9188b",
                occurrences: 1,
            },
            Site {
                symbol: "install_authority_row_guards",
                trigger: "TACHI_AUTHORITY_HARD_STATE_DELETE_GUARD",
                ddl: "e9f7d57e6ed05487",
                occurrences: 1,
            },
            Site {
                symbol: "install_authority_row_guards",
                trigger: "TACHI_AUTHORITY_HARD_STATE_INSERT_GUARD",
                ddl: "b197c7596cafdb3a",
                occurrences: 1,
            },
            Site {
                symbol: "install_authority_row_guards",
                trigger: "TACHI_AUTHORITY_HARD_STATE_UPDATE_GUARD",
                ddl: "088c65c0ac15a272",
                occurrences: 1,
            },
            Site {
                symbol: "install_authority_row_guards",
                trigger: "TACHI_CANONICAL_SUPERSESSION_EDGE_DELETE_GUARD",
                ddl: "46f26d0fed72be9c",
                occurrences: 1,
            },
            Site {
                symbol: "install_authority_row_guards",
                trigger: "TACHI_CANONICAL_SUPERSESSION_EDGE_INSERT_GUARD",
                ddl: "8c95df2eab0d1c3f",
                occurrences: 1,
            },
            Site {
                symbol: "install_authority_row_guards",
                trigger: "TACHI_CANONICAL_SUPERSESSION_EDGE_UPDATE_GUARD",
                ddl: "7d37af0a8da366de",
                occurrences: 1,
            },
            Site {
                symbol: "install_authority_row_guards",
                trigger: "TACHI_SUPERSESSION_EVENT_DELETE_GUARD",
                ddl: "9808dc1dc3f318ec",
                occurrences: 1,
            },
            Site {
                symbol: "install_authority_row_guards",
                trigger: "TACHI_SUPERSESSION_EVENT_INSERT_GUARD",
                ddl: "0ddbc17b53178011",
                occurrences: 1,
            },
            Site {
                symbol: "install_authority_row_guards",
                trigger: "TACHI_SUPERSESSION_EVENT_UPDATE_GUARD",
                ddl: "100f286dd78f4ed9",
                occurrences: 1,
            },
            Site {
                symbol: "install_ingest_stable_owner_fence",
                trigger: "INGEST_STABLE_OWNER_FENCE",
                ddl: "85cc694adcebac24",
                occurrences: 1,
            },
            Site {
                symbol: "install_ingest_stable_owner_fence",
                trigger: "INGEST_STABLE_OWNER_FENCE",
                ddl: "a9f625fe7f6cb0ea",
                occurrences: 1,
            },
            Site {
                symbol: "remove_ingest_stable_owner_fence",
                trigger: "INGEST_STABLE_OWNER_FENCE",
                ddl: "85cc694adcebac24",
                occurrences: 1,
            },
            Site {
                symbol: "scoped_owner_fence_cleans_and_relocks_after_action_error",
                trigger: "INGEST_STABLE_OWNER_FENCE",
                ddl: "80cdae411ed81731",
                occurrences: 1,
            },
            Site {
                symbol: "scoped_owner_fence_keeps_temp_ddl_private_and_cleans_after_success",
                trigger: "INGEST_STABLE_OWNER_FENCE",
                ddl: "76fdc7829814afb4",
                occurrences: 1,
            },
        ],
        reason: "the wall's own module. The nine install_authority_row_guards \
                 sites are the byte-exact TEMP triggers admitted only while \
                 the internal typed-DML or canonical-edge token is armed; bodies \
                 read 2026-08-17. \
                 INGEST_STABLE_OWNER_FENCE is the \
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
        basis: ExemptionBasis::Proven(MachineProof::NoStoreDoorwayInFile),
        sites: &[
            Site {
                symbol: "RESERVED_REFERENCE_GUARD_SQL",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "0afb6ce32fb11979",
                occurrences: 1,
            },
            Site {
                symbol: "RESERVED_REFERENCE_GUARD_SQL",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "bb3d35f8dd48927f",
                occurrences: 1,
            },
            Site {
                symbol: "RESERVED_REFERENCE_GUARD_SQL",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "07bdacf9fb6a3723",
                occurrences: 1,
            },
            Site {
                symbol: "RESERVED_REFERENCE_GUARD_SQL",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "d7422dea485ae00e",
                occurrences: 1,
            },
            Site {
                symbol: "RESERVED_REFERENCE_INSERT_TRIGGER_SQL",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "0afb81e32fb13d28",
                occurrences: 1,
            },
            Site {
                symbol: "RESERVED_REFERENCE_UPDATE_TRIGGER_SQL",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "d74214ea485ab593",
                occurrences: 1,
            },
        ],
        reason: "the canonical reserved-reference guard triggers as const DDL \
                 text; this file executes nothing. The weaker of the two proofs, \
                 on purpose — it holds only while this file names no store \
                 doorway, and voids itself the day one appears.",
    },
    Exemption {
        path: "crates/memcore/src/db/schema/migration_backup_tests.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "tachi#1443 census lane (agent), body NOT read 2026-07-26",
        },
        sites: &[
            Site {
                symbol: "seed_v22_fixture",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "90d32439353dd888",
                occurrences: 1,
            },
            Site {
                symbol: "seed_v22_fixture",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "dbf593b87f7b0e74",
                occurrences: 1,
            },
        ],
        reason: "seed_v22_fixture drops the canonical guards on a fixture \
                 database to manufacture a v22 shape. Enclosing symbol \
                 extracted mechanically; body NOT read. The file names a store \
                 doorway, so the no-doorway proof does not hold and this is a \
                 declaration.",
    },
    Exemption {
        path: "crates/memcore/src/db/search_generation.rs",
        basis: ExemptionBasis::Proven(MachineProof::MemcoreArmsTheMigrationToken),
        sites: &[
            Site {
                symbol: "GENERATION_SCHEMA_SQL",
                trigger: "MEMORY_ACCESS_SEARCH_GENERATION_AFTER_DELETE",
                ddl: "340dc41ab0ac4309",
                occurrences: 1,
            },
            Site {
                symbol: "GENERATION_SCHEMA_SQL",
                trigger: "MEMORY_ACCESS_SEARCH_GENERATION_AFTER_INSERT",
                ddl: "f679f34f65adb809",
                occurrences: 1,
            },
            Site {
                symbol: "GENERATION_SCHEMA_SQL",
                trigger: "MEMORY_ACCESS_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "247319cd75ecf49d",
                occurrences: 1,
            },
            Site {
                symbol: "GENERATION_SCHEMA_SQL",
                trigger: "MEMORY_EDGE_SEARCH_GENERATION_AFTER_DELETE",
                ddl: "3a8f9bb2587a6ee1",
                occurrences: 1,
            },
            Site {
                symbol: "GENERATION_SCHEMA_SQL",
                trigger: "MEMORY_EDGE_SEARCH_GENERATION_AFTER_INSERT",
                ddl: "5ffa4851db0ced95",
                occurrences: 1,
            },
            Site {
                symbol: "GENERATION_SCHEMA_SQL",
                trigger: "MEMORY_EDGE_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "56321119342ca80d",
                occurrences: 1,
            },
            Site {
                symbol: "GENERATION_SCHEMA_SQL",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_DELETE",
                ddl: "149dc807d99e4b9e",
                occurrences: 1,
            },
            Site {
                symbol: "GENERATION_SCHEMA_SQL",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_INSERT",
                ddl: "41639d22f6c14602",
                occurrences: 1,
            },
            Site {
                symbol: "GENERATION_SCHEMA_SQL",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "45058f250231c66a",
                occurrences: 1,
            },
            Site {
                symbol: "migrate_previous_memory_update_trigger",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "d232a9fb5db0971e",
                occurrences: 1,
            },
            Site {
                symbol: "missing_or_drifted_trigger_refuses_generation_read",
                trigger: "",
                ddl: "c5435d9d26bb4068",
                occurrences: 1,
            },
            Site {
                symbol: "missing_or_drifted_trigger_refuses_generation_read",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "d232a9fb5db0971e",
                occurrences: 1,
            },
            Site {
                symbol: "trigger_sql",
                trigger: "",
                ddl: "95545aefb7bebb4e",
                occurrences: 1,
            },
        ],
        reason: "the canonical search-generation triggers themselves, and the \
                 migration that replaces a drifted one, all under an armed \
                 migration token. The two name-less digests are trigger_sql's \
                 `CREATE TRIGGER {name}` builder and the literal \
                 `.expect(\"drop trigger\")` message in \
                 missing_or_drifted_trigger_refuses_generation_read, which \
                 uppercases into a keyword with no name after it. Both read \
                 2026-07-26.",
    },
    Exemption {
        path: "crates/memcore/src/db/tests/graph.rs",
        basis: ExemptionBasis::Proven(MachineProof::NoStoreDoorwayInFile),
        sites: &[
            Site {
                symbol: "confirmed_contradiction_transaction_rolls_back_at_every_side_effect_boundary",
                trigger: "FAIL_CONFIRMED_FIRST",
                ddl: "8a89cd01d4e45380",
                occurrences: 1,
            },
            Site {
                symbol: "confirmed_contradiction_transaction_rolls_back_at_every_side_effect_boundary",
                trigger: "FAIL_CONFIRMED_LIFECYCLE",
                ddl: "b1ebf1af945a77ea",
                occurrences: 1,
            },
            Site {
                symbol: "confirmed_contradiction_transaction_rolls_back_at_every_side_effect_boundary",
                trigger: "FAIL_CONFIRMED_SECOND",
                ddl: "88c5f7e5dcc60ddd",
                occurrences: 1,
            },
        ],
        reason: "confirmed_contradiction_transaction_rolls_back_at_every_side_effect_boundary \
                 installs each temp trigger on the direct rusqlite Connection \
                 returned by make_conn and executes the transaction on that same \
                 unguarded connection. Body read 2026-07-30. The proof remains \
                 valid only while this file names no MemoryStore doorway.",
    },
    Exemption {
        path: "crates/memcore/src/db/tests/model_catalog_ops.rs",
        basis: ExemptionBasis::Proven(MachineProof::NoStoreDoorwayInFile),
        sites: &[Site {
            symbol: "break_the_event_append",
            trigger: "REFUSE_EVENT_APPEND",
            ddl: "0aba31b541e6e02d",
            occurrences: 1,
        }],
        reason: "break_the_event_append installs its RAISE(ABORT) trigger on the \
                 bare in-memory rusqlite Connection returned by catalog_conn and \
                 the append-failure rollback and connection-state tests execute \
                 the store door on that same unguarded connection — #1443's \
                 sanctioned memcore pattern. Body read 2026-08-13. The proof \
                 remains valid only while this file names no MemoryStore doorway.",
    },
    Exemption {
        path: "crates/memcore/src/db/tests/search_generation.rs",
        basis: ExemptionBasis::Proven(MachineProof::MemcoreArmsTheMigrationToken),
        sites: &[
            Site {
                symbol: "known_previous_memory_update_trigger_migrates_to_all_column_coverage",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "dcadfb203310d3df",
                occurrences: 1,
            },
            Site {
                symbol: "known_previous_memory_update_trigger_migrates_to_all_column_coverage",
                trigger: "MEMORY_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "fd31dd35ddd478a9",
                occurrences: 1,
            },
            Site {
                symbol: "missing_trigger_refuses_search_generation",
                trigger: "",
                ddl: "c5435d9d26bb4068",
                occurrences: 1,
            },
            Site {
                symbol: "missing_trigger_refuses_search_generation",
                trigger: "MEMORY_EDGE_SEARCH_GENERATION_AFTER_UPDATE",
                ddl: "836f7494890a3bf8",
                occurrences: 1,
            },
        ],
        reason: "drops and recreates the canonical generation triggers under an \
                 armed migration token to exercise the migration path \
                 (missing_trigger_refuses_search_generation, \
                 known_previous_memory_update_trigger_migrates_to_all_column_coverage). \
                 The name-less digest is the same `.expect(\"drop trigger\")` \
                 message shape; read 2026-07-26.",
    },
    Exemption {
        path: "crates/memcore/src/db/tests/write_ops.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "Codex 5.5 local agent, body read 2026-07-30",
        },
        sites: &[
            Site {
                symbol: "enrichment_sql_failure_rolls_back_field_and_receipt_together",
                trigger: "FAIL_ENRICHMENT_FIELD_RECEIPT_CAS",
                ddl: "b134e245f67c7163",
                occurrences: 1,
            },
            Site {
                symbol: "typed_noncanonical_transition_archives_without_hard_delete",
                trigger: "NO_HARD_DELETE",
                ddl: "d4b5b7c7036c68da",
                occurrences: 1,
            },
        ],
        reason: "enrichment_sql_failure_rolls_back_field_and_receipt_together \
                 installs its trigger on the direct rusqlite Connection \
                 returned by make_conn, then drives update_enrichment_fields \
                 on that same unguarded connection. Body read 2026-07-30. \
                 typed_noncanonical_transition_archives_without_hard_delete \
                 installs its trigger on the direct rusqlite Connection \
                 returned by make_conn. Body read 2026-08-01. \
                 This file also contains unrelated MemoryStore doorway tests, \
                 so NoStoreDoorwayInFile cannot prove the exemption.",
    },
    Exemption {
        path: "crates/memcore/src/store/memory_lifecycle.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "tachi#1443 census lane (agent), body NOT read 2026-07-26",
        },
        sites: &[Site {
            symbol: "apply_rolls_back_memory_when_proposal_stamp_aborts",
            trigger: "ABORT_LIFECYCLE_PROPOSAL_STAMP",
            ddl: "38fc27464aa57215",
            occurrences: 1,
        }],
        reason: "apply_rolls_back_memory_when_proposal_stamp_aborts injects a \
                 rollback fault on memcore's raw second connection. Enclosing \
                 symbol extracted mechanically; body NOT read. The file also \
                 names a store doorway, so no proof in the enum holds and this \
                 is a declaration.",
    },
    Exemption {
        path: "crates/memcore/src/store/open.rs",
        basis: ExemptionBasis::Proven(MachineProof::MemcoreArmsTheMigrationToken),
        sites: &[
            Site {
                symbol: "authorized_v22_migration_installs_missing_reference_guards",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "90d32439353dd888",
                occurrences: 1,
            },
            Site {
                symbol: "authorized_v22_migration_installs_missing_reference_guards",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "dbf593b87f7b0e74",
                occurrences: 1,
            },
            Site {
                symbol: "existing_opens_refuse_missing_reserved_reference_guards_before_repair",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "90d32439353dd888",
                occurrences: 1,
            },
            Site {
                symbol: "existing_opens_refuse_missing_reserved_reference_guards_before_repair",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "dbf593b87f7b0e74",
                occurrences: 1,
            },
            Site {
                symbol: "open_refuses_unknown_or_spoofed_persistent_triggers",
                trigger: "MALICIOUS_ACCESS_CHAIN",
                ddl: "43ad5ed527d39e80",
                occurrences: 1,
            },
            Site {
                symbol: "open_refuses_unknown_or_spoofed_persistent_triggers",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "36f015c4c2878ed5",
                occurrences: 1,
            },
            Site {
                symbol: "open_refuses_unknown_or_spoofed_persistent_triggers",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "dbf593b87f7b0e74",
                occurrences: 1,
            },
            Site {
                symbol:
                    "read_only_existing_schema_compat_opens_stamped_older_without_write_authority",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "90d32439353dd888",
                occurrences: 1,
            },
            Site {
                symbol:
                    "read_only_existing_schema_compat_opens_stamped_older_without_write_authority",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "dbf593b87f7b0e74",
                occurrences: 1,
            },
            Site {
                symbol: "stamp_v22_without_v23_guards",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "bb3d35f8dd48927f",
                occurrences: 1,
            },
            Site {
                symbol: "stamp_v22_without_v23_guards",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "07bdacf9fb6a3723",
                occurrences: 1,
            },
        ],
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
        basis: ExemptionBasis::Proven(MachineProof::NoStoreDoorwayInFile),
        sites: &[Site {
            symbol: "vault_replace_api_key_pool_rolls_back_when_rotation_write_fails",
            trigger: "FAIL_POOL_ROTATION",
            ddl: "9ec39925504e73bc",
            occurrences: 1,
        }],
        reason: "vault_replace_api_key_pool_rolls_back_when_rotation_write_fails \
                 — memcore's raw second-connection idiom, the shape #1443's \
                 doorway doc points fixture authors at. Re-derived: this file \
                 names no store doorway at all, so the trigger cannot be \
                 reaching a guarded connection from here.",
    },
    Exemption {
        path: "crates/tachi-server/src/bootstrap/wiki_corpus.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "Codex Luna, bodies read 2026-08-01",
        },
        sites: &[
            Site {
                symbol: "maybe_inject_copy_after_receipt_prepared",
                trigger: "WIKI_CORPUS_NO_HARD_DELETE",
                ddl: "ffd56ed2b7788f70",
                occurrences: 1,
            },
            Site {
                symbol: "remove_memory_hard_delete_guard",
                trigger: "WIKI_CORPUS_NO_HARD_DELETE",
                ddl: "b59c6c444efa524c",
                occurrences: 1,
            },
        ],
        reason: "maybe_inject_copy_after_receipt_prepared installs the hard-delete \
                 sentinel through a second direct rusqlite::Connection::open(target_path), \
                 and remove_memory_hard_delete_guard removes it through a second \
                 direct rusqlite::Connection::open(path). These test-fixture sites \
                 use an unrestricted/file connection to install or remove the sentinel; \
                 they are not #1443 false-failure injection through guarded \
                 MemoryStore::connection. Bodies read 2026-08-01.",
    },
    Exemption {
        path: "crates/tachi-server/src/mcp_pool/proxy.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "tachi#1443 census lane (agent), bodies read 2026-07-26",
        },
        sites: &[
            Site {
                symbol: "failed_auto_ingest_does_not_replace_successful_mcp_result",
                trigger: "FAIL_PROXY_AUTO_INGEST_ROW",
                ddl: "943337696f6b7b43",
                occurrences: 1,
            },
            Site {
                symbol: "failed_auto_ingest_staging_is_visible_in_pipeline_status",
                trigger: "FAIL_PROXY_AUTO_INGEST_STAGE",
                ddl: "6c121fc727d3c6ed",
                occurrences: 1,
            },
        ],
        reason: "both installed inside with_unrestricted_fixture_connection on \
                 the server's global DB path; bodies read 2026-07-26 \
                 (failed_auto_ingest_does_not_replace_successful_mcp_result, \
                 failed_auto_ingest_staging_is_visible_in_pipeline_status). This \
                 is the sanctioned shape — but the file also names a store \
                 doorway, so nothing machine-checkable establishes it and this \
                 is a declaration.",
    },
    Exemption {
        path: "crates/tachi-server/src/pipeline_ops/audit.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "tachi#1443 census lane (agent), body read 2026-07-26",
        },
        sites: &[
            Site {
                symbol: "lost_heartbeat_fences_stale_graph_observation_after_takeover",
                trigger: "FAIL_INGEST_HEARTBEAT",
                ddl: "087fb80573a0e8da",
                occurrences: 1,
            },
            Site {
                symbol: "lost_heartbeat_fences_stale_graph_observation_after_takeover",
                trigger: "FAIL_INGEST_HEARTBEAT",
                ddl: "5567a2a03d22ca97",
                occurrences: 1,
            },
            Site {
                symbol: "lost_heartbeat_fences_stale_graph_observation_after_takeover",
                trigger: "MARK_HEARTBEAT_AFTER_JOIN",
                ddl: "8c1bd387cd98c4f6",
                occurrences: 1,
            },
        ],
        reason: "both installed through with_offline_global_fixture_connection \
                 (a local wrapper over the unguarded route) inside \
                 lost_heartbeat_fences_stale_graph_observation_after_takeover, \
                 and dropped again in the same test; body read 2026-07-26. The \
                 file also names a store doorway, so this is a declaration.",
    },
    Exemption {
        path: "crates/tachi-server/src/repair/tests/plan_c_restore.rs",
        basis: ExemptionBasis::Proven(MachineProof::NoStoreDoorwayInFile),
        sites: &[
            Site {
                symbol: "arm_memories_fts_delete_failure",
                trigger: "MEMORIES_FTS_FAIL_DELETE",
                ddl: "99f4dc6641aad4d7",
                occurrences: 1,
            },
            Site {
                symbol: "arm_memories_fts_update_failure",
                trigger: "MEMORIES_FTS_FAIL_UPDATE",
                ddl: "e91f547436ca6aea",
                occurrences: 1,
            },
            Site {
                symbol: "arm_symbolic_fts_delete_failure",
                trigger: "MEMORIES_SYMBOLIC_FTS_FAIL_DELETE",
                ddl: "93b4b5bf33a9b407",
                occurrences: 1,
            },
        ],
        reason: "FTS restore fault injection on an unguarded fixture connection. \
                 Re-derived: this file names no store doorway, so no site in it \
                 can reach a guarded connection from here. Enclosing symbols \
                 extracted mechanically; bodies NOT read.",
    },
    Exemption {
        path: "crates/tachi-server/src/repair/tests/quarantine_jobs_integrity.rs",
        basis: ExemptionBasis::Proven(MachineProof::NoStoreDoorwayInFile),
        sites: &[Site {
            symbol: "quarantine_purge_uses_default_deny_v23_connection",
            trigger: "QUARANTINE_PURGE_REQUIRES_DEFAULT_DENY_GUARD",
            ddl: "9ecc2dee2bc20b47",
            occurrences: 1,
        }],
        reason: "installs the quarantine default-deny guard on an unguarded \
                 fixture connection. Re-derived: this file names no store \
                 doorway. Body NOT read.",
    },
    Exemption {
        path: "crates/tachi-server/src/tests/memory_tests/save_policy/recall_cache_invalidation.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "tachi#1443 census lane (agent), body read 2026-07-26",
        },
        sites: &[Site {
            symbol:
                "missing_generation_trigger_bypasses_a_warm_cache_instead_of_serving_stale_rows",
            trigger: "MEMORY_SEARCH_GENERATION_AFTER_INSERT",
            ddl: "7866d9740159f9e6",
            occurrences: 1,
        }],
        reason: "missing_generation_trigger_bypasses_a_warm_cache_instead_of_serving_stale_rows \
                 drops the canonical insert trigger through \
                 with_unrestricted_fixture_connection to simulate drift; body \
                 read 2026-07-26. The file also names a store doorway, so this \
                 is a declaration.",
    },
    Exemption {
        path: "crates/tachi-server/src/tests/mod.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "tachi#1443 census lane (agent), body NOT read 2026-07-26",
        },
        sites: &[
            Site {
                symbol: "seed_pre_v23_wiki_reference_metadata",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "bb3d35f8dd48927f",
                occurrences: 1,
            },
            Site {
                symbol: "seed_pre_v23_wiki_reference_metadata",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "07bdacf9fb6a3723",
                occurrences: 1,
            },
        ],
        reason: "seed_pre_v23_wiki_reference_metadata drops the canonical guards \
                 on a fixture database to manufacture a pre-v23 shape. Enclosing \
                 symbol extracted mechanically; body NOT read. This is the \
                 fixture hub — it names a store doorway many times over, so it \
                 gets no proof and every site in it is a declaration.",
    },
    Exemption {
        path: "crates/tachi-server/src/tests/skill_tests/builtin_ingest/auto_ingest.rs",
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "tachi#1756 contract repair (Codex), bodies read 2026-08-14",
        },
        sites: &[
            Site {
                symbol: "admitted_auto_ingest_chunk_write_failure_is_partial_and_replayable",
                trigger: "FAIL_ADMITTED_CHUNK_WRITE",
                ddl: "83faca4f9eb4b9f1",
                occurrences: 1,
            },
            Site {
                symbol: "admitted_auto_ingest_chunk_write_failure_is_partial_and_replayable",
                trigger: "FAIL_ADMITTED_CHUNK_WRITE",
                ddl: "fdb0ef03dadcb089",
                occurrences: 1,
            },
            Site {
                symbol:
                    "admitted_auto_ingest_completion_receipt_failure_is_partial_and_replayable",
                trigger: "FAIL_ADMITTED_AUTO_INGEST_COMPLETION",
                ddl: "b92e6ecff09fdedc",
                occurrences: 1,
            },
            Site {
                symbol:
                    "admitted_auto_ingest_completion_receipt_failure_is_partial_and_replayable",
                trigger: "FAIL_ADMITTED_AUTO_INGEST_COMPLETION",
                ddl: "e8d2cf7827ea37f7",
                occurrences: 1,
            },
            Site {
                symbol: "admitted_auto_ingest_edge_failure_is_partial_without_duplicate_chunks",
                trigger: "FAIL_ADMITTED_EDGE_WRITE",
                ddl: "18e035a5d422f961",
                occurrences: 1,
            },
            Site {
                symbol: "admitted_auto_ingest_edge_failure_is_partial_without_duplicate_chunks",
                trigger: "FAIL_ADMITTED_EDGE_WRITE",
                ddl: "73a542736e95f78b",
                occurrences: 1,
            },
        ],
        reason: "the chunk-write, completion-receipt, and edge-write recovery tests each \
                 install and then drop their named fault trigger through \
                 with_unrestricted_fixture_connection, assert the typed partial stage, \
                 and prove replay completes without duplicate chunks. Bodies read \
                 2026-08-14. The file also names store doorways, so this sanctioned \
                 fault-injection shape is declared rather than machine-proven.",
    },
    Exemption {
        path: "crates/tachi-server/src/tests/wiki_tests/write/facade_routing/guide_metadata.rs",
        basis: ExemptionBasis::Proven(MachineProof::NoStoreDoorwayInFile),
        sites: &[
            Site {
                symbol: "v22_named_project_write_migrates_before_guide_pattern_and_reference_reads",
                trigger: "MEMORIES_RESERVED_REFS_INSERT_GUARD",
                ddl: "bb3d35f8dd48927f",
                occurrences: 1,
            },
            Site {
                symbol: "v22_named_project_write_migrates_before_guide_pattern_and_reference_reads",
                trigger: "MEMORIES_RESERVED_REFS_UPDATE_GUARD",
                ddl: "07bdacf9fb6a3723",
                occurrences: 1,
            },
        ],
        reason: "pre-v23 wiki fixture seeding on an unguarded connection. \
                 Re-derived: this file names no store doorway. Enclosing symbol \
                 extracted mechanically; body NOT read.",
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

fn exemption(relative: &str) -> Option<&'static Exemption<'static>> {
    EXEMPTIONS.iter().find(|entry| entry.path == relative)
}

// ---------------------------------------------------------------- scan roots

/// The `[workspace]` table's body, up to the next table header.
fn workspace_section(manifest: &str) -> &str {
    const HEADER: &str = "[workspace]\n";
    let start = if manifest.starts_with(HEADER) {
        HEADER.len()
    } else {
        let found = manifest
            .find(&format!("\n{}", HEADER))
            .expect("root Cargo.toml declares a [workspace] table");
        found + 1 + HEADER.len()
    };
    let rest = &manifest[start..];
    match rest.find("\n[") {
        Some(end) => &rest[..end + 1],
        None => rest,
    }
}

fn collect_quoted(fragment: &str, out: &mut Vec<String>) {
    let mut rest = fragment;
    while let Some(open_at) = rest.find('"') {
        let after = &rest[open_at + 1..];
        let Some(close_at) = after.find('"') else {
            return;
        };
        out.push(after[..close_at].to_string());
        rest = &after[close_at + 1..];
    }
}

/// Every path in the root manifest's `[workspace] members` array.
///
/// Hand-parsed on purpose: this test must not pull a TOML dependency into
/// tachi-server's dev-dependencies to read one array. The parse is deliberately
/// narrow — it reads only the `members` key of the `[workspace]` table, ignores
/// `#` comments, and is pinned by `census_scan_roots_come_from_workspace_membership`,
/// which asserts the two members that bracket the interesting cases
/// (`crates/tachi-server`, and `tools/cleaner` — the one outside `crates/`).
fn workspace_members(manifest: &str) -> Vec<String> {
    let mut members = Vec::new();
    let mut in_array = false;
    for raw in workspace_section(manifest).lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if in_array {
            collect_quoted(line, &mut members);
            if line.contains(']') {
                in_array = false;
            }
            continue;
        }
        let Some(after_key) = line.strip_prefix("members") else {
            continue;
        };
        let Some(after_eq) = after_key.trim_start().strip_prefix('=') else {
            continue;
        };
        let tail = after_eq.trim_start();
        if !tail.starts_with('[') {
            continue;
        }
        in_array = true;
        collect_quoted(tail, &mut members);
        if tail.contains(']') {
            in_array = false;
        }
    }
    members
}

/// The directories the census walks: every workspace member that is not on
/// [`EXCLUDED_WORKSPACE_MEMBERS`]. Derived, not hardcoded — tachi#1443's second
/// gap was `root.join("crates")`, which silently skipped `tools/cleaner`.
fn scan_roots(root: &Path) -> Vec<String> {
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).expect("read root Cargo.toml");
    let mut roots: Vec<String> = workspace_members(&manifest)
        .into_iter()
        .filter(|member| {
            !EXCLUDED_WORKSPACE_MEMBERS
                .iter()
                .any(|(excluded, _)| *excluded == member.as_str())
        })
        .collect();
    roots.sort();
    roots.dedup();
    assert!(
        !roots.is_empty(),
        "no scan roots parsed out of the root Cargo.toml — the manifest parse \
         is broken, and a census that scans nothing passes everything"
    );
    roots
}

fn rust_sources(dir: &Path, files: &mut Vec<PathBuf>) {
    // Loud on a missing scan root on purpose: a walk that silently skips a
    // workspace member is exactly tachi#1443's second gap.
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("read source directory {}: {error}", dir.display()));
    for entry in entries {
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

// ------------------------------------------------------------- normalization

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
/// Rust string-continuation lines still reads as one statement. Each output
/// character carries the zero-based index of the `code` line it came from, so a
/// site can be attributed to its enclosing symbol. Returned as characters
/// rather than bytes so the scanner can index without worrying about UTF-8
/// boundaries — uppercasing non-ASCII source text can widen a character.
fn normalize_sql(code: &str) -> Vec<(char, usize)> {
    let mut out: Vec<(char, usize)> = Vec::with_capacity(code.len());
    let mut pending_space = false;
    let mut line = 0_usize;
    for character in code.chars() {
        if character == '\n' {
            line += 1;
        }
        if character == '\\' {
            continue;
        }
        if character.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push((' ', line));
            pending_space = false;
        }
        out.extend(character.to_uppercase().map(|upper| (upper, line)));
    }
    out
}

fn matches_at(text: &[(char, usize)], index: usize, needle: &str) -> bool {
    let mut cursor = index;
    for expected in needle.chars() {
        match text.get(cursor) {
            Some((actual, _)) if *actual == expected => cursor += 1,
            _ => return false,
        }
    }
    true
}

fn is_trigger_name_char(character: char) -> bool {
    character.is_ascii_uppercase()
        || character.is_ascii_digit()
        || character == '_'
        || character == '.'
}

fn is_word_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

fn word_matches_at(text: &[(char, usize)], index: usize, word: &str) -> bool {
    if !matches_at(text, index, word) {
        return false;
    }
    if index > 0 && text.get(index - 1).is_some_and(|(c, _)| is_word_char(*c)) {
        return false;
    }
    let after = index + word.chars().count();
    !text.get(after).is_some_and(|(c, _)| is_word_char(*c))
}

// ------------------------------------------------------------------- digests

/// The normalized statement text a site's digest covers. Deterministic extent,
/// not a parse — see the module header's "What the digest deliberately
/// ignores".
fn ddl_statement(text: &[(char, usize)], start: usize) -> String {
    let mut out = String::new();
    let mut depth = 0_usize;
    let mut cursor = start;
    let limit = text.len().min(start + MAX_DDL_SLICE_CHARS);
    while cursor < limit {
        let character = text[cursor].0;
        if character == '"' && depth == 0 {
            break;
        }
        out.push(character);
        if character == ';' && depth == 0 {
            break;
        }
        if word_matches_at(text, cursor, "BEGIN") {
            depth += 1;
        } else if word_matches_at(text, cursor, "END") {
            depth = depth.saturating_sub(1);
        }
        cursor += 1;
    }
    out
}

/// FNV-1a, 64-bit. Hand-rolled on purpose: `DefaultHasher` is explicitly not
/// stable across Rust releases, and a digest pinned in source has to survive a
/// toolchain upgrade. FNV is also small enough to re-implement in any language,
/// which is what lets a reviewer check an entry without running this test.
fn fnv1a_hex(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

// ------------------------------------------------------------------- symbols

const VISIBILITY_PREFIXES: &[&str] = &["pub(crate) ", "pub(super) ", "pub(self) ", "pub "];
const ITEM_MODIFIERS: &[&str] = &[
    "default ",
    "async ",
    "unsafe ",
    "extern \"C\" ",
    "extern \"Rust\" ",
];
const ITEM_KEYWORDS: &[&str] = &["fn ", "const fn ", "const ", "static mut ", "static "];

fn take_identifier(rest: &str) -> String {
    rest.chars()
        .take_while(|character| character.is_alphanumeric() || *character == '_')
        .collect()
}

/// The item name a line declares, if it declares one.
fn declaration_name(line: &str) -> Option<String> {
    let mut text = line.trim_start();
    loop {
        let mut stripped = false;
        for prefix in VISIBILITY_PREFIXES.iter().chain(ITEM_MODIFIERS.iter()) {
            if let Some(rest) = text.strip_prefix(*prefix) {
                text = rest;
                stripped = true;
                break;
            }
        }
        if !stripped {
            break;
        }
    }
    for keyword in ITEM_KEYWORDS {
        if let Some(rest) = text.strip_prefix(*keyword) {
            let name = take_identifier(rest);
            return if name.is_empty() { None } else { Some(name) };
        }
    }
    None
}

/// Nearest preceding item declaration, textually. Not a scope analysis: a site
/// inside a nested `fn` is attributed to that nested `fn`, which is the answer
/// a reader wants anyway. Renaming the enclosing item moves the site, and the
/// census says so — that is intended, a rename is not an irrelevant edit.
fn enclosing_symbol(code_lines: &[&str], line_index: usize) -> String {
    if code_lines.is_empty() {
        return MODULE_SCOPE_SYMBOL.to_string();
    }
    let mut index = line_index.min(code_lines.len() - 1);
    loop {
        if let Some(name) = declaration_name(code_lines[index]) {
            return name;
        }
        if index == 0 {
            return MODULE_SCOPE_SYMBOL.to_string();
        }
        index -= 1;
    }
}

// --------------------------------------------------------------------- sites

/// The identity of one trigger-DDL site. Field order is the sort order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SiteKey {
    symbol: String,
    trigger: String,
    ddl: String,
}

impl SiteKey {
    fn describe(&self) -> String {
        if self.trigger.is_empty() {
            format!(
                "a runtime-assembled trigger name at `{}` (ddl {})",
                self.symbol, self.ddl
            )
        } else {
            format!(
                "trigger `{}` at `{}` (ddl {})",
                self.trigger, self.symbol, self.ddl
            )
        }
    }
}

/// Every trigger-DDL site in one file's comment-stripped code, in source order.
fn observed_sites(code: &str) -> Vec<SiteKey> {
    let text = normalize_sql(code);
    let code_lines: Vec<&str> = code.lines().collect();
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
        if text.get(cursor).map(|(c, _)| *c) == Some(' ') {
            cursor += 1;
        }
        for prefix in TRIGGER_NAME_PREFIXES {
            if matches_at(&text, cursor, prefix) {
                cursor += prefix.chars().count();
                break;
            }
        }
        let start = cursor;
        while text
            .get(cursor)
            .is_some_and(|(c, _)| is_trigger_name_char(*c))
        {
            cursor += 1;
        }
        let mut trigger: String = text[start..cursor].iter().map(|(c, _)| *c).collect();
        for qualifier in TRIGGER_SCHEMA_QUALIFIERS {
            if trigger.starts_with(qualifier) {
                trigger.drain(..qualifier.len());
                break;
            }
        }
        sites.push(SiteKey {
            symbol: enclosing_symbol(&code_lines, text[index].1),
            trigger,
            ddl: fnv1a_hex(&ddl_statement(&text, index)),
        });
        index += needle.chars().count();
    }
    sites
}

/// [`observed_sites`], sorted and folded into `(key, occurrences)` pairs.
fn counted_sites(code: &str) -> Vec<(SiteKey, usize)> {
    let mut keys = observed_sites(code);
    keys.sort();
    let mut out: Vec<(SiteKey, usize)> = Vec::new();
    for key in keys {
        match out.last_mut() {
            Some((last, count)) if *last == key => *count += 1,
            _ => out.push((key, 1)),
        }
    }
    out
}

fn pinned_from(counted: &[(SiteKey, usize)]) -> Vec<Site<'_>> {
    counted
        .iter()
        .map(|(key, count)| Site {
            symbol: &key.symbol,
            trigger: &key.trigger,
            ddl: &key.ddl,
            occurrences: *count,
        })
        .collect()
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

// ------------------------------------------------------------------ the rule

/// The whole rule, over one file. Factored out so it can be exercised on
/// fixtures — including fixtures paired with a hand-built [`Exemption`] —
/// rather than only on the live tree.
///
/// Returns one finding per problem. Empty means every trigger-DDL site in
/// `source` is pinned, with the right count, and the entry pins nothing that is
/// no longer there. Every failure path here adds findings; none removes them.
fn file_findings(relative: &str, source: &str, entry: Option<&Exemption<'_>>) -> Vec<String> {
    let code = strip_line_comments(source);
    let observed = counted_sites(&code);
    let mut findings = Vec::new();

    let Some(entry) = entry else {
        for (key, count) in &observed {
            findings.push(format!(
                "{relative}: {} x{count} is not inventoried",
                key.describe()
            ));
        }
        return findings;
    };

    if !entry.basis.holds(relative, &code) {
        // Fail closed: a basis that stopped holding voids the entry outright
        // rather than degrading it, so every site it pinned is uncovered.
        findings.push(format!(
            "{relative}: inventory basis {:?} no longer holds ({}) — the entry is void",
            entry.basis,
            entry.basis.requirement()
        ));
        for (key, count) in &observed {
            findings.push(format!(
                "{relative}: {} x{count} is covered only by the voided entry",
                key.describe()
            ));
        }
        return findings;
    }

    for (key, count) in &observed {
        match entry.sites.iter().find(|site| site.matches(key)) {
            None => findings.push(format!(
                "{relative}: {} x{count} is a NEW site — no inventory entry pins this \
                 (symbol, trigger, ddl)",
                key.describe()
            )),
            Some(site) if site.occurrences != *count => findings.push(format!(
                "{relative}: {} occurs {count} time(s), the inventory pins {}",
                key.describe(),
                site.occurrences
            )),
            Some(_) => {}
        }
    }

    for site in entry.sites {
        if !observed.iter().any(|(key, _)| site.matches(key)) {
            let named = if site.trigger.is_empty() {
                "a runtime-assembled trigger name".to_string()
            } else {
                format!("trigger `{}`", site.trigger)
            };
            findings.push(format!(
                "{relative}: the inventory still pins {named} at `{}` (ddl {}), which this \
                 file no longer contains — drop the pin so an exemption cannot outlive its site",
                site.symbol, site.ddl
            ));
        }
    }

    findings
}

/// The exact inventory text for a file's current sites. Printed with every
/// failure: a guard that reports drift without handing over the replacement
/// gets pasted over with `#[ignore]` the third time it fires.
fn remedy(relative: &str, source: &str, inventoried: bool) -> String {
    let counted = counted_sites(&strip_line_comments(source));
    let mut block = String::new();
    for (key, count) in &counted {
        block.push_str(&format!(
            "            Site {{ symbol: {:?}, trigger: {:?}, ddl: {:?}, occurrences: {count} }},\n",
            key.symbol, key.trigger, key.ddl
        ));
    }
    if inventoried {
        format!(
            "{relative} — replace this entry's `sites:` list with:\n\
             \x20       sites: &[\n{block}        ],\n"
        )
    } else {
        format!(
            "{relative} — no inventory entry exists. Paste this into EXEMPTIONS (keep it \
             sorted by path) and fill in the reason:\n\
             \x20   Exemption {{\n\
             \x20       path: {relative:?},\n\
             \x20       basis: ExemptionBasis::DeclaredByReviewerNotProven {{\n\
             \x20           signed_by: \"<your name>, bodies read/NOT read <YYYY-MM-DD>\",\n\
             \x20       }},\n\
             \x20       sites: &[\n{block}        ],\n\
             \x20       reason: \"<why these sites are not #1443 fault injection through a \
             store connection>\",\n\
             \x20   }},\n"
        )
    }
}

#[derive(Debug, Default)]
struct CensusReport {
    findings: Vec<String>,
    remedies: Vec<String>,
}

fn run_census(root: &Path) -> CensusReport {
    let mut files = Vec::new();
    for scan_root in scan_roots(root) {
        rust_sources(&root.join(&scan_root), &mut files);
    }
    files.sort();
    files.dedup();

    let mut scanned = 0_usize;
    let mut report = CensusReport::default();
    for path in &files {
        let relative = relative_path(root, path);
        if relative == CENSUS_RELATIVE_PATH {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        scanned += 1;
        let entry = exemption(&relative);
        let findings = file_findings(&relative, &source, entry);
        if !findings.is_empty() {
            report
                .remedies
                .push(remedy(&relative, &source, entry.is_some()));
            report.findings.extend(findings);
        }
    }

    assert!(
        scanned > 100,
        "census scanned only {scanned} Rust sources — the walker is broken, \
         not the tree"
    );
    report
}

#[test]
fn store_connection_trigger_ddl_census() {
    let report = run_census(&repo_root());
    if report.findings.is_empty() {
        return;
    }

    let mut message = String::from("uninventoried or drifted trigger DDL:\n");
    for finding in &report.findings {
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
         Once the trigger really is installed on an unguarded connection, pin \
         its SITE below. Note what is NOT accepted: writing `Connection::open(` \
         somewhere in the file. That used to satisfy a proof and no longer \
         does. If no MachineProof applies, say so with \
         ExemptionBasis::DeclaredByReviewerNotProven and sign it — an honest \
         declaration is the correct answer, an invented proof is not.\n\n\
         Exact replacement inventory text:\n\n",
    );
    for block in &report.remedies {
        message.push_str(block);
        message.push('\n');
    }
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
    let findings = file_findings("crates/tachi-server/src/fixture.rs", source, None);
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
    let findings = file_findings("crates/tachi-server/src/fixture.rs", source, None);
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
    let findings = file_findings("crates/tachi-server/src/fixture_sql.rs", sql_half, None);
    assert_eq!(
        findings.len(),
        1,
        "a lone SQL literal with no store connection in sight must still be \
         flagged, or the split-file refactor buys silence: {findings:?}"
    );
    assert!(
        findings[0].contains("LOADOUT_FAILURE_TRIGGER"),
        "the finding must name the enclosing const, not just the file: {findings:?}"
    );
}

#[test]
fn a_dead_unguarded_connection_line_cannot_mint_an_exemption() {
    // tachi#1443's first surviving gap. `ExemptionProof::FileOpensAnUnguardedConnection`
    // was satisfied by the text `Connection::open(` appearing anywhere in the
    // file, so this one dead line "proved" the exemption for the #1411 fixture
    // sitting right underneath it. The variant is gone; the only way to silence
    // this file now is a signed declaration that says so in its own name.
    let source = r#"
        fn nearly_the_1411_shape(store: &memcore::MemoryStore) {
            let _ = rusqlite::Connection::open("/dev/null");
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
    let relative = "crates/tachi-server/src/fixture.rs";
    let counted = counted_sites(&strip_line_comments(source));
    let pins = pinned_from(&counted);

    assert_eq!(
        file_findings(relative, source, None).len(),
        1,
        "the dead-code line must not make the site invisible"
    );

    for proof in [
        MachineProof::NoStoreDoorwayInFile,
        MachineProof::MemcoreArmsTheMigrationToken,
    ] {
        let entry = Exemption {
            path: relative,
            basis: ExemptionBasis::Proven(proof),
            sites: &pins,
            reason: "claims a proof this source does not establish",
        };
        let findings = file_findings(relative, source, Some(&entry));
        assert!(
            findings.iter().any(|finding| finding.contains("is void")),
            "no MachineProof may be mintable by this source ({proof:?}): {findings:?}"
        );
    }

    let declared = ExemptionBasis::DeclaredByReviewerNotProven {
        signed_by: "a person who has to type their own name 2026-07-26",
    };
    let entry = Exemption {
        path: relative,
        basis: declared,
        sites: &pins,
        reason: "asserted, not proven",
    };
    assert!(
        file_findings(relative, source, Some(&entry)).is_empty(),
        "an honest declaration must still be able to exempt a site — otherwise \
         authors go back to inventing proofs"
    );
    assert!(
        format!("{declared:?}").contains("Declared"),
        "a declaration must be visibly a declaration wherever it is printed"
    );
}

#[test]
fn a_voided_basis_covers_nothing_it_previously_covered() {
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
    let relative = "crates/tachi-server/src/fixture.rs";
    let counted = counted_sites(&strip_line_comments(source));
    let pins = pinned_from(&counted);
    let entry = Exemption {
        path: relative,
        basis: ExemptionBasis::Proven(MachineProof::NoStoreDoorwayInFile),
        sites: &pins,
        reason: "claims an absence this source contradicts",
    };
    let findings = file_findings(relative, source, Some(&entry));
    assert!(
        findings.iter().any(|f| f.contains("is void")),
        "a failed proof must void the entry loudly: {findings:?}"
    );
    assert!(
        findings
            .iter()
            .any(|f| f.contains("FAIL_SECOND_ACCESS_COUNT_TOUCH")),
        "a voided entry must stop covering its pinned sites: {findings:?}"
    );
}

#[test]
fn memcore_only_proof_is_unreachable_from_tachi_server() {
    // authorize_schema_migration is pub(crate) to memcore. A tachi-server
    // fixture cannot arm the token, so it cannot claim this proof even by
    // writing the identifier into a comment-stripped line.
    let code = "let _ = crate::db::authorize_schema_migration(&flag);";
    assert!(
        MachineProof::MemcoreArmsTheMigrationToken.holds("crates/memcore/src/db/tests/x.rs", code),
        "memcore must be able to claim the strongest proof"
    );
    assert!(
        !MachineProof::MemcoreArmsTheMigrationToken
            .holds("crates/tachi-server/src/tests/x.rs", code),
        "the crate that produced every #1411/#1431 fixture must never reach the \
         migration-token proof"
    );
}

#[test]
fn no_store_doorway_proof_self_invalidates_when_a_doorway_appears() {
    assert!(
        MachineProof::NoStoreDoorwayInFile.holds(
            "crates/memcore/src/db/schema/ddl.rs",
            "const GUARD: &str = \"CREATE TRIGGER x AFTER UPDATE ON t BEGIN SELECT 1; END;\";"
        ),
        "const DDL text with no doorway must satisfy the weaker proof"
    );
    assert!(
        !MachineProof::NoStoreDoorwayInFile.holds(
            "crates/memcore/src/db/schema/ddl.rs",
            "let _ = store.connection();"
        ),
        "the weaker proof must stop holding the moment the file gains a doorway"
    );
}

#[test]
fn a_second_copy_of_a_pinned_site_is_a_new_finding() {
    // tachi#1443's third surviving gap. While entries pinned trigger *names*,
    // installing a second copy of an already-listed name inside an already
    // registered file produced zero findings. The count is part of the pin now.
    let source = r#"
        fn arm(store: &memcore::MemoryStore) {
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
    let relative = "crates/tachi-server/src/fixture.rs";
    let counted = counted_sites(&strip_line_comments(source));
    let pins = pinned_from(&counted);
    let entry = Exemption {
        path: relative,
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "fixture 2026-07-26",
        },
        sites: &pins,
        reason: "fixture",
    };
    assert!(
        file_findings(relative, source, Some(&entry)).is_empty(),
        "the pinned site must be covered"
    );

    let doubled = format!("{source}{source}");
    let findings = file_findings(relative, &doubled, Some(&entry));
    assert_eq!(
        findings.len(),
        1,
        "a byte-identical second copy of a pinned site must be a finding: {findings:?}"
    );
    assert!(
        findings[0].contains("occurs 2 time(s)"),
        "the finding must say what moved: {findings:?}"
    );
}

#[test]
fn the_same_trigger_under_a_different_symbol_is_a_new_site() {
    let first = r#"
        fn arm_one(connection: &rusqlite::Connection) {
            connection
                .execute_batch(
                    "CREATE TRIGGER fail_ingest_success_audit \
                     BEFORE INSERT ON audit_log \
                     BEGIN SELECT RAISE(FAIL, 'boom'); END;",
                )
                .unwrap();
        }
    "#;
    let relative = "crates/tachi-server/src/fixture.rs";
    let counted = counted_sites(&strip_line_comments(first));
    let pins = pinned_from(&counted);
    let entry = Exemption {
        path: relative,
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "fixture 2026-07-26",
        },
        sites: &pins,
        reason: "fixture",
    };
    assert!(
        file_findings(relative, first, Some(&entry)).is_empty(),
        "the pinned site must be covered"
    );

    let second = first.replace("arm_one", "arm_two");
    let findings = file_findings(relative, &second, Some(&entry));
    assert_eq!(
        findings.len(),
        2,
        "moving a pinned site to another symbol must report both the new site \
         and the stale pin: {findings:?}"
    );
    assert!(
        findings.iter().any(|f| f.contains("is a NEW site")),
        "the new home must be reported: {findings:?}"
    );
    assert!(
        findings.iter().any(|f| f.contains("no longer contains")),
        "the abandoned pin must be reported: {findings:?}"
    );
}

#[test]
fn site_digests_ignore_whitespace_indentation_and_comment_edits() {
    // If a digest moved on unrelated edits, every commit would turn the census
    // red and people would start rubber-stamping it.
    let original = r#"
        fn arm(connection: &rusqlite::Connection) {
            // arms the fixture failure
            connection
                .execute_batch(
                    "CREATE TRIGGER fail_ingest_success_audit \
                     BEFORE INSERT ON audit_log \
                     BEGIN SELECT RAISE(FAIL, 'boom'); END;",
                )
                .unwrap();
        }
    "#;
    let reworded = r#"
        fn arm(connection: &rusqlite::Connection) {
            // reworded comment that mentions CREATE TRIGGER and a store.connection()
            connection
                .execute_batch(
                        "CREATE   TRIGGER fail_ingest_success_audit BEFORE INSERT ON audit_log \
                             BEGIN   SELECT RAISE(FAIL, 'boom'); END;",
                )
                .unwrap();
        }
    "#;
    let before = counted_sites(&strip_line_comments(original));
    let after = counted_sites(&strip_line_comments(reworded));
    assert_eq!(
        before, after,
        "reindenting the SQL and rewording a comment must not move the digest"
    );

    let changed = original.replace("audit_log", "processed_events");
    let changed = counted_sites(&strip_line_comments(&changed));
    assert_ne!(
        before, changed,
        "changing the statement's own tokens MUST move the digest"
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
        file_findings("crates/tachi-server/src/fixture.rs", source, None).is_empty(),
        "documentation quoting the rule must not trip the rule"
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
    let findings = file_findings("crates/tachi-server/src/fixture.rs", source, None);
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
fn census_pins_a_runtime_interpolated_trigger_name_by_digest() {
    // A name assembled at runtime cannot be pinned by name, so it is pinned by
    // digest with an empty trigger. One more of them is still a RED.
    let source = r#"
        fn build(name: &str) -> String {
            format!("CREATE TRIGGER {name} AFTER UPDATE ON memories BEGIN SELECT 1; END;")
        }
    "#;
    let relative = "crates/tachi-server/src/fixture.rs";
    let counted = counted_sites(&strip_line_comments(source));
    assert_eq!(counted.len(), 1, "one site expected: {counted:?}");
    assert!(
        counted[0].0.trigger.is_empty(),
        "an interpolated name must not be read as a trigger name: {counted:?}"
    );

    let pins = pinned_from(&counted);
    let entry = Exemption {
        path: relative,
        basis: ExemptionBasis::DeclaredByReviewerNotProven {
            signed_by: "fixture 2026-07-26",
        },
        sites: &pins,
        reason: "fixture",
    };
    assert!(
        file_findings(relative, source, Some(&entry)).is_empty(),
        "a pinned digest must cover the interpolated builder"
    );

    let doubled = format!("{source}{source}");
    let findings = file_findings(relative, &doubled, Some(&entry));
    assert_eq!(
        findings.len(),
        1,
        "a second interpolated site must break the pinned count: {findings:?}"
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
fn census_scan_roots_come_from_workspace_membership() {
    // tachi#1443's second surviving gap: the walk was hardcoded to `crates/`,
    // so `tools/cleaner` — a workspace member that depends on memcore — was
    // never scanned, and nothing in the census said so.
    let root = repo_root();
    let roots = scan_roots(&root);
    assert!(
        roots
            .iter()
            .any(|member| member.as_str() == "crates/tachi-server"),
        "the manifest parse lost crates/tachi-server: {roots:?}"
    );
    assert!(
        roots
            .iter()
            .any(|member| member.as_str() == "tools/cleaner"),
        "the walk must reach workspace members outside crates/: {roots:?}"
    );
    for member in &roots {
        assert!(
            root.join(member).is_dir(),
            "workspace member {member} is not a directory — the manifest parse is drifting"
        );
    }

    // Anything that lands under crates/ is a workspace member today. If that
    // stops being true, the walk would still cover it (members are the source
    // of truth) — but a crate outside the workspace is a surprise worth a RED.
    let all_members = workspace_members(
        &std::fs::read_to_string(root.join("Cargo.toml")).expect("read root Cargo.toml"),
    );
    for entry in std::fs::read_dir(root.join("crates")).expect("read crates/") {
        let entry = entry.expect("read crates/ entry");
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let relative = format!("crates/{name}");
        assert!(
            all_members.contains(&relative),
            "{relative} is not a workspace member — either add it to the root \
             Cargo.toml or the census's scan roots stop covering it"
        );
    }
}

#[test]
fn census_excluded_members_are_real_and_explained() {
    let root = repo_root();
    let members = workspace_members(
        &std::fs::read_to_string(root.join("Cargo.toml")).expect("read root Cargo.toml"),
    );
    for (excluded, why) in EXCLUDED_WORKSPACE_MEMBERS {
        assert!(
            members.iter().any(|member| member.as_str() == *excluded),
            "excluded scan root {excluded} is not a workspace member — drop the \
             stale exclusion instead of leaving a hole nobody can see"
        );
        assert!(
            !why.trim().is_empty(),
            "excluded scan root {excluded} has no reason"
        );
    }
}

#[test]
fn census_declared_allowances_are_signed_and_dated() {
    for entry in EXEMPTIONS {
        let Some(signature) = entry.basis.signature() else {
            continue;
        };
        assert!(
            signature.trim().len() > 10,
            "inventory entry {} is declared but not signed — a declaration is \
             worth what its signer is worth, so it has to have one",
            entry.path
        );
        let tail: Vec<char> = signature.chars().rev().take(10).collect();
        let date: String = tail.into_iter().rev().collect();
        let shape: Vec<bool> = date.chars().map(|c| c.is_ascii_digit()).collect();
        assert_eq!(
            shape,
            vec![true, true, true, true, false, true, true, false, true, true],
            "inventory entry {}'s signature must end in an ISO date so a reader \
             can see how old the assertion is; got {signature:?}",
            entry.path
        );
    }
}

#[test]
fn census_inventory_is_sorted_exact_and_deduplicated() {
    let roots = scan_roots(&repo_root());
    let mut previous: Option<&str> = None;
    for entry in EXEMPTIONS {
        assert!(
            !entry.path.ends_with('/'),
            "inventory entry {} is a directory prefix — a subtree-wide exemption \
             is an unbounded hole; list the files",
            entry.path
        );
        assert!(
            entry.path.ends_with(".rs")
                && roots
                    .iter()
                    .any(|root| entry.path.starts_with(&format!("{root}/"))),
            "inventory entry {} is not a Rust source inside a scanned workspace \
             member — it would never be reached by the walk",
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
        assert!(
            !entry.sites.is_empty(),
            "inventory entry {} pins nothing — drop it",
            entry.path
        );

        let mut previous_site: Option<&Site<'_>> = None;
        for site in entry.sites {
            assert!(
                site.occurrences >= 1,
                "inventory entry {} pins a site with no occurrences",
                entry.path
            );
            assert!(
                site.ddl.len() == 16 && site.ddl.chars().all(|c| c.is_ascii_hexdigit()),
                "inventory entry {} pins {:?}, which is not a 16-digit FNV-1a digest",
                entry.path,
                site.ddl
            );
            assert!(
                site.trigger.is_empty() || site.trigger.chars().all(is_trigger_name_char),
                "inventory entry {} pins {:?}, which is not an uppercased trigger \
                 name in the shape observed_sites yields",
                entry.path,
                site.trigger
            );
            if let Some(previous_site) = previous_site {
                let before = (
                    previous_site.symbol,
                    previous_site.trigger,
                    previous_site.ddl,
                );
                let now = (site.symbol, site.trigger, site.ddl);
                assert!(
                    before < now,
                    "inventory entry {} pins sites out of order or twice: \
                     {before:?} then {now:?}",
                    entry.path
                );
            }
            previous_site = Some(site);
        }
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
        let findings = file_findings(entry.path, &source, Some(entry));
        assert!(
            findings.is_empty(),
            "inventory entry {} no longer describes its file: {findings:?}",
            entry.path
        );
    }
}
