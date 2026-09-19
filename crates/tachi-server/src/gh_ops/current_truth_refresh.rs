//! Production GitHub → CurrentTruth refresh (#1696).
//!
//! The live reader reuses `gh_ops`' bounded, credential-hardened command
//! path. It reads one issue, its typed GitHub timeline cross-references, and
//! every PR whose `closingIssuesReferences` confirms the relation. No title
//! or body text participates in linkage. The resulting adapter feeds the
//! existing CurrentTruth assertion store/reducer and then exercises the
//! existing WorkReadModel status consumer in the same production action.

use super::*;
use async_trait::async_trait;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;
use tachi_github_runtime::github_corpus_ops::parse::parse_pr_snapshot_from_gh_json;
use tachi_params::current_truth::consumer::{self, CallerAuthorizationV1, CurrentTruthViewV1};
use tachi_params::current_truth::refresh::{
    reconcile_refresh, GithubRefreshAdapter, GithubRepositoryStateV1, RefreshOutcomeV1,
    SnapshotIssueStateV1, SnapshotIssueV1, SnapshotObservationKindV1, SnapshotObservationV1,
    SnapshotPrStateV1, SnapshotPrV1, GITHUB_SNAPSHOT_ISSUER, GITHUB_SNAPSHOT_SOURCE_ID,
};
use tachi_params::current_truth::store::CurrentTruthSqliteStore;
use tachi_params::current_truth::types::{
    ordering_instant, AssertionV1, AuthorityClassV1, PredicateV1, VisibilityClassV1,
};
use tachi_params::gh_json_parse::parse_issue_snapshot_from_gh_json;
use tachi_params::work_read_model::{
    project, status_view, CurrentTruthFactsV1, ProjectionOptions, SourceFacts, SourceKind,
    SourceSnapshot, WorkProjectionIndex,
};

const CURRENT_TRUTH_GH_TIMEOUT: Duration = Duration::from_secs(6);
const CURRENT_TRUTH_REFRESH_TIMEOUT: Duration = Duration::from_secs(30);
const TIMELINE_PAGE_SIZE: usize = 100;
const MAX_TIMELINE_PAGES: usize = 10;
const PR_FIELDS: &str = "number,title,body,state,headRefOid,baseRefOid,updatedAt,mergeCommit,reviews,statusCheckRollup,closingIssuesReferences";

const REASON_UNAVAILABLE: &str = "github_read_unavailable";
const REASON_MALFORMED: &str = "github_data_malformed";
const REASON_INCOMPLETE: &str = "github_data_incomplete";
const REASON_STALE: &str = "github_data_stale";
const REASON_CONTRADICTORY: &str = "github_data_contradictory_revision";

#[async_trait]
trait BoundedGithubRefreshReader: Send + Sync {
    async fn repo_visibility(&self, repo: &str) -> Result<Value, String>;
    async fn issue(&self, repo: &str, number: u64) -> Result<Value, String>;
    async fn issue_timeline_page(
        &self,
        repo: &str,
        number: u64,
        page: usize,
    ) -> Result<Value, String>;
    async fn pull_request(&self, repo: &str, number: u64) -> Result<Value, String>;
}

struct ServerBoundedGithubRefreshReader<'a> {
    server: &'a MemoryServer,
}

#[async_trait]
impl BoundedGithubRefreshReader for ServerBoundedGithubRefreshReader<'_> {
    async fn repo_visibility(&self, repo: &str) -> Result<Value, String> {
        run_gh_json_bounded(
            self.server,
            vec![
                "repo".to_string(),
                "view".to_string(),
                repo.to_string(),
                "--json".to_string(),
                "visibility".to_string(),
            ],
            CURRENT_TRUTH_GH_TIMEOUT,
            "gh repo view",
        )
        .await
    }

    async fn issue(&self, repo: &str, number: u64) -> Result<Value, String> {
        // This is the pre-existing bounded GitHub issue read path. Keeping
        // this call (instead of rebuilding its argv here) makes the adapter
        // inherit the hot-path timeout/kill contract by construction.
        read_issue_snapshot_bounded(self.server, repo, number).await
    }

    async fn issue_timeline_page(
        &self,
        repo: &str,
        number: u64,
        page: usize,
    ) -> Result<Value, String> {
        run_gh_json_bounded(
            self.server,
            vec![
                "api".to_string(),
                "--method".to_string(),
                "GET".to_string(),
                format!(
                    "repos/{repo}/issues/{number}/timeline?per_page={TIMELINE_PAGE_SIZE}&page={page}"
                ),
                "-H".to_string(),
                "Accept: application/vnd.github+json".to_string(),
            ],
            CURRENT_TRUTH_GH_TIMEOUT,
            "gh issue timeline",
        )
        .await
    }

    async fn pull_request(&self, repo: &str, number: u64) -> Result<Value, String> {
        run_gh_json_bounded(
            self.server,
            vec![
                "pr".to_string(),
                "view".to_string(),
                number.to_string(),
                "--repo".to_string(),
                repo.to_string(),
                "--json".to_string(),
                PR_FIELDS.to_string(),
            ],
            CURRENT_TRUTH_GH_TIMEOUT,
            "gh pr view",
        )
        .await
    }
}

#[derive(Debug)]
enum LoadFailure {
    Unavailable,
    Malformed,
    Incomplete,
}

impl LoadFailure {
    fn reason(&self) -> &'static str {
        match self {
            Self::Unavailable => REASON_UNAVAILABLE,
            Self::Malformed => REASON_MALFORMED,
            Self::Incomplete => REASON_INCOMPLETE,
        }
    }
}

/// The production `GithubRefreshAdapter`. Its async loader performs the live
/// bounded reads once; the synchronous trait method then exposes that exact,
/// immutable observation to the existing reconciliation seam.
struct ProductionGithubRefreshAdapter {
    outcome: RefreshOutcomeV1,
    repository_private: bool,
}

impl ProductionGithubRefreshAdapter {
    async fn load(
        server: &MemoryServer,
        repo: &str,
        issue_number: u64,
    ) -> ProductionGithubRefreshAdapter {
        let reader = ServerBoundedGithubRefreshReader { server };
        Self::load_from_with_timeout(
            &reader,
            repo,
            issue_number,
            Some(CURRENT_TRUTH_REFRESH_TIMEOUT),
        )
        .await
    }

