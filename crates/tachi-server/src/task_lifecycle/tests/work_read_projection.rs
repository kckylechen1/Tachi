//! #1693 focused tests: the CurrentTruth→WorkReadModel section behind the
//! real `tachi_task` STATUS lifecycle view, driven through the production
//! handler (`MemoryServer::tachi_task` → `handle_task_cycle_status`),
//! with real CurrentTruth store seeding via the production
//! `mint_assertions` mint — no hand-built views, no mocks at the
//! production boundary.
//!
//! Guarded invariants, each named at the test that guards it:
//! * **Exact refs** — only the requested work items are projected: an
//!   unrelated same-repo issue never surfaces because a neighbor was
//!   asked about, and a foreign repo with live data in the same store
//!   contributes no row and no existence signal, while the bound repo's
//!   own work still progresses (allowed peer progress alongside blocked
//!   neighbors). A requested PR projects its owning issue item when the
//!   CurrentTruth view's ADMITTED link set proves the link, and an
//!   orphaned PR projects exactly its own item.
//! * **Unknown ≠ healthy** — a repo with no refresh posture is typed
//!   `unknown`/`no_refresh_posture`, never an empty healthy board.
//! * **Conflict blocks success** — a GitHub lifecycle conflict renders a
//!   `conflicted` column with `success_shaped=false`.
//! * **Staleness is not success** — a stale posture renders `stale`, not
//!   a completion column.
//! * **Repeated revision** — the per-item board/status/brief views share
//!   the work token and revision fingerprint, stable across repeated
//!   reads of the same input revision.
//! * **Compact omits whole content** — `compact=true` drops GitHub
//!   snapshot bodies, the raw event replay, and the cached flow block's
//!   free-form fields (including the intake risk rows' body-derived
//!   `evidence` snippets) whole, never truncated, while identifiers/
//!   revisions/state/blockers/evidence references stay; `null` snapshots
//!   stay `null` exactly in both shapes; the explicit full reads retain
//!   content.
//!
//! The project-scoped board/brief views are deliberately not wired (no
//! canonical flow/project ownership authority exists — owner adjudication
//! 2026-09-25); no board/brief CurrentTruth coverage is claimed here.

use super::*;
use rmcp::handler::server::wrapper::Parameters;
use tachi_params::current_truth::refresh::{
    mint_assertions, GithubRepositoryStateV1, SnapshotIssueStateV1, SnapshotIssueV1,
    SnapshotObservationKindV1, SnapshotObservationV1, SnapshotPrStateV1, SnapshotPrV1,
};
use tachi_params::current_truth::types::VisibilityClassV1;

const REPO: &str = "kckylechen1/tachi";
const FOREIGN_REPO: &str = "zeroclaw/zeroclaw";
const ISSUE_TOKEN: &str = "kckylechen1/tachi#issue:438";
#[cfg(unix)]
const BODY_SENTINEL: &str = "ZEROCLAW-1693-ISSUE-BODY-SENTINEL";

// ─── Seeding fixtures (production mint path) ────────────────────────────────

fn issue(number: u64, state: SnapshotIssueStateV1, at: &str, revision: &str) -> SnapshotIssueV1 {
    SnapshotIssueV1 {
        number,
        state,
        updated_at: at.to_string(),
        snapshot_revision: revision.to_string(),
        visibility: VisibilityClassV1::Public,
    }
}

fn repo_state(
    revision: &str,
    refreshed_at: &str,
    issues: Vec<SnapshotIssueV1>,
) -> GithubRepositoryStateV1 {
    GithubRepositoryStateV1 {
        repo: REPO.to_string(),
        refresh_revision: revision.to_string(),
        refreshed_at: refreshed_at.to_string(),
        issues,
        pull_requests: Vec::new(),
        observations: Vec::new(),
    }
}

fn foreign_state() -> GithubRepositoryStateV1 {
    GithubRepositoryStateV1 {
        repo: FOREIGN_REPO.to_string(),
        refresh_revision: "zr1".to_string(),
        refreshed_at: "2026-09-25T10:00:00Z".to_string(),
        issues: vec![issue(
            186,
            SnapshotIssueStateV1::Open,
            "2026-09-25T10:00:00Z",
            "ziss186-a1",
        )],
        pull_requests: Vec::new(),
        observations: Vec::new(),
    }
}

/// Seed one repository's assertions and refresh posture through the
/// production mint path. All `states` must belong to one repo.
fn seed_current_truth(
    server: &crate::MemoryServer,
    states: &[GithubRepositoryStateV1],
    fresh: bool,
    last_revision: &str,
    last_refreshed_at: &str,
) {
    let repo = states
        .first()
        .expect("seed requires at least one state")
        .repo
        .clone();
    server
        .with_current_truth_store(|store| {
            for state in states {
                store
                    .append_all(&mint_assertions(state))
                    .map_err(|error| error.to_string())?;
            }
            store
                .record_refresh(
                    &repo,
                    fresh,
                    Some(last_revision),
                    Some(last_refreshed_at),
                    "2026-09-25T12:00:00Z",
                    if fresh {
                        None
                    } else {
                        Some("github_read_unavailable")
                    },
                )
                .map_err(|error| error.to_string())
        })
        .expect("seed CurrentTruth store");
}

fn fresh_issue_438_state() -> GithubRepositoryStateV1 {
    repo_state(
        "r1",
        "2026-09-25T10:00:00Z",
        vec![issue(
            438,
            SnapshotIssueStateV1::Open,
            "2026-09-25T10:00:00Z",
            "iss438-a1",
        )],
    )
}

