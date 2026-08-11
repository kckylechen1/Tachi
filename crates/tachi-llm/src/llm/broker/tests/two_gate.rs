//! The two-gate law, tested at the type level.
//!
//! The claim is that **passing admission does not let a caller select a
//! credential**, and that an adapter cannot leak one because it never gets one.
//! The compile-fail doctests on
//! [`CanonicalInvocationRequest`](super::CanonicalInvocationRequest) and
//! [`AuthMaterialRef`](super::AuthMaterialRef) pin the "no such field / no such
//! accessor" half. This module pins the runtime half: the *serialized* request
//! schema has no credential-shaped key, and a built wire request carries a
//! placement rather than a header value.
//!
//! Both halves are needed. A doctest proves nothing about a field added later
//! whose name the doctest does not mention; the field-name sweep below catches
//! any new field whose name looks like a credential, and the wire-request test
//! catches an adapter that starts embedding material even if the field list
//! never changes.

use super::*;

/// The canonical request's frozen top-level field list.
///
/// The strongest half of this file: it catches **any** new field, whatever it
/// is called, so a credential selector cannot slip in under an innocent name.
/// Adding a field here is a deliberate act that has to be argued for in review.
const FROZEN_REQUEST_FIELDS: &[&str] = &[
    "admitted",
    "budget",
    "cancellation",
    "data_policy",
    "deadline",
    "idempotency_key",
    "messages",
    "response_format",
    "sampling",
    "stream",
    "target",
    "tool_choice",
    "tools",
];

/// Substrings that must never appear in *any* key of a serialized request,
/// however deeply nested.
///
/// Narrower than "anything containing `key` or `token`" on purpose:
/// `idempotency_key` and `max_total_tokens` are legitimate and the sweep has to
/// stay useful rather than be suppressed by a growing exception list. These are
/// the spellings an actual credential selector would use.
const CREDENTIAL_SHAPED: &[&str] = &[
    "secret",
    "credential",
    "vault",
    "api_key",
    "apikey",
    "password",
    "passphrase",
    "bearer",
    "rotation",
    "access_token",
    "refresh_token",
    "auth_token",
    "private_key",
    "keyring",
    "key_env",
    "key_id",
];

fn collect_keys(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                out.push(key.clone());
                collect_keys(nested, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_keys(item, out);
            }
        }
        _ => {}
    }
}

#[test]
fn the_request_schema_has_no_credential_shaped_field() {
    let mut parts = minimal_parts();
    // Populate every optional field so none of them hides from the sweep.
    parts.idempotency_key = Some(IdempotencyKey::new("idem-1").expect("valid"));
    parts.budget = BudgetConstraint {
        max_total_tokens: Some(1000),
        max_cost_micros: Some(2000),
        pricing_snapshot_ref: Some("pricing-snap-1".to_string()),
    };
    parts.deadline = DeadlineContext {
        total_ms: Some(30_000),
        connect_ms: Some(2_000),
        first_byte_ms: Some(10_000),
    };
    parts.cancellation = CancellationContext {
        scope_ref: Some("scope-1".to_string()),
        retry_on_outcome_unknown: false,
    };
    parts.data_policy.required_residency = Some("eu".to_string());
    parts.admitted.task_ref = Some("task-1".to_string());
    parts.tools = vec![ToolDeclaration {
        name: "search".to_string(),
        description: Some("d".to_string()),
        parameters: json!({"type": "object"}),
    }];
    parts.response_format = ResponseFormat::JsonSchema {
        name: "answer".to_string(),
        strict: true,
        schema: json!({"type": "object"}),
    };
    let request = CanonicalInvocationRequest::new(parts).expect("valid");

    let serialized = serde_json::to_value(&request).expect("request serializes");

    let mut top_level: Vec<&str> = serialized
        .as_object()
        .expect("a request serializes to an object")
        .keys()
        .map(String::as_str)
        .collect();
    top_level.sort_unstable();
    assert_eq!(
        top_level, FROZEN_REQUEST_FIELDS,
        "the canonical request's field list changed — every field here is \
         caller-settable and crosses the admission boundary, so a new one is a \
         review decision, not a refactor"
    );

    let mut keys = Vec::new();
    collect_keys(&serialized, &mut keys);
    for key in &keys {
        for needle in CREDENTIAL_SHAPED {
            assert!(
                !key.to_ascii_lowercase().contains(needle),
                "canonical request field {key:?} matches credential-shaped {needle:?}: \
                 a field a caller can set must never be able to name a credential \
                 (gate 1 is admission, gate 2 is the lease)"
            );
        }
    }
    assert!(
        keys.len() > 20,
        "the sweep found only {} keys — it is not actually walking the request",
        keys.len()
    );
}

