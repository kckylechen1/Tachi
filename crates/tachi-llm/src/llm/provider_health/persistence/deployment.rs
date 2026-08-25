//! Deployment-health recording at the lane outcome seam (tachi#1681 D4, PR-C
//! item 4).
//!
//! # Where this hooks, and why here
//!
//! `apply_key_outcome` is the one place this client turns a lane outcome into
//! durable health. The deployment authority hangs off exactly that seam: the
//! same observation that writes `vault_key_health` also writes
//! `model_deployment_health` when — and only when — it is attributable to a
//! catalog row. No new decision point, no second classification of the same
//! HTTP response, and `lane_calls.rs` keeps its call semantics: what it gained
//! is the attribution it already knew (which lane, which endpoint, which
//! model), not a new branch.
//!
//! # Dual-record, zero cross-interference
//!
//! A 429 is recorded twice on purpose (#1681 D4): once against the credential
//! that was throttled, once against the deployment that throttled it. The two
//! writes share nothing but this function — different tables, different
//! writers, different vocabularies — so neither can clear, shorten or
//! contradict the other's cooldown. That is the property that lets a later
//! resolver keep a healthy sibling deployment selectable while one credential
//! cools down (the selection half is PR-D's).
//!
//! 401/403 never gets here at all: the credential vocabulary's auth outcomes
//! have no deployment counterpart to map onto, and
//! `memcore::catalog::health::DeploymentOutcome` has no variant that could
//! carry one.
//!
//! # Fail-safe
//!
//! Health recording is observation, never control flow. A missing deployment
//! row, a mis-attributed request, a locked database — all are counted and
//! logged, and none can turn into an error on the path that produced the
//! outcome. The invocation already happened; failing a chat lane because a
//! bookkeeping row could not be written would be strictly worse than not
//! having the row.

use memcore::catalog::health::{DeploymentOutcome, ProviderResponseSignal, RetryAfter};
use memcore::db::model_catalog::{
    record_model_deployment_outcome, DeploymentHealthSkip, DeploymentHealthWrite,
    DeploymentOutcomeTarget,
};

use super::*;
use crate::llm::catalog_import::DeploymentAttribution;

/// The deployment authority's reading of an outcome the credential authority
/// already classified.
///
/// `None` means "this outcome is not deployment evidence", and the two arms
/// that return it are the load-bearing half of #1681 D4's attribution rule:
///
/// - [`TypedOutcome::AuthFailed`] — 401/403. Never deployment health: an auth
///   failure says nothing about the deployment, and recording it would cool
///   down every sibling that shares the rejected key.
/// - [`TypedOutcome::Exhausted`] — reached in production from a **403** whose
///   body reads as billing/quota (`lane_calls.rs`), so at this seam it is an
///   auth-status outcome wearing a quota label, and the frozen rule is
///   status-keyed. A genuine quota signal that is *not* 401/403 (a 402, or an
///   explicit quota report) classifies as `Throttled` through
///   `DeploymentOutcome::classify`, which is where a status-carrying caller
///   should enter.
/// - [`TypedOutcome::Error`] / [`TypedOutcome::Unknown`] — these arrive from
///   the caller-reported channel carrying no status this seam can see, so they
///   could be a 5xx or a 400; guessing would manufacture health facts.
///   `classify` is the entry point that has the status.
pub(in crate::llm) fn deployment_outcome_for(outcome: TypedOutcome) -> Option<DeploymentOutcome> {
    match outcome {
        TypedOutcome::Success => Some(DeploymentOutcome::Served),
        TypedOutcome::RateLimited { retry_after_secs } => Some(DeploymentOutcome::Throttled {
            retry_after: retry_after_secs.map(RetryAfter::DeltaSeconds),
        }),
        TypedOutcome::AuthFailed
        | TypedOutcome::Exhausted
        | TypedOutcome::Error
        | TypedOutcome::Unknown => None,
    }
}

impl super::super::super::LlmClient {
    /// Record the deployment half of a lane outcome, if it has one.
    ///
    /// Called from `apply_key_outcome` after the credential write. Returns
    /// nothing: every failure mode is a counter, by design (see the module
    /// header).
    pub(in crate::llm) fn note_deployment_outcome(
        &self,
        attribution: DeploymentAttribution<'_>,
        outcome: TypedOutcome,
        evidence: EvidenceKind,
    ) {
        let Some(outcome) = deployment_outcome_for(outcome) else {
            return;
        };
        self.record_deployment_outcome(attribution, outcome, evidence);
    }

