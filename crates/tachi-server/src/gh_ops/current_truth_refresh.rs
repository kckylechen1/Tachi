//! Production GitHub → CurrentTruth refresh (#1696).
//!
//! The live reader reuses `gh_ops`' bounded, credential-hardened command
//! path. It reads one issue, its typed GitHub timeline cross-references, and
//! every PR whose `closingIssuesReferences` confirms the relation. No title
//! or body text participates in linkage. The resulting adapter feeds the
//! existing CurrentTruth assertion store/reducer and then exercises the
//! existing WorkReadModel status consumer in the same production action.
//! GitHub does not expose an authoritative merge-to-revert relation in this
//! bounded surface, so any merged PR makes the refresh typed-unavailable
//! rather than allowing implementation truth to remain falsely fresh.

use super::*;
use async_trait::async_trait;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;
use tachi_params::current_truth::consumer::{self, CallerAuthorizationV1, CurrentTruthViewV1};
use tachi_params::current_truth::refresh::{
    reconcile_refresh, GithubRefreshAdapter, GithubRepositoryStateV1, RefreshOutcomeV1,
    SnapshotIssueStateV1, SnapshotIssueV1, SnapshotObservationKindV1, SnapshotObservationV1,
    SnapshotPrStateV1, SnapshotPrV1, GITHUB_SNAPSHOT_ISSUER, GITHUB_SNAPSHOT_SOURCE_ID,
};
use tachi_params::current_truth::store::{CurrentTruthSqliteStore, CurrentTruthStoreError};
use tachi_params::current_truth::types::{
    ordering_instant, AssertionV1, AuthorityClassV1, PredicateV1, VisibilityClassV1,
};
use tachi_params::work_read_model::{
    project, status_view, CurrentTruthFactsV1, ProjectionOptions, SourceFacts, SourceKind,
    SourceSnapshot, WorkProjectionIndex,
};

mod graphql;
#[cfg(all(test, unix))]
mod partial_error_tests;

use graphql::{parse_graphql_bundle, GITHUB_REFRESH_QUERY};

const CURRENT_TRUTH_GH_TIMEOUT: Duration = Duration::from_secs(6);
const CURRENT_TRUTH_HANDLER_FLOOR: Duration = Duration::from_secs(6);

const REASON_UNAVAILABLE: &str = "github_read_unavailable";
const REASON_MALFORMED: &str = "github_data_malformed";
const REASON_INCOMPLETE: &str = "github_data_incomplete";
const REASON_STALE: &str = "github_data_stale";
const REASON_CONTRADICTORY: &str = "github_data_contradictory_revision";
const REASON_REVERT_UNAVAILABLE: &str = "github_revert_relation_unavailable";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoadFailure {
    Unavailable,
    Malformed,
    Incomplete,
    RevertRelationUnavailable,
}

impl LoadFailure {
    fn reason(&self) -> &'static str {
        match self {
            Self::Unavailable => REASON_UNAVAILABLE,
            Self::Malformed => REASON_MALFORMED,
            Self::Incomplete => REASON_INCOMPLETE,
            Self::RevertRelationUnavailable => REASON_REVERT_UNAVAILABLE,
        }
    }
}

#[derive(Debug)]
struct GithubReadFailure {
    failure: LoadFailure,
    repository_visibility: Option<VisibilityClassV1>,
}

impl GithubReadFailure {
    fn new(failure: LoadFailure, repository_visibility: Option<VisibilityClassV1>) -> Self {
        Self {
            failure,
            repository_visibility,
        }
    }
}

#[derive(Debug, Clone)]
struct GithubReadBundle {
    visibility: VisibilityClassV1,
    issue: Value,
    timeline: Vec<Value>,
    pull_requests: BTreeMap<u64, Value>,
    complete: bool,
}

#[async_trait]
trait BoundedGithubRefreshReader: Send + Sync {
    async fn repository_slice(
        &self,
        repo: &str,
        number: u64,
    ) -> Result<GithubReadBundle, GithubReadFailure>;
}

struct ServerBoundedGithubRefreshReader<'a> {
    server: &'a MemoryServer,
}

#[async_trait]
impl BoundedGithubRefreshReader for ServerBoundedGithubRefreshReader<'_> {
    async fn repository_slice(
        &self,
        repo: &str,
        number: u64,
    ) -> Result<GithubReadBundle, GithubReadFailure> {
        let (owner, name) = repo
            .split_once('/')
            .ok_or_else(|| GithubReadFailure::new(LoadFailure::Malformed, None))?;
        let value = run_gh_json_observed_bounded(
            self.server,
            vec![
                "api".to_string(),
                "graphql".to_string(),
                "-f".to_string(),
                format!("query={GITHUB_REFRESH_QUERY}"),
                "-F".to_string(),
                format!("owner={owner}"),
                "-F".to_string(),
                format!("name={name}"),
                "-F".to_string(),
                format!("number={number}"),
            ],
            CURRENT_TRUTH_GH_TIMEOUT,
            "gh current truth GraphQL read",
        )
        .await
        .map_err(|failure| {
            // A failed command cannot mint fresh truth or relax access. Its
            // independently valid restrictive observation must still survive.
            let visibility = failure
                .observed_json()
                .and_then(graphql::repository_visibility)
                .filter(|visibility| *visibility == VisibilityClassV1::Private);
            GithubReadFailure::new(LoadFailure::Unavailable, visibility)
        })?;
        parse_graphql_bundle(repo, number, &value)
    }
}

/// The production `GithubRefreshAdapter`. Its async loader performs the live
/// bounded reads once; the synchronous trait method then exposes that exact,
/// immutable observation to the existing reconciliation seam.
struct ProductionGithubRefreshAdapter {
    outcome: RefreshOutcomeV1,
    repository_visibility: Option<VisibilityClassV1>,
}

impl ProductionGithubRefreshAdapter {
    async fn load(
        server: &MemoryServer,
        repo: &str,
        issue_number: u64,
    ) -> ProductionGithubRefreshAdapter {
        let reader = ServerBoundedGithubRefreshReader { server };
        Self::load_from(&reader, repo, issue_number).await
    }

