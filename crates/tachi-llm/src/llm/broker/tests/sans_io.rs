//! The sans-IO claim, made machine-checkable.
//!
//! # The claim, stated precisely
//!
//! The design says an adapter "holds no HTTP client, opens no connection,
//! sleeps for no retry". The signatures carry most of that — four pure
//! functions cannot await anything — but a signature does not stop a helper
//! function three screens down from constructing a client, and "I read the
//! module and saw no IO" is a review artifact that expires the moment someone
//! edits the module.
//!
//! So the property is asserted against the **source text** of the adapter
//! layer: the modules under `broker/` that ship, which is every file except
//! this test tree. The test harness itself reads fixtures off disk on purpose
//! and is excluded — the claim is about the code that runs in production, not
//! about the code that checks it.
//!
//! # What the law actually forbids
//!
//! The law is *"an adapter cannot send"*, not *"an adapter must not mention a
//! crate"*. `reqwest` is in this crate's dependency graph because the executor
//! and the shipped lanes use it; the adapter layer borrows exactly one thing
//! from it — `reqwest::Url`, its re-export of the `url` crate's parser — to
//! decide whether an endpoint string is addressable. Parsing a string is not
//! IO: no socket, no DNS, no client, no runtime.
//!
//! That distinction is what
//! [`the_adapter_layer_borrows_reqwest_for_parsing_and_nothing_else`] pins. It
//! is deliberately not a "no `reqwest` anywhere" grep, because such a predicate
//! would be false (and was: the first cut of this slice claimed it while
//! `EndpointUrl::new` called `reqwest::Url::parse`). A false predicate that
//! passes is worse than no predicate, so this one is written to be true and to
//! stay true only while the boundary holds: every non-comment line mentioning
//! `reqwest` must be on the allowlist below, so a second borrow — a client, a
//! header map, a body — has to come here and argue for itself.

use std::fs;

use super::*;

/// Broker-directory `.rs` files that are not shipped modules, and why each one
/// is exempt from the adapter-layer scan.
///
/// This is the *only* hand-maintained list left: everything else is
/// discovered from disk at test time, so a shipped module added to `broker/`
/// after this test was written is scanned automatically rather than silently
/// skipped because nobody updated a constant.
const NON_SHIPPED_ALLOWLIST: &[(&str, &str)] = &[(
    "broker/tests.rs",
    "the test tree's own mod-glue file (`#[cfg(test)] mod tests;`), not a \
     module that ships",
)];

/// Shipped Broker modules that are intentionally not provider adapters.
const SHIPPED_NON_ADAPTER_ALLOWLIST: &[(&str, &str)] = &[(
    "broker/executor.rs",
    "the single injected-client HTTP executor; provider adapters remain sans-IO",
)];

/// Every `.rs` file directly under `src/llm/broker/`, recursively, except
/// anything inside a directory literally named `tests` (the fixture loader
/// reads directories by design, and the test tree is excluded on purpose —
/// see the module doc). Returns paths relative to `src/llm/`, matching the
/// spelling `ADAPTER_SOURCES` used to use (e.g. `"broker/canonical.rs"`).
fn candidate_broker_files() -> Vec<String> {
    let broker_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/llm/broker");
    let base = broker_dir.parent().expect("broker/ has a parent (src/llm)");
    let mut out = Vec::new();
    walk_rs_files(&broker_dir, base, &mut out);
    out
}

/// Recursion helper for [`candidate_broker_files`]: appends every `.rs` file
/// under `dir` (paths relative to `base`) except files under a `tests`
/// subdirectory.
fn walk_rs_files(dir: &Path, base: &Path, out: &mut Vec<String>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|err| {
        panic!("reading directory {}: {err}", dir.display());
    });
    for entry in entries {
        let path = entry.expect("directory entry is readable").path();
        if path.is_dir() {
            if path.file_name().and_then(|n| n.to_str()) == Some("tests") {
                continue;
            }
            walk_rs_files(&path, base, out);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let rel = path
            .strip_prefix(base)
            .expect("walked path is under base")
            .to_string_lossy()
            .replace('\\', "/");
        out.push(rel);
    }
}

