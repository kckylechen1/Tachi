//! Certification receipts + the vendor-version probe (#894 S2d, invariant 5).
//!
//! # Why a receipt exists at all
//!
//! The kill-test that certifies a provider ([`crate::authority`] invariant 5)
//! spawns a real vendor binary, needs real credentials, and takes ~60s. It
//! cannot run inside an ordinary `cargo test` — a CI box with no `codex` install
//! would either fail the suite or, far worse, "pass" it without exercising
//! anything. So the kill-test stays `#[ignore]`d and certification becomes what
//! it actually is: **an out-of-band event with a checked-in, machine-verified
//! receipt**.
//!
//! A [`CertificationReceipt`] is a statement about **one execution** of **one
//! kill-test** against **one vendor binary version** on **one OS**, covering
//! **one mutation matrix**. It is not a statement about the vendor, about
//! "sandboxes" in general, or about any other version of the same binary. Round
//! 2's ratchet said "a `KillTested` row may not point at an `#[ignore]`d test";
//! this replaces it with the stronger, truer one:
//!
//! > A row may claim `KillTested` **iff** a receipt exists that (a) passed, (b)
//! > names an existing kill-test, (c) covers the level being asked for, and (d)
//! > matches the version of the vendor binary that is actually installed.
//!
//! (d) is the part that has teeth at runtime. `codex --version` is probed before
//! spawn ([`probe_backend_version`]); a version the receipt does not cover —
//! including a version we could not determine at all — makes the row behave
//! exactly like [`crate::authority::Certification::Unverified`]: the read-only
//! lane fails closed. Provider conformance does not carry across vendor
//! versions, and an unknown version is not a certified version.
//!
//! # The artifact of record
//!
//! `crates/tachi-dispatch/certifications/*.toml` is the human-auditable receipt
//! (who ran it, when, on what binary, which mutations, which git blob of the
//! test). [`CODEX_CLI_RECEIPT`] below is its Rust mirror — the qualification
//! table points at the const, so certification is compiled into the binary and
//! cannot go missing on a deployed host. `receipt_const_matches_the_checked_in_receipt_file`
//! fails the build if the two ever disagree, field for field (the #873
//! single-source + embed + parity-test pattern).

use crate::authority::{version_components, TransportKind, WorkspaceAuthority};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

/// The outcome of an executed kill-test. Only [`CertificationResult::Pass`]
/// certifies anything; a recorded `Fail` is kept deliberately expressible so a
/// regression can be *checked in* rather than silently deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertificationResult {
    Pass,
    Fail,
}

impl CertificationResult {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
        }
    }
}

/// One executed kill-test, recorded. Every field is a fact about a run that
/// happened, not a plan for one that might.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CertificationReceipt {
    /// Stable id, stamped into dispatch receipts so a reader can find the file.
    pub id: &'static str,
    /// The checked-in artifact of record this const mirrors.
    pub source_file: &'static str,
    pub backend: &'static str,
    pub transport: TransportKind,
    /// What `--version` calls itself (e.g. `codex-cli`).
    pub vendor_binary: &'static str,
    /// The EXACT version that was exercised. The runtime gate compares the
    /// installed binary against this and refuses on any mismatch.
    pub vendor_version: &'static str,
    pub host_os: &'static str,
    pub host_os_version: &'static str,
    /// Repo-relative path of the kill-test, and the test fn inside it.
    pub kill_test: &'static str,
    pub kill_test_fn: &'static str,
    pub result: CertificationResult,
    pub executed_at: &'static str,
    pub executed_by: &'static str,
    pub duration_secs: &'static str,
    /// The commit whose kill-test source was executed, and that source's git
    /// blob — so "what exactly ran" is diffable, not a memory.
    pub executed_on_commit: &'static str,
    pub kill_test_source_blob: &'static str,
    /// The workspace-authority levels this execution certifies — and ONLY these.
    /// A level the run did not exercise is never certified by it.
    pub covers: &'static [WorkspaceAuthority],
    /// The mutation labels the sandbox was observed to refuse. Machine-coupled
    /// to the kill-test's own command table by
    /// `the_certified_matrix_matches_the_kill_tests_command_table`, so a matrix
    /// that grows without a re-run turns red instead of quietly over-claiming.
    pub matrix: &'static [&'static str],
}

impl CertificationReceipt {
    pub fn passed(self) -> bool {
        self.result == CertificationResult::Pass
    }