    #[cfg(test)]
    async fn load_from(
        reader: &dyn BoundedGithubRefreshReader,
        repo: &str,
        issue_number: u64,
    ) -> ProductionGithubRefreshAdapter {
        Self::load_from_with_timeout(reader, repo, issue_number, None).await
    }

    async fn load_from_with_timeout(
        reader: &dyn BoundedGithubRefreshReader,
        repo: &str,
        issue_number: u64,
        state_timeout: Option<Duration>,
    ) -> ProductionGithubRefreshAdapter {
        let visibility_json = match reader.repo_visibility(repo).await {
            Ok(value) => value,
            Err(_) => return Self::unavailable(repo, LoadFailure::Unavailable, false),
        };
        let visibility = match parse_visibility(&visibility_json) {
            Ok(visibility) => visibility,
            Err(failure) => return Self::unavailable(repo, failure, false),
        };
        let repository_private = visibility == VisibilityClassV1::Private;
        let state = match state_timeout {
            Some(timeout) => tokio::time::timeout(
                timeout,
                load_repository_state(reader, repo, issue_number, visibility),
            )
            .await
            .unwrap_or(Err(LoadFailure::Unavailable)),
            None => load_repository_state(reader, repo, issue_number, visibility).await,
        };
        match state {
            Ok(state) => Self {
                outcome: RefreshOutcomeV1::Fresh(Box::new(state)),
                repository_private,
            },
            Err(failure) => Self::unavailable(repo, failure, repository_private),
        }
    }

    fn unavailable(repo: &str, failure: LoadFailure, repository_private: bool) -> Self {
        Self {
            outcome: RefreshOutcomeV1::Unavailable {
                repo: repo.to_string(),
                // Deliberately content-free: denied, offline, and absent
                // subjects have the same external error posture.
                reason: failure.reason().to_string(),
            },
            repository_private,
        }
    }
}

impl GithubRefreshAdapter for ProductionGithubRefreshAdapter {
    fn refresh(&self, _repo: &str) -> RefreshOutcomeV1 {
        self.outcome.clone()
    }
}

async fn load_repository_state(
    reader: &dyn BoundedGithubRefreshReader,
    repo: &str,
    issue_number: u64,
    visibility: VisibilityClassV1,
) -> Result<GithubRepositoryStateV1, LoadFailure> {
    let issue_json = reader
        .issue(repo, issue_number)
        .await
        .map_err(|_| LoadFailure::Unavailable)?;
    let issue = parse_issue(repo, issue_number, &issue_json, visibility)?;

    let timeline = read_complete_timeline(reader, repo, issue_number).await?;
    let (candidate_prs, observations) = parse_timeline(repo, issue_number, &timeline, visibility)?;

    let mut pull_requests = Vec::new();
    for pr_number in candidate_prs {
        let value = reader
            .pull_request(repo, pr_number)
            .await
            .map_err(|_| LoadFailure::Unavailable)?;
        if let Some(pr) = parse_linked_pr(repo, issue_number, pr_number, &value, visibility)? {
            pull_requests.push(pr);
        }
    }
    pull_requests.sort_by_key(|pr| pr.number);

    let refreshed_at = std::iter::once(issue.updated_at.as_str())
        .chain(pull_requests.iter().map(|pr| pr.updated_at.as_str()))
        .chain(observations.iter().map(|event| event.observed_at.as_str()))
        .max_by_key(|timestamp| ordering_instant(timestamp))
        .ok_or(LoadFailure::Malformed)?
        .to_string();
    let mut state = GithubRepositoryStateV1 {
        repo: repo.to_string(),
        refresh_revision: String::new(),
        refreshed_at,
        issues: vec![issue],
        pull_requests,
        observations,
    };
    let revision_basis = serde_json::json!({
        "repo": state.repo,
        "issues": state.issues,
        "pull_requests": state.pull_requests,
        "observations": state.observations,
    });
    state.refresh_revision = format!(
        "github-refresh-{}",
        memcore::canonical_digest::canonical_json_digest_hex(&revision_basis)
    );
    Ok(state)
}

fn parse_visibility(value: &Value) -> Result<VisibilityClassV1, LoadFailure> {
    match value.get("visibility").and_then(Value::as_str) {
        Some("PUBLIC") | Some("public") => Ok(VisibilityClassV1::Public),
        Some("PRIVATE") | Some("private") | Some("INTERNAL") | Some("internal") => {
            Ok(VisibilityClassV1::Private)
        }
        _ => Err(LoadFailure::Malformed),
    }
}

fn parse_issue(
    repo: &str,
    number: u64,
    value: &Value,
    visibility: VisibilityClassV1,
) -> Result<SnapshotIssueV1, LoadFailure> {
    if value.get("number").and_then(Value::as_u64) != Some(number) {
        return Err(LoadFailure::Malformed);
    }
    let state = match value.get("state").and_then(Value::as_str) {
        Some("OPEN") | Some("open") => SnapshotIssueStateV1::Open,
        Some("CLOSED") | Some("closed") => SnapshotIssueStateV1::Closed,
        _ => return Err(LoadFailure::Malformed),
    };
    let snapshot = parse_issue_snapshot_from_gh_json(repo, number, value);
    if snapshot.issue_snapshot_hash.is_empty()
        || chrono::DateTime::parse_from_rfc3339(&snapshot.updated_at).is_err()
    {
        return Err(LoadFailure::Malformed);
    }
    Ok(SnapshotIssueV1 {
        number,
        state,
        updated_at: snapshot.updated_at,
        snapshot_revision: visibility_bound_revision(
            "issue",
            &snapshot.issue_snapshot_hash,
            visibility,
        ),
        visibility,
    })
}

fn visibility_bound_revision(
    kind: &str,
    object_revision: &str,
    visibility: VisibilityClassV1,
) -> String {
    format!(
        "github-{kind}-{}",
        memcore::canonical_digest::canonical_json_digest_hex(&serde_json::json!({
            "object_revision": object_revision,
            "visibility": visibility,
        }))
    )
}

