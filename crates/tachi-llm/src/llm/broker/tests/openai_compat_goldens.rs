//! The OpenAI-compatible dialect's two goldens: canonical → wire bytes, and
//! wire bytes → canonical outcome.
//!
//! # What a golden here is worth
//!
//! Under sans-IO the fake adapter and the real one are the same code, so this
//! corpus *is* the conformance suite for the dialect — there is no separate
//! "real" behaviour to test later, only a network in front of it. What the
//! corpus does not cover is stated out loud in the trait's module doc: connect
//! failures, stalled bodies, mid-stream drops, key-pool rotation and health
//! side effects all live in the executor and need their own fault-injection
//! suite. A green matrix here is not coverage of the invocation path.
//!
//! # The fixtures are data, and each says why it exists
//!
//! Every fixture carries a `why`, because a golden whose motivation is lost
//! gets "updated" the first time it fails. The suite asserts that too — a
//! fixture with no rationale is rejected rather than quietly executed.
//!
//! # Two divergences from the shipped lane show up here
//!
//! - **No `enable_thinking: false`.** The shipped lane patches that into the
//!   body for SiliconFlow/Qwen from an environment variable. A
//!   provider-and-model-keyed body quirk belongs in the #1681 catalog, not
//!   compiled into a dialect, so no body this adapter builds carries it.
//! - **The finish reason comes from the chosen choice.** The shipped lane reads
//!   `choices[0].finish_reason` while selecting content from the first
//!   *non-blank* choice, so with `n > 1` and an empty first choice it reports
//!   one choice's reason for another choice's text. Every caller in the repo
//!   sends `n = 1`, where the two agree.

use super::*;

/// The request goldens' fixture directory.
const REQUESTS: &str = "openai_compat_request";
/// The response goldens' fixture directory.
const RESPONSES: &str = "openai_compat_response";

/// Projects a built wire request into the fixture's `expected` shape.
///
/// [`WireHttpRequest`] is not `Serialize` on purpose — its fields are private
/// so nothing can staple a header on after the adapter built it — so the
/// comparison shape is built here from its accessors.
fn project(built: &WireHttpRequest) -> Value {
    json!({
        "method": built.method().as_str(),
        "url": built.url(),
        "headers": built
            .headers()
            .iter()
            .map(|header| json!([header.name(), header.value()]))
            .collect::<Vec<_>>(),
        "auth_placement": serde_json::to_value(built.auth_placement())
            .expect("an auth placement serializes"),
        "body": serde_json::from_slice::<Value>(built.body())
            .expect("the adapter must emit a JSON body"),
    })
}

#[test]
fn every_request_fixture_builds_the_expected_wire_bytes() {
    let adapter = OpenAiCompatWire::new();
    let fixtures = load_fixtures(REQUESTS);
    assert!(
        fixtures.len() >= 10,
        "the request corpus shrank to {} fixtures",
        fixtures.len()
    );

    for (name, fixture) in &fixtures {
        assert!(
            !fixture_str(fixture, name, "/why").trim().is_empty(),
            "fixture {name} has no rationale — a golden whose motivation is lost \
             gets 'updated' the first time it fails"
        );

        // The canonical request is deserialized, not built in Rust: that runs
        // the same fallible constructor the gateway's JSON body will, so a
        // fixture cannot describe a request the constructor would refuse.
        let request: CanonicalInvocationRequest =
            serde_json::from_value(fixture_value(fixture, name, "/request").clone())
                .unwrap_or_else(|err| panic!("fixture {name}: canonical request rejected: {err}"));

        let built = adapter
            .build_request(&request, api_key_lease())
            .unwrap_or_else(|refusal| panic!("fixture {name}: adapter refused: {refusal:?}"));

        assert_eq!(
            &project(&built),
            fixture_value(fixture, name, "/expected"),
            "{name}: the wire request drifted from its golden\n  actual: {}",
            serde_json::to_string_pretty(&project(&built)).unwrap_or_default()
        );
    }
}

#[test]
fn every_response_fixture_parses_to_the_expected_outcome() {
    let adapter = OpenAiCompatWire::new();
    let fixtures = load_fixtures(RESPONSES);
    assert!(
        fixtures.len() >= 15,
        "the response corpus shrank to {} fixtures",
        fixtures.len()
    );

    for (name, fixture) in &fixtures {
        assert!(
            !fixture_str(fixture, name, "/why").trim().is_empty(),
            "fixture {name} has no rationale"
        );
        let status = fixture
            .pointer("/status")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("fixture {name} is missing /status"))
            as u16;
        let body = fixture_str(fixture, name, "/body");

        let outcome = adapter.parse_response(status, &fixture_headers(fixture), body.as_bytes());
        assert_golden(
            name,
            "wire outcome",
            &outcome,
            fixture_value(fixture, name, "/expected"),
        );
    }
}