    /// Does this receipt attest the *installed* binary? `None` (we could not
    /// determine the version) is a NO — an unknown version is not a certified
    /// version (fail closed).
    pub fn certifies_version(self, installed: Option<&str>) -> bool {
        installed.is_some_and(|actual| versions_match(actual, self.vendor_version))
    }

    pub fn certifies_level(self, level: WorkspaceAuthority) -> bool {
        self.covers.contains(&level)
    }
}

/// codex CLI, read-only, macOS — the one certification that exists.
///
/// Mirrors `crates/tachi-dispatch/certifications/codex-cli.toml`. See that file
/// for the boundaries of the experiment; the short version is that this
/// certifies **codex-cli 0.144.1 on macOS at `read-only`, over the ten mutations
/// listed**, and nothing else.
pub const CODEX_CLI_RECEIPT: CertificationReceipt = CertificationReceipt {
    id: "codex-cli-0.144.1-macos-20260713",
    source_file: "crates/tachi-dispatch/certifications/codex-cli.toml",
    backend: "codex",
    transport: TransportKind::Cli,
    vendor_binary: "codex-cli",
    vendor_version: "0.144.1",
    host_os: "macos",
    host_os_version: "26.5.1",
    kill_test: "crates/tachi-dispatch/tests/codex_sandbox_kill_test.rs",
    kill_test_fn: "codex_read_only_sandbox_refuses_every_mutation_in_the_matrix",
    result: CertificationResult::Pass,
    executed_at: "2026-07-13",
    executed_by: "Oz",
    duration_secs: "60.71",
    executed_on_commit: "9b531012264db0b7bf0f50398ec7779b0bb13ffc",
    kill_test_source_blob: "c4df6aebfe9394f92f854c88f9c8f7a1b637b912",
    // Read-only ONLY: the run observed nothing about workspace-write containment.
    covers: &[WorkspaceAuthority::ReadOnly],
    matrix: CODEX_KILL_TEST_MATRIX,
};

/// The mutation labels [`CODEX_CLI_RECEIPT`] was issued for. Lives here (not in
/// the test) because it is part of the *claim*: the kill-test's command table is
/// checked against it on every ordinary `cargo test` run.
pub const CODEX_KILL_TEST_MATRIX: &[&str] = &[
    "create",
    "append",
    "truncate",
    "rename",
    "unlink",
    "chmod_then_write",
    "git_internals",
    "mounted_skill_write",
    "absolute_path_outside_worktree",
    "descendant_process_write",
];

/// Every receipt that ships. Used by the ratchet tests; the qualification table
/// references the consts directly.
pub const RECEIPTS: &[&CertificationReceipt] = &[&CODEX_CLI_RECEIPT];

// ─── Version comparison ──────────────────────────────────────────────────────

/// Do these two version strings name the same release? Compared numerically by
/// component (`0.144.1` == `v0.144.1`), never lexically — and an unparseable
/// version only matches itself, exactly.
pub fn versions_match(actual: &str, certified: &str) -> bool {
    match (version_components(actual), version_components(certified)) {
        (Some(actual), Some(certified)) => actual == certified,
        _ => actual.trim().eq_ignore_ascii_case(certified.trim()),
    }
}

/// Pull the version token out of a `--version` line: `codex-cli 0.144.1` ->
/// `0.144.1`. Requires at least two dotted numeric components, so a stray `1` or
/// a binary name never passes for a version.
pub fn parse_version_output(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .find(|token| version_components(token).is_some_and(|parts| parts.len() >= 2))
        .map(|token| token.trim_start_matches('v').to_string())
}

// ─── The runtime version gate ────────────────────────────────────────────────

/// What the installed vendor binary reports, or `None` when we could not find
/// out (not on `PATH`, non-zero exit, unparseable output, or it hung).
///
/// `None` is honest ignorance and it **fails closed**: `qualify_provider` refuses
/// to hand back a certified row for a version it cannot confirm, so a read-only
/// dispatch is refused rather than run against an unknown binary that may not
/// enforce anything.
///
/// Called once per dispatch, on the pre-spawn path, and only for providers that
/// even have a sandbox primitive (there is nothing to gate otherwise). The result
/// is cached per binary *identity* — path + mtime + length — so upgrading codex
/// under a running daemon invalidates the cache instead of certifying the new
/// binary with the old binary's answer.
pub fn probe_backend_version(backend: &str) -> Option<String> {
    let program = which(backend)?;
    let identity = binary_identity(&program);

    let cache = version_cache();
    if let Ok(cache) = cache.lock() {
        if let Some((cached_identity, version)) = cache.get(backend) {
            if *cached_identity == identity {
                return version.clone();
            }
        }
    }

    let version = run_version_probe(&program);

    if let Ok(mut cache) = cache.lock() {
        cache.insert(backend.to_string(), (identity, version.clone()));
    }
    version
}

