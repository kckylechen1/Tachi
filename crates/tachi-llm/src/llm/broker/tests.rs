//! Broker tests.
//!
//! Every test here is discriminative: it can fail. The bar the golden corpus
//! has to clear is that a plausible-but-wrong adapter fails it — a fixture
//! that only asserts "some outcome came back" is a smoke test wearing a
//! golden's clothes.
//!
//! Layout:
//!
//! | module | what it pins |
//! |---|---|
//! | [`spelling`] | every frozen wire spelling, against a literal golden |
//! | [`canonical_bounds`] | that the caps hold on both construction paths |
//! | [`capability_refusals`] | each capability gate, one refusal at a time |
//! | [`disposition_goldens`] | the disposition/usage vocabularies' serialized shape and their safety semantics |
//! | [`two_gate`] | that no credential can be named, selected, or leaked |
//! | [`sans_io`] | that the adapter layer contains no way to perform IO |
//! | [`classification_parity`] | that the new classifier reproduces the legacy one, fixture for fixture |
//! | [`openai_compat_goldens`] | canonical → wire bytes, and wire → canonical outcome |
//! | [`stream_transcripts`] | recorded provider stream bytes → canonical events, per grammar |
//! | [`stream_chunk_invariance`] | that where the network split the bytes cannot change the answer |
//! | [`stream_bounds`] | the decoder's ceilings, and that a terminal answer never moves |

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::*;

mod canonical_bounds;
mod capability_refusals;
mod classification_parity;
mod disposition_goldens;
mod openai_compat_goldens;
mod sans_io;
mod spelling;
mod stream_bounds;
mod stream_chunk_invariance;
mod stream_transcripts;
mod two_gate;

// ---------------------------------------------------------------------------
// Shared builders
// ---------------------------------------------------------------------------

/// A stable endpoint for goldens. `.test` is a reserved TLD, so a fixture can
/// never resolve to something real if a future executor test forgets to mock.
const TEST_ENDPOINT: &str = "https://provider.test/v1/chat/completions";

fn resolved_target() -> ResolvedWireTarget {
    ResolvedWireTarget::new(target_parts()).expect("fixture target must be valid")
}

fn target_parts() -> ResolvedWireTargetParts {
    ResolvedWireTargetParts {
        deployment_id: "dep-openai-compat-1".to_string(),
        endpoint: EndpointUrl::new(TEST_ENDPOINT).expect("fixture endpoint must be valid"),
        provider_model_id: "test-model-1".to_string(),
    }
}

fn admitted() -> AdmittedRefs {
    AdmittedRefs {
        caller_ref: "uid:501".to_string(),
        host_ref: Some("host:local".to_string()),
        task_ref: None,
    }
}

fn strict_data_policy() -> DataPolicyConstraint {
    DataPolicyConstraint {
        prohibit_training_on_input: true,
        prohibit_retention: true,
        required_residency: None,
    }
}

fn user_message(text: &str) -> CanonicalMessage {
    CanonicalMessage {
        role: MessageRole::User,
        content: MessageContent::Text {
            text: text.to_string(),
        },
        tool_call_id: None,
        name: None,
    }
}

/// The smallest legal request: one user turn, no tools, no structure, no
/// stream.
fn minimal_parts() -> CanonicalInvocationRequestParts {
    CanonicalInvocationRequestParts {
        target: InvocationTarget::Resolved {
            target: resolved_target(),
        },
        messages: vec![user_message("hello")],
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        response_format: ResponseFormat::Text,
        stream: StreamSelection::Disabled,
        sampling: SamplingParams::none(),
        data_policy: strict_data_policy(),
        budget: BudgetConstraint::unbounded(),
        idempotency_key: None,
        deadline: DeadlineContext::unbounded(),
        cancellation: CancellationContext::none(),
        admitted: admitted(),
    }
}

fn minimal_request() -> CanonicalInvocationRequest {
    CanonicalInvocationRequest::new(minimal_parts()).expect("minimal request must be valid")
}

fn api_key_lease() -> AuthMaterialRef<'static> {
    AuthMaterialRef::leased(AuthMaterialKind::ApiKey, "lease-fixture-001")
}

// ---------------------------------------------------------------------------
// Fixture loading
// ---------------------------------------------------------------------------

