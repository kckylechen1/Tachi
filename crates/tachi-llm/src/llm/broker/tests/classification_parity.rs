//! The classification parity suite: the new classifier must reproduce the
//! decision the shipped lane has been making.
//!
//! # Why parity, and why it is not one assertion
//!
//! `chat_lanes::lane_calls` is untouched by this leaf — it keeps serving all
//! four lanes and its fifteen-plus consumers. What the broker copies is its
//! *classification semantics*, accumulated over a long tail of real provider
//! misbehaviour. A fresh reimplementation of that is exactly the kind of change
//! that looks right and is subtly wrong on a customer's dead account, so it is
//! pinned rather than reviewed.
//!
//! The cross-vendor review of the frozen design was explicit that comparing
//! `failure_class_for_status` alone is **insufficient** — the old behaviour also
//! depends on the response body, on the retry phase, and on what the tier does
//! to the key pool. So every fixture is checked on four axes:
//!
//! | axis | old | new |
//! |---|---|---|
//! | failure class | `failure_class_for_status(status, body)` | [`ProviderErrorClass::to_legacy_failure_class`] |
//! | billing-vs-auth | `chat_auth_failure_class(body)` | [`ProviderErrorClass::BillingOrQuota`] |
//! | retry phase | what the tier loop does next | [`RetryAdvice`] |
//! | retry-after | `header.parse::<u64>()` | [`RetryAfter::as_seconds`] |
//!
//! # Three legs, because two would let a wrong answer through
//!
//! 1. The oracle in [`legacy`] is a **transcription** of the private functions
//!    in `lane_calls.rs` (private, and that file is out of scope for this
//!    leaf — so it is copied, not called).
//! 2. [`the_transcribed_oracle_still_matches_the_lane_calls_source`] pins that
//!    transcription against the **source text** of the original, so the oracle
//!    cannot go stale while the suite stays green. Its top-level functions
//!    cover two of the four axes; the other two — retry phase and
//!    `Retry-After` — live in branches inside `call_provider_tier`, and
//!    [`the_transcribed_retry_phase_still_matches_the_lane_calls_source`] pins
//!    those blocks too. A hand-written oracle for a live loop that nothing
//!    pins is not a parity test, it is a second opinion.
//! 3. The fixtures assert the new classifier against a **literal expected
//!    answer** as well as against the oracle — a pure mirror test passes
//!    happily when both sides are wrong in the same direction.
//!
//! Plus [`the_projection_agrees_for_every_status_code`], which sweeps the whole
//! status space rather than trusting a hand-written corpus to be complete.
//!
//! # Declared divergences
//!
//! Parity is not identity. Three behaviours deliberately differ, and
//! [`the_declared_divergences_are_still_divergent`] asserts each one *still
//! differs* — so a later "fix" that silently converges has to come here and
//! change the record.

use super::*;

// ---------------------------------------------------------------------------
// The legacy oracle, transcribed
// ---------------------------------------------------------------------------

/// A transcription of `chat_lanes::lane_calls`'s private classification
/// functions.
///
/// Copied rather than called because they are module-private and `lane_calls.rs`
/// is explicitly out of scope for this leaf — widening their visibility to test
/// the copy would be a change to the very file the leaf promised not to touch.
/// [`the_transcribed_oracle_still_matches_the_lane_calls_source`] is what keeps
/// the copy honest.
mod legacy {
    use crate::ProviderInvocationFailureClass;

    /// Transcribed from `lane_calls::is_retriable_billing_failure`.
    pub fn is_retriable_billing_failure(resp_text: &str) -> bool {
        let lower = resp_text.to_ascii_lowercase();
        (lower.contains("balance") && lower.contains("insufficient"))
            || lower.contains("insufficient balance")
            || lower.contains("billing")
            || lower.contains("quota exceeded")
            || lower.contains("余额不足")
    }