    /// The deployment half of an outcome the credential authority has nothing
    /// to say about (#1681 D4: `timeout/5xx/protocol` → deployment only, and a
    /// `402` whose quota signal is not an auth status).
    ///
    /// The chat and embedding lanes reach it through the three
    /// intention-named wrappers below rather than through
    /// `DeploymentOutcome::classify` directly, so the classification stays in
    /// this module — the call sites gain one line each and no HTTP-response
    /// judgment. That is what keeps "`lane_calls.rs` only gained hooks" true:
    /// there is nothing at those sites to get wrong.
    pub(in crate::llm) fn note_deployment_response(
        &self,
        attribution: DeploymentAttribution<'_>,
        signal: ProviderResponseSignal,
        retry_after: Option<RetryAfter>,
    ) {
        let Some(outcome) = DeploymentOutcome::classify(signal, retry_after) else {
            // 401/403. The credential and account authorities own those, and
            // the deployment vocabulary cannot express them at all.
            return;
        };
        self.record_deployment_outcome(attribution, outcome, EvidenceKind::SelfReported);
    }

    /// The request never produced a status line: a connect failure, a timeout,
    /// a dropped connection.
    pub(in crate::llm) fn note_deployment_transport_failure(
        &self,
        attribution: DeploymentAttribution<'_>,
    ) {
        self.note_deployment_response(attribution, ProviderResponseSignal::NoResponse, None);
    }

    /// The deployment answered and the answer was unusable: a body that never
    /// finished arriving, one that would not parse, or a completion with no
    /// content in it.
    pub(in crate::llm) fn note_deployment_unusable_body(
        &self,
        attribution: DeploymentAttribution<'_>,
    ) {
        self.note_deployment_response(attribution, ProviderResponseSignal::UnusableBody, None);
    }

    /// The deployment answered with `status`.
    ///
    /// `retry_after_header` is the raw header, parsed **here** through
    /// [`RetryAfter::parse`] so all three RFC 9110 HTTP-date formats reach a
    /// row. The lane's own `retry_after` — a delta-seconds `u64` feeding the
    /// retry sleep — is deliberately left alone: widening it would change how
    /// long a retry waits, and this seam records, it does not steer.
    ///
    /// The clock is this client's own [`Self::now_utc`], the same one the
    /// record below is stamped with: RFC 9110 §5.6.7 resolves the RFC 850
    /// form's two-digit year against the moment of receipt, so the header and
    /// the observation must not be read against two different "now"s.
    pub(in crate::llm) fn note_deployment_http_status(
        &self,
        attribution: DeploymentAttribution<'_>,
        status: u16,
        retry_after_header: Option<&str>,
    ) {
        let received_at = Self::now_utc();
        self.note_deployment_response(
            attribution,
            ProviderResponseSignal::Status(status),
            retry_after_header.and_then(|raw| RetryAfter::parse(raw, received_at)),
        );
    }