/// REPO with issue 438 AND an unrelated same-repo neighbor 439: exact-ref
/// status queries for 438 must not expose 439 in any form.
fn repo_state_with_unrelated_neighbor() -> GithubRepositoryStateV1 {
    repo_state(
        "r1",
        "2026-09-25T10:00:00Z",
        vec![
            issue(
                438,
                SnapshotIssueStateV1::Open,
                "2026-09-25T10:00:00Z",
                "iss438-a1",
            ),
            issue(
                439,
                SnapshotIssueStateV1::Open,
                "2026-09-25T10:00:00Z",
                "iss439-a1",
            ),
        ],
    )
}

fn pr(
    number: u64,
    state: SnapshotPrStateV1,
    sha: Option<&str>,
    at: &str,
    revision: &str,
    linked: Vec<u64>,
) -> SnapshotPrV1 {
    SnapshotPrV1 {
        number,
        state,
        merge_commit_sha: sha.map(str::to_string),
        updated_at: at.to_string(),
        snapshot_revision: revision.to_string(),
        linked_issues: linked,
        visibility: VisibilityClassV1::Public,
    }
}

fn merge_reverted_observation(number: u64, at: &str, revision: &str) -> SnapshotObservationV1 {
    SnapshotObservationV1 {
        kind: SnapshotObservationKindV1::MergeReverted {
            number,
            revert_commit_sha: format!("revert-{number}"),
            original_merge_sha: format!("merge-{number}"),
        },
        observed_at: at.to_string(),
        revision: revision.to_string(),
        visibility: VisibilityClassV1::Public,
    }
}

/// A flow record bound to `REPO#438`, written through the production
/// intake-artifact writer so flow-bound STATUS reads exercise the real
/// cycle-status ref resolution (status keeps its existing auth policy —
/// distinct from the blocked board/brief binding).
fn write_bound_flow(flow_id: &str) {
    let issue = IssueSnapshot {
        repo: REPO.to_string(),
        number: 438,
        title: "#1693 work read model task consumer".to_string(),
        body: None,
        labels: Vec::new(),
        state: Some("OPEN".to_string()),
        url: format!("https://github.com/{REPO}/issues/438"),
        doc_paths: Vec::new(),
        spec_paths: Vec::new(),
    };
    let plan = build_issue_automation_plan(&issue, None);
    write_intake_flow_artifacts(flow_id, "#1693 task consumer", &issue, &plan)
        .expect("write intake flow artifacts");
}

fn task_params(action: &str) -> TachiTaskParams {
    serde_json::from_value(json!({ "action": action })).expect("task params")
}

async fn tachi_task_raw(server: &crate::MemoryServer, params: TachiTaskParams) -> String {
    server
        .tachi_task(Parameters(params))
        .await
        .expect("tachi_task production call")
}

fn cycle_view(raw: &str) -> Value {
    let response: Value = serde_json::from_str(raw).expect("status response JSON");
    response["cycle"].clone()
}

fn section_items(section: &Value) -> &Vec<Value> {
    section["items"].as_array().expect("section items array")
}

fn find_item<'a>(section: &'a Value, token: &str) -> &'a Value {
    section_items(section)
        .iter()
        .find(|item| item["work_token"] == json!(token))
        .unwrap_or_else(|| panic!("work item {token} missing from section: {section:#}"))
}

// ─── Bounded gh `issue view` fixture (hermetic; no network) ─────────────────

/// Serves one canned `gh issue view --json ...` payload for ANY gh
/// invocation, so the production `issue_read` path inside cycle status /
/// intake runs hermetically (same PATH-fixture pattern as the
/// CurrentTruth refresh partial-error tests). The caller's
/// [`IsolatedEnv`] already holds the crate's global test lock and the
/// isolated TACHI_* environment.
#[cfg(unix)]
struct IssueViewFixture {
    _root: tempfile::TempDir,
    previous_path: Option<std::ffi::OsString>,
}

#[cfg(unix)]
impl IssueViewFixture {
    fn new(body: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("gh fixture root");
        let bin = root.path().join("gh-bin");
        std::fs::create_dir(&bin).expect("gh fixture bin dir");
        let executable = bin.join("gh");
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\nset -eu\nprintf '%s' '{}'\n",
                body.replace('\'', "'\\''")
            ),
        )
        .expect("write gh fixture script");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
            .expect("gh fixture executable");
        let previous_path = std::env::var_os("PATH");
        let mut paths = vec![bin.clone()];
        if let Some(previous) = &previous_path {
            paths.extend(std::env::split_paths(previous));
        }
        // SAFETY: the caller's IsolatedEnv holds the crate's global test
        // lock for the test's lifetime (same convention as the run-root
        // fixtures).
        unsafe {
            std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
        }
        Self {
            _root: root,
            previous_path,
        }
    }
}

#[cfg(unix)]
impl Drop for IssueViewFixture {
    fn drop(&mut self) {
        // Restore PATH only when it was previously set (same convention
        // as the CurrentTruth refresh GhFixture).
        if let Some(previous) = self.previous_path.as_ref() {
            // SAFETY: the crate's global test lock is still held.
            unsafe {
                std::env::set_var("PATH", previous);
            }
        }
    }
}

#[cfg(unix)]
fn issue_view_payload() -> String {
    issue_view_payload_with_body(BODY_SENTINEL)
}

#[cfg(unix)]
fn issue_view_payload_with_body(body: &str) -> String {
    json!({
        "number": 438,
        "title": "#1693 work read model task consumer",
        "state": "OPEN",
        "body": body,
        "labels": [],
        "comments": [{ "body": "comment sentinel must not leak in compact either" }],
    })
    .to_string()
}

/// Isolated TACHI_HOME + TACHI_RUN_ROOT under the crate's global test
/// lock, held for the test's lifetime (same convention as the
/// cycle-status dispatch tests).
struct IsolatedEnv {
    _lock: std::sync::MutexGuard<'static, ()>,
    _home: tempfile::TempDir,
    _runs: tempfile::TempDir,
    _home_env: crate::test_support::EnvRestore,
    _runs_env: crate::test_support::EnvRestore,
}

