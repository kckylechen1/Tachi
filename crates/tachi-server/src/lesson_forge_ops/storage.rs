//! Persist a forged `LessonCandidateV1` as a pending row.
//!
//! "Emit pending `LessonCandidateV1`; never write established `/precedents`
//! rows." (frozen contract). Every row this module writes lands at
//! `/lesson_candidates/<project>/<candidate_id>` — never `/precedents` —
//! and carries `metadata.candidate_status = "pending"` unconditionally
//! (already type-guaranteed by `LessonCandidateStatusV1`'s single
//! constructible variant; this module just serializes what the type
//! already enforces).
//!
//! Reuses `save_eval_memory` — the same internal capture path
//! `precedent_candidate_ops` (#1076) uses — rather than a second bespoke
//! persistence route, so this leaf's writes get the identical scrub/
//! validate/enrich/#1041-write-affinity treatment every other memory entry
//! gets.

use serde_json::{json, Value};

use crate::memory_search_ops::save_eval_memory;
use crate::tool_params::SaveMemoryParams;
use crate::MemoryServer;
use tachi_params::LessonCandidateV1;

pub(crate) const LESSON_CANDIDATE_DOMAIN: &str = "lesson_candidate";

/// Scrub every free-text prose field of a candidate for secrets, returning a
/// scrubbed clone plus the total redaction count.
///
/// `save_eval_memory`'s pipeline (`handle_save_memory`) only scrubs `text`/
/// `summary` on the way in — `metadata` is opaque to it. `build_metadata`
/// below copies the candidate's raw `situation`/`proposed_ruling`/`why`/
/// `how_to_apply` fields verbatim, so without this step a model-authored
/// draft containing a leaked secret would land redacted in `text` (the
/// pipeline's own pass) but UN-redacted in `metadata` — a secret-leak side
/// channel the pipeline's scrub was never asked to close. Scrubbing here,
/// before `render_body`/`summary_line`/`build_metadata` all run, guarantees
/// all three carry identical redactions. Same discipline as
/// `precedent_ops::scrub_ruling`, which exists for the identical reason
/// (see that function's doc, `precedent_ops.rs:164-170`).
fn scrub_candidate(candidate: &LessonCandidateV1) -> (LessonCandidateV1, usize) {
    let mut scrubbed = candidate.clone();
    let mut redactions = 0usize;
    let (situation, c) = crate::memory_search_ops::scrub_secrets(&scrubbed.situation);
    scrubbed.situation = situation;
    redactions += c;
    let (proposed_ruling, c) = crate::memory_search_ops::scrub_secrets(&scrubbed.proposed_ruling);
    scrubbed.proposed_ruling = proposed_ruling;
    redactions += c;
    let (why, c) = crate::memory_search_ops::scrub_secrets(&scrubbed.why);
    scrubbed.why = why;
    redactions += c;
    let (how_to_apply, c) = crate::memory_search_ops::scrub_secrets(&scrubbed.how_to_apply);
    scrubbed.how_to_apply = how_to_apply;
    redactions += c;
    (scrubbed, redactions)
}

fn render_body(candidate: &LessonCandidateV1) -> String {
    let mut lines = vec![format!(
        "Lesson candidate ({}) — pending",
        candidate.kind.as_str()
    )];
    lines.push(format!("Situation: {}", candidate.situation));
    lines.push(format!("Proposed ruling: {}", candidate.proposed_ruling));
    lines.push(format!("Why: {}", candidate.why));
    lines.push(format!("How to apply: {}", candidate.how_to_apply));
    lines.push(format!(
        "Source: {}@{}",
        candidate.source_row_id, candidate.source_revision
    ));
    if candidate.refs.is_empty() {
        lines.push("Refs: none".to_string());
    } else {
        lines.push(format!("Refs ({}):", candidate.refs.len()));
        for r in &candidate.refs {
            lines.push(format!(
                "  - {:?} {:?} {}",
                r.relation, r.target_kind, r.target_ref
            ));
        }
    }
    lines.push(format!(
        "Coverage: {}/{} bytes (full={})",
        candidate.coverage.covered_bytes,
        candidate.coverage.source_bytes,
        candidate.coverage.is_full()
    ));
    lines.push(format!(
        "Candidate status: {}",
        candidate.candidate_status.as_str()
    ));
    lines.join("\n")
}