    fn record_deployment_outcome(
        &self,
        attribution: DeploymentAttribution<'_>,
        outcome: DeploymentOutcome,
        evidence: EvidenceKind,
    ) {
        let (Some(deployment_id), Some(endpoint), Some(model)) = (
            attribution.env_deployment_id(),
            attribution.endpoint(),
            attribution.model(),
        ) else {
            // Nothing in the catalog describes this request. Not a skip worth
            // counting: it is a channel that never had a deployment, not a
            // deployment we failed to find.
            return;
        };
        let Some(db_path) = self.vault_db_path.clone() else {
            // Attributable, but this client has nowhere to write. Counted
            // rather than dropped in silence: "the seam recorded nothing"
            // and "the seam had no store to record into" are different
            // operational states, and only one of them is a bug.
            self.deployment_health.note_no_store();
            return;
        };

        let record = DeploymentHealthRecord {
            deployment_id,
            endpoint: endpoint.to_string(),
            model: model.to_string(),
            outcome,
            evidence,
            now: Self::now_utc(),
        };
        let migration = self.vault_db_migration.clone();
        let counters = Arc::clone(&self.deployment_health);

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            // Same shape as `persist_key_health`, including the persist
            // tracker, so `await_provider_health_persistence` covers this write
            // too — a doctor that waits for provider health must not return
            // while half of it is still in flight.
            let persist_state = Arc::clone(&self.provider_health_persist);
            let tracker = persist_state
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .tracker();
            let background_persist_lock = Arc::clone(&self.background_persist_lock);
            let completion = tracker.track();
            handle.spawn(async move {
                let _completion = completion;
                let outcome = {
                    let _persist_guard = background_persist_lock.lock().await;
                    tokio::task::spawn_blocking(move || {
                        Self::record_deployment_outcome_blocking(db_path, migration, record)
                    })
                    .await
                    .map_err(|err| format!("join failure: {err}"))
                    .and_then(|inner| inner)
                };
                Self::note_deployment_outcome_result(&counters, outcome);
            });
        } else {
            let outcome = Self::record_deployment_outcome_blocking(db_path, migration, record);
            Self::note_deployment_outcome_result(&counters, outcome);
        }
    }

    /// The recorded counts so far. Test and status surface; the rows are the
    /// record, these are the "did anything land at all" signal.
    pub fn deployment_health_record_counts(&self) -> DeploymentHealthRecordCounts {
        self.deployment_health.snapshot()
    }

    fn record_deployment_outcome_blocking(
        db_path: PathBuf,
        migration: memcore::MigrationAuthority,
        record: DeploymentHealthRecord,
    ) -> Result<DeploymentHealthWrite, String> {
        let Some(db_path) = db_path.to_str() else {
            return Err("invalid db path".to_string());
        };
        let open_context = memcore::DbOpenContext {
            intent: memcore::OpenIntent::OpenExisting,
            migration,
            // #1585 D2: `model_deployment_health` is a product table.
            required_profile: memcore::StoreProfile::TachiFull,
        };
        let store = memcore::MemoryStore::open_with_context_and_busy_timeout(
            db_path,
            &open_context,
            DEPLOYMENT_HEALTH_SQLITE_BUSY_TIMEOUT,
        )
        .map_err(|err| err.to_string())?;
        let target = DeploymentOutcomeTarget::request(
            &record.deployment_id,
            &record.endpoint,
            &record.model,
        );
        record_model_deployment_outcome(
            store.connection(),
            &target,
            record.outcome,
            record.evidence,
            record.now,
        )
        .map_err(|err| err.to_string())
    }

    fn note_deployment_outcome_result(
        counters: &Arc<DeploymentHealthCounters>,
        result: Result<DeploymentHealthWrite, String>,
    ) {
        match result {
            Ok(DeploymentHealthWrite::Recorded { .. }) => counters.note_recorded(),
            Ok(DeploymentHealthWrite::Skipped(DeploymentHealthSkip::NoSuchDeployment)) => {
                counters.note_unknown_deployment()
            }
            Ok(DeploymentHealthWrite::Skipped(
                DeploymentHealthSkip::DescribesADifferentRequest,
            )) => counters.note_different_request(),
            Ok(DeploymentHealthWrite::Skipped(DeploymentHealthSkip::StaleObservation)) => {
                counters.note_stale_observation()
            }
            Ok(DeploymentHealthWrite::Skipped(DeploymentHealthSkip::AuthClassStatus)) => {
                counters.note_auth_class_status()
            }
            Err(err) => {
                counters.note_failure();
                // Logged, never propagated: the invocation this describes has
                // already happened.
                tracing::warn!("[provider] deployment health not recorded: {err}");
            }
        }
    }
}

/// One pending deployment-health write, owned so it can cross a
/// `spawn_blocking` boundary.
struct DeploymentHealthRecord {
    deployment_id: String,
    endpoint: String,
    model: String,
    outcome: DeploymentOutcome,
    evidence: EvidenceKind,
    now: chrono::DateTime<chrono::Utc>,
}

/// Matches the credential-health persist budget: a health row is never worth
/// holding a caller (or a one-shot doctor process) longer than the write it
/// travels beside.
const DEPLOYMENT_HEALTH_SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(2);

#[cfg(test)]
mod tests {
    use super::*;

    /// The mapping is the load-bearing half of #1681 D4's attribution rule, so
    /// it is pinned beside the function rather than only through the seam: a
    /// future variant added to `TypedOutcome` has to make a decision here, and
    /// this is where that decision is visible.
    #[test]
    fn only_throttles_and_successes_cross_from_the_credential_vocabulary() {
        assert_eq!(
            deployment_outcome_for(TypedOutcome::RateLimited {
                retry_after_secs: Some(30)
            }),
            Some(DeploymentOutcome::Throttled {
                retry_after: Some(RetryAfter::DeltaSeconds(30))
            })
        );
        assert_eq!(
            deployment_outcome_for(TypedOutcome::RateLimited {
                retry_after_secs: None
            }),
            Some(DeploymentOutcome::Throttled { retry_after: None }),
            "a throttle with no Retry-After still cools the deployment down, at the class default"
        );
        assert_eq!(
            deployment_outcome_for(TypedOutcome::Success),
            Some(DeploymentOutcome::Served)
        );
        for outcome in [
            TypedOutcome::AuthFailed,
            // Reached from a 403 whose body reads as billing/quota, so at this
            // seam it is an auth-status outcome wearing a quota label. A
            // genuine non-auth quota signal enters through
            // `DeploymentOutcome::classify`, which can see the status.
            TypedOutcome::Exhausted,
            // No status is visible here, so these could be a 5xx or a 400;
            // guessing would manufacture health facts.
            TypedOutcome::Error,
            TypedOutcome::Unknown,
        ] {
            assert_eq!(
                deployment_outcome_for(outcome),
                None,
                "{outcome:?} must not become deployment evidence at this seam"
            );
        }
    }
}