fn isolated_env() -> IsolatedEnv {
    let lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().expect("temp tachi home");
    let runs = tempfile::tempdir().expect("temp run root");
    let home_env = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
    let runs_env = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", runs.path());
    IsolatedEnv {
        _lock: lock,
        _home: home,
        _runs: runs,
        _home_env: home_env,
        _runs_env: runs_env,
    }
}

// ─── Guards ─────────────────────────────────────────────────────────────────

/// Invariant: bound scope. The status projection renders ONLY the
/// explicitly bound repo; a foreign repo with real CurrentTruth data in
/// the same store contributes no work item and no existence signal
/// (no-foreign-leak), while the bound repo's own work still progresses
/// (allowed peer progress alongside the blocked foreign repo).
#[allow(clippy::await_holding_lock)]
#[cfg(unix)]
#[tokio::test]
async fn status_projection_binds_to_explicit_refs_and_leaks_no_foreign_repo() {
    let _env = isolated_env();
    let _fixture = IssueViewFixture::new(&issue_view_payload());
    let server = crate::tests::make_server();
    seed_current_truth(
        &server,
        &[fresh_issue_438_state()],
        true,
        "r1",
        "2026-09-25T10:00:00Z",
    );
    seed_current_truth(
        &server,
        &[foreign_state()],
        true,
        "zr1",
        "2026-09-25T10:00:00Z",
    );

    let mut params = task_params("status");
    params.issue_ref = Some(format!("{REPO}#438"));
    let raw = tachi_task_raw(&server, params).await;

    let section = &cycle_view(&raw)["work_read_model"];
    assert_eq!(section["available"], json!(true), "{section:#}");
    assert_eq!(section["scope"]["binding"], json!("explicit_status_refs"));
    assert_eq!(section["scope"]["repos"], json!([REPO]));
    let item = find_item(section, ISSUE_TOKEN);
    assert_eq!(
        item["status"]["github"],
        json!(format!("{REPO}:not_linked"))
    );
    // The foreign repo's data lives in the same store yet contributes no
    // row and its identity never appears anywhere in the response.
    assert!(
        section_items(section).iter().all(|row| row["work_token"]
            .as_str()
            .is_some_and(|t| t.starts_with(REPO))),
        "foreign work item leaked into bound-scope projection: {section:#}"
    );
    assert!(
        !raw.contains("zeroclaw"),
        "foreign repo identity leaked: {raw}"
    );
}

/// Invariant: unknown ≠ healthy. A bound repo with no refresh posture is
/// typed `unknown` with `no_refresh_posture` — not an empty, healthy
/// projection.
#[allow(clippy::await_holding_lock)]
#[cfg(unix)]
#[tokio::test]
async fn status_unknown_repo_is_typed_unavailable_not_healthy() {
    let _env = isolated_env();
    let _fixture = IssueViewFixture::new(&issue_view_payload());
    let server = crate::tests::make_server();
    // No CurrentTruth posture for REPO.

    let mut params = task_params("status");
    params.issue_ref = Some(format!("{REPO}#438"));
    let raw = tachi_task_raw(&server, params).await;

    let section = &cycle_view(&raw)["work_read_model"];
    assert_eq!(section["available"], json!(false), "{section:#}");
    let github = section["sources"]["github"]
        .as_array()
        .expect("github source rows");
    assert_eq!(github.len(), 1);
    assert_eq!(github[0]["available"], json!(false));
    assert_eq!(github[0]["state"], json!("unknown"));
    assert_eq!(github[0]["reason"], json!("no_refresh_posture"));
    assert!(section_items(section).is_empty());
    assert_eq!(section["health"]["visible_work_count"], json!(0));
    // Partial sources are named unavailable, never empty/healthy.
    assert_eq!(
        section["sources"]["work_claims"]["state"],
        json!("unavailable")
    );
    assert_eq!(
        section["sources"]["verification"]["state"],
        json!("unavailable")
    );
}

/// Invariant: GitHub/current-truth conflict blocks success-shaped
/// projection (same-immutable-revision open/closed contradiction).
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn status_conflict_blocks_success_shaped_projection() {
    let _env = isolated_env();
    let server = crate::tests::make_server();
    // Open and Closed minted at the SAME observed time and snapshot
    // revision: the lifecycle family ties at the max key → Conflicted.
    let open = repo_state(
        "r1",
        "2026-09-25T10:00:00Z",
        vec![issue(
            438,
            SnapshotIssueStateV1::Open,
            "2026-09-25T10:00:00Z",
            "iss438-tie",
        )],
    );
    let closed = repo_state(
        "r2",
        "2026-09-25T11:00:00Z",
        vec![issue(
            438,
            SnapshotIssueStateV1::Closed,
            "2026-09-25T10:00:00Z",
            "iss438-tie",
        )],
    );
    seed_current_truth(&server, &[open, closed], true, "r2", "2026-09-25T11:00:00Z");
    let flow_id = "flow_20260925T000001Z_1693_conflict";
    write_bound_flow(flow_id);

    let mut params = task_params("status");
    params.flow_id = Some(flow_id.to_string());
    let raw = tachi_task_raw(&server, params).await;

    let section = &cycle_view(&raw)["work_read_model"];
    let item = find_item(section, ISSUE_TOKEN);
    assert_eq!(item["board"]["column"], json!("conflicted"), "{item:#}");
    assert_eq!(item["status"]["success_shaped"], json!(false));
    assert!(
        item["board"]["blocker_count"]
            .as_u64()
            .is_some_and(|n| n >= 1),
        "conflicted item must carry a blocker: {item:#}"
    );
}