#[test]
fn the_response_corpus_reaches_every_outcome_and_every_completion_kind() {
    // A corpus of twenty happy paths proves nothing about the failure shapes.
    let adapter = OpenAiCompatWire::new();
    let mut completed = 0usize;
    let mut rejected = 0usize;
    let mut violated = 0usize;
    let mut kinds: Vec<CompletionKindV1> = Vec::new();
    let mut violations: Vec<ProtocolViolationKind> = Vec::new();

    for (name, fixture) in load_fixtures(RESPONSES) {
        let status = fixture
            .pointer("/status")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("fixture {name} is missing /status"))
            as u16;
        let body = fixture_str(&fixture, &name, "/body");
        match adapter.parse_response(status, &fixture_headers(&fixture), body.as_bytes()) {
            WireOutcome::Completed { completion, .. } => {
                completed += 1;
                if !kinds.contains(&completion) {
                    kinds.push(completion);
                }
            }
            WireOutcome::Rejected { .. } => rejected += 1,
            WireOutcome::ProtocolViolation { violation } => {
                violated += 1;
                if !violations.contains(&violation.kind()) {
                    violations.push(violation.kind());
                }
            }
        }
    }

    assert!(completed >= 5 && rejected >= 3 && violated >= 4,
        "outcome coverage is lopsided: {completed} completed / {rejected} rejected / {violated} violations");
    for kind in CompletionKindV1::ALL {
        assert!(
            kinds.contains(kind),
            "no response fixture produces completion kind {kind:?}"
        );
    }
    for kind in [
        ProtocolViolationKind::MalformedBody,
        ProtocolViolationKind::SchemaViolation,
        ProtocolViolationKind::EmptyAssistantContent,
    ] {
        assert!(
            violations.contains(&kind),
            "no response fixture produces violation {kind:?}"
        );
    }
    // StreamDecode is the one violation this slice cannot reach: the decoder
    // is the next slice, and there is no code path that fabricates one.
    assert!(!violations.contains(&ProtocolViolationKind::StreamDecode));
}

#[test]
fn no_built_body_carries_the_shipped_lanes_enable_thinking_patch() {
    // Declared divergence. `should_disable_thinking` is a
    // provider-and-model-keyed body patch driven by an environment variable;
    // per-deployment body quirks belong in the #1681 catalog, not compiled
    // into a dialect. Asserted across the whole corpus rather than once, so a
    // later "just add it for SiliconFlow" has to delete this test on purpose.
    let adapter = OpenAiCompatWire::new();
    for (name, fixture) in load_fixtures(REQUESTS) {
        let request: CanonicalInvocationRequest =
            serde_json::from_value(fixture_value(&fixture, &name, "/request").clone())
                .unwrap_or_else(|err| panic!("fixture {name}: {err}"));
        let built = adapter
            .build_request(&request, api_key_lease())
            .unwrap_or_else(|refusal| panic!("fixture {name}: {refusal:?}"));
        let body = built.body_utf8().expect("body is UTF-8");
        assert!(
            !body.contains("enable_thinking"),
            "{name}: the dialect grew a per-deployment body quirk"
        );
        assert!(
            !body.contains("\"stream\""),
            "{name}: a non-streaming request must not mention stream at all"
        );
    }
}

#[test]
fn the_adapter_sets_exactly_one_header_and_lowercases_it() {
    // One non-secret header, lowercased so the executor, the goldens and any
    // future signing step agree on a single spelling. Anything else the
    // deployment needs is the executor's to add.
    let built = OpenAiCompatWire::new()
        .build_request(&minimal_request(), api_key_lease())
        .expect("builds");
    let headers: Vec<(&str, &str)> = built
        .headers()
        .iter()
        .map(|header| (header.name(), header.value()))
        .collect();
    assert_eq!(headers, vec![("content-type", "application/json")]);

    // And the lowercasing is real, not an accident of the input.
    let shouted = WireHttpRequest::new(
        HttpMethod::Post,
        "https://provider.test/v1/chat/completions",
        vec![WireHeader::new("X-Tachi-Trace", "abc")],
        AuthPlacement::None,
        Vec::new(),
    )
    .expect("a trace header is not a credential header");
    assert_eq!(shouted.headers()[0].name(), "x-tachi-trace");
    assert_eq!(
        shouted.headers()[0].value(),
        "abc",
        "only the name is normalized; a value is the caller's bytes"
    );
}

#[test]
fn the_url_is_the_resolved_endpoint_verbatim() {
    // The adapter does not append `/chat/completions`, strip a trailing slash,
    // or otherwise negotiate with the URL. The resolved deployment already
    // named the endpoint; guessing at it is how a request lands on the wrong
    // path against a gateway that proxies several dialects.
    let odd = "https://gateway.test/openai/v1/chat/completions?deployment=blue";
    let mut parts = minimal_parts();
    parts.target = InvocationTarget::Resolved {
        target: ResolvedWireTarget::new(ResolvedWireTargetParts {
            deployment_id: "dep-2".to_string(),
            endpoint: EndpointUrl::new(odd).expect("valid endpoint"),
            provider_model_id: "vendor/model:tag".to_string(),
        })
        .expect("valid target"),
    };
    let request = CanonicalInvocationRequest::new(parts).expect("valid");
    let built = OpenAiCompatWire::new()
        .build_request(&request, api_key_lease())
        .expect("builds");
    assert_eq!(built.url(), odd);

    // The provider-side model id is sent verbatim too — slashes, colons and
    // all — rather than being derived from the deployment id.
    let body: Value = serde_json::from_slice(built.body()).expect("json body");
    assert_eq!(body["model"], json!("vendor/model:tag"));
}