fn fixture_dir(sub: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/llm/broker/fixtures")
        .join(sub)
}

/// Every `.json` fixture in a directory, sorted by file name.
///
/// Reading the directory rather than `include_str!`-ing a hand-maintained list
/// is deliberate: a fixture added to the tree is executed automatically, so a
/// new case cannot sit in the repository being silently skipped.
fn load_fixtures(sub: &str) -> Vec<(String, Value)> {
    let dir = fixture_dir(sub);
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|err| panic!("fixture dir {} unreadable: {err}", dir.display()))
        .map(|entry| entry.expect("fixture dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    entries.sort();
    assert!(
        !entries.is_empty(),
        "fixture dir {} is empty — a golden suite with no fixtures always passes",
        dir.display()
    );
    entries
        .into_iter()
        .map(|path| {
            let name = path
                .file_name()
                .expect("fixture file name")
                .to_string_lossy()
                .into_owned();
            let raw = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("fixture {name} unreadable: {err}"));
            let value: Value = serde_json::from_str(&raw)
                .unwrap_or_else(|err| panic!("fixture {name} is not valid JSON: {err}"));
            (name, value)
        })
        .collect()
}

/// A required string field of a fixture.
fn fixture_str<'a>(fixture: &'a Value, name: &str, pointer: &str) -> &'a str {
    fixture
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("fixture {name} is missing string at {pointer}"))
}

/// A required sub-value of a fixture.
fn fixture_value<'a>(fixture: &'a Value, name: &str, pointer: &str) -> &'a Value {
    fixture
        .pointer(pointer)
        .unwrap_or_else(|| panic!("fixture {name} is missing value at {pointer}"))
}

/// Header pairs from a fixture's `headers` array of two-element arrays.
fn fixture_headers(fixture: &Value) -> ResponseHeaders {
    let pairs = fixture
        .get("headers")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    let pair = entry.as_array().expect("header entry must be a 2-array");
                    (
                        pair[0].as_str().expect("header name").to_string(),
                        pair[1].as_str().expect("header value").to_string(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ResponseHeaders::from_pairs(pairs)
}

// ---------------------------------------------------------------------------
// Pinning the shipped lane's source text
// ---------------------------------------------------------------------------

/// The current text of `lane_calls.rs`, read at compile time.
///
/// The only place in the broker that reads another module's source, and
/// deliberately so: `lane_calls`'s classification and retry decisions are
/// private, and that file is out of scope for this leaf, so widening their
/// visibility to test the copy would mean editing the file the leaf promised
/// not to touch. It lives here rather than in one test module because both the
/// parity suite and the dialect goldens have to pin legacy behaviour — a
/// divergence asserted on only the new side is not a divergence test, it is a
/// description of the new side.
const LANE_CALLS_SOURCE: &str = include_str!("../chat_lanes/lane_calls.rs");

/// Pins one block of `lane_calls.rs` — including the ones inside method
/// bodies, which a top-level-function extractor cannot reach.
///
/// Exactly one occurrence is required: zero means the block moved or changed
/// (the transcription must be re-derived), and more than one means the pin is
/// ambiguous and could be satisfied by the wrong copy.
#[track_caller]
fn assert_lane_calls_block(what: &str, block: &str) {
    let occurrences = LANE_CALLS_SOURCE.matches(block).count();
    assert_eq!(
        occurrences, 1,
        "lane_calls.rs no longer contains exactly one copy of the {what} block \
         ({occurrences} found).\n\nWhen this fails: the shipped lane changed. \
         Re-derive the transcribed oracle and the expectations that depend on \
         it from the new text — do not paste the new text in and move on, \
         because the broker's copy may now disagree with it.\n\nExpected \
         block:\n{block}"
    );
}

/// Serializes a value and compares it against a golden, with a diff-friendly
/// message.
#[track_caller]
fn assert_golden<T: serde::Serialize>(name: &str, what: &str, actual: &T, expected: &Value) {
    let actual = serde_json::to_value(actual).expect("value must serialize");
    assert_eq!(
        &actual,
        expected,
        "{name}: {what} drifted from its golden\n  actual:   {}\n  expected: {}",
        serde_json::to_string_pretty(&actual).unwrap_or_default(),
        serde_json::to_string_pretty(expected).unwrap_or_default(),
    );
}