/// Invariant: staleness is not success. A non-fresh posture renders the
/// `stale` column, never a completion column.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn status_stale_posture_renders_stale_not_success() {
    let _env = isolated_env();
    let server = crate::tests::make_server();
    seed_current_truth(
        &server,
        &[fresh_issue_438_state()],
        false,
        "r1",
        "2026-09-25T10:00:00Z",
    );
    let flow_id = "flow_20260925T000002Z_1693_stale";
    write_bound_flow(flow_id);

    let mut params = task_params("status");
    params.flow_id = Some(flow_id.to_string());
    let raw = tachi_task_raw(&server, params).await;

    let section = &cycle_view(&raw)["work_read_model"];
    assert_eq!(section["available"], json!(true));
    assert_eq!(
        section["sources"]["github"][0]["fresh"],
        json!(false),
        "{section:#}"
    );
    let item = find_item(section, ISSUE_TOKEN);
    assert_eq!(item["board"]["column"], json!("stale"), "{item:#}");
    assert_eq!(item["status"]["success_shaped"], json!(false));
}

/// Invariant: repeated revision. The per-item board/status/brief views
/// are rendered from ONE model instance and share the work token and
/// revision fingerprint, stable across repeated reads of the same input
/// revision (read_at is reader time, never freshness).
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn status_item_views_share_work_token_and_revision_across_repeats() {
    let _env = isolated_env();
    let server = crate::tests::make_server();
    seed_current_truth(
        &server,
        &[fresh_issue_438_state()],
        true,
        "r1",
        "2026-09-25T10:00:00Z",
    );
    let flow_id = "flow_20260925T000003Z_1693_shared_revision";
    write_bound_flow(flow_id);

    let mut status_params = task_params("status");
    status_params.flow_id = Some(flow_id.to_string());
    let section = cycle_view(&tachi_task_raw(&server, status_params.clone()).await)
        ["work_read_model"]
        .clone();
    assert_eq!(section["available"], json!(true), "{section:#}");
    let item = find_item(&section, ISSUE_TOKEN);
    assert_eq!(item["work_token"], json!(ISSUE_TOKEN));
    // The three per-item views come from ONE model: the brief's github
    // summary equals the status row's github token, and the board column
    // is a function of the same revision-keyed model.
    assert_eq!(
        item["brief"]["sections"]["github"],
        item["status"]["github"]
    );
    assert!(item["revision"].as_str().is_some_and(|r| !r.is_empty()));

    // Repeated read of the same input revision renders the same
    // fingerprint (read_at is reader time, never freshness).
    let repeat =
        cycle_view(&tachi_task_raw(&server, status_params).await)["work_read_model"].clone();
    let repeat_item = find_item(&repeat, ISSUE_TOKEN);
    assert_eq!(repeat_item["revision"], item["revision"]);
    assert_eq!(repeat_item["work_token"], item["work_token"]);
    assert_eq!(repeat_item["status"]["github"], item["status"]["github"]);
}

/// Invariant: exact refs. Asking about issue 438 must not expose the
/// unrelated same-repo issue 439 — no id, token, state, or action of the
/// neighbor may appear anywhere in the response — while 438's own work
/// still projects and the foreign-repo regression (live foreign data in
/// the same store contributing nothing) keeps holding.
#[allow(clippy::await_holding_lock)]
#[cfg(unix)]
#[tokio::test]
async fn status_projection_exact_refs_hide_unrelated_same_repo_work() {
    let _env = isolated_env();
    let _fixture = IssueViewFixture::new(&issue_view_payload());
    let server = crate::tests::make_server();
    seed_current_truth(
        &server,
        &[repo_state_with_unrelated_neighbor()],
        true,
        "r1",
        "2026-09-25T10:00:00Z",
    );
    seed_current_truth(
        &server,
        &[foreign_state()],
        true,
        "zr1",
        "2026-09-25T10:00:00Z",
    );

    let mut params = task_params("status");
    params.issue_ref = Some(format!("{REPO}#438"));
    let raw = tachi_task_raw(&server, params).await;

    let section = &cycle_view(&raw)["work_read_model"];
    assert_eq!(section["available"], json!(true), "{section:#}");
    assert_eq!(section["scope"]["refs"], json!([format!("{REPO}#438")]));
    let items = section_items(section);
    assert_eq!(items.len(), 1, "only the requested work item: {section:#}");
    assert_eq!(items[0]["work_token"], json!(ISSUE_TOKEN));
    assert_eq!(
        section["scope"]["requested_refs"],
        json!([{ "ref": format!("{REPO}#438"), "kind": "issue", "projected": true }]),
        "{section:#}"
    );
    // The unrelated neighbor and the foreign repo never appear — not even
    // as an id, and their work items leak nowhere in the response.
    assert!(
        !raw.contains("439") && !raw.contains("zeroclaw"),
        "unrelated same-repo or foreign work leaked into an exact-ref status read: {raw}"
    );
}

