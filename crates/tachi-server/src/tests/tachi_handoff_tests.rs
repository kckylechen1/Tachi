use super::make_server;
use crate::tool_params::TachiHandoffParams;
use rmcp::handler::server::wrapper::Parameters;

fn params(action: &str) -> TachiHandoffParams {
    TachiHandoffParams {
        action: action.to_string(),
        memo_id: None,
        repo: None,
        title: None,
        labels: Vec::new(),
        flow_id: None,
        force: false,
    }
}

/// #1099 discrimination: on origin/main, `action='leave'` created a new
/// handoff memo (nominal success). The route is retired now — this must be
/// a loud, actionable refusal (`Err`) pointing at the A2A replacement,
/// not a silent no-op and not a panic on the now-removed params fields.
/// Red on origin/main (old code returns `Ok("memo_left"...)`), green after
/// this change (returns `Err(..)` mentioning the replacement action).
#[tokio::test]
async fn tachi_handoff_leave_is_retired_not_silently_accepted() {
    let server = make_server();
    let err = server
        .tachi_handoff(Parameters(params("leave")))
        .await
        .expect_err("action='leave' must be refused, not accepted");
    assert!(err.contains("#1099"), "{err}");
    assert!(err.contains("tachi_a2a"), "{err}");
}

/// Same discrimination for `action='check'` (on origin/main this listed
/// pending memos as nominal success).
#[tokio::test]
async fn tachi_handoff_check_is_retired_not_silently_accepted() {
    let server = make_server();
    let err = server
        .tachi_handoff(Parameters(params("check")))
        .await
        .expect_err("action='check' must be refused, not accepted");
    assert!(err.contains("#1099"), "{err}");
    assert!(err.contains("tachi_a2a"), "{err}");
}

/// `promote_issue` is the one action that must still work end-to-end
/// (not just "not panic") — it fails loudly on a missing memo rather than
/// fabricating a promotion, proving the surviving action is still honest.
#[tokio::test]
async fn tachi_handoff_promote_issue_still_reachable_and_fails_loudly_on_missing_memo() {
    let server = make_server();
    let mut p = params("promote_issue");
    p.memo_id = Some("does-not-exist".to_string());
    p.repo = Some("owner/repo".to_string());

    let err = server
        .tachi_handoff(Parameters(p))
        .await
        .expect_err("promoting a nonexistent memo must fail loudly");
    assert!(
        err.contains("not found") || err.contains("Handoff memo not found"),
        "{err}"
    );
}

/// Unknown actions stay loud (never silently ignored) and the error message
/// reflects that 'promote_issue' is now the only supported action.
#[tokio::test]
async fn tachi_handoff_unknown_action_is_loud() {
    let server = make_server();
    let err = server
        .tachi_handoff(Parameters(params("bogus")))
        .await
        .expect_err("unknown action must error");
    assert!(err.contains("promote_issue"), "{err}");
}