async fn read_complete_timeline(
    reader: &dyn BoundedGithubRefreshReader,
    repo: &str,
    issue_number: u64,
) -> Result<Vec<Value>, LoadFailure> {
    let mut events = Vec::new();
    for page in 1..=MAX_TIMELINE_PAGES {
        let value = reader
            .issue_timeline_page(repo, issue_number, page)
            .await
            .map_err(|_| LoadFailure::Unavailable)?;
        let page_events = value.as_array().ok_or(LoadFailure::Malformed)?;
        events.extend(page_events.iter().cloned());
        if page_events.len() < TIMELINE_PAGE_SIZE {
            return Ok(events);
        }
    }
    // A full final page means another page may exist. Refuse the partial
    // relation set instead of minting an authoritative-looking omission.
    Err(LoadFailure::Incomplete)
}

fn parse_timeline(
    repo: &str,
    issue_number: u64,
    events: &[Value],
    visibility: VisibilityClassV1,
) -> Result<(BTreeSet<u64>, Vec<SnapshotObservationV1>), LoadFailure> {
    let mut prs = BTreeSet::new();
    let mut observations = Vec::new();
    for event in events {
        match event.get("event").and_then(Value::as_str) {
            Some("cross-referenced") => {
                let source = event.pointer("/source/issue");
                let is_pr = source
                    .and_then(|source| source.get("pull_request"))
                    .is_some_and(|pull_request| !pull_request.is_null());
                let source_repo = source
                    .and_then(|source| source.pointer("/repository/full_name"))
                    .and_then(Value::as_str)
                    .or_else(|| {
                        source
                            .and_then(|source| source.get("repository_url"))
                            .and_then(Value::as_str)
                            .and_then(repo_from_api_url)
                    });
                if is_pr
                    && source_repo
                        .ok_or(LoadFailure::Malformed)?
                        .eq_ignore_ascii_case(repo)
                {
                    let number = source
                        .and_then(|source| source.get("number"))
                        .and_then(Value::as_u64)
                        .ok_or(LoadFailure::Malformed)?;
                    prs.insert(number);
                }
            }
            Some("reopened") => {
                let id = event
                    .get("id")
                    .and_then(Value::as_u64)
                    .ok_or(LoadFailure::Malformed)?;
                let observed_at = event
                    .get("created_at")
                    .and_then(Value::as_str)
                    .filter(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).is_ok())
                    .ok_or(LoadFailure::Malformed)?;
                observations.push(SnapshotObservationV1 {
                    kind: SnapshotObservationKindV1::IssueReopened {
                        number: issue_number,
                    },
                    observed_at: observed_at.to_string(),
                    revision: visibility_bound_revision("event", &id.to_string(), visibility),
                    visibility,
                });
            }
            _ => {}
        }
    }
    observations.sort_by(|left, right| {
        (&left.observed_at, &left.revision).cmp(&(&right.observed_at, &right.revision))
    });
    Ok((prs, observations))
}

fn repo_from_api_url(url: &str) -> Option<&str> {
    url.split_once("/repos/")
        .map(|(_, repo)| repo)
        .filter(|repo| repo.matches('/').count() == 1)
}

fn parse_linked_pr(
    repo: &str,
    issue_number: u64,
    number: u64,
    value: &Value,
    visibility: VisibilityClassV1,
) -> Result<Option<SnapshotPrV1>, LoadFailure> {
    if value.get("number").and_then(Value::as_u64) != Some(number) {
        return Err(LoadFailure::Malformed);
    }
    let links = value
        .get("closingIssuesReferences")
        .and_then(Value::as_array)
        .ok_or(LoadFailure::Malformed)?;
    let mut linked_issues = links
        .iter()
        .map(|issue| {
            issue
                .get("number")
                .and_then(Value::as_u64)
                .ok_or(LoadFailure::Malformed)
        })
        .collect::<Result<Vec<_>, _>>()?;
    linked_issues.sort_unstable();
    linked_issues.dedup();
    if !linked_issues.contains(&issue_number) {
        // Timeline cross-reference alone is not implementation linkage.
        return Ok(None);
    }

    let snapshot =
        parse_pr_snapshot_from_gh_json(repo, number, value).map_err(|_| LoadFailure::Malformed)?;
    if snapshot.pr_snapshot_hash.is_empty()
        || chrono::DateTime::parse_from_rfc3339(&snapshot.updated_at).is_err()
    {
        return Err(LoadFailure::Malformed);
    }
    let state = if snapshot.merged || snapshot.state.eq_ignore_ascii_case("MERGED") {
        SnapshotPrStateV1::Merged
    } else if snapshot.state.eq_ignore_ascii_case("OPEN") {
        SnapshotPrStateV1::Open
    } else if snapshot.state.eq_ignore_ascii_case("CLOSED") {
        SnapshotPrStateV1::ClosedUnmerged
    } else {
        return Err(LoadFailure::Malformed);
    };
    Ok(Some(SnapshotPrV1 {
        number,
        state,
        merge_commit_sha: snapshot.merge_commit_sha,
        updated_at: snapshot.updated_at,
        snapshot_revision: visibility_bound_revision(
            "pull-request",
            &snapshot.pr_snapshot_hash,
            visibility,
        ),
        linked_issues,
        visibility,
    }))
}

struct ConsumedRefresh {
    view: CurrentTruthViewV1,
    work_statuses: Vec<tachi_params::work_read_model::WorkStatusRowV1>,
    fresh: bool,
}