    async fn load_from(
        reader: &dyn BoundedGithubRefreshReader,
        repo: &str,
        issue_number: u64,
    ) -> ProductionGithubRefreshAdapter {
        let bundle = match reader.repository_slice(repo, issue_number).await {
            Ok(bundle) => bundle,
            Err(failure) => {
                return Self::unavailable(repo, failure.failure, failure.repository_visibility)
            }
        };
        let visibility = bundle.visibility;
        match load_repository_state(repo, issue_number, visibility, bundle) {
            Ok(state) => Self {
                outcome: RefreshOutcomeV1::Fresh(Box::new(state)),
                repository_visibility: Some(visibility),
            },
            Err(failure) => Self::unavailable(repo, failure, Some(visibility)),
        }
    }

    fn unavailable(
        repo: &str,
        failure: LoadFailure,
        repository_visibility: Option<VisibilityClassV1>,
    ) -> Self {
        Self {
            outcome: RefreshOutcomeV1::Unavailable {
                repo: repo.to_string(),
                // Deliberately content-free: denied, offline, and absent
                // subjects have the same external error posture.
                reason: failure.reason().to_string(),
            },
            repository_visibility,
        }
    }

    fn repository_private(&self) -> bool {
        self.repository_visibility == Some(VisibilityClassV1::Private)
    }
}

impl GithubRefreshAdapter for ProductionGithubRefreshAdapter {
    fn refresh(&self, _repo: &str) -> RefreshOutcomeV1 {
        self.outcome.clone()
    }
}

fn load_repository_state(
    repo: &str,
    issue_number: u64,
    visibility: VisibilityClassV1,
    bundle: GithubReadBundle,
) -> Result<GithubRepositoryStateV1, LoadFailure> {
    if !bundle.complete {
        return Err(LoadFailure::Incomplete);
    }
    let issue = parse_issue(repo, issue_number, &bundle.issue, visibility)?;
    let (candidate_prs, observations) =
        parse_timeline(repo, issue_number, &bundle.timeline, visibility)?;

    let mut pull_requests = Vec::new();
    for pr_number in candidate_prs {
        let value = bundle
            .pull_requests
            .get(&pr_number)
            .ok_or(LoadFailure::Malformed)?;
        if let Some(pr) = parse_linked_pr(repo, issue_number, pr_number, value, visibility)? {
            pull_requests.push(pr);
        }
    }
    pull_requests.sort_by_key(|pr| pr.number);
    if pull_requests
        .iter()
        .any(|pr| pr.state == SnapshotPrStateV1::Merged)
    {
        // GitHub's bounded typed issue/PR surface identifies merges but has
        // no authoritative original-merge → reverting-commit relation. Do
        // not preserve a merge as fresh when its overturn status is unknown.
        return Err(LoadFailure::RevertRelationUnavailable);
    }

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

#[cfg(test)]
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
    let updated_at = value
        .get("updatedAt")
        .and_then(Value::as_str)
        .filter(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).is_ok())
        .ok_or(LoadFailure::Malformed)?;
    let object_revision = memcore::canonical_digest::canonical_json_digest_hex(&json!({
        "repo": repo,
        "number": number,
        "state": state,
        "updated_at": updated_at,
    }));
    Ok(SnapshotIssueV1 {
        number,
        state,
        updated_at: updated_at.to_string(),
        snapshot_revision: visibility_bound_revision("issue", &object_revision, visibility),
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
                let id = match event.get("id") {
                    Some(Value::String(id)) if !id.is_empty() => id.clone(),
                    Some(Value::Number(id)) => id.to_string(),
                    _ => return Err(LoadFailure::Malformed),
                };
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
                    revision: visibility_bound_revision("event", &id, visibility),
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

    let source_state = value
        .get("state")
        .and_then(Value::as_str)
        .ok_or(LoadFailure::Malformed)?;
    let state = if source_state.eq_ignore_ascii_case("MERGED") {
        SnapshotPrStateV1::Merged
    } else if source_state.eq_ignore_ascii_case("OPEN") {
        SnapshotPrStateV1::Open
    } else if source_state.eq_ignore_ascii_case("CLOSED") {
        SnapshotPrStateV1::ClosedUnmerged
    } else {
        return Err(LoadFailure::Malformed);
    };
    let updated_at = value
        .get("updatedAt")
        .and_then(Value::as_str)
        .filter(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).is_ok())
        .ok_or(LoadFailure::Malformed)?;
    let head_sha = value
        .get("headRefOid")
        .and_then(Value::as_str)
        .filter(|sha| !sha.is_empty())
        .ok_or(LoadFailure::Malformed)?;
    let base_sha = value
        .get("baseRefOid")
        .and_then(Value::as_str)
        .filter(|sha| !sha.is_empty())
        .ok_or(LoadFailure::Malformed)?;
    let merge_commit_sha = value
        .pointer("/mergeCommit/oid")
        .and_then(Value::as_str)
        .filter(|sha| !sha.is_empty())
        .map(str::to_string);
    let object_revision = memcore::canonical_digest::canonical_json_digest_hex(&json!({
        "repo": repo,
        "number": number,
        "state": state,
        "head_sha": head_sha,
        "base_sha": base_sha,
        "merge_commit_sha": merge_commit_sha,
        "updated_at": updated_at,
        "linked_issues": linked_issues,
    }));
    Ok(Some(SnapshotPrV1 {
        number,
        state,
        merge_commit_sha,
        updated_at: updated_at.to_string(),
        snapshot_revision: visibility_bound_revision("pull-request", &object_revision, visibility),
        linked_issues,
        visibility,
    }))
}

struct ConsumedRefresh {
    view: CurrentTruthViewV1,
    work_statuses: Vec<tachi_params::work_read_model::WorkStatusRowV1>,
    fresh: bool,
    repository_private: bool,
}

// Preserve the consumer error inside the public handler boundary while the
// store owns transaction rollback and typed SQLite failures.
enum RefreshConsumptionError {
    Store(CurrentTruthStoreError),
    Consumer(String),
}

impl RefreshConsumptionError {
    fn into_message(self) -> String {
        match self {
            Self::Store(error) => error.to_string(),
            Self::Consumer(error) => error,
        }
    }
}

impl From<CurrentTruthStoreError> for RefreshConsumptionError {
    fn from(error: CurrentTruthStoreError) -> Self {
        Self::Store(error)
    }
}