/// The adapter layer's shipping source files, read from disk at test time.
///
/// `broker.rs` is added by hand because it is the one shipped file *outside*
/// `src/llm/broker/` (its sibling module declaration); everything under
/// `src/llm/broker/` itself is discovered by [`candidate_broker_files`], minus
/// [`NON_SHIPPED_ALLOWLIST`].
fn adapter_sources() -> Vec<(String, String)> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = vec![{
        let path = manifest_dir.join("src/llm/broker.rs");
        let content = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
        ("broker.rs".to_string(), content)
    }];

    for rel in candidate_broker_files() {
        if NON_SHIPPED_ALLOWLIST
            .iter()
            .any(|(exempt, _)| *exempt == rel)
            || SHIPPED_NON_ADAPTER_ALLOWLIST
                .iter()
                .any(|(exempt, _)| *exempt == rel)
        {
            continue;
        }
        let path = manifest_dir.join("src/llm").join(&rel);
        let content = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
        out.push((rel, content));
    }
    out
}

/// Constructs that would give the adapter layer a way to perform IO, and why
/// each one is disqualifying.
const IO_CONSTRUCTS: &[(&str, &str)] = &[
    (
        "reqwest::Client",
        "the pooled client (with #1621's no_proxy handling and the self-healing \
         rebuild) lives in the executor; a second one here is a second pool",
    ),
    (
        "ClientBuilder",
        "building a client is owning a connection pool",
    ),
    (
        ".send(",
        "sending is the executor's single point of cancellation and retry",
    ),
    ("hyper::", "a lower-level HTTP stack is still an HTTP stack"),
    ("std::net", "sockets are the executor's"),
    ("tokio::net", "sockets are the executor's"),
    ("TcpStream", "sockets are the executor's"),
    ("UdpSocket", "sockets are the executor's"),
    (
        "async fn",
        "an adapter function that can suspend is an adapter function that can \
         wait on something outside itself",
    ),
    (
        ".await",
        "awaiting is waiting on IO, a timer, or another task — none of which an \
         adapter owns",
    ),
    (
        "block_on",
        "blocking on a runtime from inside a pure function is IO with extra steps",
    ),
    (
        "std::thread",
        "an adapter that spawns has state the executor cannot cancel",
    ),
    (
        "sleep(",
        "backoff is the executor's clock; an adapter that sleeps holds a retry \
         policy nobody configured",
    ),
    (
        "std::fs",
        "reading a file is reading a credential file eventually",
    ),
    (
        "std::process",
        "a subprocess is an unsupervised child of a pure function",
    ),
];

/// The only lines of adapter source allowed to mention `reqwest`.
///
/// One entry, and it parses a string. Adding a second is a design change:
/// state here what it borrows and why that borrow cannot send bytes.
const REQWEST_ALLOWLIST: &[&str] =
    &["let parsed = reqwest::Url::parse(raw).map_err(|_| RequestError::InvalidEndpoint {"];

/// Whether a source line is prose rather than code.
///
/// Comments are exempt so the modules can *discuss* what they are forbidden
/// from doing — this very boundary has to be explainable in the doc that
/// describes it.
fn is_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//")
}

#[test]
fn the_adapter_layer_contains_no_io_construct() {
    let sources = adapter_sources();
    let mut scanned_lines = 0;
    for (file, source) in &sources {
        for (index, line) in source.lines().enumerate() {
            scanned_lines += 1;
            if is_comment(line) {
                continue;
            }
            for (needle, why) in IO_CONSTRUCTS {
                assert!(
                    !line.contains(needle),
                    "{file}:{} does IO: {needle:?} — {why}\n  {line}",
                    index + 1
                );
            }
        }
    }
    assert!(
        scanned_lines > 3_000,
        "the sweep read only {scanned_lines} lines — it is not actually reading \
         the adapter layer, so it would pass for a module full of sockets"
    );
}