/// Invariant: exact refs, linked PR. A PR-only query projects the OWNING
/// issue item (the CurrentTruth view's admitted current link set proves
/// PR 7 implements issue 438), and the linked PR's OWN transition debt
/// and blockers travel with that item unfiltered — a merged-then-reverted
/// linked PR renders the owning issue repair-blocked, not success-shaped.
/// No separate guessed item and no unrelated neighbor.
#[allow(clippy::await_holding_lock)]
#[cfg(unix)]
#[tokio::test]
async fn status_projection_pr_ref_projects_owning_issue_via_admitted_link() {
    let _env = isolated_env();
    let _fixture = IssueViewFixture::new(&issue_view_payload());
    let server = crate::tests::make_server();
    let merged = GithubRepositoryStateV1 {
        repo: REPO.to_string(),
        refresh_revision: "r1".to_string(),
        refreshed_at: "2026-09-25T10:00:00Z".to_string(),
        issues: vec![
            issue(
                438,
                SnapshotIssueStateV1::Open,
                "2026-09-25T10:00:00Z",
                "iss438-a1",
            ),
            issue(
                439,
                SnapshotIssueStateV1::Open,
                "2026-09-25T10:00:00Z",
                "iss439-a1",
            ),
        ],
        pull_requests: vec![pr(
            7,
            SnapshotPrStateV1::Merged,
            Some("merge007"),
            "2026-09-25T10:00:00Z",
            "pr7-a1",
            vec![438],
        )],
        observations: vec![],
    };
    let reverted = GithubRepositoryStateV1 {
        repo: REPO.to_string(),
        refresh_revision: "r2".to_string(),
        refreshed_at: "2026-09-25T11:00:00Z".to_string(),
        issues: vec![
            issue(
                438,
                SnapshotIssueStateV1::Open,
                "2026-09-25T11:00:00Z",
                "iss438-a2",
            ),
            issue(
                439,
                SnapshotIssueStateV1::Open,
                "2026-09-25T11:00:00Z",
                "iss439-a2",
            ),
        ],
        pull_requests: vec![pr(
            7,
            SnapshotPrStateV1::Merged,
            Some("merge007"),
            "2026-09-25T11:00:00Z",
            "pr7-a2",
            vec![438],
        )],
        observations: vec![merge_reverted_observation(
            7,
            "2026-09-25T10:30:00Z",
            "rev7-revert",
        )],
    };
    seed_current_truth(
        &server,
        &[merged, reverted],
        true,
        "r2",
        "2026-09-25T11:00:00Z",
    );

    let mut params = task_params("status");
    params.pr_ref = Some(format!("{REPO}#7"));
    let raw = tachi_task_raw(&server, params).await;

    let section = &cycle_view(&raw)["work_read_model"];
    assert_eq!(section["available"], json!(true), "{section:#}");
    let items = section_items(section);
    assert_eq!(items.len(), 1, "only the owning issue item: {section:#}");
    assert_eq!(items[0]["work_token"], json!(ISSUE_TOKEN));
    assert_eq!(
        items[0]["status"]["github"],
        json!(format!("{REPO}:reverted+transition_debt")),
        "the linked PR's revert debt must travel with the owning item: {items:#?}"
    );
    assert_eq!(items[0]["board"]["column"], json!("reverted"), "{items:#?}");
    assert!(
        items[0]["board"]["blocker_count"]
            .as_u64()
            .is_some_and(|n| n >= 1),
        "the linked PR's transition debt must stay a blocker: {items:#?}"
    );
    assert_eq!(items[0]["status"]["success_shaped"], json!(false));
    assert_eq!(
        section["scope"]["requested_refs"],
        json!([{
            "ref": format!("{REPO}#7"),
            "kind": "pull_request",
            "projected": true,
        }]),
        "{section:#}"
    );
    assert!(
        !raw.contains("439"),
        "unrelated same-repo work leaked into a PR-only status read: {raw}"
    );
}

/// Invariant: exact refs, orphan PR. A PR that no issue's current link set
/// claims keeps its OWN attributable blocked item (R6-2 orphaned revert
/// debt); a PR-only query projects exactly that item — no repo-wide
/// expansion.
#[allow(clippy::await_holding_lock)]
#[cfg(unix)]
#[tokio::test]
async fn status_projection_pr_ref_keeps_orphan_pr_item_exactly() {
    let _env = isolated_env();
    let _fixture = IssueViewFixture::new(&issue_view_payload());
    let server = crate::tests::make_server();
    let merged_unlinked = GithubRepositoryStateV1 {
        repo: REPO.to_string(),
        refresh_revision: "r1".to_string(),
        refreshed_at: "2026-09-25T10:00:00Z".to_string(),
        issues: vec![issue(
            439,
            SnapshotIssueStateV1::Open,
            "2026-09-25T10:00:00Z",
            "iss439-a1",
        )],
        pull_requests: vec![pr(
            8,
            SnapshotPrStateV1::Merged,
            Some("merge008"),
            "2026-09-25T10:00:00Z",
            "pr8-a1",
            vec![],
        )],
        observations: vec![],
    };
    let reverted = GithubRepositoryStateV1 {
        repo: REPO.to_string(),
        refresh_revision: "r2".to_string(),
        refreshed_at: "2026-09-25T11:00:00Z".to_string(),
        issues: vec![issue(
            439,
            SnapshotIssueStateV1::Open,
            "2026-09-25T11:00:00Z",
            "iss439-a2",
        )],
        pull_requests: vec![pr(
            8,
            SnapshotPrStateV1::Merged,
            Some("merge008"),
            "2026-09-25T11:00:00Z",
            "pr8-a2",
            vec![],
        )],
        observations: vec![merge_reverted_observation(
            8,
            "2026-09-25T10:30:00Z",
            "rev8-revert",
        )],
    };
    seed_current_truth(
        &server,
        &[merged_unlinked, reverted],
        true,
        "r2",
        "2026-09-25T11:00:00Z",
    );

    let mut params = task_params("status");
    params.pr_ref = Some(format!("{REPO}#8"));
    let raw = tachi_task_raw(&server, params).await;

    let section = &cycle_view(&raw)["work_read_model"];
    assert_eq!(section["available"], json!(true), "{section:#}");
    let items = section_items(section);
    assert_eq!(
        items.len(),
        1,
        "only the requested orphan PR item: {section:#}"
    );
    assert_eq!(
        items[0]["work_token"],
        json!(format!("{REPO}#pull_request:8"))
    );
    assert_eq!(items[0]["board"]["column"], json!("reverted"), "{items:#?}");
    assert_eq!(items[0]["status"]["success_shaped"], json!(false));
    assert!(
        !raw.contains("439"),
        "unrelated same-repo work leaked into an orphan-PR status read: {raw}"
    );
}