#[test]
fn the_finish_reason_comes_from_the_choice_that_was_actually_selected() {
    // Declared divergence: the shipped lane reads `choices[0].finish_reason`
    // unconditionally while taking content from the first non-blank choice, so
    // with `n > 1` and an empty first choice it attributes one choice's ending
    // to another choice's text. Every caller in this repo sends `n = 1`, where
    // the two agree; the broker reports the ending of the answer it returned.
    let outcome = OpenAiCompatWire::new().parse_response(
        200,
        &ResponseHeaders::new(),
        br#"{"choices":[{"message":{"content":""},"finish_reason":"stop"},
             {"message":{"content":"real"},"finish_reason":"length"}]}"#,
    );
    let WireOutcome::Completed {
        message,
        completion,
        ..
    } = &outcome
    else {
        panic!("expected a completion: {outcome:?}");
    };
    assert_eq!(message.text.as_deref(), Some("real"));
    assert_eq!(
        *completion,
        CompletionKindV1::Truncated,
        "the reported ending must belong to the returned text"
    );
}

#[test]
fn a_body_excerpt_is_bounded_and_survives_invalid_utf8() {
    // Classification reads a bounded prefix of untrusted bytes. Two properties:
    // it is bounded, and it does not panic on a body that is not UTF-8 (which
    // a proxy returning a gzipped or binary error page will produce).
    let long = vec![b'x'; 100_000];
    let excerpt = OpenAiCompatWire::body_excerpt(&long);
    assert!(
        excerpt.len() <= 4096,
        "the excerpt is unbounded at {} bytes",
        excerpt.len()
    );

    let invalid = [
        0xf0, 0x9f, 0x92, 0xa9, 0xff, 0xfe, b'b', b'i', b'l', b'l', b'i', b'n', b'g',
    ];
    let excerpt = OpenAiCompatWire::body_excerpt(&invalid);
    assert!(excerpt.contains("billing"), "lossy decoding lost the tail");

    // A multi-byte character straddling the bound must not panic or produce
    // invalid UTF-8 — `from_utf8_lossy` replaces it.
    let mut straddling = vec![b'x'; 4095];
    straddling.extend_from_slice("余额不足".as_bytes());
    let excerpt = OpenAiCompatWire::body_excerpt(&straddling);
    assert!(
        excerpt.len() <= 4096 + 3,
        "the replacement must stay bounded"
    );
}

#[test]
fn a_json_schema_survives_the_round_trip_it_was_given() {
    // The passthrough claim, stated independently of the fixture corpus: the
    // schema that comes out of the body is the schema that went in, including
    // keys this dialect has never heard of.
    let schema = json!({
        "type": "object",
        "unknown-to-us": [1, 2, {"deep": {"deeper": null}}],
        "properties": {"a": {"type": "string", "pattern": "^\\d+$"}},
    });
    let mut parts = minimal_parts();
    parts.response_format = ResponseFormat::JsonSchema {
        name: "round_trip".to_string(),
        strict: true,
        schema: schema.clone(),
    };
    let request = CanonicalInvocationRequest::new(parts).expect("valid");
    let built = OpenAiCompatWire::new()
        .build_request(&request, api_key_lease())
        .expect("builds");
    let body: Value = serde_json::from_slice(built.body()).expect("json body");
    assert_eq!(
        body["response_format"]["json_schema"]["schema"], schema,
        "the caller's schema was rewritten on the way to the provider"
    );
}

#[test]
fn a_provider_response_cannot_inject_a_field_the_outcome_does_not_have() {
    // The response types are closed: extra provider fields are dropped, not
    // absorbed. A provider that sends `"disposition":"completed"` alongside a
    // rejection must not be able to talk the outcome into a different shape.
    let outcome = OpenAiCompatWire::new().parse_response(
        200,
        &ResponseHeaders::new(),
        br#"{"model":"m","disposition":"completed","provenance":"provider_authoritative",
             "usage":{"prompt_tokens":1,"total_tokens":2,"cost_micros":999999},
             "choices":[{"message":{"content":"hi","role":"assistant","extra":"x"},"finish_reason":"stop"}]}"#,
    );
    let serialized = serde_json::to_value(&outcome).expect("serializes");
    assert_eq!(serialized["outcome"], json!("completed"));
    assert!(
        serialized["usage"].get("cost_micros").is_none(),
        "a provider-supplied cost field was absorbed into the usage observation"
    );
    assert_eq!(serialized["usage"]["prompt_tokens"], json!(1));
    assert_eq!(serialized["usage"]["total_tokens"], json!(2));
    assert!(
        serialized["usage"].get("completion_tokens").is_none(),
        "an unreported number must stay absent rather than be derived"
    );
}