#[cfg(test)]
fn apply_refresh_and_consume(
    store: &CurrentTruthSqliteStore,
    adapter: &dyn GithubRefreshAdapter,
    repo: &str,
    attempted_at: &str,
) -> Result<ConsumedRefresh, String> {
    apply_subject_refresh_and_consume(
        store,
        adapter,
        repo,
        &format!("{repo}#issue:42"),
        None,
        attempted_at,
    )
}

fn apply_subject_refresh_and_consume(
    store: &CurrentTruthSqliteStore,
    adapter: &dyn GithubRefreshAdapter,
    repo: &str,
    subject_token: &str,
    repository_visibility: Option<VisibilityClassV1>,
    attempted_at: &str,
) -> Result<ConsumedRefresh, String> {
    // Commit restrictive repository metadata before any downstream truth
    // work whenever a posture row already exists. This makes a public→private
    // observation durable even if assertion/posture processing later fails.
    // A first observation has no row yet; it is recorded after that attempt
    // creates one.
    let restrictive_visibility =
        repository_visibility.filter(|visibility| *visibility == VisibilityClassV1::Private);
    let visibility_needs_row = match restrictive_visibility {
        Some(visibility) => !store
            .record_repository_visibility(repo, visibility, attempted_at)
            .map_err(|error| error.to_string())?,
        None => false,
    };
    let (outcome, assertions) = reconcile_refresh(adapter, repo);
    match outcome {
        RefreshOutcomeV1::Fresh(state) => {
            if refresh_is_stale(store, &state, &assertions)? {
                record_unavailable(store, repo, subject_token, attempted_at, REASON_STALE)?;
            } else {
                match store.append_subject_refresh_and_consume(
                    &assertions,
                    repo,
                    subject_token,
                    &state.refresh_revision,
                    &state.refreshed_at,
                    attempted_at,
                    repository_visibility,
                    |store, _| {
                        consume_refresh(store, repo, attempted_at)
                            .map_err(RefreshConsumptionError::Consumer)
                    },
                ) {
                    Ok(consumed) => return Ok(consumed),
                    Err(RefreshConsumptionError::Store(
                        CurrentTruthStoreError::ContradictsExistingRevision(_),
                    )) => {
                        record_unavailable(
                            store,
                            repo,
                            subject_token,
                            attempted_at,
                            REASON_CONTRADICTORY,
                        )?;
                    }
                    Err(RefreshConsumptionError::Store(
                        CurrentTruthStoreError::StaleRefreshRecord { .. },
                    )) => {
                        // A later-started attempt already committed. This
                        // transaction rolled back, so consume that newer
                        // posture without letting the late result regress it.
                    }
                    Err(error) => return Err(error.into_message()),
                }
            }
        }
        RefreshOutcomeV1::Unavailable { reason, .. } => {
            record_unavailable(store, repo, subject_token, attempted_at, &reason)?;
        }
    }

    // Visibility has independent ordering from subject posture. If no row
    // existed before this attempt, persist it now that posture created one.
    // A denied attempt has no observation and cannot erase a known value.
    if visibility_needs_row {
        let visibility = restrictive_visibility.expect("checked restrictive observation");
        store
            .record_repository_visibility(repo, visibility, attempted_at)
            .map_err(|error| error.to_string())?;
    }

    // Unavailable/stale/contradictory/late results consume the existing
    // restriction; none may borrow another attempt's fresh posture to relax it.
    consume_refresh(store, repo, attempted_at)
}

fn consume_refresh(
    store: &CurrentTruthSqliteStore,
    repo: &str,
    attempted_at: &str,
) -> Result<ConsumedRefresh, String> {
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
    let repository_private = store
        .repository_visibility(repo)
        .map_err(|error| error.to_string())?
        == Some(VisibilityClassV1::Private)
        || !store
            .private_subject_tokens(repo)
            .map_err(|error| error.to_string())?
            .is_empty();
    Ok(ConsumedRefresh {
        repository_private,
        fresh: view.posture.fresh,
        view,
        work_statuses,
    })
}