/// Invariant: compact preserves null exactly. A flow with no GitHub
/// bindings renders `issue_snapshot`/`pr_snapshot`/`cached` as `null` in
/// BOTH the compact and full shapes — never a fake valid empty object.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn compact_status_preserves_null_snapshots_exactly() {
    let _env = isolated_env();
    let server = crate::tests::make_server();
    // A refs-free flow record, written the way the flow artifact owner
    // writes status.json.
    let flow_id = "flow_20260925T000005Z_1693_null_snapshots";
    let run_dir = crate::task_lifecycle::run_dir_for_flow_id(flow_id).expect("run dir");
    std::fs::create_dir_all(&run_dir).expect("flow run dir");
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string(&json!({
            "flow_id": flow_id,
            "state": "flow_bound",
            "updated_at": "2026-09-25T00:00:00Z",
        }))
        .expect("status json"),
    )
    .expect("write status");

    let mut compact_params = task_params("status");
    compact_params.flow_id = Some(flow_id.to_string());
    compact_params.compact = Some(true);
    let compact = cycle_view(&tachi_task_raw(&server, compact_params).await);
    assert_eq!(
        compact["github"]["issue_snapshot"],
        Value::Null,
        "{compact:#}"
    );
    assert_eq!(compact["github"]["pr_snapshot"], Value::Null, "{compact:#}");
    assert_eq!(compact["github"]["cached"], Value::Null, "{compact:#}");

    let mut full_params = task_params("status");
    full_params.flow_id = Some(flow_id.to_string());
    let full = cycle_view(&tachi_task_raw(&server, full_params).await);
    assert_eq!(full["github"]["issue_snapshot"], Value::Null, "{full:#}");
    assert_eq!(full["github"]["pr_snapshot"], Value::Null, "{full:#}");
    assert_eq!(full["github"]["cached"], Value::Null, "{full:#}");
}

/// Invariant: compact whitelists the cached flow metadata. A REAL intake
/// (whose risk classification cites body-derived evidence snippets — the
/// classifier lowercases its input, so the sentinel is lowercase) is
/// persisted by the production writer; the compact status read must not
/// replay the body-derived sentinel, while the risk classification, rule
/// reason codes, source, and confidence stay. The full read retains the
/// evidence content.
#[allow(clippy::await_holding_lock)]
#[cfg(unix)]
#[tokio::test]
async fn compact_status_whitelists_cached_risk_evidence_from_real_intake() {
    let _env = isolated_env();
    // The security keyword hit is body-sourced (low confidence), and the
    // sentinel sits inside the evidence snippet window.
    let _fixture = IssueViewFixture::new(&issue_view_payload_with_body(
        "Acceptance criteria body. The security sentinel1693ra handling must be fixed.",
    ));
    let server = crate::tests::make_server();

    let mut intake = task_params("intake");
    intake.issue_ref = Some(format!("{REPO}#438"));
    intake.format = Some("json".to_string());
    let intake_raw = tachi_task_raw(&server, intake).await;
    let intake_receipt: Value = serde_json::from_str(&intake_raw).expect("intake receipt JSON");
    let flow_id = intake_receipt["flow_id"]
        .as_str()
        .expect("flow id")
        .to_string();
    // Fixture setup proof: the persisted plan really carries the
    // body-derived evidence snippet for the full-shape distinction below.
    let run_dir = crate::task_lifecycle::run_dir_for_flow_id(&flow_id).expect("run dir");
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("flow status"),
    )
    .expect("status json");
    let evidence = status["github"]["automation_plan"]["risk_evidence"]
        .as_array()
        .expect("risk evidence rows")
        .iter()
        .map(|row| row["evidence"].as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("|");
    assert!(
        evidence.contains("sentinel1693ra"),
        "fixture setup: the real intake must persist a body-derived evidence snippet, got {evidence}"
    );

    let mut compact_params = task_params("status");
    compact_params.flow_id = Some(flow_id.clone());
    compact_params.compact = Some(true);
    let compact_raw = tachi_task_raw(&server, compact_params).await;
    let compact = cycle_view(&compact_raw);
    assert!(
        !compact_raw.contains("sentinel1693ra"),
        "compact status replayed the body-derived risk evidence: {compact_raw}"
    );
    let plan = &compact["github"]["cached"]["automation_plan"];
    assert_eq!(plan["risk"], json!("advisory"), "{plan:#}");
    let row = &plan["risk_evidence"][0];
    assert_eq!(row["reason"], json!("touches_security"), "{row:#}");
    assert_eq!(row["source"], json!("body"));
    assert_eq!(row["confidence"], json!("low"));
    assert!(row.get("evidence").is_none(), "{row:#}");

    // The full read retains the evidence content.
    let mut full_params = task_params("status");
    full_params.flow_id = Some(flow_id);
    let full_raw = tachi_task_raw(&server, full_params).await;
    assert!(
        full_raw.contains("sentinel1693ra"),
        "full status must retain the cached evidence content"
    );
}