    /// Transcribed from `lane_calls::chat_auth_failure_class`.
    pub fn chat_auth_failure_class(resp_text: &str) -> &'static str {
        if is_retriable_billing_failure(resp_text) {
            "billing_or_quota"
        } else {
            "authentication_or_authorization"
        }
    }

    /// Transcribed from `lane_calls::failure_class_for_status`.
    pub fn failure_class_for_status(
        status: u16,
        resp_text: &str,
    ) -> ProviderInvocationFailureClass {
        match status {
            401 => ProviderInvocationFailureClass::AuthFailed,
            403 if is_retriable_billing_failure(resp_text) => {
                ProviderInvocationFailureClass::ProviderExhausted
            }
            403 => ProviderInvocationFailureClass::AuthFailed,
            429 => ProviderInvocationFailureClass::ProviderExhausted,
            500..=599 => ProviderInvocationFailureClass::Transient,
            _ => ProviderInvocationFailureClass::LaneOutage,
        }
    }

    /// The legacy `Retry-After` read, verbatim in behaviour:
    /// `headers.get("retry-after").and_then(to_str).and_then(parse::<u64>)`.
    /// Only the delta-seconds form survives it; an HTTP-date is dropped.
    pub fn retry_after_seconds(raw: Option<&str>) -> Option<u64> {
        raw.and_then(|value| value.parse::<u64>().ok())
    }

    /// What `call_provider_tier`'s loop does next for a given status.
    ///
    /// Transcribed from the branch order in `lane_calls.rs`: the 429 arm, the
    /// 401/403 arm (which re-checks the key pool before giving up on the tier),
    /// the `status.is_server_error()` arm, and the `!status.is_success()` arm
    /// that returns without retrying at all.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum RetryIntent {
        /// Retry within this tier while attempts remain, unconditionally.
        WithinTier,
        /// Retry within this tier only if the key pool still holds a usable key.
        WithinTierIfAnotherKey,
        /// Return immediately; the tier loop does not retry this at all.
        None,
    }

    /// The intent for a non-success status.
    pub fn retry_intent(status: u16) -> RetryIntent {
        match status {
            429 => RetryIntent::WithinTier,
            401 | 403 => RetryIntent::WithinTierIfAnotherKey,
            500..=599 => RetryIntent::WithinTier,
            _ => RetryIntent::None,
        }
    }

    /// Parse a `RetryIntent` from its fixture spelling.
    pub fn parse_retry_intent(raw: &str) -> Option<RetryIntent> {
        Some(match raw {
            "retry_within_tier" => RetryIntent::WithinTier,
            "retry_within_tier_if_another_key" => RetryIntent::WithinTierIfAnotherKey,
            "no_retry" => RetryIntent::None,
            _ => return None,
        })
    }
}

/// The broker advice that corresponds to a legacy retry intent.
///
/// This is the mapping the parity claim rests on, written out rather than
/// implied: "retry within the tier" is the same deployment, "only if another
/// key" is the credential axis, "no retry" is a request the provider will
/// reject identically next time.
fn advice_for(intent: legacy::RetryIntent) -> RetryAdvice {
    match intent {
        legacy::RetryIntent::WithinTier => RetryAdvice::RetrySameDeployment,
        legacy::RetryIntent::WithinTierIfAnotherKey => RetryAdvice::RetryOtherCredential,
        legacy::RetryIntent::None => RetryAdvice::DoNotRetry,
    }
}

// ---------------------------------------------------------------------------
// Leg 2: the transcription is pinned to the original source text
// ---------------------------------------------------------------------------

/// Extracts a top-level function's source text: from the line starting with
/// `fn NAME(` through the next line that is exactly `}`.
fn lane_calls_fn(name: &str) -> String {
    let opener = format!("fn {name}(");
    let mut lines = LANE_CALLS_SOURCE
        .lines()
        .skip_while(|l| !l.starts_with(&opener));
    let first = lines
        .next()
        .unwrap_or_else(|| panic!("lane_calls.rs no longer declares a top-level `fn {name}`"));
    let mut out = String::from(first);
    for line in lines {
        out.push('\n');
        out.push_str(line);
        if line == "}" {
            return out;
        }
    }
    panic!("`fn {name}` in lane_calls.rs never closed at column zero");
}