fn apply_refresh_and_consume(
    store: &CurrentTruthSqliteStore,
    adapter: &dyn GithubRefreshAdapter,
    repo: &str,
    attempted_at: &str,
) -> Result<ConsumedRefresh, String> {
    let (outcome, assertions) = reconcile_refresh(adapter, repo);
    let mut fresh = false;
    match outcome {
        RefreshOutcomeV1::Fresh(state) => {
            if refresh_is_stale(store, &state, &assertions)? {
                record_unavailable(store, repo, attempted_at, REASON_STALE)?;
            } else {
                match store.append_all_and_record_refresh(
                    &assertions,
                    repo,
                    &state.refresh_revision,
                    &state.refreshed_at,
                    attempted_at,
                ) {
                    Ok(_) => {
                        fresh = true;
                    }
                    Err(
                        tachi_params::current_truth::store::CurrentTruthStoreError::ContradictsExistingRevision(_),
                    ) => {
                        record_unavailable(store, repo, attempted_at, REASON_CONTRADICTORY)?;
                    }
                    Err(
                        tachi_params::current_truth::store::CurrentTruthStoreError::StaleRefreshRecord {
                            ..
                        },
                    ) => {
                        // A later-started attempt already committed. This
                        // transaction rolled back, so consume that newer
                        // posture without letting the late result regress it.
                    }
                    Err(error) => return Err(error.to_string()),
                }
            }
        }
        RefreshOutcomeV1::Unavailable { reason, .. } => {
            record_unavailable(store, repo, attempted_at, &reason)?;
        }
    }

    // The public tachi_gh action has no private-read grant. It always mints
    // the consumer view under `sees_private=false`; private subjects are SQL
    // filtered before decode, and health/status counts cover only that set.
    let authorization = CallerAuthorizationV1 {
        sees_private: false,
    };
    let view =
        consumer::read_view(store, repo, authorization).map_err(|error| error.to_string())?;
    let posture_revision = format!(
        "current-truth-view-{}",
        memcore::canonical_digest::canonical_json_digest_hex(
            &serde_json::to_value(&view).map_err(|error| error.to_string())?
        )
    );
    let snapshot = SourceSnapshot::new(
        SourceKind::CurrentTruth {
            repo: repo.to_string(),
        },
        posture_revision,
        attempted_at,
        SourceFacts::CurrentTruth(Box::new(CurrentTruthFactsV1 {
            view: view.clone(),
            minted_authorization: authorization,
        })),
    )
    .map_err(|error| error.to_string())?;
    let mut index = WorkProjectionIndex::new();
    index.apply(snapshot).map_err(|error| error.to_string())?;
    let model = project(
        &index,
        &ProjectionOptions::try_new(attempted_at).map_err(|error| error.to_string())?,
    );
    let work_statuses = model.items.iter().map(status_view).collect();
    Ok(ConsumedRefresh {
        view,
        work_statuses,
        fresh,
    })
}