#[test]
fn a_built_wire_request_carries_a_placement_not_a_header_value() {
    let adapter = OpenAiCompatWire::new();
    let lease_ref = "lease-that-must-not-appear-anywhere";
    let auth = AuthMaterialRef::leased(AuthMaterialKind::ApiKey, lease_ref);
    let built = adapter
        .build_request(&minimal_request(), auth)
        .expect("minimal request builds");

    assert_eq!(
        built.auth_placement(),
        &AuthPlacement::Header {
            name: "authorization",
            prefix: "Bearer ",
        },
        "the adapter must declare where material goes, and let the executor put it there"
    );
    assert!(
        built
            .headers()
            .iter()
            .all(|header| header.name != "authorization"),
        "the adapter set an authorization header itself — it has no material to \
         put in one, so any value there is either empty or forged"
    );

    // Nothing auth-shaped reaches the bytes, not even the opaque lease id.
    let rendered = format!(
        "{} {} {:?} {}",
        built.method().as_str(),
        built.url(),
        built.headers(),
        built.body_utf8().expect("body is UTF-8")
    );
    assert!(
        !rendered.contains(lease_ref),
        "the lease reference reached the wire request: {rendered}"
    );
    assert!(
        !rendered.to_ascii_lowercase().contains("bearer"),
        "the wire request body/headers mention a bearer scheme: {rendered}"
    );
}

#[test]
fn auth_material_ref_exposes_only_a_kind_and_an_opaque_reference() {
    let auth = AuthMaterialRef::leased(AuthMaterialKind::BearerToken, "lease-9");
    assert_eq!(auth.kind(), AuthMaterialKind::BearerToken);
    assert_eq!(auth.lease_ref(), "lease-9");
    // Debug is the other common leak path; there is nothing to leak, and this
    // asserts nobody added a material field later.
    let rendered = format!("{auth:?}");
    assert!(rendered.contains("lease-9"));
    assert!(rendered.contains("BearerToken"));
    assert_eq!(
        rendered.matches("lease-9").count(),
        1,
        "AuthMaterialRef grew a second string field: {rendered}"
    );
}

#[test]
fn the_adapter_holds_no_endpoint_and_no_state() {
    // A sans-IO adapter is a function table. If it ever grows a field that is
    // not a capability set, it has started holding something — a client, an
    // endpoint, a key — and the "no second HTTP pool" guarantee stops being
    // structural.
    assert_eq!(
        std::mem::size_of::<OpenAiCompatWire>(),
        std::mem::size_of::<WireCapabilities>(),
        "OpenAiCompatWire grew state beyond its capability set"
    );
    // Two adapters built the same way are indistinguishable — there is no
    // per-instance connection or credential to tell apart.
    assert_eq!(OpenAiCompatWire::new(), OpenAiCompatWire::new());
}

#[test]
fn admitted_refs_are_recorded_but_reach_no_wire_byte() {
    // Recorded, never authorizing — and never sent to the provider either.
    // Leaking a local uid to a third party is a privacy bug, not just an
    // authority one.
    let mut parts = minimal_parts();
    parts.admitted = AdmittedRefs {
        caller_ref: "uid:99887766".to_string(),
        host_ref: Some("host:workstation-7".to_string()),
        task_ref: Some("task:abc123".to_string()),
    };
    let request = CanonicalInvocationRequest::new(parts).expect("valid");
    let built = OpenAiCompatWire::new()
        .build_request(&request, api_key_lease())
        .expect("builds");
    let body = built.body_utf8().expect("body is UTF-8");
    for leak in ["uid:99887766", "host:workstation-7", "task:abc123"] {
        assert!(
            !body.contains(leak),
            "admission ref {leak:?} was sent to the provider: {body}"
        );
    }
}