#[test]
fn the_adapter_layer_borrows_reqwest_for_parsing_and_nothing_else() {
    // The honest version of the "no reqwest" claim: not absent, *bounded*.
    let sources = adapter_sources();
    let mut mentions = Vec::new();
    for (file, source) in &sources {
        for (index, line) in source.lines().enumerate() {
            if is_comment(line) || !line.contains("reqwest") {
                continue;
            }
            mentions.push((file.as_str(), index + 1, line.trim().to_string()));
        }
    }

    for (file, line_number, line) in &mentions {
        assert!(
            REQWEST_ALLOWLIST.contains(&line.as_str()),
            "{file}:{line_number} borrows something new from reqwest:\n  {line}\n\
             The adapter layer may borrow its URL parser (which is the `url` \
             crate's, re-exported) and nothing else. Anything that can open a \
             connection belongs to the executor."
        );
    }

    assert_eq!(
        mentions.len(),
        REQWEST_ALLOWLIST.len(),
        "the reqwest allowlist has {} entries but {} lines matched — an unused \
         allowlist entry means this test is guarding a line that no longer \
         exists, and would keep passing if the real borrow moved",
        REQWEST_ALLOWLIST.len(),
        mentions.len()
    );
}

#[test]
fn every_broker_rs_file_is_scanned_or_explicitly_exempt() {
    // The property the fs-enumeration exists for: a newly added shipped
    // module cannot land in a gap between "the scan" and "the allowlist" —
    // every `.rs` file this test finds on disk under `src/llm/broker/` is
    // provably one or the other. Before this test, a new file that nobody
    // added to a hand-written list would just never be scanned, silently.
    let candidates = candidate_broker_files();
    assert!(
        candidates.len() >= 6,
        "the walk found only {} candidate files — it is not actually reading \
         the broker directory",
        candidates.len()
    );

    let scanned = adapter_sources();
    for rel in &candidates {
        let is_scanned = scanned.iter().any(|(name, _)| name == rel);
        let is_exempt = NON_SHIPPED_ALLOWLIST
            .iter()
            .any(|(exempt, _)| *exempt == rel.as_str())
            || SHIPPED_NON_ADAPTER_ALLOWLIST
                .iter()
                .any(|(exempt, _)| *exempt == rel.as_str());
        assert!(
            is_scanned || is_exempt,
            "{rel} is on disk under src/llm/broker/ but is neither scanned for \
             IO/reqwest nor in NON_SHIPPED_ALLOWLIST — a newly shipped module \
             must be one or the other, never silently neither"
        );
    }
}

#[test]
fn the_executor_exemption_is_exact_and_still_executes_http() {
    assert_eq!(
        SHIPPED_NON_ADAPTER_ALLOWLIST,
        &[(
            "broker/executor.rs",
            "the single injected-client HTTP executor; provider adapters remain sans-IO",
        )]
    );
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/llm/broker/executor.rs");
    let source =
        fs::read_to_string(&path).unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
    assert!(source.contains("reqwest::Client"));
    assert!(source.contains("self.http.execute(http_request)"));
}

#[test]
fn parsing_an_endpoint_reaches_no_network() {
    // The behavioural half of the boundary: the one borrowed function is fed a
    // host that cannot resolve and an unroutable address, and answers
    // immediately from the string itself. A parser that resolved would hang or
    // fail here instead of accepting.
    assert!(EndpointUrl::new("https://this-host-does-not-exist.invalid/v1").is_ok());
    assert!(EndpointUrl::new("http://192.0.2.1:9/v1").is_ok());
    // ...and it rejects on grammar, not on reachability.
    assert!(EndpointUrl::new("https://").is_err());
}