#[test]
fn the_transcribed_retry_phase_still_matches_the_lane_calls_source() {
    // `legacy::retry_intent` claims to be "what `call_provider_tier`'s loop
    // does next". That claim is a transcription of four branches and a header
    // read, none of which are top-level functions, so each one is pinned to its
    // source text here.

    // The `Retry-After` read: delta-seconds only, HTTP-date dropped. This is
    // the whole of the fourth parity axis on the legacy side.
    assert_lane_calls_block(
        "Retry-After read",
        r#"            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());"#,
    );

    // 429 → retry within the tier, unconditionally, while attempts remain.
    assert_lane_calls_block(
        "429 retry",
        r#"            if status.as_u16() == 429 {
                last_class = ProviderInvocationFailureClass::ProviderExhausted;
                self.mark_secret_rate_limited(&selected, retry_after);
                last_err = format!(
                    "API error {status}: {}",
                    redact_provider_response(&resp_text)
                );
                if attempt < max_attempts {
                    continue;
                }"#,
    );

    // 401/403 → retry only if the *pool* still holds a usable key. This is the
    // branch that makes `RetryOtherCredential` the right broker advice rather
    // than `RetrySameDeployment`.
    assert_lane_calls_block(
        "401/403 arm",
        "            if status.as_u16() == 401 || status.as_u16() == 403 {",
    );
    assert_lane_calls_block(
        "401/403 pool re-check",
        "                if attempt < max_attempts && self.has_usable_secret_readonly(&cfg.api_key_envs) {",
    );

    // 5xx → retry within the tier, honouring `Retry-After` for the delay.
    assert_lane_calls_block(
        "5xx retry",
        r#"            if status.is_server_error() {
                last_class = ProviderInvocationFailureClass::Transient;
                last_err = format!(
                    "API error {status}: {}",
                    redact_provider_response(&resp_text)
                );
                if attempt < max_attempts {
                    let delay = if let Some(secs) = retry_after {
                        Duration::from_secs(secs)
                    } else {
                        Self::retry_delay(attempt)
                    };"#,
    );

    // Every other non-success status → return immediately, no retry at all.
    assert_lane_calls_block(
        "non-success no-retry return",
        r#"            if !status.is_success() {
                return Err(ProviderTierFailure {
                    class: ProviderInvocationFailureClass::LaneOutage,"#,
    );

    // And the retry budget those branches spend, read from the shipped
    // constant rather than transcribed: a change from 3 to 1 would make the
    // "retry within the tier" intents mean something materially different.
    assert_eq!(
        crate::llm::LlmClient::MAX_ATTEMPTS,
        3,
        "the shipped retry budget changed; `RetryIntent::WithinTier` no longer \
         means what the parity fixtures assume"
    );
}

#[test]
fn the_transcribed_oracle_still_matches_the_lane_calls_source() {
    // The whole parity suite is worthless if the thing it calls "legacy" has
    // drifted from what actually ships. This is the only test in the broker
    // that reads another module's source text, and that is deliberate: the
    // alternative is widening `lane_calls`'s visibility, which would mean
    // editing the file this leaf promised not to touch.
    //
    // When this fails: someone changed the shipped classifier. Update the
    // transcription in `legacy` *and* re-derive the fixture expectations —
    // do not just paste the new text in, because the new behaviour may now
    // disagree with the broker's copy.
    assert_eq!(
        lane_calls_fn("is_retriable_billing_failure"),
        r#"fn is_retriable_billing_failure(resp_text: &str) -> bool {
    let lower = resp_text.to_ascii_lowercase();
    (lower.contains("balance") && lower.contains("insufficient"))
        || lower.contains("insufficient balance")
        || lower.contains("billing")
        || lower.contains("quota exceeded")
        || lower.contains("余额不足")
}"#,
        "the shipped billing-failure test changed"
    );

    assert_eq!(
        lane_calls_fn("chat_auth_failure_class"),
        r#"fn chat_auth_failure_class(resp_text: &str) -> &'static str {
    if is_retriable_billing_failure(resp_text) {
        "billing_or_quota"
    } else {
        "authentication_or_authorization"
    }
}"#,
        "the shipped auth-failure sub-classifier changed"
    );

    assert_eq!(
        lane_calls_fn("failure_class_for_status"),
        r#"fn failure_class_for_status(status: u16, resp_text: &str) -> ProviderInvocationFailureClass {
    match status {
        401 => ProviderInvocationFailureClass::AuthFailed,
        403 if is_retriable_billing_failure(resp_text) => {
            ProviderInvocationFailureClass::ProviderExhausted
        }
        403 => ProviderInvocationFailureClass::AuthFailed,
        429 => ProviderInvocationFailureClass::ProviderExhausted,
        500..=599 => ProviderInvocationFailureClass::Transient,
        _ => ProviderInvocationFailureClass::LaneOutage,
    }
}"#,
        "the shipped status classifier changed"
    );

    // The disposition suite asserts `CompletionKindV1::to_completion_status_v1`
    // reproduces this rule, so it is pinned here too.
    assert_eq!(
        lane_calls_fn("completion_status_from_finish_reason"),
        r#"fn completion_status_from_finish_reason(finish_reason: Option<&str>) -> CompletionStatusV1 {
    match finish_reason {
        Some("stop") => CompletionStatusV1::Complete,
        Some("length") => CompletionStatusV1::Truncated,
        _ => CompletionStatusV1::Unknown,
    }
}"#,
        "the shipped finish-reason mapping changed"
    );
}

