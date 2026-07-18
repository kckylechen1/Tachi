// extract_quality_golden.rs — #1198: discriminative golden eval for the
// fact-extraction lane (supersede / noise / JSON-stability).
//
// `extract_facts` (crates/tachi-llm/src/llm/chat_lanes/generators.rs) had
// zero behavioral coverage before this file: `tests/chat_lanes.rs` only
// exercises the transport layer (429 retry, key rotation, response
// parsing), never whether the extracted facts are actually *good*.
//
// Per the issue's CI-safety split (nightly real-model grading vs CI mock
// pipeline): every test here runs against a **mocked** chat-completions
// server returning canned `content` strings — zero real LLM calls, fully
// deterministic. Each scenario is discriminative: a hand-crafted "real"
// (correct) extractor response must pass the judgment predicate, and a
// hand-crafted "broken" (plausible-but-wrong) response must fail it. A
// predicate that can't fail on a broken fixture is a smoke test, not a
// golden — that's exactly the gap this file exists to close.
//
// #1197/#1198 dependency note: these tests use `LlmClient::new_with_config`
// (single-tier, no fallback) — #1198 does not need the cross-provider
// fallback chain from #1197 at all. They are independent; sharing this
// crate/lane is the only thing that ties them together.

use super::super::{ChatLaneConfig, ProviderRuntimeConfig};
use super::*;

fn unused_lane(key_env: &'static str) -> ChatLaneConfig {
    ChatLaneConfig {
        base_url: "https://unused.test/v1/chat/completions".to_string(),
        model: "unused".to_string(),
        api_key_envs: vec![key_env],
    }
}

fn chat_response_with_content(content: &str) -> Value {
    json!({
        "choices": [{
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })
}

/// Spawns a mock chat-completions provider that serves a fixed sequence of
/// `content` strings in order (one per request), repeating the last one if
/// more requests arrive than responses were queued. Returns the client base
/// URL and the server task handle.
async fn spawn_content_sequence_server(
    contents: Vec<String>,
) -> (String, tokio::task::JoinHandle<()>) {
    use axum::{extract::State, routing::post, Json, Router};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    let queue = Arc::new(Mutex::new(VecDeque::from(contents)));
    let app = Router::new()
        .route(
            "/chat/completions",
            post(
                |State(queue): State<Arc<Mutex<VecDeque<String>>>>| async move {
                    let mut queue = queue.lock().unwrap_or_else(|e| e.into_inner());
                    let content = queue.pop_front().unwrap_or_else(|| "[]".to_string());
                    Json(chat_response_with_content(&content))
                },
            ),
        )
        .with_state(queue);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind extract-quality mock provider");
    let port = listener.local_addr().expect("mock addr").port();
    let base_url = format!("http://127.0.0.1:{port}/chat/completions");
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("extract-quality mock provider");
    });
    (base_url, task)
}

/// A client with only the extract lane wired to the mock provider; the other
/// three lanes point at unconfigured keys and are never exercised by these
/// tests.
fn extract_only_client(base_url: String) -> LlmClient {
    let config = ProviderRuntimeConfig {
        extract: ChatLaneConfig {
            base_url,
            model: "mock-extract-model".to_string(),
            api_key_envs: vec!["__1198_EXTRACT_KEY"],
        },
        summary: unused_lane("__1198_UNUSED_SUMMARY"),
        reasoning: unused_lane("__1198_UNUSED_REASONING"),
        distill: unused_lane("__1198_UNUSED_DISTILL"),
        rerank: RerankConfig {
            provider: RerankProviderKind::Voyage,
            local_endpoint: None,
        },
    };
    let client = LlmClient::new_with_config(config, None).expect("client should initialize");
    client.set_provider_secret_pool(
        "__1198_EXTRACT_KEY",
        vec![ProviderSecret {
            key_id: "__1198_EXTRACT_KEY".to_string(),
            value: "test-key".to_string(),
        }],
    );
    client
}

// ── Judgment predicates (the "golden" part — reused verbatim against real
//    model output in the nightly job the issue describes; CI only checks
//    they discriminate the two fixtures below). ──────────────────────────

/// Update recognition + history retention: the old term must still appear
/// *somewhere* in the extracted facts (not silently dropped), tagged with a
/// supersession signal, and the new term must also appear.
fn facts_capture_supersession(facts: &[Value], old_term: &str, new_term: &str) -> bool {
    let corpus: String = facts
        .iter()
        .filter_map(|f| f.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" \n ");
    let mentions_old = corpus.contains(old_term);
    let mentions_new = corpus.contains(new_term);
    let signals_supersession = ["superseded", "switched", "replaced", "migrat", "废弃", "替代"]
        .iter()
        .any(|signal| corpus.to_ascii_lowercase().contains(signal));
    mentions_old && mentions_new && signals_supersession
}

