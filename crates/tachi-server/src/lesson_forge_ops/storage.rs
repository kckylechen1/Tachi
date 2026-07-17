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

fn build_metadata(candidate: &LessonCandidateV1) -> Value {
    json!({
        "kind": "lesson_candidate",
        // Unconditional — see module doc. `LessonCandidateStatusV1` has no
        // other variant to construct, so this is never anything but
        // "pending".
        "candidate_status": candidate.candidate_status.as_str(),
        "lesson_kind": candidate.kind.as_str(),
        "situation": candidate.situation,
        "proposed_ruling": candidate.proposed_ruling,
        "why": candidate.why,
        "how_to_apply": candidate.how_to_apply,
        "refs": candidate.refs,
        "source_row_id": candidate.source_row_id,
        "source_revision": candidate.source_revision,
        "coverage": {
            "source_bytes": candidate.coverage.source_bytes,
            "covered_bytes": candidate.coverage.covered_bytes,
        },
        "identity_status": candidate.identity_status(),
        "engine_receipt": candidate.engine_receipt,
        "candidate_group_id": candidate.candidate_group_id,
        "established": candidate.claims_establishment(),
    })
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
    let path = format!(
        "/lesson_candidates/{}/{}",
        project_path_segment(project),
        candidate.candidate_id
    );
    let text = render_body(candidate);
    let summary = summary_line(candidate, 80);
    let metadata = build_metadata(candidate);

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
        auto_link: true,
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
        let metadata = build_metadata(&sample_candidate());
        assert_eq!(metadata["candidate_status"], json!("pending"));
        assert_eq!(metadata["established"], json!(false));
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