/// `candidate` must already be scrubbed (see `scrub_candidate`); `redactions`
/// is surfaced so a scrubbed row is visibly marked, matching the eval-record
/// convention (`complete_ops::eval_record`) and `precedent_ops::build_metadata`.
fn build_metadata(candidate: &LessonCandidateV1, redactions: usize) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("kind".to_string(), json!("lesson_candidate"));
    // Unconditional — see module doc. `LessonCandidateStatusV1` has no
    // other variant to construct, so this is never anything but "pending".
    map.insert(
        "candidate_status".to_string(),
        json!(candidate.candidate_status.as_str()),
    );
    map.insert("lesson_kind".to_string(), json!(candidate.kind.as_str()));
    map.insert("situation".to_string(), json!(candidate.situation));
    map.insert(
        "proposed_ruling".to_string(),
        json!(candidate.proposed_ruling),
    );
    map.insert("why".to_string(), json!(candidate.why));
    map.insert("how_to_apply".to_string(), json!(candidate.how_to_apply));
    map.insert("refs".to_string(), json!(candidate.refs));
    map.insert("source_row_id".to_string(), json!(candidate.source_row_id));
    map.insert(
        "source_revision".to_string(),
        json!(candidate.source_revision),
    );
    map.insert(
        "coverage".to_string(),
        json!({
            "source_bytes": candidate.coverage.source_bytes,
            "covered_bytes": candidate.coverage.covered_bytes,
        }),
    );
    map.insert(
        "identity_status".to_string(),
        json!(candidate.identity_status()),
    );
    map.insert(
        "engine_receipt".to_string(),
        json!(candidate.engine_receipt),
    );
    map.insert(
        "candidate_group_id".to_string(),
        json!(candidate.candidate_group_id),
    );
    map.insert(
        "established".to_string(),
        json!(candidate.claims_establishment()),
    );
    if redactions > 0 {
        map.insert("secret_redactions".to_string(), json!(redactions));
        map.insert(
            "secret_redaction_warning".to_string(),
            json!("Potential secrets were redacted from this lesson candidate before persistence."),
        );
    }
    Value::Object(map)
}

fn summary_line(candidate: &LessonCandidateV1, max: usize) -> String {
    let prefix = "[lesson-candidate] ";
    let mut s = format!("{prefix}{}", candidate.situation);
    if s.chars().count() > max {
        let truncated: String = s.chars().take(max.saturating_sub(1)).collect();
        s = format!("{truncated}\u{2026}");
    }
    s
}

fn extract_persisted_id(saved: &Value) -> Result<String, String> {
    saved
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("save_memory response carried no `id`: {saved}"))
}

/// A stable, filesystem/URL-safe path token for `project`. Same fallback
/// convention as `precedent_ops::project_segment`: this is ONLY the display
/// segment in the `/lesson_candidates/<segment>/<id>` path — it is never
/// fed back into `SaveMemoryParams.project` (see
/// `persist_pending_lesson_candidate`'s doc for why those two are kept
/// deliberately separate).
fn project_path_segment(project: Option<&str>) -> String {
    let raw = project
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("global");
    let slug: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = slug.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "global".to_string()
    } else {
        trimmed
    }
}

