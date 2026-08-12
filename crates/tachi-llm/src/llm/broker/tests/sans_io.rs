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

use super::*;

/// The adapter layer's shipping source files.
///
/// `tests.rs` and this tree are excluded: the fixture loader reads directories
/// by design.
const ADAPTER_SOURCES: &[(&str, &str)] = &[
    ("broker.rs", include_str!("../../broker.rs")),
    ("broker/canonical.rs", include_str!("../canonical.rs")),
    ("broker/disposition.rs", include_str!("../disposition.rs")),
    (
        "broker/openai_compat.rs",
        include_str!("../openai_compat.rs"),
    ),
    ("broker/stream.rs", include_str!("../stream.rs")),
    ("broker/usage.rs", include_str!("../usage.rs")),
    ("broker/wire.rs", include_str!("../wire.rs")),
];

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
    let mut scanned_lines = 0;
    for (file, source) in ADAPTER_SOURCES {
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
    let mut mentions = Vec::new();
    for (file, source) in ADAPTER_SOURCES {
        for (index, line) in source.lines().enumerate() {
            if is_comment(line) || !line.contains("reqwest") {
                continue;
            }
            mentions.push((*file, index + 1, line.trim().to_string()));
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