type BinaryIdentity = (PathBuf, Option<SystemTime>, u64);

#[allow(clippy::type_complexity)]
fn version_cache() -> &'static Mutex<HashMap<String, (BinaryIdentity, Option<String>)>> {
    static CACHE: OnceLock<Mutex<HashMap<String, (BinaryIdentity, Option<String>)>>> =
        OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn binary_identity(program: &std::path::Path) -> BinaryIdentity {
    let meta = std::fs::metadata(program).ok();
    (
        program.to_path_buf(),
        meta.as_ref().and_then(|m| m.modified().ok()),
        meta.map(|m| m.len()).unwrap_or(0),
    )
}

/// `<program> --version`, on a leash: a vendor binary that hangs must not hang
/// the dispatch, so the probe runs on a worker thread and a timeout resolves to
/// `None` (fail closed), not to a wait.
fn run_version_probe(program: &std::path::Path) -> Option<String> {
    const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

    let (tx, rx) = std::sync::mpsc::channel();
    let program = program.to_path_buf();
    std::thread::spawn(move || {
        let output = std::process::Command::new(&program)
            .arg("--version")
            .output();
        let _ = tx.send(output);
    });

    let output = rx.recv_timeout(PROBE_TIMEOUT).ok()?.ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_version_output(&stdout)
        .or_else(|| parse_version_output(&String::from_utf8_lossy(&output.stderr)))
}