/// Invariant: compact omits whole content. `compact=true` drops GitHub
/// snapshot bodies and the raw event replay whole (never truncated) and
/// keeps identifiers, the canonical projection, state, and blockers; the
/// full read (compact omitted) retains content.
#[allow(clippy::await_holding_lock)]
#[cfg(unix)]
#[tokio::test]
async fn compact_status_omits_bodies_whole_and_keeps_presence_assertions() {
    let _env = isolated_env();
    let _fixture = IssueViewFixture::new(&issue_view_payload());
    let server = crate::tests::make_server();
    seed_current_truth(
        &server,
        &[fresh_issue_438_state()],
        true,
        "r1",
        "2026-09-25T10:00:00Z",
    );

    let mut compact_params = task_params("status");
    compact_params.issue_ref = Some(format!("{REPO}#438"));
    compact_params.compact = Some(true);
    let compact_raw = tachi_task_raw(&server, compact_params).await;
    let compact = cycle_view(&compact_raw);
    assert!(
        !compact_raw.contains(BODY_SENTINEL),
        "compact status leaked the issue body: {compact_raw}"
    );
    let snapshot = &compact["github"]["issue_snapshot"];
    assert!(snapshot.get("body").is_none(), "{snapshot:#}");
    assert!(snapshot.get("comments").is_none(), "{snapshot:#}");
    assert_eq!(snapshot["repo"], json!(REPO));
    assert_eq!(snapshot["number"], json!(438));
    assert!(snapshot["state"].is_string());
    // The raw event replay is omitted whole, not truncated.
    assert!(compact.get("events").is_none(), "{compact:#}");
    // Presence assertions stay: identifiers, canonical projection,
    // state, blockers, evidence references.
    assert_eq!(compact["issue_ref"], json!(format!("{REPO}#438")));
    assert_eq!(compact["work_read_model"]["available"], json!(true));
    assert!(compact["spec_drift"].as_array().is_some());
    assert!(compact["artifacts"].is_object());
    assert!(compact["next_action"].is_string());

    // The full read retains content.
    let mut full_params = task_params("status");
    full_params.issue_ref = Some(format!("{REPO}#438"));
    let full_raw = tachi_task_raw(&server, full_params).await;
    let full = cycle_view(&full_raw);
    assert_eq!(
        full["github"]["issue_snapshot"]["body"],
        json!(BODY_SENTINEL),
        "full status must retain the issue body"
    );
    assert!(full["events"].as_array().is_some());
}

/// Invariant: intake's default receipt omits the issue body whole; the
/// existing explicit full read (`format=full`) retains it.
#[allow(clippy::await_holding_lock)]
#[cfg(unix)]
#[tokio::test]
async fn intake_default_receipt_omits_issue_body_and_full_retains_it() {
    let _env = isolated_env();
    let _fixture = IssueViewFixture::new(&issue_view_payload());
    let server = crate::tests::make_server();

    let mut intake = task_params("intake");
    intake.issue_ref = Some(format!("{REPO}#438"));
    intake.format = Some("json".to_string());
    let default_raw = tachi_task_raw(&server, intake).await;
    assert!(
        !default_raw.contains(BODY_SENTINEL),
        "default intake receipt leaked the issue body: {default_raw}"
    );
    let default_receipt: Value = serde_json::from_str(&default_raw).expect("intake receipt JSON");
    assert!(default_receipt["issue"].get("body").is_none());
    assert_eq!(default_receipt["issue"]["number"], json!(438));
    assert_eq!(default_receipt["issue"]["state"], json!("OPEN"));

    let mut full = task_params("intake");
    full.issue_ref = Some(format!("{REPO}#438"));
    full.format = Some("full".to_string());
    let full_raw = tachi_task_raw(&server, full).await;
    let full_receipt: Value = serde_json::from_str(&full_raw).expect("full intake JSON");
    assert_eq!(
        full_receipt["issue"]["body"],
        json!(BODY_SENTINEL),
        "format=full intake must retain the full snapshot"
    );
}

// ─── Whitelist unit guards ──────────────────────────────────────────────────

/// The shared compact snapshot function omits bodies/comments whole and
/// keeps exactly the identifier/state/reference whitelist — unknown
/// fields never pass through.
#[test]
fn compact_snapshot_whitelists_identifiers_and_drops_content_whole() {
    let snapshot = json!({
        "repo": REPO,
        "number": 438,
        "title": "title stays",
        "body": "body must be omitted whole, not truncated",
        "labels": ["rust"],
        "state": "OPEN",
        "url": "https://github.com/kckylechen1/tachi/issues/438",
        "doc_paths": ["docs/a.md"],
        "spec_paths": [],
        "source": "live",
        "unknown_future_field": "must not pass the whitelist",
    });
    let compact = compact_issue_snapshot_value(&snapshot);
    assert_eq!(
        compact,
        json!({
            "repo": REPO,
            "number": 438,
            "title": "title stays",
            "labels": ["rust"],
            "state": "OPEN",
            "url": "https://github.com/kckylechen1/tachi/issues/438",
            "doc_paths": ["docs/a.md"],
            "spec_paths": [],
            "source": "live",
        })
    );

    let pr = json!({
        "repo": REPO,
        "number": 9,
        "title": "pr",
        "state": "OPEN",
        "url": "https://github.com/kckylechen1/tachi/pull/9",
        "head_ref": "feat/x",
        "base_ref": "main",
        "review_decision": "APPROVED",
        "mergeable": "MERGEABLE",
        "body": "a PR body must never survive compaction",
    });
    let compact_pr = compact_pr_snapshot_value(&pr);
    assert!(compact_pr.get("body").is_none(), "{compact_pr:#}");
    assert_eq!(compact_pr["head_ref"], json!("feat/x"));
}

/// The status binding keeps the EXACT typed issue/PR identities (parsed
/// canonically, repo lowercased to the store spelling) and the deduped
/// repo list for view reads.
#[test]
fn status_bound_work_keeps_exact_typed_refs() {
    let bound = status_bound_work(Some("Kckylechen1/Tachi#438"), Some("kckylechen1/tachi#9"));
    assert_eq!(bound.repos, vec![REPO.to_string()]);
    assert_eq!(
        bound.issues,
        vec![tachi_params::work_read_model::WorkKey::Issue {
            repo: REPO.to_string(),
            number: 438,
        }]
    );
    assert_eq!(
        bound.pull_requests,
        vec![tachi_params::work_read_model::WorkKey::PullRequest {
            repo: REPO.to_string(),
            number: 9,
        }]
    );

    let empty = status_bound_work(None, None);
    assert!(empty.repos.is_empty() && empty.issues.is_empty() && empty.pull_requests.is_empty());

    // Malformed refs bind nothing (no guessed identity).
    let malformed = status_bound_work(Some("not-a-ref"), Some("a/b/c#1"));
    assert!(malformed.issues.is_empty() && malformed.pull_requests.is_empty());
}