/// Noise filtering: none of the extracted facts' text may contain any of the
/// known-junk phrases from the source conversation.
fn facts_exclude_noise(facts: &[Value], noise_phrases: &[&str]) -> bool {
    facts
        .iter()
        .filter_map(|f| f.get("text").and_then(Value::as_str))
        .all(|text| noise_phrases.iter().all(|noise| !text.contains(noise)))
}

/// JSON-stability / schema-holds: every fact is an object carrying all five
/// required keys with the right JSON types, `scope` is one of the three
/// allowed values, and `importance` is a number in `[0.0, 1.0]`.
fn facts_json_shape_holds(facts: &[Value]) -> bool {
    if facts.is_empty() {
        return false;
    }
    facts.iter().all(|fact| {
        let Some(obj) = fact.as_object() else {
            return false;
        };
        let text_ok = obj
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty());
        let topic_ok = obj.get("topic").and_then(Value::as_str).is_some();
        let keywords_ok = obj.get("keywords").is_some_and(Value::is_array);
        let entities_ok = obj.get("entities").is_some_and(Value::is_array);
        let scope_ok = obj
            .get("scope")
            .and_then(Value::as_str)
            .is_some_and(|s| matches!(s, "user" | "project" | "general"));
        let importance_ok = obj
            .get("importance")
            .and_then(Value::as_f64)
            .is_some_and(|v| (0.0..=1.0).contains(&v));
        text_ok && topic_ok && keywords_ok && entities_ok && scope_ok && importance_ok
    })
}

// ── Scenario 1: supersede ────────────────────────────────────────────────

const REAL_SUPERSEDE_RESPONSE: &str = r#"[
  {"text": "Team initially chose backend A for auth", "topic": "auth backend", "keywords": ["auth", "backend-a"], "entities": ["backend A"], "scope": "project", "importance": 0.5},
  {"text": "Team switched auth backend from backend A to backend B; backend A is now superseded", "topic": "auth backend", "keywords": ["auth", "backend-b", "migration"], "entities": ["backend A", "backend B"], "scope": "project", "importance": 0.8}
]"#;

/// A plausible *broken* extractor: it correctly picks up the final decision
/// but silently drops the superseded history — exactly the failure mode
/// #1198's "历史保留" axis exists to catch.
const BROKEN_SUPERSEDE_RESPONSE: &str = r#"[
  {"text": "Team uses backend B for auth", "topic": "auth", "keywords": ["auth"], "entities": ["backend B"], "scope": "project", "importance": 0.5}
]"#;

#[tokio::test]
async fn extract_facts_supersede_golden_discriminates_real_vs_broken() {
    let (real_url, real_task) =
        spawn_content_sequence_server(vec![REAL_SUPERSEDE_RESPONSE.to_string()]).await;
    let real_client = extract_only_client(real_url);
    let real_facts = real_client
        .extract_facts("we decided backend A for auth, then later switched to backend B")
        .await
        .expect("real extraction response should parse");
    assert!(
        facts_capture_supersession(&real_facts, "backend A", "backend B"),
        "GREEN case: real extraction must retain the superseded backend A history \
         and signal the switch to backend B, got: {real_facts:?}"
    );
    real_task.abort();

    let (broken_url, broken_task) =
        spawn_content_sequence_server(vec![BROKEN_SUPERSEDE_RESPONSE.to_string()]).await;
    let broken_client = extract_only_client(broken_url);
    let broken_facts = broken_client
        .extract_facts("we decided backend A for auth, then later switched to backend B")
        .await
        .expect("broken extraction response is still valid JSON — it fails the *judgment*, not parsing");
    assert!(
        !facts_capture_supersession(&broken_facts, "backend A", "backend B"),
        "RED case: a broken extractor that drops the superseded history must \
         fail the supersession predicate, got: {broken_facts:?}"
    );
    broken_task.abort();
}

// ── Scenario 2: noise filtering ──────────────────────────────────────────

const REAL_NOISE_RESPONSE: &str = r#"[
  {"text": "Decided to migrate the recall path to the new rerank provider", "topic": "recall", "keywords": ["recall", "rerank"], "entities": ["rerank provider"], "scope": "project", "importance": 0.6}
]"#;