#[test]
fn the_brokers_billing_marker_test_matches_the_legacy_one() {
    // The one place classification depends on provider prose rather than on a
    // status code, so it gets its own corpus. Both the honest markers and the
    // near-misses: a test that only fed it positives would pass for a function
    // that returns `true`.
    let positive = [
        "insufficient balance",
        "Insufficient Balance",
        "INSUFFICIENT BALANCE",
        "your account balance is too low; insufficient funds remain",
        "Billing has not been configured for this organization",
        "BILLING REQUIRED",
        "quota exceeded for this month",
        "余额不足,请充值",
        r#"{"error":{"message":"Insufficient Balance","type":"insufficient_balance"}}"#,
    ];
    let negative = [
        "",
        "invalid api key provided",
        "the model does not exist",
        "balance",
        "insufficient",
        "insufficient permissions",
        "quota",
        "exceeded",
        "rate limit exceeded",
        "you exceeded your current requests per minute",
        "余额充足",
    ];

    for body in positive {
        assert!(
            legacy::is_retriable_billing_failure(body),
            "the transcribed oracle stopped recognising {body:?}"
        );
        assert!(
            super::super::openai_compat::is_retriable_billing_failure(body),
            "the broker's copy does not recognise the billing marker in {body:?}"
        );
    }
    for body in negative {
        assert!(
            !legacy::is_retriable_billing_failure(body),
            "the transcribed oracle over-matched {body:?}"
        );
        assert!(
            !super::super::openai_compat::is_retriable_billing_failure(body),
            "the broker's copy over-matched {body:?} — a plain auth failure would be \
             reported as a billing failure and the key marked exhausted instead of bad"
        );
    }

    // `insufficient permissions` is the trap the `&&` arm has to survive: it
    // contains "insufficient" but no "balance", so it must not match.
    assert!(!legacy::is_retriable_billing_failure(
        "insufficient permissions to access this model"
    ));
}

// ---------------------------------------------------------------------------
// Leg 3: the fixture corpus
// ---------------------------------------------------------------------------

