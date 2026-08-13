//! Discriminators for the `model_deployment_health` single writer
//! (tachi#1681 D4, PR-C).
//!
//! The store-side half — a health write never touching catalog metadata, and
//! health events folding into the catalog projection — is in
//! `db::tests::model_catalog_ops`. What is asserted here is what the *type
//! face* and the row arithmetic guarantee before any connection exists:
//!
//! - **Discrimination 4 / the D4 attribution rule** — no auth-class outcome can
//!   become a deployment record, and a 429 both cools the deployment down and
//!   leaves the credential authority alone (there is nothing in this writer's
//!   inputs or outputs that could reach it).
//! - **Cooldown semantics** — `Retry-After` in both RFC 9110 forms, resolved to
//!   an instant, defaulted per class, and clamped in both directions.

use super::*;

fn at(seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000 + seconds, 0).expect("fixed test instant")
}

fn iso(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// A `5xx` outcome, through the fallible constructor every caller must use.
fn server_error(status: u16, retry_after: Option<RetryAfter>) -> DeploymentOutcome {
    DeploymentOutcome::ServerError {
        status: ServerErrorStatus::new(status).expect("a 5xx status"),
        retry_after,
    }
}

/// An unusable answer that carried a status.
fn unusable_response(status: u16) -> DeploymentOutcome {
    DeploymentOutcome::UnusableResponse {
        status: Some(UnusableResponseStatus::new(status).expect("a non-auth status")),
    }
}

fn record(
    existing: Option<&ModelDeploymentHealth>,
    outcome: DeploymentOutcome,
    now: DateTime<Utc>,
) -> DeploymentOutcomeWrite {
    record_deployment_outcome(
        existing,
        "env:extract",
        7,
        outcome,
        EvidenceKind::SelfReported,
        now,
    )
}

// ─── the attribution rule (D4) ───────────────────────────────────────────────

#[test]
fn no_auth_class_status_can_become_a_deployment_outcome() {
    // The runtime half. The compile-time half is that `DeploymentOutcome` has
    // no auth variant at all, so a caller cannot route around this classifier
    // by constructing one directly — there is nothing to construct.
    for status in [401, 403] {
        assert_eq!(
            DeploymentOutcome::classify(ProviderResponseSignal::Status(status), None),
            None,
            "HTTP {status} belongs to the credential and account authorities; a deployment that \
             served a request behind a revoked key is not itself unhealthy, and cooling it down \
             would take every sibling deployment sharing that key out of selection"
        );
    }
}

#[test]
fn an_auth_class_status_cannot_be_written_down_as_any_outcome_at_all() {
    // The hole the codex review found: `classify` refusing 401/403 protected
    // only the callers that went through `classify`. A caller holding the
    // status could still *construct* `ServerError { status: 401 }` or
    // `UnusableResponse { status: Some(403) }` and hand it straight to the
    // store door. Both status carriers are now sealed, so the refusal is a
    // property of the vocabulary rather than of one code path through it.
    //
    // The compile-time half — that the private fields cannot be reached at
    // all — is the `compile_fail` doctest on each carrier.
    for status in AUTH_CLASS_STATUSES {
        assert!(
            ServerErrorStatus::new(status).is_none(),
            "HTTP {status} must not be expressible as a server error"
        );
        assert!(
            UnusableResponseStatus::new(status).is_none(),
            "HTTP {status} must not be expressible as an unusable response"
        );
    }

    // …and the carriers stay honest about their own class, so the seal cannot
    // be widened into "any status is a server error".
    assert!(ServerErrorStatus::new(429).is_none());
    assert!(ServerErrorStatus::new(404).is_none());
    assert_eq!(
        ServerErrorStatus::new(500).map(ServerErrorStatus::get),
        Some(500)
    );
    assert_eq!(
        ServerErrorStatus::new(599).map(ServerErrorStatus::get),
        Some(599)
    );
    assert_eq!(
        UnusableResponseStatus::new(422).map(UnusableResponseStatus::get),
        Some(422)
    );
}

#[test]
fn every_outcome_that_reports_a_status_reports_a_non_auth_one() {
    // The property the store door re-checks, stated over the whole
    // vocabulary: whatever a caller builds, `status()` can never hand back an
    // auth-class number.
    let outcomes = [
        DeploymentOutcome::Served,
        DeploymentOutcome::Throttled { retry_after: None },
        DeploymentOutcome::Unreachable,
        server_error(500, None),
        server_error(599, Some(RetryAfter::DeltaSeconds(1))),
        unusable_response(400),
        unusable_response(404),
        unusable_response(422),
        DeploymentOutcome::UnusableResponse { status: None },
    ];
    for outcome in outcomes {
        assert!(
            !outcome.status().is_some_and(is_auth_class_status),
            "{outcome:?} reported an auth-class status"
        );
    }
}

#[test]
fn a_retry_after_cannot_smuggle_an_auth_failure_into_deployment_health() {
    // The one shape that could plausibly slip through: a 401 that also carries
    // a Retry-After header.
    assert_eq!(
        DeploymentOutcome::classify(
            ProviderResponseSignal::Status(401),
            Some(RetryAfter::DeltaSeconds(30))
        ),
        None
    );
}

#[test]
fn throttling_and_quota_are_the_classes_that_cool_a_deployment_down() {
    for status in [429, 402] {
        assert_eq!(
            DeploymentOutcome::classify(
                ProviderResponseSignal::Status(status),
                Some(RetryAfter::DeltaSeconds(30))
            ),
            Some(DeploymentOutcome::Throttled {
                retry_after: Some(RetryAfter::DeltaSeconds(30))
            })
        );
    }
}

#[test]
fn transport_server_and_protocol_failures_are_all_this_authoritys_business() {
    assert_eq!(
        DeploymentOutcome::classify(ProviderResponseSignal::NoResponse, None),
        Some(DeploymentOutcome::Unreachable)
    );
    assert_eq!(
        DeploymentOutcome::classify(ProviderResponseSignal::Status(503), None),
        Some(DeploymentOutcome::ServerError {
            status: ServerErrorStatus::new(503).expect("503 is a server-error status"),
            retry_after: None
        })
    );
    assert_eq!(
        DeploymentOutcome::classify(ProviderResponseSignal::UnusableBody, None),
        Some(DeploymentOutcome::UnusableResponse { status: None })
    );
    assert_eq!(
        DeploymentOutcome::classify(ProviderResponseSignal::Status(404), None),
        Some(DeploymentOutcome::UnusableResponse {
            status: UnusableResponseStatus::new(404)
        })
    );
    assert_eq!(
        DeploymentOutcome::classify(ProviderResponseSignal::Status(200), None),
        Some(DeploymentOutcome::Served)
    );
}

// ─── Retry-After: two forms, one instant ─────────────────────────────────────

/// The instant a `Retry-After` header resolves to, or a panic naming the form
/// that failed. Every HTTP-date case below goes through this, so "parsed at
/// all" and "parsed to the right moment" are never conflated.
fn http_date(raw: &str) -> String {
    match RetryAfter::parse(raw).unwrap_or_else(|| panic!("{raw:?} must parse as an HTTP-date")) {
        RetryAfter::HttpDate(instant) => iso(instant),
        RetryAfter::DeltaSeconds(seconds) => {
            panic!("{raw:?} was read as {seconds} delta-seconds, not as a date")
        }
    }
}

#[test]
fn retry_after_parses_both_forms_the_spec_allows() {
    assert_eq!(
        RetryAfter::parse("120"),
        Some(RetryAfter::DeltaSeconds(120))
    );
    assert_eq!(
        RetryAfter::parse(" 120 "),
        Some(RetryAfter::DeltaSeconds(120))
    );
    assert_eq!(
        http_date("Wed, 21 Oct 2026 07:28:00 GMT"),
        "2026-10-21T07:28:00.000Z"
    );
}

#[test]
fn every_http_date_format_a_recipient_must_accept_is_accepted() {
    // RFC 9110 §5.6.7: a sender emits IMF-fixdate, but a **recipient** must
    // accept all three formats, because deployed servers still emit the
    // obsolete two. An earlier revision refused them and fell back to the class
    // default, which threw away an instruction the provider actually gave
    // (codex review of PR-C, CP3). All three name the same moment here, so the
    // assertion is that the *format* changed and the instant did not.
    assert_eq!(
        http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
        "1994-11-06T08:49:37.000Z",
        "IMF-fixdate"
    );
    assert_eq!(
        http_date("Sunday, 06-Nov-94 08:49:37 GMT"),
        "1994-11-06T08:49:37.000Z",
        "RFC 850"
    );
    assert_eq!(
        http_date("Sun Nov  6 08:49:37 1994"),
        "1994-11-06T08:49:37.000Z",
        "asctime — space-padded day, no zone at all"
    );
    // asctime with a two-digit day is the other half of its grammar.
    assert_eq!(
        http_date("Fri Oct 23 08:49:37 2026"),
        "2026-10-23T08:49:37.000Z",
        "asctime, two-digit day"
    );
    // A near-future RFC 850 date, the only kind a `Retry-After` realistically
    // carries: the two-digit year must land in this century, not the last.
    assert_eq!(
        http_date("Wednesday, 21-Oct-26 07:28:00 GMT"),
        "2026-10-21T07:28:00.000Z",
        "RFC 850 with a two-digit year the provider means as 2026"
    );
}

#[test]
fn an_obsolete_date_form_buys_the_same_cooldown_as_the_modern_one() {
    // The point of accepting all three: the row must not depend on which
    // grammar the provider happened to render.
    let now = DateTime::parse_from_rfc3339("2026-10-21T07:26:30Z")
        .expect("fixed instant")
        .with_timezone(&Utc);
    let renderings = [
        "Wed, 21 Oct 2026 07:28:00 GMT",
        "Wednesday, 21-Oct-26 07:28:00 GMT",
        "Wed Oct 21 07:28:00 2026",
    ];
    for raw in renderings {
        let header = RetryAfter::parse(raw).unwrap_or_else(|| panic!("{raw:?} parses"));
        assert_eq!(
            header.cooldown_secs_from(now),
            90,
            "{raw:?} names a moment 90 seconds out"
        );
    }
}

#[test]
fn an_unparseable_retry_after_is_not_guessed_at() {
    // Junk still falls back to the class default rather than being coerced
    // into a confident wrong instant. What is *not* junk is any of the three
    // HTTP-date formats — see the test above.
    assert_eq!(RetryAfter::parse("soon"), None);
    assert_eq!(RetryAfter::parse(""), None);
    assert_eq!(RetryAfter::parse("-30"), None);
    assert_eq!(
        RetryAfter::parse("2026-10-21T07:28:00Z"),
        None,
        "RFC 3339 is not an HTTP-date; reading it as one would be the guess this refuses"
    );
    assert_eq!(
        RetryAfter::parse("Wed, 21 Oct 2026 07:28:00"),
        None,
        "an HTTP-date with its zone missing is malformed, not asctime"
    );
}

#[test]
fn an_http_date_cooldown_is_measured_from_the_parsed_instant_not_the_string() {
    // The discriminating case: the header names a moment 90 seconds out, in a
    // rendering that sorts *below* `now` as a string. An implementation that
    // compared or subtracted text would read this as "already elapsed".
    let now = DateTime::parse_from_rfc3339("2026-10-21T07:28:00+08:00")
        .expect("fixed instant")
        .with_timezone(&Utc);
    let header = RetryAfter::parse("Tue, 20 Oct 2026 23:29:30 GMT").expect("parses");
    assert_eq!(header.cooldown_secs_from(now), 90);
    assert_eq!(iso(header.cooldown_until(now)), "2026-10-20T23:29:30.000Z");
}

#[test]
fn a_retry_after_in_the_past_still_buys_a_nonzero_cooldown() {
    let now = at(0);
    let past = RetryAfter::HttpDate(at(-600));
    assert_eq!(past.cooldown_secs_from(now), MIN_COOLDOWN_SECS);
    assert_eq!(
        RetryAfter::DeltaSeconds(0).cooldown_secs_from(now),
        MIN_COOLDOWN_SECS
    );
}

#[test]
fn an_unbounded_retry_after_cannot_park_a_deployment_forever() {
    let now = at(0);
    assert_eq!(
        RetryAfter::DeltaSeconds(u64::MAX).cooldown_secs_from(now),
        MAX_COOLDOWN_SECS
    );
    assert_eq!(
        RetryAfter::HttpDate(at(86_400 * 30)).cooldown_secs_from(now),
        MAX_COOLDOWN_SECS
    );
}

#[test]
fn a_throttle_without_a_retry_after_uses_the_class_default() {
    let now = at(0);
    assert_eq!(throttle_cooldown_secs(None, now), 60);
    let write = record(
        None,
        DeploymentOutcome::Throttled { retry_after: None },
        now,
    );
    assert_eq!(write.cooldown_until, Some(at(60)));
    assert_eq!(
        write.health.cooldown_until.as_deref(),
        Some(iso(at(60))).as_deref()
    );
}

// ─── the row a write produces ────────────────────────────────────────────────

#[test]
fn a_throttle_writes_a_cooldown_state_and_its_event() {
    let now = at(0);
    let write = record(
        None,
        DeploymentOutcome::Throttled {
            retry_after: Some(RetryAfter::DeltaSeconds(30)),
        },
        now,
    );

    assert_eq!(write.health.deployment_id, "env:extract");
    assert_eq!(write.health.state, DEPLOYMENT_HEALTH_STATE_COOLDOWN);
    assert_eq!(
        write.health.cooldown_until.as_deref(),
        Some(iso(at(30))).as_deref()
    );
    assert_eq!(write.health.error_count, 1);
    assert_eq!(write.health.evidence_kind, Some(EvidenceKind::SelfReported));
    assert_eq!(write.health.observed_at, iso(now));
    assert_eq!(write.cooldown_until, Some(at(30)));
    assert!(!write.clear_cooldown);

    assert_eq!(
        write.event.event_kind,
        DeploymentEventKind::HealthCooldown.as_str()
    );
    assert_eq!(
        write.event.revision, 7,
        "a health event records the catalog revision it observed; it does not advance one"
    );
    let evidence: Value = serde_json::from_str(&write.event.evidence).expect("evidence is JSON");
    assert_eq!(evidence["outcome"], "throttled");
    assert_eq!(evidence["cooldown_secs"], 30);
    assert_eq!(evidence["cooldown_until"], iso(at(30)));
    assert_eq!(evidence["evidence_kind"], "self_reported");
}

#[test]
fn a_success_clears_the_cooldown_and_the_error_count() {
    let now = at(0);
    let throttled = record(
        None,
        DeploymentOutcome::Throttled {
            retry_after: Some(RetryAfter::DeltaSeconds(30)),
        },
        now,
    );
    let served = record(Some(&throttled.health), DeploymentOutcome::Served, at(60));

    assert_eq!(served.health.state, DEPLOYMENT_HEALTH_STATE_OK);
    assert_eq!(served.health.cooldown_until, None);
    assert_eq!(served.health.error_count, 0);
    assert_eq!(served.health.last_error, None);
    assert_eq!(
        served.health.last_success_at.as_deref(),
        Some(iso(at(60))).as_deref()
    );
    assert!(served.clear_cooldown);
    assert_eq!(
        served.event.event_kind,
        DeploymentEventKind::HealthServed.as_str()
    );
}

#[test]
fn a_server_error_arriving_mid_cooldown_does_not_shorten_it() {
    // The realistic sequence: a 429 parks the deployment for 30s, and eight
    // seconds later a 503 from the same deployment lands. Overwriting
    // `cooldown_until` with "nothing" would make it selectable again early —
    // a fail-open that only shows up under load.
    let now = at(0);
    let throttled = record(
        None,
        DeploymentOutcome::Throttled {
            retry_after: Some(RetryAfter::DeltaSeconds(30)),
        },
        now,
    );
    let errored = record(Some(&throttled.health), server_error(503, None), at(8));

    assert_eq!(
        errored.health.cooldown_until.as_deref(),
        Some(iso(at(30))).as_deref(),
        "an outcome that sets no cooldown must leave the running one alone"
    );
    assert_eq!(errored.health.state, DEPLOYMENT_HEALTH_STATE_ERROR);
    assert_eq!(errored.health.error_count, 2);
    assert_eq!(errored.cooldown_until, None);
}

#[test]
fn a_server_error_honours_an_explicit_retry_after_but_invents_none() {
    let now = at(0);
    let without = record(None, server_error(500, None), now);
    assert_eq!(without.health.cooldown_until, None);
    assert_eq!(without.health.state, DEPLOYMENT_HEALTH_STATE_ERROR);

    let with = record(
        None,
        server_error(503, Some(RetryAfter::DeltaSeconds(15))),
        now,
    );
    assert_eq!(with.cooldown_until, Some(at(15)));
}

#[test]
fn the_recorded_error_text_is_generated_never_provider_supplied() {
    // There is no parameter through which a provider's response body could
    // reach `last_error`; this pins what does reach it.
    let now = at(0);
    assert_eq!(
        record(None, server_error(503, None), now)
            .health
            .last_error
            .as_deref(),
        Some("provider returned HTTP 503")
    );
    assert_eq!(
        record(None, DeploymentOutcome::Unreachable, now)
            .health
            .last_error
            .as_deref(),
        Some("no response from the provider")
    );
    assert_eq!(
        record(None, unusable_response(422), now)
            .health
            .last_error
            .as_deref(),
        Some("unusable response (HTTP 422)")
    );
    assert_eq!(
        record(
            None,
            DeploymentOutcome::Throttled {
                retry_after: Some(RetryAfter::DeltaSeconds(30))
            },
            now
        )
        .health
        .last_error
        .as_deref(),
        Some("throttled by the provider; retry after 30s")
    );
}

#[test]
fn a_write_names_its_own_deployment_even_from_a_stale_row() {
    let now = at(0);
    let mut stale = new_deployment_health("env:summary", &iso(at(-1)));
    stale.state = DEPLOYMENT_HEALTH_STATE_ERROR.to_string();
    let write = record(Some(&stale), DeploymentOutcome::Served, now);
    assert_eq!(
        write.health.deployment_id, "env:extract",
        "a stale id on the supplied row must never redirect a write at another deployment"
    );
    assert_eq!(write.event.deployment_id, "env:extract");
}

#[test]
fn metadata_keeps_foreign_keys_and_drops_stale_ones() {
    let now = at(0);
    let mut existing = new_deployment_health("env:extract", &iso(at(-10)));
    existing.metadata =
        json!({"probe_note": "kept", "status": 503, "cooldown_secs": 60}).to_string();
    let write = record(Some(&existing), DeploymentOutcome::Served, now);
    let metadata: Value = serde_json::from_str(&write.health.metadata).expect("metadata is JSON");
    assert_eq!(metadata["probe_note"], "kept");
    assert_eq!(metadata["outcome"], "served");
    assert!(
        metadata.get("status").is_none(),
        "a status left over from an earlier outcome must not survive one that has none"
    );
    assert!(metadata.get("cooldown_secs").is_none());
}

#[test]
fn unreadable_metadata_is_replaced_rather_than_parsed() {
    let now = at(0);
    let mut existing = new_deployment_health("env:extract", &iso(at(-10)));
    existing.metadata = "not json at all".to_string();
    let write = record(Some(&existing), DeploymentOutcome::Unreachable, now);
    let metadata: Value = serde_json::from_str(&write.health.metadata).expect("metadata is JSON");
    assert_eq!(metadata["outcome"], "unreachable");
}

#[test]
fn every_outcome_maps_to_exactly_one_event_kind_and_state() {
    let cases = [
        (
            DeploymentOutcome::Served,
            DEPLOYMENT_HEALTH_STATE_OK,
            DeploymentEventKind::HealthServed,
        ),
        (
            DeploymentOutcome::Throttled { retry_after: None },
            DEPLOYMENT_HEALTH_STATE_COOLDOWN,
            DeploymentEventKind::HealthCooldown,
        ),
        (
            DeploymentOutcome::Unreachable,
            DEPLOYMENT_HEALTH_STATE_ERROR,
            DeploymentEventKind::HealthError,
        ),
        (
            server_error(500, None),
            DEPLOYMENT_HEALTH_STATE_ERROR,
            DeploymentEventKind::HealthError,
        ),
        (
            DeploymentOutcome::UnusableResponse { status: None },
            DEPLOYMENT_HEALTH_STATE_ERROR,
            DeploymentEventKind::HealthError,
        ),
    ];
    for (outcome, state, kind) in cases {
        assert_eq!(outcome.state(), state);
        assert_eq!(outcome.event_kind(), kind);
        assert!(
            kind.is_health(),
            "every outcome this writer records must be a health event, or the fold would read it \
             as a change to what the deployment is"
        );
        assert_eq!(
            kind.health_state(),
            Some(state),
            "the state the writer stores and the state the fold reconstructs from the event kind \
             are one mapping; if they drift, a replay silently reports a different health state \
             than the table holds"
        );
        let write = record(None, outcome, at(0));
        assert_eq!(write.health.state, state);
        assert_eq!(write.event.event_kind, kind.as_str());
    }
}