/// Compact snapshot helpers preserve `null` exactly, render a malformed
/// non-object shape as typed unknown (never a fake valid empty object),
/// and keep only the identifier/state/reference whitelist.
#[test]
fn compact_snapshot_helpers_preserve_null_and_reject_malformed_shapes() {
    assert_eq!(compact_issue_snapshot_value(&Value::Null), Value::Null);
    assert_eq!(compact_pr_snapshot_value(&Value::Null), Value::Null);
    for malformed in [json!("text"), json!(7), json!([1, 2])] {
        let shaped = compact_issue_snapshot_value(&malformed);
        assert_eq!(
            shaped["state"],
            json!("unknown"),
            "malformed snapshot must be typed unknown, got {shaped}"
        );
        assert_eq!(shaped["reason"], json!("snapshot_not_object"));
    }
}

/// The cached flow GitHub block whitelist is CLOSED: decision fields and
/// their nested state objects stay, free-form content (body-derived risk
/// `evidence`, coaching prose, unknown fields at any level) is omitted
/// WHOLE, and null/malformed inputs are preserved/typed. Asserted as an
/// exact object so an accidentally-open branch cannot pass.
#[test]
fn compact_cached_github_whitelist_is_closed_and_drops_free_form_content() {
    let cached = json!({
        "repo": REPO,
        "issue_number": 438,
        "issue_title": "title stays as an identifier",
        "merge_state": "ready",
        "head_sha": "abc123",
        "policy": "standard",
        "checks": { "state": "success", "updated_at": "2026-09-25T00:00:00Z", "raw_log": "MUST NOT SURVIVE" },
        "review": { "state": "approved", "updated_at": "2026-09-25T00:00:00Z", "body": "MUST NOT SURVIVE" },
        "unknown_future_field": "MUST NOT SURVIVE",
        "automation_plan": {
            "status": "needs_leader",
            "dispatch_allowed": false,
            "risk": "advisory",
            "leader_gate_reasons": ["missing_acceptance_criteria"],
            "branch": "tachi/issue-438-x",
            "recommended_next_action": "coaching prose MUST NOT SURVIVE",
            "risk_evidence": [
                { "reason": "touches_security", "needle": "security", "source": "body", "confidence": "low", "evidence": "BODY-DERIVED-SENTINEL" }
            ],
            "risk_advisory": [
                { "reason": "touches_secrets", "needle": "secret", "source": "body", "confidence": "low", "evidence": "BODY-DERIVED-SENTINEL" }
            ]
        }
    });
    let compact = compact_cached_github_value(&cached);
    assert_eq!(
        compact,
        json!({
            "repo": REPO,
            "issue_number": 438,
            "issue_title": "title stays as an identifier",
            "merge_state": "ready",
            "head_sha": "abc123",
            "policy": "standard",
            "checks": { "state": "success", "updated_at": "2026-09-25T00:00:00Z" },
            "review": { "state": "approved", "updated_at": "2026-09-25T00:00:00Z" },
            "automation_plan": {
                "status": "needs_leader",
                "dispatch_allowed": false,
                "risk": "advisory",
                "leader_gate_reasons": ["missing_acceptance_criteria"],
                "branch": "tachi/issue-438-x",
                "risk_evidence": [
                    { "reason": "touches_security", "needle": "security", "source": "body", "confidence": "low" }
                ],
                "risk_advisory": [
                    { "reason": "touches_secrets", "needle": "secret", "source": "body", "confidence": "low" }
                ]
            }
        }),
        "closed whitelist only: {compact:#}"
    );
    assert!(!compact.to_string().contains("MUST NOT SURVIVE"));
    assert!(!compact.to_string().contains("BODY-DERIVED-SENTINEL"));

    // null stays null; a malformed shape is typed unknown.
    assert_eq!(compact_cached_github_value(&Value::Null), Value::Null);
    assert_eq!(
        compact_cached_github_value(&json!("text"))["state"],
        json!("unknown")
    );
}
#[test]
fn compact_cached_gate_and_ship_decisions_preserve_evidence() {
    let decisions = json!({
        "head_consistency": {
            "head_sha": "abc", "checks_head_sha": null,
            "review_decision_head_sha": null, "head_consistent": false,
            "state": "unknown", "requirement": true, "source": "single_pr_snapshot"
        },
        "flow": {"flow_id": "flow_gate", "linked_issue_refs": ["o/r#8"],
            "has_linked_issue": true, "required": true},
        "verification": {"flow_id": "flow_gate", "overall": "pending", "required_total": 1,
            "current_head_sha": "abc", "expected_head": "abc", "observed_pr_head_sha": "def",
            "claim_id": "claim_gate", "claim_transition_version": 2,
            "passed": [], "failed": [], "pending": ["test"], "stale": [],
            "waiting_on": ["verification:claim_head_mismatch"], "reasons": [],
            "ledger_updated_at": "2026-09-25T00:00:00Z"},
        "ship": {"status": "blocked", "branch": "work", "commit_sha": null,
            "files": ["src/lib.rs"], "warnings": ["verification_required"]}
    });
    let mut input = decisions.clone();
    input["verification"]["error"] = json!("raw diagnostic body");
    input["head_consistency"]["note"] = json!("free-form explanation");
    input["ship"]["steps"] = json!([{"stdout": "raw command output"}]);
    assert_eq!(compact_cached_github_value(&input), decisions);
}