#[test]
fn every_classification_fixture_agrees_with_the_legacy_decision_on_all_four_axes() {
    let adapter = OpenAiCompatWire::new();
    let fixtures = load_fixtures("classification");
    assert!(
        fixtures.len() >= 20,
        "the classification corpus shrank to {} fixtures — it is the only thing \
         standing between a reimplemented classifier and a silent behaviour change",
        fixtures.len()
    );

    for (name, fixture) in &fixtures {
        let status = fixture
            .pointer("/status")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("fixture {name} is missing /status"))
            as u16;
        let body = fixture_str(fixture, name, "/body");
        let headers = fixture_headers(fixture);
        let excerpt = OpenAiCompatWire::body_excerpt(body.as_bytes());

        let actual = adapter.classify_error(status, &headers, &excerpt);

        // --- axis 0: the literal expected answer -------------------------
        // Not derived from either implementation, so both being wrong the
        // same way still fails.
        assert_eq!(
            actual.class.as_str(),
            fixture_str(fixture, name, "/expected/class"),
            "{name}: the broker's error class drifted from the recorded golden"
        );
        assert_eq!(
            actual.advice.as_str(),
            fixture_str(fixture, name, "/expected/advice"),
            "{name}: the broker's retry advice drifted from the recorded golden"
        );

        // --- axis 1: the legacy failure class ----------------------------
        let legacy_class = legacy::failure_class_for_status(status, body);
        assert_eq!(
            actual.class.to_legacy_failure_class(),
            legacy_class,
            "{name}: projecting the broker's class back onto the shipped four-member \
             class does not reproduce the shipped decision"
        );
        assert_eq!(
            legacy_class.as_str(),
            fixture_str(fixture, name, "/expected/legacy_failure_class"),
            "{name}: the recorded legacy class is not what the oracle says"
        );

        // --- axis 2: billing versus auth ---------------------------------
        // The split that costs information when it is flattened: a dead
        // account needs a human, a busy one needs a clock.
        let legacy_says_billing =
            status == 403 && legacy::chat_auth_failure_class(body) == "billing_or_quota";
        assert_eq!(
            actual.class == ProviderErrorClass::BillingOrQuota,
            legacy_says_billing,
            "{name}: billing-vs-auth disagrees with the shipped sub-classifier \
             (the shipped tier marks the key exhausted rather than auth-failed \
             on exactly this condition)"
        );

        // --- axis 3: the retry phase -------------------------------------
        match fixture.pointer("/expected/legacy_retry_intent") {
            Some(Value::String(spelling)) => {
                let recorded = legacy::parse_retry_intent(spelling)
                    .unwrap_or_else(|| panic!("fixture {name}: unknown retry intent {spelling:?}"));
                let oracle = legacy::retry_intent(status);
                assert_eq!(
                    oracle, recorded,
                    "{name}: the recorded retry intent is not what the tier loop does"
                );
                assert_eq!(
                    actual.advice,
                    advice_for(oracle),
                    "{name}: the broker would take a different next step than the \
                     shipped tier loop"
                );
            }
            Some(Value::Null) => {
                // Success statuses have no tier-loop retry decision keyed on
                // status; they are covered by the divergence record instead.
                assert!(
                    (200..300).contains(&status),
                    "{name}: only a success status may opt out of the retry-phase axis"
                );
            }
            other => panic!("fixture {name}: /expected/legacy_retry_intent must be a string or null, got {other:?}"),
        }

        // --- axis 4: retry-after -----------------------------------------
        let legacy_seconds = legacy::retry_after_seconds(headers.get("retry-after"));
        assert_eq!(
            actual.retry_after.as_ref().and_then(RetryAfter::as_seconds),
            legacy_seconds,
            "{name}: the delta-seconds retry directive the shipped lane reads was lost"
        );
        assert_golden(
            name,
            "retry_after",
            &actual.retry_after,
            fixture_value(fixture, name, "/expected/retry_after"),
        );
    }
}