/// First `program` on `PATH` — the same resolution `Command::new("codex")` does,
/// so we probe the binary that will actually be spawned. (A shell *alias* is not
/// on this path: `Command` never goes through a shell, which is why the kill-test
/// exercised the real binary and not the operator's
/// `codex --dangerously-bypass-approvals-and-sandbox` alias.)
fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The checked-in receipt file, read at compile time.
    const RECEIPT_FILE: &str = include_str!("../certifications/codex-cli.toml");

    #[derive(Debug, PartialEq, Eq)]
    enum TomlValue {
        Str(String),
        List(Vec<String>),
    }

    impl TomlValue {
        fn as_str(&self) -> &str {
            match self {
                Self::Str(value) => value,
                Self::List(_) => panic!("expected a string, got a list"),
            }
        }

        fn as_list(&self) -> &[String] {
            match self {
                Self::List(values) => values,
                Self::Str(_) => panic!("expected a list, got a string"),
            }
        }
    }

    /// Deliberately tiny: the receipt format is `key = "value"` and
    /// `key = ["a", "b"]`, comments on their own lines. A real TOML dependency
    /// for one 20-key file would be the tail wagging the dog.
    fn parse_receipt(src: &str) -> HashMap<String, TomlValue> {
        let mut out = HashMap::new();
        let mut lines = src
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'));
        while let Some(line) = lines.next() {
            let Some((key, rest)) = line.split_once('=') else {
                continue;
            };
            let mut rest = rest.trim().to_string();
            if rest.starts_with('[') && !rest.contains(']') {
                for next in lines.by_ref() {
                    rest.push(' ');
                    rest.push_str(next);
                    if next.contains(']') {
                        break;
                    }
                }
            }
            let value = if let Some(inner) = rest.strip_prefix('[') {
                let inner = inner.trim_end_matches(']');
                TomlValue::List(
                    inner
                        .split(',')
                        .map(|item| item.trim().trim_matches('"').to_string())
                        .filter(|item| !item.is_empty())
                        .collect(),
                )
            } else {
                TomlValue::Str(rest.trim_matches('"').to_string())
            };
            out.insert(key.trim().to_string(), value);
        }
        out
    }

    /// **The parity ratchet.** The TOML file is the artifact a human audits; the
    /// const is what the qualification table actually trusts. If they can drift,
    /// the audit is theater — so they cannot drift.
    #[test]
    fn receipt_const_matches_the_checked_in_receipt_file() {
        let file = parse_receipt(RECEIPT_FILE);
        let receipt = CODEX_CLI_RECEIPT;

        for (key, expected) in [
            ("id", receipt.id),
            ("backend", receipt.backend),
            ("transport", "cli"),
            ("vendor_binary", receipt.vendor_binary),
            ("vendor_version", receipt.vendor_version),
            ("host_os", receipt.host_os),
            ("host_os_version", receipt.host_os_version),
            ("kill_test", receipt.kill_test),
            ("kill_test_fn", receipt.kill_test_fn),
            ("result", receipt.result.as_str()),
            ("executed_at", receipt.executed_at),
            ("executed_by", receipt.executed_by),
            ("duration_secs", receipt.duration_secs),
            ("executed_on_commit", receipt.executed_on_commit),
            ("kill_test_source_blob", receipt.kill_test_source_blob),
        ] {
            let got = file
                .get(key)
                .unwrap_or_else(|| panic!("receipt file is missing '{key}'"))
                .as_str();
            assert_eq!(
                got, expected,
                "'{key}' disagrees between {} and the Rust const",
                receipt.source_file
            );
        }

        let covers = file.get("covers").expect("covers").as_list();
        let const_covers = receipt
            .covers
            .iter()
            .map(|level| level.as_str().to_string())
            .collect::<Vec<_>>();
        assert_eq!(covers, const_covers.as_slice(), "certified levels disagree");

        let matrix = file.get("matrix").expect("matrix").as_list();
        assert_eq!(
            matrix, receipt.matrix,
            "the certified mutation matrix disagrees"
        );
        assert_eq!(
            receipt.transport,
            TransportKind::Cli,
            "the receipt file says transport = 'cli'"
        );
    }

    /// A receipt that did not pass, or that certifies nothing, must never reach
    /// the table. (Both are expressible on purpose — a recorded regression is
    /// worth keeping — but neither may certify.)
    #[test]
    fn shipped_receipts_passed_and_certify_at_least_one_level() {
        for receipt in RECEIPTS {
            assert!(receipt.passed(), "'{}' did not pass", receipt.id);
            assert!(
                !receipt.covers.is_empty(),
                "'{}' certifies no level",
                receipt.id
            );
            assert!(
                !receipt.matrix.is_empty(),
                "'{}' has an empty mutation matrix — it observed nothing",
                receipt.id
            );
        }
    }

    /// The codex run exercised read-only and *only* read-only. Pinned so a later
    /// edit cannot quietly widen `covers` to workspace-write, which no execution
    /// backs.
    #[test]
    fn the_codex_receipt_certifies_read_only_only() {
        assert_eq!(CODEX_CLI_RECEIPT.covers, &[WorkspaceAuthority::ReadOnly]);
        assert!(!CODEX_CLI_RECEIPT.certifies_level(WorkspaceAuthority::WorkspaceWrite));
        assert!(!CODEX_CLI_RECEIPT.certifies_level(WorkspaceAuthority::DangerFullAccess));
    }

    /// The runtime gate, in miniature: only the exercised version is certified,
    /// and an unknown version is not an old-enough version.
    #[test]
    fn a_receipt_certifies_exactly_the_version_it_exercised() {
        let receipt = CODEX_CLI_RECEIPT;
        assert!(receipt.certifies_version(Some("0.144.1")));
        assert!(
            receipt.certifies_version(Some("v0.144.1")),
            "the comparison is numeric-by-component, not a string match"
        );
        for other in ["0.144.2", "0.145.0", "0.144", "1.0.0", "0.9.0"] {
            assert!(
                !receipt.certifies_version(Some(other)),
                "'{other}' was never exercised and must not be certified"
            );
        }
        assert!(
            !receipt.certifies_version(None),
            "an unknown version is not a certified version (fail closed)"
        );
    }

    #[test]
    fn version_output_parsing_finds_the_version_token() {
        assert_eq!(
            parse_version_output("codex-cli 0.144.1").as_deref(),
            Some("0.144.1")
        );
        assert_eq!(
            parse_version_output("codex-cli v0.144.1\n").as_deref(),
            Some("0.144.1")
        );
        // A bare integer is not a version; a name is not a version.
        assert_eq!(parse_version_output("codex 1"), None);
        assert_eq!(parse_version_output("no version here"), None);
    }

    /// `0.9.0` is NOT newer than `0.144.1`, even though it is as a string. This
    /// is the exact comparison bug the lane card for this seat warns about, and
    /// the reason `versions_match` goes through `version_components`.
    #[test]
    fn version_comparison_is_never_lexicographic() {
        assert!(!versions_match("0.9.0", "0.144.1"));
        assert!(versions_match("0.144.1", "0.144.1"));
        assert!(!versions_match("0.144.10", "0.144.1"));
    }
}