/// Persist one pending lesson candidate.
///
/// `project`/`project_explicit` deliberately mirror
/// `precedent_ops::record_complete_rulings`'s own convention rather than
/// inventing a new one: `project` is threaded straight into
/// `SaveMemoryParams.project` UNCHANGED (`None` stays `None` — a caller
/// with no explicit project decision lets the #1041 write-affinity gate and
/// the server's own bound-session resolution place the row, the same "same
/// store by construction" shape `write_affinity.rs`'s module doc carves out
/// for foundry distill), and `project_explicit` is the caller's own
/// resolved signal, never re-derived from `project.is_some()` here (#1041
/// B7: presence alone can't distinguish a genuine caller decision from a
/// transport-injected default). Only the PATH's display segment
/// (`project_path_segment`) falls back to `"global"` when `project` is
/// `None` — a cosmetic path convention, not a routing decision.
///
/// `#[allow(dead_code)]`: this is the one genuinely `pub(crate)`-bound leaf
/// in the module tree (blocked from going fully `pub` by `MemoryServer`'s
/// own `pub(crate)` visibility — see `mod.rs`'s doc on the `storage`
/// declaration). Its only caller today is this file's own tests; a
/// follow-up harness-runner leaf (module doc, "What this module explicitly
/// does NOT do") is the intended production caller. Flagged here rather
/// than silently suppressed at the module level.
#[allow(dead_code)]
pub(crate) async fn persist_pending_lesson_candidate(
    server: &MemoryServer,
    project: Option<&str>,
    project_explicit: bool,
    candidate: &LessonCandidateV1,
) -> Result<String, String> {
    // Scrub BEFORE rendering body/summary/metadata so all three carry
    // identical redactions — see `scrub_candidate`'s doc for why the
    // pipeline's own `text`-only scrub isn't enough.
    let (candidate, redactions) = scrub_candidate(candidate);
    let candidate = &candidate;

    let path = format!(
        "/lesson_candidates/{}/{}",
        project_path_segment(project),
        candidate.candidate_id
    );
    let text = render_body(candidate);
    let summary = summary_line(candidate, 80);
    let metadata = build_metadata(candidate, redactions);

    let keywords = vec![
        "lesson_candidate".to_string(),
        "pending".to_string(),
        candidate.kind.as_str().to_string(),
        candidate.source_row_id.clone(),
    ];

    let params = SaveMemoryParams {
        text,
        summary,
        path,
        importance: 0.6,
        category: "decision".to_string(),
        topic: candidate.situation.clone(),
        keywords,
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        scope: "project".to_string(),
        vector: None,
        id: None,
        force: false,
        // A pending, model-authored, not-yet-established candidate must not
        // auto-link into the graph as though it were vetted content — the
        // same "no influence before establishment" boundary
        // (`issue-refinery-memory-lanes.md:346-349`) that motivates
        // excluding lesson-candidate rows from generic recall below
        // (`filters.rs`'s `is_lesson_candidate_entry`).
        auto_link: false,
        project: project.map(str::to_string),
        project_explicit,
        retention_policy: None,
        domain: Some(LESSON_CANDIDATE_DOMAIN.to_string()),
        timestamp: None,
        valid_from: None,
        valid_until: None,
        metadata: Some(metadata),
        emit_continuity: false,
    };

    let raw = save_eval_memory(server, params).await?;
    let saved: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({ "raw": raw }));
    extract_persisted_id(&saved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::handler::server::wrapper::Parameters;
    use tachi_params::{
        EvidenceRefV1, EvidenceRelationV1, ImmutableRevisionV1, LessonCandidateKindV1,
        LessonCandidateStatusV1, LessonCoverageV1, SourceKindV1,
    };

    use crate::tests::make_server;
    use crate::tool_params::GetMemoryParams;

    #[test]
    fn project_path_segment_falls_back_to_global_when_none() {
        assert_eq!(project_path_segment(None), "global");
        assert_eq!(project_path_segment(Some("  ")), "global");
    }

    #[test]
    fn project_path_segment_normalizes_unsafe_characters() {
        assert_eq!(project_path_segment(Some("My Proj/v2")), "my-proj-v2");
    }

    fn sample_ref() -> EvidenceRefV1 {
        EvidenceRefV1 {
            relation: EvidenceRelationV1::DerivedFrom,
            target_kind: SourceKindV1::EpisodicMemory,
            target_ref: "/scratch/row-1".to_string(),
            immutable_revision: ImmutableRevisionV1::MemoryRevision("3".to_string()),
            section_or_span: None,
            captured_at: "2026-07-17T00:00:00Z".to_string(),
        }
    }

    fn sample_candidate() -> LessonCandidateV1 {
        LessonCandidateV1 {
            candidate_id: "abc123".to_string(),
            candidate_group_id: "group1".to_string(),
            kind: LessonCandidateKindV1::Precedent,
            situation: "Agent faced env-gated auth bypass".to_string(),
            proposed_ruling: "Never trust an env var alone".to_string(),
            why: "attacker-controllable".to_string(),
            how_to_apply: "require signed capability token".to_string(),
            refs: vec![sample_ref()],
            source_row_id: "row-1".to_string(),
            source_revision: "3".to_string(),
            coverage: LessonCoverageV1::full(1200),
            candidate_status: LessonCandidateStatusV1::Pending,
            engine_receipt: None,
        }
    }

    #[test]
    fn render_body_never_claims_established() {
        let body = render_body(&sample_candidate());
        assert!(body.contains("Candidate status: pending"));
        assert!(!body.to_ascii_lowercase().contains("established"));
    }

    #[test]
    fn metadata_candidate_status_is_always_pending_and_established_is_always_false() {
        let metadata = build_metadata(&sample_candidate(), 0);
        assert_eq!(metadata["candidate_status"], json!("pending"));
        assert_eq!(metadata["established"], json!(false));
        assert!(metadata.get("secret_redactions").is_none());
    }

    #[test]
    fn metadata_surfaces_redaction_count_and_warning_when_positive() {
        let metadata = build_metadata(&sample_candidate(), 2);
        assert_eq!(metadata["secret_redactions"], json!(2));
        assert!(metadata["secret_redaction_warning"]
            .as_str()
            .unwrap()
            .contains("redacted"));
    }

    #[test]
    fn scrub_candidate_redacts_secrets_from_every_free_text_field_not_just_situation() {
        // Regression for the cross-vendor review finding: `save_eval_memory`
        // only scrubs `text`/`summary`, never `metadata` — and
        // `build_metadata` copies these four fields verbatim. Every one of
        // them must come back redacted, not just the ones that happen to
        // feed `render_body`'s first line.
        let mut candidate = sample_candidate();
        let secret = "sk-abcdefghijklmnopqrstuvwxyz012345";
        candidate.situation = format!("Leaked token {secret} in situation");
        candidate.proposed_ruling = format!("Ruling references {secret}");
        candidate.why = format!("Why cites {secret}");
        candidate.how_to_apply = format!("Apply using {secret}");

        let (scrubbed, redactions) = scrub_candidate(&candidate);
        assert_eq!(redactions, 4);
        assert!(!scrubbed.situation.contains(secret));
        assert!(!scrubbed.proposed_ruling.contains(secret));
        assert!(!scrubbed.why.contains(secret));
        assert!(!scrubbed.how_to_apply.contains(secret));

        let metadata = build_metadata(&scrubbed, redactions);
        let metadata_str = metadata.to_string();
        assert!(
            !metadata_str.contains(secret),
            "metadata still carries the raw secret: {metadata_str}"
        );
    }

    #[test]
    fn extract_persisted_id_reads_the_id_field() {
        let saved = json!({ "id": "row-xyz", "other": "field" });
        assert_eq!(extract_persisted_id(&saved).unwrap(), "row-xyz");
    }

    #[test]
    fn extract_persisted_id_errors_when_id_missing() {
        let saved = json!({ "other": "field" });
        assert!(extract_persisted_id(&saved).is_err());
    }

    #[test]
    fn summary_line_truncates_long_situations_with_an_ellipsis() {
        let mut candidate = sample_candidate();
        candidate.situation = "x".repeat(200);
        let summary = summary_line(&candidate, 80);
        assert!(summary.chars().count() <= 80);
        assert!(summary.ends_with('\u{2026}'));
    }

    /// End-to-end through the real `save_eval_memory`/`handle_save_memory`
    /// pipeline (not just the JSON-shape assertions above): a persisted
    /// candidate lands at `/lesson_candidates/global/<id>` (never
    /// `/precedents/...`), is retrievable, and its `metadata.candidate_status`
    /// is `"pending"` after a full round trip through scrub/validate/
    /// enrich/#1041-write-affinity — not just at construction time.
    #[tokio::test]
    async fn persisted_candidate_lands_under_lesson_candidates_never_precedents_and_stays_pending()
    {
        let server = make_server();
        let candidate = sample_candidate();

        let id = persist_pending_lesson_candidate(&server, None, false, &candidate)
            .await
            .expect("persist_pending_lesson_candidate should succeed");

        let fetched_str = server
            .get_memory(Parameters(GetMemoryParams {
                id,
                include_archived: false,
                project: None,
            }))
            .await
            .expect("get_memory should succeed");
        let fetched: Value = serde_json::from_str(&fetched_str).expect("memory JSON");

        let path = fetched["path"].as_str().expect("path present");
        assert!(
            path.starts_with("/lesson_candidates/global/"),
            "lesson candidate path should nest under /lesson_candidates/, never /precedents/: \
             {path}"
        );
        assert!(!path.starts_with("/precedents/"), "path was: {path}");

        let meta = &fetched["metadata"];
        assert_eq!(meta["kind"], json!("lesson_candidate"));
        assert_eq!(meta["candidate_status"], json!("pending"));
        assert_eq!(meta["established"], json!(false));
        assert_eq!(meta["lesson_kind"], json!("precedent"));
    }
}