fn record_unavailable(
    store: &CurrentTruthSqliteStore,
    repo: &str,
    subject_token: &str,
    attempted_at: &str,
    reason: &str,
) -> Result<(), String> {
    match store.record_subject_refresh(
        repo,
        subject_token,
        false,
        None,
        None,
        attempted_at,
        Some(reason),
    ) {
        Ok(()) | Err(CurrentTruthStoreError::StaleRefreshRecord { .. }) => Ok(()),
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
    handle_current_truth_refresh_with_floor(server, params, CURRENT_TRUTH_HANDLER_FLOOR).await
}

async fn handle_current_truth_refresh_with_floor(
    server: &MemoryServer,
    params: &TachiGhParams,
    floor: Duration,
) -> Result<String, String> {
    // A single minimum completion floor covers the complete handler:
    // command execution, post-command JSON decode, persistence, reduction,
    // consumer projection, and response serialization. The command timeout
    // bounds only the subprocess wait; this floor is neither a timeout nor a
    // claim of statistical latency equality for work that exceeds it.
    complete_with_floor(floor, handle_current_truth_refresh_unpadded(server, params)).await
}

async fn complete_with_floor<T>(
    floor: Duration,
    operation: impl std::future::Future<Output = T>,
) -> T {
    let ((), result) = tokio::join!(tokio::time::sleep(floor), operation);
    result
}

async fn handle_current_truth_refresh_unpadded(
    server: &MemoryServer,
    params: &TachiGhParams,
) -> Result<String, String> {
    let repo = super::router::required_repo(params, "current_truth_refresh")?;
    let issue_number = params
        .number
        .ok_or("current_truth_refresh requires issue 'number'")?;
    validate_repo(&repo)?;
    // GitHub owner/repository identity is ASCII case-insensitive. One stable
    // spelling prevents privacy, debt, and subject history from splitting
    // across caller-selected casing.
    let repo = repo.to_ascii_lowercase();
    let attempted_at = chrono::Utc::now().to_rfc3339();
    let adapter = ProductionGithubRefreshAdapter::load(server, &repo, issue_number).await;
    let subject_token = format!("{repo}#issue:{issue_number}");
    let consumed = server.with_current_truth_store(|store| {
        // Keep failed reductions inside this public boundary. Decode errors
        // may carry private row values, and failure must establish debt without
        // first decoding the same potentially corrupt identity for privacy.
        let consumed = match apply_subject_refresh_and_consume(
            store,
            &adapter,
            &repo,
            &subject_token,
            adapter.repository_visibility,
            &attempted_at,
        ) {
            Ok(consumed) => consumed,
            Err(_) => {
                record_unavailable(
                    store,
                    &repo,
                    &subject_token,
                    &attempted_at,
                    REASON_UNAVAILABLE,
                )?;
                // A first failed attempt just created its posture row. Reuse
                // the original independent visibility ordering to attach the
                // observed restriction, even when private identity is corrupt.
                if let Some(visibility) = adapter
                    .repository_visibility
                    .filter(|visibility| *visibility == VisibilityClassV1::Private)
                {
                    store
                        .record_repository_visibility(&repo, visibility, &attempted_at)
                        .map_err(|error| error.to_string())?;
                }
                return Ok(None);
            }
        };
        Ok(Some(consumed))
    });

    // Store/metadata failures are unavailable at this public boundary too.
    // A failed debt write cannot claim durable persistence, and its error
    // shape must not reveal whether inaccessible history exists.
    match consumed {
        Ok(Some(consumed)) => serialize_refresh_response(
            &repo,
            &consumed,
            adapter.repository_private() || consumed.repository_private,
            &attempted_at,
        ),
        Ok(None) | Err(_) => serialize_unavailable_refresh_response(&repo, &attempted_at),
    }
}

fn serialize_unavailable_refresh_response(
    repo: &str,
    attempted_at: &str,
) -> Result<String, String> {
    serde_json::to_string(&json!({
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
    .map_err(|error| format!("serialize current truth refresh: {error}"))
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
        return serialize_unavailable_refresh_response(repo, attempted_at);
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
        incomplete: bool,
        calls: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl BoundedGithubRefreshReader for FixtureReader {
        async fn repository_slice(
            &self,
            _repo: &str,
            _number: u64,
        ) -> Result<GithubReadBundle, GithubReadFailure> {
            self.calls.lock().unwrap().push("slice".to_string());
            let visibility_value = self
                .visibility
                .clone()
                .unwrap_or_else(|| Ok(json!({"visibility": "PUBLIC"})))
                .map_err(|_| GithubReadFailure::new(LoadFailure::Unavailable, None))?;
            let visibility = parse_visibility(&visibility_value)
                .map_err(|failure| GithubReadFailure::new(failure, None))?;
            let issue =
                self.issue.clone().expect("issue fixture").map_err(|_| {
                    GithubReadFailure::new(LoadFailure::Unavailable, Some(visibility))
                })?;
            let timeline = self
                .timeline
                .get(&1)
                .cloned()
                .unwrap_or_else(|| Ok(json!([])))
                .map_err(|_| GithubReadFailure::new(LoadFailure::Unavailable, Some(visibility)))?
                .as_array()
                .cloned()
                .ok_or_else(|| GithubReadFailure::new(LoadFailure::Malformed, Some(visibility)))?;
            let pull_requests = self
                .prs
                .iter()
                .map(|(number, value)| {
                    Ok((
                        *number,
                        value.clone().map_err(|_| {
                            GithubReadFailure::new(LoadFailure::Unavailable, Some(visibility))
                        })?,
                    ))
                })
                .collect::<Result<BTreeMap<_, _>, GithubReadFailure>>()?;
            Ok(GithubReadBundle {
                visibility,
                issue,
                timeline,
                pull_requests,
                complete: !self.incomplete,
            })
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

    fn synthetic_merged_adapter(merge_commit_sha: &str) -> ProductionGithubRefreshAdapter {
        ProductionGithubRefreshAdapter {
            outcome: RefreshOutcomeV1::Fresh(Box::new(GithubRepositoryStateV1 {
                repo: "owner/repo".to_string(),
                refresh_revision: "synthetic-merged-refresh".to_string(),
                refreshed_at: "2026-09-02T00:00:00Z".to_string(),
                issues: vec![SnapshotIssueV1 {
                    number: 42,
                    state: SnapshotIssueStateV1::Open,
                    updated_at: "2026-09-01T00:00:00Z".to_string(),
                    snapshot_revision: "synthetic-issue-revision".to_string(),
                    visibility: VisibilityClassV1::Public,
                }],
                pull_requests: vec![SnapshotPrV1 {
                    number: 7,
                    state: SnapshotPrStateV1::Merged,
                    merge_commit_sha: Some(merge_commit_sha.to_string()),
                    updated_at: "2026-09-02T00:00:00Z".to_string(),
                    snapshot_revision: "synthetic-pr-revision".to_string(),
                    linked_issues: vec![42],
                    visibility: VisibilityClassV1::Public,
                }],
                observations: vec![],
            })),
            repository_visibility: Some(VisibilityClassV1::Public),
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
    async fn merged_pr_without_typed_revert_relation_marks_refresh_unavailable() {
        let adapter = adapter_for(
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
        match adapter.refresh("owner/repo") {
            RefreshOutcomeV1::Unavailable { reason, .. } => {
                assert_eq!(reason, REASON_REVERT_UNAVAILABLE)
            }
            other => panic!("merged state must be unavailable, got {other:?}"),
        }
        let store = CurrentTruthSqliteStore::open_in_memory().unwrap();
        let consumed =
            apply_refresh_and_consume(&store, &adapter, "owner/repo", "2026-09-02T01:00:00Z")
                .unwrap();
        assert!(!consumed.fresh);
        assert_eq!(
            consumed.view.posture.unavailable_reason.as_deref(),
            Some(REASON_REVERT_UNAVAILABLE)
        );
        assert!(consumed.view.subjects.is_empty());
        assert!(store.assertions_for_repo("owner/repo").unwrap().is_empty());
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

    #[test]
    fn graphql_relation_normalizes_to_the_canonical_closed_unmerged_state() {
        let raw = json!({"data": {"repository": {
            "visibility": "PUBLIC",
            "issueOrPullRequest": {
                "__typename": "Issue",
                "number": 42,
                "state": "OPEN",
                "updatedAt": "2026-09-01T00:00:00Z",
                "timelineItems": {
                    "pageInfo": {"hasNextPage": false},
                    "nodes": [{
                        "__typename": "CrossReferencedEvent",
                        "id": "event-8",
                        "createdAt": "2026-09-02T00:00:00Z",
                        "source": {
                            "__typename": "PullRequest",
                            "number": 8,
                            "state": "CLOSED",
                            "updatedAt": "2026-09-02T00:00:00Z",
                            "headRefOid": "head000000000000000000000000000000000008",
                            "baseRefOid": "base000000000000000000000000000000000008",
                            "mergeCommit": null,
                            "repository": {"nameWithOwner": "owner/repo"},
                            "closingIssuesReferences": {
                                "pageInfo": {"hasNextPage": false},
                                "nodes": [{
                                    "number": 42,
                                    "repository": {"nameWithOwner": "owner/repo"}
                                }]
                            }
                        }
                    }]
                }
            }
        }}});
        let bundle = parse_graphql_bundle("owner/repo", 42, &raw).unwrap();
        let state =
            load_repository_state("owner/repo", 42, VisibilityClassV1::Public, bundle).unwrap();
        assert_eq!(state.pull_requests.len(), 1);
        assert_eq!(
            state.pull_requests[0].state,
            SnapshotPrStateV1::ClosedUnmerged
        );
        assert_eq!(state.pull_requests[0].linked_issues, vec![42]);
    }

    #[test]
    fn malformed_cross_reference_source_fails_closed_instead_of_minting_empty_linkage() {
        fn raw(source: Value) -> Value {
            json!({"data": {"repository": {
                "visibility": "PUBLIC",
                "issueOrPullRequest": {
                    "__typename": "Issue",
                    "number": 42,
                    "state": "OPEN",
                    "updatedAt": "2026-09-01T00:00:00Z",
                    "timelineItems": {
                        "pageInfo": {"hasNextPage": false},
                        "nodes": [{
                            "__typename": "CrossReferencedEvent",
                            "id": "event-malformed",
                            "createdAt": "2026-09-02T00:00:00Z",
                            "source": source
                        }]
                    }
                }
            }}})
        }

        for source in [
            Value::Null,
            json!("not-an-object"),
            json!({}),
            json!({"__typename": null}),
            json!({"__typename": "UnknownTypedSource"}),
        ] {
            assert!(matches!(
                parse_graphql_bundle("owner/repo", 42, &raw(source)),
                Err(GithubReadFailure {
                    failure: LoadFailure::Malformed,
                    ..
                })
            ));
        }
        let mut missing = raw(json!({"__typename": "Issue"}));
        missing
            .pointer_mut("/data/repository/issueOrPullRequest/timelineItems/nodes/0")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("source");
        assert!(matches!(
            parse_graphql_bundle("owner/repo", 42, &missing),
            Err(GithubReadFailure {
                failure: LoadFailure::Malformed,
                ..
            })
        ));

        let valid_non_pr =
            parse_graphql_bundle("owner/repo", 42, &raw(json!({"__typename": "Issue"}))).unwrap();
        assert!(valid_non_pr.timeline.is_empty());
        assert!(valid_non_pr.pull_requests.is_empty());
    }

    #[test]
    fn graphql_envelope_and_nonclosing_pr_fields_are_validated_before_skipping() {
        fn raw(source: Value) -> Value {
            json!({"data": {"repository": {
                "visibility": "PRIVATE",
                "issueOrPullRequest": {
                    "__typename": "Issue",
                    "number": 42,
                    "state": "OPEN",
                    "updatedAt": "2026-09-01T00:00:00Z",
                    "timelineItems": {
                        "pageInfo": {"hasNextPage": false},
                        "nodes": [{
                            "__typename": "CrossReferencedEvent",
                            "id": "event-7",
                            "createdAt": "2026-09-02T00:00:00Z",
                            "source": source
                        }]
                    }
                }
            }}})
        }
        fn valid_nonclosing_pr() -> Value {
            json!({
                "__typename": "PullRequest",
                "number": 7,
                "state": "OPEN",
                "updatedAt": "2026-09-02T00:00:00Z",
                "headRefOid": "head000000000000000000000000000000000007",
                "baseRefOid": "base000000000000000000000000000000000007",
                "mergeCommit": null,
                "repository": {"nameWithOwner": "owner/repo"},
                "closingIssuesReferences": {
                    "pageInfo": {"hasNextPage": false},
                    "nodes": []
                }
            })
        }

        for field in ["state", "updatedAt", "headRefOid", "baseRefOid"] {
            let mut source = valid_nonclosing_pr();
            source[field] = Value::Null;
            let failure = parse_graphql_bundle("owner/repo", 42, &raw(source)).unwrap_err();
            assert_eq!(failure.failure, LoadFailure::Malformed, "field {field}");
            assert_eq!(
                failure.repository_visibility,
                Some(VisibilityClassV1::Private),
                "restrictive visibility survives malformed field {field}"
            );
        }
        let mut malformed_merge = valid_nonclosing_pr();
        malformed_merge["mergeCommit"] = json!({});
        assert_eq!(
            parse_graphql_bundle("owner/repo", 42, &raw(malformed_merge))
                .unwrap_err()
                .failure,
            LoadFailure::Malformed
        );

        let mut wrong_errors = raw(valid_nonclosing_pr());
        wrong_errors["errors"] = json!({"message": "wrong envelope type"});
        let failure = parse_graphql_bundle("owner/repo", 42, &wrong_errors).unwrap_err();
        assert_eq!(failure.failure, LoadFailure::Malformed);
        assert_eq!(
            failure.repository_visibility,
            Some(VisibilityClassV1::Private)
        );
        wrong_errors["errors"] = json!([]);
        parse_graphql_bundle("owner/repo", 42, &wrong_errors)
            .expect("an empty GraphQL errors array is valid");
    }

    #[tokio::test]
    async fn production_adapter_reopen_is_append_only() {
        let mut reopened = cross_reference(7);
        reopened["event"] = json!("reopened");
        reopened["id"] = json!(9001);
        reopened["created_at"] = json!("2026-09-03T00:00:00Z");
        let adapter = adapter_for(
            issue("OPEN", "2026-09-03T00:00:00Z"),
            vec![cross_reference(7), reopened],
            vec![(7, pr(7, "OPEN", "2026-09-02T00:00:00Z", None))],
        )
        .await;
        let state = fresh(&adapter);
        assert!(state.observations.iter().any(|observation| matches!(
            observation.kind,
            SnapshotObservationKindV1::IssueReopened { number: 42 }
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
            repository_visibility: Some(VisibilityClassV1::Public),
        };
        let store = CurrentTruthSqliteStore::open_in_memory().unwrap();
        apply_refresh_and_consume(&store, &newer, "owner/repo", "2026-09-04T01:00:00Z").unwrap();
        let result = apply_subject_refresh_and_consume(
            &store,
            &older_other_issue,
            "owner/repo",
            "owner/repo#issue:43",
            older_other_issue.repository_visibility,
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

    #[tokio::test]
    async fn successful_sibling_issue_refresh_cannot_clear_merged_relation_debt() {
        let issue_a_open = adapter_for(issue("OPEN", "2026-09-01T00:00:00Z"), vec![], vec![]).await;
        let issue_a_merged = adapter_for(
            issue("OPEN", "2026-09-02T00:00:00Z"),
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
        let mut issue_b_state =
            fresh(&adapter_for(issue("OPEN", "2026-09-03T00:00:00Z"), vec![], vec![]).await);
        issue_b_state.issues[0].number = 43;
        issue_b_state.issues[0].snapshot_revision =
            visibility_bound_revision("issue", "issue-43", VisibilityClassV1::Public);
        issue_b_state.refresh_revision = "issue-43-refresh".to_string();
        let issue_b = ProductionGithubRefreshAdapter {
            outcome: RefreshOutcomeV1::Fresh(Box::new(issue_b_state)),
            repository_visibility: Some(VisibilityClassV1::Public),
        };

        let store = CurrentTruthSqliteStore::open_in_memory().unwrap();
        apply_subject_refresh_and_consume(
            &store,
            &issue_a_open,
            "owner/repo",
            "owner/repo#issue:42",
            issue_a_open.repository_visibility,
            "2026-09-01T01:00:00Z",
        )
        .unwrap();
        let debt = apply_subject_refresh_and_consume(
            &store,
            &issue_a_merged,
            "owner/repo",
            "owner/repo#issue:42",
            issue_a_merged.repository_visibility,
            "2026-09-02T01:00:00Z",
        )
        .unwrap();
        assert!(!debt.fresh);

        let after_b = apply_subject_refresh_and_consume(
            &store,
            &issue_b,
            "owner/repo",
            "owner/repo#issue:43",
            issue_b.repository_visibility,
            "2026-09-03T01:00:00Z",
        )
        .unwrap();
        assert!(!after_b.fresh);
        assert_eq!(
            after_b.view.posture.unavailable_reason.as_deref(),
            Some(REASON_REVERT_UNAVAILABLE)
        );
        assert_eq!(after_b.view.health.repos_with_refresh_debt, 1);
        let issue_a = after_b
            .view
            .subjects
            .iter()
            .find(|subject| subject.subject_token == "owner/repo#issue:42")
            .unwrap();
        assert_eq!(
            issue_a.open_action.as_ref().unwrap().kind,
            tachi_params::current_truth::types::OpenActionKindV1::RefreshUnavailableSource
        );
        assert!(!store
            .assertions_for_repo("owner/repo")
            .unwrap()
            .iter()
            .any(|assertion| {
                assertion.predicate == PredicateV1::PrMerged
                    || (assertion.predicate == PredicateV1::ImplementationPresent
                        && matches!(
                            assertion.value,
                            tachi_params::current_truth::types::AssertionValueV1::CommitSha(_)
                        ))
            }));
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
        let result = apply_subject_refresh_and_consume(
            &store,
            &adapter,
            "owner/repo",
            "owner/repo#issue:42",
            adapter.repository_visibility,
            "2026-09-01T01:00:00Z",
        )
        .unwrap();
        assert!(result.fresh);
        assert!(result.view.subjects.is_empty());
        assert_eq!(result.view.health.conflicted_predicates, 0);
        assert_eq!(result.view.health.repos_with_refresh_debt, 0);
        assert!(result.work_statuses.is_empty());
        assert_eq!(reader.calls.lock().unwrap().as_slice(), ["slice"]);
        let response = serialize_refresh_response(
            "owner/repo",
            &result,
            adapter.repository_private(),
            "2026-09-01T01:00:00Z",
        )
        .unwrap();
        let response: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["fresh"], json!(false));
        assert_eq!(response["posture"]["last_fresh_revision"], Value::Null);
        assert_eq!(response["work_status"], json!([]));

        let denied_reader = FixtureReader {
            visibility: Some(Err("denied".to_string())),
            ..FixtureReader::default()
        };
        let denied =
            ProductionGithubRefreshAdapter::load_from(&denied_reader, "owner/repo", 42).await;
        assert_eq!(denied_reader.calls.lock().unwrap().as_slice(), ["slice"]);
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
    async fn failed_private_refresh_then_denied_read_durably_hides_public_history() {
        let public = adapter_for(issue("OPEN", "2026-09-01T00:00:00Z"), vec![], vec![]).await;
        let private_reader = FixtureReader {
            visibility: Some(Ok(json!({"visibility": "PRIVATE"}))),
            issue: Some(Ok(issue("OPEN", "2026-09-02T00:00:00Z"))),
            timeline: BTreeMap::from([(1, Ok(json!([cross_reference(7)])))]),
            prs: BTreeMap::from([(
                7,
                Ok(pr(
                    7,
                    "MERGED",
                    "2026-09-02T00:00:00Z",
                    Some("merge00000000000000000000000000000000007"),
                )),
            )]),
            ..FixtureReader::default()
        };
        let private_failed =
            ProductionGithubRefreshAdapter::load_from(&private_reader, "owner/repo", 42).await;
        let denied_reader = FixtureReader {
            visibility: Some(Err("not found or denied".to_string())),
            ..FixtureReader::default()
        };
        let denied =
            ProductionGithubRefreshAdapter::load_from(&denied_reader, "owner/repo", 42).await;
        let store = CurrentTruthSqliteStore::open_in_memory().unwrap();

        apply_subject_refresh_and_consume(
            &store,
            &public,
            "owner/repo",
            "owner/repo#issue:42",
            Some(VisibilityClassV1::Public),
            "2026-09-01T01:00:00Z",
        )
        .unwrap();
        let after_private_failure = apply_subject_refresh_and_consume(
            &store,
            &private_failed,
            "owner/repo",
            "owner/repo#issue:42",
            private_failed.repository_visibility,
            "2026-09-02T01:00:00Z",
        )
        .unwrap();
        assert!(after_private_failure.view.subjects.is_empty());
        let after_denied = apply_subject_refresh_and_consume(
            &store,
            &denied,
            "owner/repo",
            "owner/repo#issue:42",
            denied.repository_visibility,
            "2026-09-03T01:00:00Z",
        )
        .unwrap();

        assert_eq!(
            store.repository_visibility("owner/repo").unwrap(),
            Some(VisibilityClassV1::Private)
        );
        assert!(after_denied.view.subjects.is_empty());
        assert!(after_denied.work_statuses.is_empty());
        let response =
            serialize_refresh_response("owner/repo", &after_denied, true, "2026-09-03T01:00:00Z")
                .unwrap();
        assert!(!response.contains("issue:42"));
        assert!(!response.contains("github-issue"));
        let response: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["work_status"], json!([]));
        assert_eq!(response["posture"]["last_fresh_revision"], Value::Null);
    }

    #[tokio::test]
    async fn stale_and_equal_private_observations_survive_subject_posture_refusal() {
        let public = adapter_for(issue("OPEN", "2026-09-01T00:00:00Z"), vec![], vec![]).await;
        let denied_reader = FixtureReader {
            visibility: Some(Err("denied".to_string())),
            ..FixtureReader::default()
        };
        let denied =
            ProductionGithubRefreshAdapter::load_from(&denied_reader, "owner/repo", 42).await;
        let private_malformed_reader = FixtureReader {
            visibility: Some(Ok(json!({"visibility": "PRIVATE"}))),
            issue: Some(Ok(json!({"number": 42, "state": "UNKNOWN"}))),
            ..FixtureReader::default()
        };
        let private_malformed =
            ProductionGithubRefreshAdapter::load_from(&private_malformed_reader, "owner/repo", 42)
                .await;

        for private_attempt_at in ["2026-09-01T11:00:00Z", "2026-09-01T12:00:00Z"] {
            let store = CurrentTruthSqliteStore::open_in_memory().unwrap();
            apply_subject_refresh_and_consume(
                &store,
                &public,
                "owner/repo",
                "owner/repo#issue:42",
                public.repository_visibility,
                "2026-09-01T10:00:00Z",
            )
            .unwrap();
            apply_subject_refresh_and_consume(
                &store,
                &denied,
                "owner/repo",
                "owner/repo#issue:42",
                denied.repository_visibility,
                "2026-09-01T12:00:00Z",
            )
            .unwrap();
            let consumed = apply_subject_refresh_and_consume(
                &store,
                &private_malformed,
                "owner/repo",
                "owner/repo#issue:42",
                private_malformed.repository_visibility,
                private_attempt_at,
            )
            .unwrap();

            let posture = store.refresh_posture_row("OWNER/REPO").unwrap().unwrap();
            assert_eq!(
                posture.repository_visibility,
                Some(VisibilityClassV1::Private)
            );
            assert!(consumed.view.subjects.is_empty());
            assert_eq!(posture.last_attempt_at, "2026-09-01T12:00:00Z");
            assert_eq!(
                posture.unavailable_reason.as_deref(),
                Some(REASON_UNAVAILABLE),
                "the later/equal first committer keeps subject posture"
            );
        }
    }

    #[tokio::test]
    async fn production_and_fake_adapters_mint_canonically_equivalent_assertions() {
        let production = adapter_for(
            issue("OPEN", "2026-09-01T00:00:00Z"),
            vec![cross_reference(7)],
            vec![(7, pr(7, "OPEN", "2026-09-02T00:00:00Z", None))],
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
  "api graphql") echo '{"data":{"repository":{"visibility":"PUBLIC","issueOrPullRequest":{"__typename":"Issue","number":42,"state":"OPEN","updatedAt":"2026-09-01T00:00:00Z","timelineItems":{"pageInfo":{"hasNextPage":false},"nodes":[]}}}}}' ;;
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
    async fn paginated_graphql_relation_set_fails_closed_as_incomplete() {
        let reader = FixtureReader {
            issue: Some(Ok(issue("OPEN", "2026-09-01T00:00:00Z"))),
            incomplete: true,
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
        assert!(private_malformed.repository_private());
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
                denied.repository_private(),
                attempted_at,
            )
            .unwrap(),
            serialize_refresh_response(
                "owner/repo",
                &private_malformed_consumed,
                private_malformed.repository_private(),
                attempted_at,
            )
            .unwrap(),
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn full_handler_private_and_denied_paths_share_one_call_and_content_free_shape() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let bin = tempfile::tempdir().unwrap();
        let mode = bin.path().join("mode");
        let calls = bin.path().join("calls");
        let script = format!(
            r#"#!/bin/sh
echo call >> '{}'
if [ "$(cat '{}')" = private ]; then
  echo '{{"data":{{"repository":{{"visibility":"PRIVATE","issueOrPullRequest":{{"__typename":"Issue","number":42,"state":"OPEN","updatedAt":"2026-09-01T00:00:00Z","timelineItems":{{"pageInfo":{{"hasNextPage":false}},"nodes":[]}}}}}}}}}}'
else
  exit 1
fi
"#,
            calls.display(),
            mode.display(),
        );
        write_executable(&bin.path().join("gh"), &script);
        let _path = PathEnvGuard::prepend(bin.path());
        let server = crate::tests::make_server();
        let params = TachiGhParams {
            action: "current_truth_refresh".to_string(),
            repo: Some("owner/repo".to_string()),
            number: Some(42),
            ..TachiGhParams::default()
        };
        std::fs::write(&mode, "private").unwrap();
        let private = handle_current_truth_refresh_with_floor(&server, &params, Duration::ZERO)
            .await
            .unwrap();
        std::fs::write(&mode, "denied").unwrap();
        let denied = handle_current_truth_refresh_with_floor(&server, &params, Duration::ZERO)
            .await
            .unwrap();

        assert_eq!(std::fs::read_to_string(calls).unwrap().lines().count(), 2);
        let mut private: Value = serde_json::from_str(&private).unwrap();
        let mut denied: Value = serde_json::from_str(&denied).unwrap();
        private["posture"]["last_attempt_at"] = Value::Null;
        denied["posture"]["last_attempt_at"] = Value::Null;
        assert_eq!(private, denied);
        assert_eq!(denied["work_status"], json!([]));
    }

    #[tokio::test(start_paused = true)]
    async fn complete_handler_floor_holds_an_immediate_operation_until_deadline() {
        let floor = Duration::from_secs(6);
        let task = tokio::spawn(complete_with_floor(floor, async { "complete" }));
        tokio::task::yield_now().await;
        assert!(
            !task.is_finished(),
            "an immediate operation must still wait"
        );

        tokio::time::advance(floor - Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert!(
            !task.is_finished(),
            "the floor must hold before its deadline"
        );

        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(task.await.unwrap(), "complete");
    }

    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn raw_private_parse_failures_survive_mixed_case_then_denied_reads() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let bin = tempfile::tempdir().unwrap();
        let mode = bin.path().join("mode");
        let payload = bin.path().join("payload");
        let script = format!(
            r#"#!/bin/sh
if [ "$(cat '{}')" = denied ]; then
  exit 1
fi
cat '{}'
"#,
            mode.display(),
            payload.display(),
        );
        write_executable(&bin.path().join("gh"), &script);
        let _path = PathEnvGuard::prepend(bin.path());
        let public = json!({"data": {"repository": {
            "visibility": "PUBLIC",
            "issueOrPullRequest": {
                "__typename": "Issue",
                "number": 42,
                "state": "OPEN",
                "updatedAt": "2026-09-01T00:00:00Z",
                "timelineItems": {
                    "pageInfo": {"hasNextPage": false},
                    "nodes": []
                }
            }
        }}});
        let private_null = json!({"data": {"repository": {
            "visibility": "PRIVATE",
            "issueOrPullRequest": null
        }}});
        let private_malformed = json!({"data": {"repository": {
            "visibility": "PRIVATE",
            "issueOrPullRequest": {
                "__typename": "Issue",
                "number": 42,
                "state": "OPEN",
                "updatedAt": "2026-09-02T00:00:00Z",
                "timelineItems": {
                    "pageInfo": {"hasNextPage": false},
                    "nodes": [{
                        "__typename": "CrossReferencedEvent",
                        "id": "event-malformed-private",
                        "createdAt": "2026-09-02T00:00:00Z",
                        "source": {
                            "__typename": "PullRequest",
                            "number": 7,
                            "state": null,
                            "updatedAt": "2026-09-02T00:00:00Z",
                            "headRefOid": "head000000000000000000000000000000000007",
                            "baseRefOid": "base000000000000000000000000000000000007",
                            "mergeCommit": null,
                            "repository": {"nameWithOwner": "owner/repo"},
                            "closingIssuesReferences": {
                                "pageInfo": {"hasNextPage": false},
                                "nodes": []
                            }
                        }
                    }]
                }
            }
        }}});

        for private_failure in [private_null, private_malformed] {
            let server = crate::tests::make_server();
            std::fs::write(&mode, "payload").unwrap();
            std::fs::write(&payload, public.to_string()).unwrap();
            let public_response = handle_current_truth_refresh_with_floor(
                &server,
                &TachiGhParams {
                    action: "current_truth_refresh".to_string(),
                    repo: Some("Owner/Repo".to_string()),
                    number: Some(42),
                    ..TachiGhParams::default()
                },
                Duration::ZERO,
            )
            .await
            .unwrap();
            let public_response: Value = serde_json::from_str(&public_response).unwrap();
            assert_eq!(public_response["repo"], json!("owner/repo"));
            assert_eq!(public_response["fresh"], json!(true));
            assert_eq!(
                public_response["work_status"][0]["work_token"],
                json!("owner/repo#issue:42")
            );

            std::fs::write(&payload, private_failure.to_string()).unwrap();
            let private_response = handle_current_truth_refresh_with_floor(
                &server,
                &TachiGhParams {
                    action: "current_truth_refresh".to_string(),
                    repo: Some("OWNER/repo".to_string()),
                    number: Some(42),
                    ..TachiGhParams::default()
                },
                Duration::ZERO,
            )
            .await
            .unwrap();
            assert!(!private_response.contains("issue:42"));

            std::fs::write(&mode, "denied").unwrap();
            let denied_response = handle_current_truth_refresh_with_floor(
                &server,
                &TachiGhParams {
                    action: "current_truth_refresh".to_string(),
                    repo: Some("owner/REPO".to_string()),
                    number: Some(42),
                    ..TachiGhParams::default()
                },
                Duration::ZERO,
            )
            .await
            .unwrap();
            assert!(!denied_response.contains("issue:42"));
            let denied_response: Value = serde_json::from_str(&denied_response).unwrap();
            assert_eq!(denied_response["repo"], json!("owner/repo"));
            assert_eq!(denied_response["fresh"], json!(false));
            assert_eq!(denied_response["work_status"], json!([]));

            server
                .with_current_truth_store(|store| {
                    assert_eq!(
                        store.repository_visibility("OWNER/Repo").unwrap(),
                        Some(VisibilityClassV1::Private)
                    );
                    assert_eq!(store.refresh_debt_repos().unwrap(), 1);
                    assert_eq!(store.assertion_count("OWNER/Repo").unwrap(), 3);
                    Ok(())
                })
                .unwrap();
            std::fs::write(&mode, "payload").unwrap();
        }
    }

    #[tokio::test]
    async fn contradictory_same_source_revision_fails_closed() {
        let production = synthetic_merged_adapter("merge00000000000000000000000000000000007");
        let mut contradictory_state = fresh(&production);
        contradictory_state.pull_requests[0].merge_commit_sha =
            Some("other00000000000000000000000000000000007".to_string());
        let contradictory = ProductionGithubRefreshAdapter {
            outcome: RefreshOutcomeV1::Fresh(Box::new(contradictory_state)),
            repository_visibility: Some(VisibilityClassV1::Public),
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