fn record_unavailable(
    store: &CurrentTruthSqliteStore,
    repo: &str,
    attempted_at: &str,
    reason: &str,
) -> Result<(), String> {
    match store.record_refresh(repo, false, None, None, attempted_at, Some(reason)) {
        Ok(())
        | Err(tachi_params::current_truth::store::CurrentTruthStoreError::StaleRefreshRecord {
            ..
        }) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn refresh_is_stale(
    store: &CurrentTruthSqliteStore,
    state: &GithubRepositoryStateV1,
    incoming: &[AssertionV1],
) -> Result<bool, String> {
    let existing = store
        .assertions_for_repo(&state.repo)
        .map_err(|error| error.to_string())?;
    let mut existing_heads: BTreeMap<(String, String), chrono::DateTime<chrono::Utc>> =
        BTreeMap::new();
    for assertion in existing
        .iter()
        .filter(|assertion| is_snapshot_assertion(assertion))
    {
        let key = snapshot_family_key(assertion);
        let observed_at = ordering_instant(&assertion.observed_at);
        existing_heads
            .entry(key)
            .and_modify(|head| *head = std::cmp::max(*head, observed_at))
            .or_insert(observed_at);
    }
    for assertion in incoming
        .iter()
        .filter(|assertion| is_snapshot_assertion(assertion))
    {
        if let Some(existing) = existing_heads.get(&snapshot_family_key(assertion)) {
            // Snapshot revisions are immutable identities, not monotonic
            // sequence numbers. Only source observation time can establish
            // that an input is older; equal-time distinct revisions proceed
            // to the store so same-revision contradictions are refused and
            // distinct revisions follow the reducer's frozen tie law.
            if ordering_instant(&assertion.observed_at) < *existing {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn is_snapshot_assertion(assertion: &AssertionV1) -> bool {
    assertion.authority_class == AuthorityClassV1::GitHubTypedObject
        && assertion.issuer == GITHUB_SNAPSHOT_ISSUER
        && assertion.source_ref.source == GITHUB_SNAPSHOT_SOURCE_ID
        && !matches!(
            assertion.predicate,
            PredicateV1::IssueReopened | PredicateV1::MergeReverted
        )
}

fn snapshot_family_key(assertion: &AssertionV1) -> (String, String) {
    let family = match assertion.predicate {
        PredicateV1::IssueOpen | PredicateV1::IssueClosed => "issue_lifecycle",
        PredicateV1::PrOpen | PredicateV1::PrMerged | PredicateV1::PrClosedUnmerged => {
            "pr_lifecycle"
        }
        predicate => predicate.as_str(),
    };
    (assertion.subject.as_token(), family.to_string())
}

pub(in crate::gh_ops) async fn handle_current_truth_refresh(
    server: &MemoryServer,
    params: &TachiGhParams,
) -> Result<String, String> {
    let repo = super::router::required_repo(params, "current_truth_refresh")?;
    let issue_number = params
        .number
        .ok_or("current_truth_refresh requires issue 'number'")?;
    validate_repo(&repo)?;
    let attempted_at = chrono::Utc::now().to_rfc3339();
    let adapter = ProductionGithubRefreshAdapter::load(server, &repo, issue_number).await;
    let (consumed, private_history) = server.with_current_truth_store(|store| {
        let consumed = apply_refresh_and_consume(store, &adapter, &repo, &attempted_at)?;
        let private_history = !store
            .private_subject_tokens(&repo)
            .map_err(|error| error.to_string())?
            .is_empty();
        Ok((consumed, private_history))
    })?;

    serialize_refresh_response(
        &repo,
        &consumed,
        adapter.repository_private || private_history,
        &attempted_at,
    )
}

fn serialize_refresh_response(
    repo: &str,
    consumed: &ConsumedRefresh,
    repository_private: bool,
    attempted_at: &str,
) -> Result<String, String> {
    if repository_private {
        // The production adapter must ingest a public→private transition so
        // the existing consumer can hide historical public rows, but this
        // unprivileged facade receipt must not confirm that private subjects
        // exist. Its shape is the same content-free unavailable posture used
        // when GitHub cannot be read.
        return serde_json::to_string(&json!({
            "tool": "tachi_gh_current_truth_refresh",
            "repo": repo,
            "fresh": false,
            "posture": {
                "fresh": false,
                "last_fresh_revision": null,
                "last_fresh_at": null,
                "last_attempt_at": attempted_at,
                "unavailable_reason": REASON_UNAVAILABLE,
            },
            "visible_health": {
                "conflicted_predicates": 0,
                "stale_handoff_claims": 0,
                "repos_with_refresh_debt": 1,
            },
            "work_status": [],
        }))
        .map_err(|error| format!("serialize current truth refresh: {error}"));
    }

    let statuses = consumed
        .work_statuses
        .iter()
        .map(|status| {
            json!({
                "work_token": status.work_token,
                "revision": status.revision,
                "read_at": status.read_at,
                "github": status.github,
                "claim": status.claim,
                "run": status.run,
                "adjudication": status.adjudication,
                "delivery": status.delivery,
                "success_shaped": status.success_shaped,
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&json!({
        "tool": "tachi_gh_current_truth_refresh",
        "repo": repo,
        "fresh": consumed.fresh,
        "posture": consumed.view.posture,
        "visible_health": consumed.view.health,
        "work_status": statuses,
    }))
    .map_err(|error| format!("serialize current truth refresh: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FixtureReader {
        visibility: Option<Result<Value, String>>,
        issue: Option<Result<Value, String>>,
        timeline: BTreeMap<usize, Result<Value, String>>,
        prs: BTreeMap<u64, Result<Value, String>>,
        calls: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl BoundedGithubRefreshReader for FixtureReader {
        async fn repo_visibility(&self, _repo: &str) -> Result<Value, String> {
            self.calls.lock().unwrap().push("repo".to_string());
            self.visibility
                .clone()
                .unwrap_or_else(|| Ok(json!({"visibility": "PUBLIC"})))
        }

        async fn issue(&self, _repo: &str, _number: u64) -> Result<Value, String> {
            self.calls.lock().unwrap().push("issue".to_string());
            self.issue.clone().expect("issue fixture")
        }

        async fn issue_timeline_page(
            &self,
            _repo: &str,
            _number: u64,
            page: usize,
        ) -> Result<Value, String> {
            self.calls.lock().unwrap().push(format!("timeline:{page}"));
            self.timeline
                .get(&page)
                .cloned()
                .unwrap_or_else(|| Ok(json!([])))
        }

        async fn pull_request(&self, _repo: &str, number: u64) -> Result<Value, String> {
            self.calls.lock().unwrap().push(format!("pr:{number}"));
            self.prs.get(&number).cloned().expect("pr fixture")
        }
    }

    fn issue(state: &str, updated_at: &str) -> Value {
        json!({
            "number": 42,
            "title": "bounded fixture",
            "body": "body",
            "state": state,
            "labels": [],
            "milestone": null,
            "updatedAt": updated_at,
            "comments": [],
        })
    }

    fn pr(number: u64, state: &str, updated_at: &str, merge_sha: Option<&str>) -> Value {
        json!({
            "number": number,
            "title": "implementation",
            "body": "body",
            "state": state,
            "headRefOid": format!("head{number:036}"),
            "baseRefOid": format!("base{number:036}"),
            "updatedAt": updated_at,
            "mergeCommit": merge_sha.map(|oid| json!({"oid": oid})),
            "reviews": [],
            "statusCheckRollup": [],
            "closingIssuesReferences": [{"number": 42}],
        })
    }

    fn cross_reference(pr_number: u64) -> Value {
        json!({
            "id": 5000 + pr_number,
            "event": "cross-referenced",
            "created_at": "2026-09-01T00:00:00Z",
            "source": {
                "issue": {
                    "number": pr_number,
                    "pull_request": {"url": format!("https://api.github.test/pulls/{pr_number}")},
                    "repository_url": "https://api.github.test/repos/owner/repo"
                }
            }
        })
    }

    async fn adapter_for(
        issue_value: Value,
        timeline: Vec<Value>,
        prs: Vec<(u64, Value)>,
    ) -> ProductionGithubRefreshAdapter {
        let reader = FixtureReader {
            issue: Some(Ok(issue_value)),
            timeline: BTreeMap::from([(1, Ok(Value::Array(timeline)))]),
            prs: prs
                .into_iter()
                .map(|(number, value)| (number, Ok(value)))
                .collect(),
            ..FixtureReader::default()
        };
        ProductionGithubRefreshAdapter::load_from(&reader, "owner/repo", 42).await
    }

    fn fresh(adapter: &ProductionGithubRefreshAdapter) -> GithubRepositoryStateV1 {
        match adapter.refresh("owner/repo") {
            RefreshOutcomeV1::Fresh(state) => *state,
            other => panic!("expected fresh adapter, got {other:?}"),
        }
    }

    struct PathEnvGuard(Option<std::ffi::OsString>);

    impl PathEnvGuard {
        fn prepend(dir: &std::path::Path) -> Self {
            let original = std::env::var_os("PATH");
            let mut paths = vec![dir.to_path_buf()];
            if let Some(path) = original.as_ref() {
                paths.extend(std::env::split_paths(path));
            }
            std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
            Self(original)
        }
    }

    impl Drop for PathEnvGuard {
        fn drop(&mut self) {
            match self.0.as_ref() {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
        }
    }

    #[cfg(unix)]
    fn write_executable(path: &std::path::Path, contents: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, contents).unwrap();
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    #[tokio::test]
    async fn production_adapter_merged_while_open_then_close_later() {
        let open_adapter = adapter_for(
            issue("OPEN", "2026-09-01T00:00:00Z"),
            vec![cross_reference(7)],
            vec![(
                7,
                pr(
                    7,
                    "MERGED",
                    "2026-09-02T00:00:00Z",
                    Some("merge00000000000000000000000000000000007"),
                ),
            )],
        )
        .await;
        let closed_adapter = adapter_for(
            issue("CLOSED", "2026-09-03T00:00:00Z"),
            vec![cross_reference(7)],
            vec![(
                7,
                pr(
                    7,
                    "MERGED",
                    "2026-09-02T00:00:00Z",
                    Some("merge00000000000000000000000000000000007"),
                ),
            )],
        )
        .await;
        let store = CurrentTruthSqliteStore::open_in_memory().unwrap();
        let open =
            apply_refresh_and_consume(&store, &open_adapter, "owner/repo", "2026-09-02T01:00:00Z")
                .unwrap();
        let issue_row = open
            .view
            .subjects
            .iter()
            .find(|subject| subject.subject_token == "owner/repo#issue:42")
            .unwrap();
        assert_eq!(
            issue_row
                .predicates
                .iter()
                .find(|predicate| predicate.predicate == PredicateV1::ImplementationPresent)
                .unwrap()
                .value_token,
            "merge00000000000000000000000000000000007"
        );
        assert!(issue_row
            .predicates
            .iter()
            .any(|predicate| predicate.predicate == PredicateV1::IssueOpen
                && predicate.status
                    == tachi_params::current_truth::types::ReductionStatusV1::Current));

        let closed = apply_refresh_and_consume(
            &store,
            &closed_adapter,
            "owner/repo",
            "2026-09-03T01:00:00Z",
        )
        .unwrap();
        let issue_row = closed
            .view
            .subjects
            .iter()
            .find(|subject| subject.subject_token == "owner/repo#issue:42")
            .unwrap();
        assert!(issue_row
            .predicates
            .iter()
            .any(|predicate| predicate.predicate == PredicateV1::IssueClosed
                && predicate.status
                    == tachi_params::current_truth::types::ReductionStatusV1::Current));
        assert!(closed
            .work_statuses
            .iter()
            .any(|row| row.work_token == "owner/repo#issue:42"));
    }

    #[tokio::test]
    async fn production_adapter_closed_unmerged_never_projects_implementation() {
        let adapter = adapter_for(
            issue("OPEN", "2026-09-01T00:00:00Z"),
            vec![cross_reference(8)],
            vec![(8, pr(8, "CLOSED", "2026-09-02T00:00:00Z", None))],
        )
        .await;
        let state = fresh(&adapter);
        assert_eq!(
            state.pull_requests[0].state,
            SnapshotPrStateV1::ClosedUnmerged
        );
        let assertions = tachi_params::current_truth::refresh::mint_assertions(&state);
        assert!(!assertions.iter().any(|assertion| {
            assertion.predicate == PredicateV1::ImplementationPresent
                && matches!(
                    assertion.value,
                    tachi_params::current_truth::types::AssertionValueV1::CommitSha(_)
                )
        }));
    }

    #[tokio::test]
    async fn production_adapter_reopen_is_append_only_and_revert_is_not_inferred() {
        let mut reopened = cross_reference(7);
        reopened["event"] = json!("reopened");
        reopened["id"] = json!(9001);
        reopened["created_at"] = json!("2026-09-03T00:00:00Z");
        let adapter = adapter_for(
            issue("OPEN", "2026-09-03T00:00:00Z"),
            vec![cross_reference(7), reopened],
            vec![(
                7,
                pr(
                    7,
                    "MERGED",
                    "2026-09-02T00:00:00Z",
                    Some("merge00000000000000000000000000000000007"),
                ),
            )],
        )
        .await;
        let state = fresh(&adapter);
        assert!(state.observations.iter().any(|observation| matches!(
            observation.kind,
            SnapshotObservationKindV1::IssueReopened { number: 42 }
        )));
        assert!(!state.observations.iter().any(|observation| matches!(
            observation.kind,
            SnapshotObservationKindV1::MergeReverted { .. }
        )));
        let store = CurrentTruthSqliteStore::open_in_memory().unwrap();
        let consumed =
            apply_refresh_and_consume(&store, &adapter, "owner/repo", "2026-09-03T01:00:00Z")
                .unwrap();
        let issue = consumed
            .view
            .subjects
            .iter()
            .find(|subject| subject.subject_token == "owner/repo#issue:42")
            .unwrap();
        assert!(issue.predicates.iter().any(|predicate| {
            predicate.predicate == PredicateV1::IssueReopened
                && predicate.status
                    == tachi_params::current_truth::types::ReductionStatusV1::Current
        }));
    }

    #[tokio::test]
    async fn stale_refresh_marks_debt_and_does_not_regress_consumer() {
        let newer = adapter_for(issue("CLOSED", "2026-09-04T00:00:00Z"), vec![], vec![]).await;
        let older = adapter_for(issue("OPEN", "2026-09-01T00:00:00Z"), vec![], vec![]).await;
        let store = CurrentTruthSqliteStore::open_in_memory().unwrap();
        apply_refresh_and_consume(&store, &newer, "owner/repo", "2026-09-04T01:00:00Z").unwrap();
        let result =
            apply_refresh_and_consume(&store, &older, "owner/repo", "2026-09-05T01:00:00Z")
                .unwrap();
        assert!(!result.fresh);
        assert_eq!(
            result.view.posture.unavailable_reason.as_deref(),
            Some(REASON_STALE)
        );
        let issue = result
            .view
            .subjects
            .iter()
            .find(|subject| subject.subject_token == "owner/repo#issue:42")
            .unwrap();
        assert!(issue.predicates.iter().any(|predicate| {
            predicate.predicate == PredicateV1::IssueClosed
                && predicate.status
                    == tachi_params::current_truth::types::ReductionStatusV1::Current
        }));
    }

    #[tokio::test]
    async fn older_observation_for_unseen_issue_is_not_repository_stale() {
        let newer = adapter_for(issue("CLOSED", "2026-09-04T00:00:00Z"), vec![], vec![]).await;
        let older = adapter_for(issue("OPEN", "2026-09-01T00:00:00Z"), vec![], vec![]).await;
        let mut older_other_issue = fresh(&older);
        older_other_issue.issues[0].number = 43;
        older_other_issue.issues[0].snapshot_revision =
            visibility_bound_revision("issue", "independent-issue-43", VisibilityClassV1::Public);
        older_other_issue.refresh_revision = "independent-refresh-43".to_string();
        let older_other_issue = ProductionGithubRefreshAdapter {
            outcome: RefreshOutcomeV1::Fresh(Box::new(older_other_issue)),
            repository_private: false,
        };
        let store = CurrentTruthSqliteStore::open_in_memory().unwrap();
        apply_refresh_and_consume(&store, &newer, "owner/repo", "2026-09-04T01:00:00Z").unwrap();
        let result = apply_refresh_and_consume(
            &store,
            &older_other_issue,
            "owner/repo",
            "2026-09-05T01:00:00Z",
        )
        .unwrap();

        assert!(result.fresh);
        assert_eq!(result.view.subjects.len(), 2);
        assert!(result
            .view
            .subjects
            .iter()
            .any(|subject| subject.subject_token == "owner/repo#issue:43"));
    }

    #[test]
    fn unavailable_refresh_marks_debt_without_fabricating_truth() {
        struct Unavailable;
        impl GithubRefreshAdapter for Unavailable {
            fn refresh(&self, repo: &str) -> RefreshOutcomeV1 {
                RefreshOutcomeV1::Unavailable {
                    repo: repo.to_string(),
                    reason: REASON_UNAVAILABLE.to_string(),
                }
            }
        }
        let store = CurrentTruthSqliteStore::open_in_memory().unwrap();
        let result =
            apply_refresh_and_consume(&store, &Unavailable, "owner/repo", "2026-09-01T00:00:00Z")
                .unwrap();
        assert!(!result.fresh);
        assert!(!result.view.posture.fresh);
        assert!(result.view.subjects.is_empty());
        assert_eq!(result.view.health.repos_with_refresh_debt, 1);
    }

    #[tokio::test]
    async fn unauthorized_consumer_hides_private_subject_counts_refs_and_statuses() {
        let public = adapter_for(issue("OPEN", "2026-09-01T00:00:00Z"), vec![], vec![]).await;
        let reader = FixtureReader {
            visibility: Some(Ok(json!({"visibility": "PRIVATE"}))),
            issue: Some(Ok(issue("OPEN", "2026-09-01T00:00:00Z"))),
            timeline: BTreeMap::from([(1, Ok(json!([])))]),
            ..FixtureReader::default()
        };
        let adapter = ProductionGithubRefreshAdapter::load_from(&reader, "owner/repo", 42).await;
        let store = CurrentTruthSqliteStore::open_in_memory().unwrap();
        let before =
            apply_refresh_and_consume(&store, &public, "owner/repo", "2026-09-01T00:30:00Z")
                .unwrap();
        assert_eq!(before.view.subjects.len(), 1);
        let result =
            apply_refresh_and_consume(&store, &adapter, "owner/repo", "2026-09-01T01:00:00Z")
                .unwrap();
        assert!(result.fresh);
        assert!(result.view.subjects.is_empty());
        assert_eq!(result.view.health.conflicted_predicates, 0);
        assert_eq!(result.view.health.repos_with_refresh_debt, 0);
        assert!(result.work_statuses.is_empty());
        assert_eq!(
            reader.calls.lock().unwrap().as_slice(),
            ["repo", "issue", "timeline:1"]
        );
        let response = serialize_refresh_response(
            "owner/repo",
            &result,
            adapter.repository_private,
            "2026-09-01T01:00:00Z",
        )
        .unwrap();
        let response: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["fresh"], json!(false));
        assert_eq!(response["posture"]["last_fresh_revision"], Value::Null);
        assert_eq!(response["work_status"], json!([]));

        let denied = FixtureReader {
            visibility: Some(Err("denied".to_string())),
            ..FixtureReader::default()
        };
        let denied = ProductionGithubRefreshAdapter::load_from(&denied, "owner/repo", 42).await;
        let denied_store = CurrentTruthSqliteStore::open_in_memory().unwrap();
        let denied =
            apply_refresh_and_consume(&denied_store, &denied, "owner/repo", "2026-09-01T01:00:00Z")
                .unwrap();
        let denied_response: Value = serde_json::from_str(
            &serialize_refresh_response("owner/repo", &denied, false, "2026-09-01T01:00:00Z")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response, denied_response);
    }

    #[tokio::test]
    async fn production_and_fake_adapters_mint_canonically_equivalent_assertions() {
        let production = adapter_for(
            issue("OPEN", "2026-09-01T00:00:00Z"),
            vec![cross_reference(7)],
            vec![(
                7,
                pr(
                    7,
                    "MERGED",
                    "2026-09-02T00:00:00Z",
                    Some("merge00000000000000000000000000000000007"),
                ),
            )],
        )
        .await;
        let expected_state = fresh(&production);
        struct Fake(GithubRepositoryStateV1);
        impl GithubRefreshAdapter for Fake {
            fn refresh(&self, _repo: &str) -> RefreshOutcomeV1 {
                RefreshOutcomeV1::Fresh(Box::new(self.0.clone()))
            }
        }
        let (_, production_assertions) = reconcile_refresh(&production, "owner/repo");
        let (_, fake_assertions) = reconcile_refresh(&Fake(expected_state), "owner/repo");
        assert_eq!(production_assertions, fake_assertions);
    }

    #[tokio::test]
    async fn duplicate_refresh_is_idempotent() {
        let adapter = adapter_for(
            issue("OPEN", "2026-09-01T00:00:00Z"),
            vec![cross_reference(7)],
            vec![(7, pr(7, "OPEN", "2026-09-02T00:00:00Z", None))],
        )
        .await;
        let store = CurrentTruthSqliteStore::open_in_memory().unwrap();
        let first =
            apply_refresh_and_consume(&store, &adapter, "owner/repo", "2026-09-02T01:00:00Z")
                .unwrap();
        let count = store.assertions_for_repo("owner/repo").unwrap().len();
        let second =
            apply_refresh_and_consume(&store, &adapter, "owner/repo", "2026-09-02T02:00:00Z")
                .unwrap();
        assert!(first.fresh && second.fresh);
        assert_eq!(
            store.assertions_for_repo("owner/repo").unwrap().len(),
            count
        );
        assert_eq!(first.view.subjects, second.view.subjects);
    }

    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn production_router_reads_persists_and_runs_work_status_consumer() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let bin = tempfile::tempdir().unwrap();
        write_executable(
            &bin.path().join("gh"),
            r#"#!/bin/sh
case "$1 $2" in
  "repo view") echo '{"visibility":"PUBLIC"}' ;;
  "issue view") echo '{"number":42,"title":"production path","body":"","state":"OPEN","labels":[],"milestone":null,"updatedAt":"2026-09-01T00:00:00Z","comments":[]}' ;;
  "api --method") echo '[]' ;;
  *) echo "unexpected gh call: $*" >&2; exit 1 ;;
esac
"#,
        );
        let _path = PathEnvGuard::prepend(bin.path());
        let server = crate::tests::make_server();
        let response = crate::gh_ops::handle_tachi_gh(
            &server,
            TachiGhParams {
                action: "current_truth_refresh".to_string(),
                repo: Some("owner/repo".to_string()),
                number: Some(42),
                ..TachiGhParams::default()
            },
        )
        .await
        .unwrap();
        let response: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["fresh"], json!(true));
        assert_eq!(
            response["work_status"][0]["work_token"],
            json!("owner/repo#issue:42")
        );
        let count = server
            .with_current_truth_store(|store| {
                store
                    .assertions_for_repo("owner/repo")
                    .map(|assertions| assertions.len())
                    .map_err(|error| error.to_string())
            })
            .unwrap();
        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn full_final_timeline_page_fails_closed_as_incomplete() {
        let full_page = Value::Array(
            (0..TIMELINE_PAGE_SIZE)
                .map(|id| json!({"id": id, "event": "commented"}))
                .collect(),
        );
        let reader = FixtureReader {
            issue: Some(Ok(issue("OPEN", "2026-09-01T00:00:00Z"))),
            timeline: (1..=MAX_TIMELINE_PAGES)
                .map(|page| (page, Ok(full_page.clone())))
                .collect(),
            ..FixtureReader::default()
        };
        let adapter = ProductionGithubRefreshAdapter::load_from(&reader, "owner/repo", 42).await;
        match adapter.refresh("owner/repo") {
            RefreshOutcomeV1::Unavailable { reason, .. } => {
                assert_eq!(reason, REASON_INCOMPLETE)
            }
            other => panic!("expected incomplete refresh, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn malformed_and_denied_reads_share_content_free_unavailable_shape() {
        let denied = FixtureReader {
            visibility: Some(Err("permission denied for private repo".to_string())),
            issue: Some(Ok(issue("OPEN", "2026-09-01T00:00:00Z"))),
            ..FixtureReader::default()
        };
        let malformed = FixtureReader {
            issue: Some(Ok(json!({"number": 42, "state": "UNKNOWN"}))),
            ..FixtureReader::default()
        };
        let denied = ProductionGithubRefreshAdapter::load_from(&denied, "owner/repo", 42).await;
        let malformed =
            ProductionGithubRefreshAdapter::load_from(&malformed, "owner/repo", 42).await;
        match denied.refresh("owner/repo") {
            RefreshOutcomeV1::Unavailable { reason, .. } => {
                assert_eq!(reason, REASON_UNAVAILABLE)
            }
            other => panic!("expected unavailable, got {other:?}"),
        }
        match malformed.refresh("owner/repo") {
            RefreshOutcomeV1::Unavailable { reason, .. } => {
                assert_eq!(reason, REASON_MALFORMED)
            }
            other => panic!("expected unavailable, got {other:?}"),
        }

        let private_malformed = FixtureReader {
            visibility: Some(Ok(json!({"visibility": "PRIVATE"}))),
            issue: Some(Ok(json!({"number": 42, "state": "UNKNOWN"}))),
            ..FixtureReader::default()
        };
        let private_malformed =
            ProductionGithubRefreshAdapter::load_from(&private_malformed, "owner/repo", 42).await;
        assert!(private_malformed.repository_private);
        let attempted_at = "2026-09-01T01:00:00Z";
        let denied_consumed = apply_refresh_and_consume(
            &CurrentTruthSqliteStore::open_in_memory().unwrap(),
            &denied,
            "owner/repo",
            attempted_at,
        )
        .unwrap();
        let private_malformed_consumed = apply_refresh_and_consume(
            &CurrentTruthSqliteStore::open_in_memory().unwrap(),
            &private_malformed,
            "owner/repo",
            attempted_at,
        )
        .unwrap();
        assert_eq!(
            serialize_refresh_response(
                "owner/repo",
                &denied_consumed,
                denied.repository_private,
                attempted_at,
            )
            .unwrap(),
            serialize_refresh_response(
                "owner/repo",
                &private_malformed_consumed,
                private_malformed.repository_private,
                attempted_at,
            )
            .unwrap(),
        );
    }

    #[tokio::test]
    async fn whole_refresh_timeout_preserves_known_private_visibility() {
        struct PrivateSlowReader;

        #[async_trait]
        impl BoundedGithubRefreshReader for PrivateSlowReader {
            async fn repo_visibility(&self, _repo: &str) -> Result<Value, String> {
                Ok(json!({"visibility": "PRIVATE"}))
            }

            async fn issue(&self, _repo: &str, _number: u64) -> Result<Value, String> {
                tokio::time::sleep(Duration::from_millis(50)).await;
                unreachable!("the whole-refresh timeout must cancel this read")
            }

            async fn issue_timeline_page(
                &self,
                _repo: &str,
                _number: u64,
                _page: usize,
            ) -> Result<Value, String> {
                unreachable!("issue read must time out first")
            }

            async fn pull_request(&self, _repo: &str, _number: u64) -> Result<Value, String> {
                unreachable!("issue read must time out first")
            }
        }

        let adapter = ProductionGithubRefreshAdapter::load_from_with_timeout(
            &PrivateSlowReader,
            "owner/repo",
            42,
            Some(Duration::from_millis(1)),
        )
        .await;
        assert!(adapter.repository_private);
        match adapter.refresh("owner/repo") {
            RefreshOutcomeV1::Unavailable { reason, .. } => {
                assert_eq!(reason, REASON_UNAVAILABLE)
            }
            other => panic!("expected unavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn contradictory_same_source_revision_fails_closed() {
        let production = adapter_for(
            issue("OPEN", "2026-09-01T00:00:00Z"),
            vec![cross_reference(7)],
            vec![(
                7,
                pr(
                    7,
                    "MERGED",
                    "2026-09-02T00:00:00Z",
                    Some("merge00000000000000000000000000000000007"),
                ),
            )],
        )
        .await;
        let mut contradictory_state = fresh(&production);
        contradictory_state.pull_requests[0].merge_commit_sha =
            Some("other00000000000000000000000000000000007".to_string());
        let contradictory = ProductionGithubRefreshAdapter {
            outcome: RefreshOutcomeV1::Fresh(Box::new(contradictory_state)),
            repository_private: false,
        };
        let store = CurrentTruthSqliteStore::open_in_memory().unwrap();
        apply_refresh_and_consume(&store, &production, "owner/repo", "2026-09-01T01:00:00Z")
            .unwrap();
        let assertion_count = store.assertions_for_repo("owner/repo").unwrap().len();
        let result =
            apply_refresh_and_consume(&store, &contradictory, "owner/repo", "2026-09-01T02:00:00Z")
                .unwrap();
        assert!(!result.fresh);
        assert_eq!(
            store.assertions_for_repo("owner/repo").unwrap().len(),
            assertion_count,
            "the contradictory batch and its pre-conflict prefix must roll back"
        );
        assert_eq!(
            result.view.posture.unavailable_reason.as_deref(),
            Some(REASON_CONTRADICTORY)
        );
    }
}