/// A broken extractor that treats small talk as memorable facts — the exact
/// "junk input doesn't produce spurious facts" regression #1198 asks for.
const BROKEN_NOISE_RESPONSE: &str = r#"[
  {"text": "Decided to migrate the recall path to the new rerank provider", "topic": "recall", "keywords": ["recall", "rerank"], "entities": ["rerank provider"], "scope": "project", "importance": 0.6},
  {"text": "today's weather is hot", "topic": "small talk", "keywords": ["weather"], "entities": [], "scope": "general", "importance": 0.1},
  {"text": "just had a glass of water", "topic": "small talk", "keywords": ["water"], "entities": [], "scope": "general", "importance": 0.1}
]"#;

#[tokio::test]
async fn extract_facts_noise_golden_discriminates_real_vs_broken() {
    let noise_phrases = ["today's weather is hot", "just had a glass of water"];
    let source_text = "Decided to migrate the recall path to the new rerank provider. \
         By the way, today's weather is hot, and I just had a glass of water.";

    let (real_url, real_task) =
        spawn_content_sequence_server(vec![REAL_NOISE_RESPONSE.to_string()]).await;
    let real_client = extract_only_client(real_url);
    let real_facts = real_client
        .extract_facts(source_text)
        .await
        .expect("real extraction response should parse");
    assert!(
        facts_exclude_noise(&real_facts, &noise_phrases),
        "GREEN case: real extraction must filter out conversational noise, got: {real_facts:?}"
    );
    real_task.abort();

    let (broken_url, broken_task) =
        spawn_content_sequence_server(vec![BROKEN_NOISE_RESPONSE.to_string()]).await;
    let broken_client = extract_only_client(broken_url);
    let broken_facts = broken_client
        .extract_facts(source_text)
        .await
        .expect("broken extraction response is still valid JSON");
    assert!(
        !facts_exclude_noise(&broken_facts, &noise_phrases),
        "RED case: a broken extractor that turns small talk into facts must \
         fail the noise-filtering predicate, got: {broken_facts:?}"
    );
    broken_task.abort();
}

// ── Scenario 3: JSON-stability across repeated calls ─────────────────────

const REAL_STABLE_RESPONSE_1: &str = r#"[{"text": "First stable fact", "topic": "t1", "keywords": ["k1"], "entities": [], "scope": "user", "importance": 0.4}]"#;
const REAL_STABLE_RESPONSE_2: &str = r#"[{"text": "Second stable fact", "topic": "t2", "keywords": ["k2"], "entities": [], "scope": "project", "importance": 0.5}]"#;
const REAL_STABLE_RESPONSE_3: &str = r#"[{"text": "Third stable fact", "topic": "t3", "keywords": ["k3"], "entities": [], "scope": "general", "importance": 0.6}]"#;

#[tokio::test]
async fn extract_facts_json_stability_golden_holds_across_three_consecutive_calls() {
    let (url, task) = spawn_content_sequence_server(vec![
        REAL_STABLE_RESPONSE_1.to_string(),
        REAL_STABLE_RESPONSE_2.to_string(),
        REAL_STABLE_RESPONSE_3.to_string(),
    ])
    .await;
    let client = extract_only_client(url);

    for call_index in 0..3 {
        let facts = client
            .extract_facts("some memory-worthy text")
            .await
            .unwrap_or_else(|e| panic!("call {call_index} should parse: {e}"));
        assert!(
            facts_json_shape_holds(&facts),
            "GREEN case: call {call_index} must hold full fact schema, got: {facts:?}"
        );
    }

    task.abort();
}

/// RED case: a provider that returns a syntactically malformed payload on
/// the second call (trailing comma) — a real, previously-seen regression
/// class in this codebase (`extract_json_payload` / `serde_json::from_str`
/// failures) — must surface as a typed `Err` from `extract_facts`, not a
/// panic, not a silently-empty `Ok(vec![])`, and not a schema-shape pass.
#[tokio::test]
async fn extract_facts_json_stability_golden_fails_loud_on_malformed_payload() {
    let malformed = r#"[{"text": "ok fact", "topic": "t", "keywords": [], "entities": [], "scope": "user", "importance": 0.5,}]"#;
    let (url, task) = spawn_content_sequence_server(vec![malformed.to_string()]).await;
    let client = extract_only_client(url);

    let err = client
        .extract_facts("some memory-worthy text")
        .await
        .expect_err("a trailing-comma-malformed payload must be a typed parse error, not Ok");
    assert!(
        err.contains("Failed to parse facts JSON"),
        "got: {err}"
    );

    task.abort();
}