#[test]
fn the_corpus_covers_every_error_class_and_every_advice() {
    // A corpus that never produces `billing_or_quota` proves nothing about the
    // billing split, however many fixtures it has.
    let adapter = OpenAiCompatWire::new();
    let mut seen_classes: Vec<ProviderErrorClass> = Vec::new();
    let mut seen_advice: Vec<RetryAdvice> = Vec::new();
    for (name, fixture) in load_fixtures("classification") {
        let status = fixture
            .pointer("/status")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("fixture {name} is missing /status"))
            as u16;
        let body = fixture_str(&fixture, &name, "/body");
        let classified = adapter.classify_error(
            status,
            &fixture_headers(&fixture),
            &OpenAiCompatWire::body_excerpt(body.as_bytes()),
        );
        if !seen_classes.contains(&classified.class) {
            seen_classes.push(classified.class);
        }
        if !seen_advice.contains(&classified.advice) {
            seen_advice.push(classified.advice);
        }
    }
    for class in ProviderErrorClass::ALL {
        assert!(
            seen_classes.contains(class),
            "no classification fixture produces {class:?}"
        );
    }
    for advice in RetryAdvice::ALL {
        assert!(
            seen_advice.contains(advice),
            "no classification fixture produces {advice:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The whole status space, not just the corpus
// ---------------------------------------------------------------------------

#[test]
fn the_projection_agrees_for_every_status_code() {
    // A hand-written corpus is a sample. This is the proof: for every status a
    // provider can return, crossed with the body shapes classification actually
    // depends on, the projection of the broker's answer equals the shipped one.
    let adapter = OpenAiCompatWire::new();
    let bodies = [
        "",
        "{}",
        "Insufficient Balance",
        "余额不足",
        "billing",
        "quota exceeded",
        "invalid api key",
        "insufficient permissions",
        "balance insufficient",
    ];
    for status in 100u16..=599 {
        for body in bodies {
            let actual = adapter.classify_error(status, &ResponseHeaders::new(), body);
            assert_eq!(
                actual.class.to_legacy_failure_class(),
                legacy::failure_class_for_status(status, body),
                "status {status} with body {body:?}: the projection diverged"
            );
            if !(200..300).contains(&status) {
                assert_eq!(
                    actual.advice,
                    advice_for(legacy::retry_intent(status)),
                    "status {status} with body {body:?}: the next step diverged"
                );
            }
        }
    }
}

#[test]
fn a_billing_marker_only_reclassifies_a_403() {
    // The ordering trap. `is_retriable_billing_failure` is consulted *inside*
    // the 403 arm, so a 401 or a 429 whose body happens to mention billing is
    // still an auth failure and still a rate limit. An implementation that
    // tested the body before the status would pass a status-only corpus and
    // fail here.
    let adapter = OpenAiCompatWire::new();
    let billing_body = "Insufficient Balance: billing quota exceeded";
    let cases = [
        (401u16, ProviderErrorClass::AuthInvalid),
        (403, ProviderErrorClass::BillingOrQuota),
        (429, ProviderErrorClass::RateLimited),
        (500, ProviderErrorClass::ServerError),
        (400, ProviderErrorClass::BadRequest),
    ];
    for (status, expected) in cases {
        let actual = adapter.classify_error(status, &ResponseHeaders::new(), billing_body);
        assert_eq!(
            actual.class, expected,
            "status {status} with a billing body"
        );
        assert_eq!(
            actual.class.to_legacy_failure_class(),
            legacy::failure_class_for_status(status, billing_body),
            "status {status}: the projection diverged on a billing-marked body"
        );
    }
}

#[test]
fn an_http_date_retry_after_is_preserved_rather_than_dropped() {
    // Parity says the delta-seconds value must survive. This says the broker
    // keeps strictly more: the shipped lane parses only `u64` and silently
    // discards `Retry-After: <HTTP-date>`, then backs off by a guess.
    let adapter = OpenAiCompatWire::new();
    let date = "Tue, 12 Aug 2026 09:00:00 GMT";
    let headers = ResponseHeaders::from_pairs([("Retry-After", date)]);
    let classified = adapter.classify_error(429, &headers, "");

    assert_eq!(legacy::retry_after_seconds(Some(date)), None);
    assert_eq!(
        classified
            .retry_after
            .as_ref()
            .and_then(RetryAfter::as_seconds),
        None,
        "parity: neither side produces a delta-seconds value from an HTTP date"
    );
    assert_eq!(
        classified.retry_after,
        Some(RetryAfter::At(date.to_string())),
        "the directive itself must survive — 'wait until Tuesday' is a real answer"
    );

    // Header lookup is case-insensitive, and an unparseable value is not
    // mistaken for a number by either side.
    for (raw, expected) in [
        ("30", Some(RetryAfter::Seconds(30))),
        ("-5", Some(RetryAfter::At("-5".to_string()))),
        ("", None),
    ] {
        let headers = ResponseHeaders::from_pairs([("RETRY-AFTER", raw)]);
        let classified = adapter.classify_error(429, &headers, "");
        assert_eq!(classified.retry_after, expected, "Retry-After: {raw:?}");
        assert_eq!(
            classified
                .retry_after
                .as_ref()
                .and_then(RetryAfter::as_seconds),
            legacy::retry_after_seconds(Some(raw)),
            "Retry-After: {raw:?} — the seconds the shipped lane would read"
        );
    }
}

// ---------------------------------------------------------------------------
// Declared divergences
// ---------------------------------------------------------------------------

#[test]
fn the_declared_divergences_are_still_divergent() {
    let adapter = OpenAiCompatWire::new();

    // ---- D1: empty assistant content on a 2xx ---------------------------
    // The shipped tier retries an empty completion up to `MAX_ATTEMPTS`
    // (SiliconFlow/Qwen return empty bodies transiently). The broker records
    // that the provider *answered*, in the wrong shape, and forbids an
    // automatic re-send — a protocol violation buys the same answer at the
    // same price. The executor slice inherits this and may want the shipped
    // lane's retry back as an explicit, budgeted policy rather than as a
    // classification.
    let outcome = adapter.parse_response(
        200,
        &ResponseHeaders::new(),
        br#"{"choices":[{"message":{"content":""},"finish_reason":"stop"}]}"#,
    );
    assert_eq!(
        outcome.disposition().retry_posture(),
        RetryPosture::Forbidden,
        "D1 changed: empty content is no longer a forbidden-retry protocol error"
    );
    assert_eq!(
        legacy::retry_intent(200),
        legacy::RetryIntent::None,
        "the status-keyed oracle has no opinion on a 2xx; D1 lives in the body path"
    );
    // ...and that is exactly why the status-keyed oracle cannot carry this
    // divergence: the legacy side of D1 is a body-path branch. Pin it, or this
    // test asserts one property of the broker and calls it a divergence.
    assert_lane_calls_block(
        "legacy empty-content retry",
        r#"            if attempt < max_attempts {
                eprintln!(
                    "[llm] empty content (attempt {}/{}): {last_err}; retrying",
                    attempt, max_attempts
                );
                tokio::time::sleep(Self::retry_delay(attempt)).await;
                continue;
            }"#,
    );
    assert_eq!(
        crate::llm::LlmClient::MAX_ATTEMPTS,
        3,
        "D1 is 'legacy re-sends an empty completion up to three times, the \
         broker re-sends never'. If the budget is no longer three, the \
         divergence has changed shape and this record is stale."
    );

    // ---- D2: the body excerpt bound -------------------------------------
    // `classify_error` takes a bounded prefix by trait contract, because a
    // response body is untrusted input. So a 403 that hides its billing
    // marker past the bound is classified auth-invalid where the shipped lane
    // (which scans the whole body) says billing. Every real provider puts its
    // error message first; this is the price of the bound, recorded rather
    // than discovered.
    let buried = format!("{}Insufficient Balance", "x".repeat(8192));
    assert!(
        legacy::is_retriable_billing_failure(&buried),
        "the shipped lane scans the whole body"
    );
    let excerpt = OpenAiCompatWire::body_excerpt(buried.as_bytes());
    assert!(
        !super::super::openai_compat::is_retriable_billing_failure(&excerpt),
        "D2 changed: the excerpt bound no longer hides a far-away marker"
    );
    assert_eq!(
        adapter
            .classify_error(403, &ResponseHeaders::new(), &excerpt)
            .class,
        ProviderErrorClass::AuthInvalid,
        "D2 changed: a marker past the excerpt bound is now visible"
    );
    // ...and the same marker inside the bound is seen, so this is a bound and
    // not a broken matcher.
    let near = format!("Insufficient Balance{}", "x".repeat(8192));
    assert_eq!(
        adapter
            .classify_error(
                403,
                &ResponseHeaders::new(),
                &OpenAiCompatWire::body_excerpt(near.as_bytes())
            )
            .class,
        ProviderErrorClass::BillingOrQuota
    );

    // ---- D3: assistant text is not trimmed ------------------------------
    // The shipped lane returns `content.trim()`. Leading and trailing
    // whitespace is meaningful in code and diff output, so the broker returns
    // it verbatim. The *emptiness* test still trims, which is the part parity
    // covers: a whitespace-only answer is still an empty answer.
    let padded = adapter.parse_response(
        200,
        &ResponseHeaders::new(),
        br#"{"choices":[{"message":{"content":"  fn main() {}\n"},"finish_reason":"stop"}]}"#,
    );
    let WireOutcome::Completed { message, .. } = &padded else {
        panic!("a padded but non-empty completion must succeed: {padded:?}");
    };
    assert_eq!(
        message.text.as_deref(),
        Some("  fn main() {}\n"),
        "D3 changed: the broker started trimming assistant text"
    );

    let blank = adapter.parse_response(
        200,
        &ResponseHeaders::new(),
        br#"{"choices":[{"message":{"content":"   \n  "},"finish_reason":"stop"}]}"#,
    );
    assert!(
        matches!(blank, WireOutcome::ProtocolViolation { .. }),
        "a whitespace-only answer must still be empty on both sides: {blank:?}"
    );

    // The legacy side of D3: the shipped lane really does trim, and the same
    // expression is where its emptiness test comes from — which is why the
    // divergence is "the broker stops trimming the *returned text*" and not
    // "the broker changed what counts as empty".
    assert_lane_calls_block(
        "legacy content trim",
        r#"            let content = json["choices"].as_array().and_then(|choices| {
                choices.iter().find_map(|choice| {
                    choice["message"]["content"]
                        .as_str()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                })
            });"#,
    );
}
