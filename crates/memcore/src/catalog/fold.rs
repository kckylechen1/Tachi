//! Catalog-domain event fold and its replay-equivalence property
//! (tachi#1681 discrimination 12).
//!
//! # Why this is not a reuse of `agent_eval`'s replay module
//!
//! The frozen design cited a generic `replay_equivalence`; the cross-vendor
//! review (finding 9) established that no such generic thing exists — the real
//! module is scoped to the agent-eval projection and folds an entirely
//! different event vocabulary. So this is the catalog domain's **own** fold,
//! as the review's disposition directs, not a borrowed one wearing a catalog
//! costume.
//!
//! # The property
//!
//! Replaying the whole `model_deployment_events` log from empty must produce
//! exactly the state that applying the same events incrementally produces, in
//! any batching. Two things make that true rather than hopeful:
//!
//! 1. **Ordering is imposed, not assumed.** [`CatalogProjection::replay`]
//!    sorts by event id before folding, so a caller that hands over a
//!    differently-ordered read cannot silently get a different answer.
//! 2. **Re-applying an event is a no-op.** [`CatalogProjection::apply`]
//!    refuses any event whose id it has already seen. Without that, the
//!    natural incremental pattern — "read everything after my watermark" with
//!    an inclusive boundary, or a retried batch — would double-count, and the
//!    equivalence would hold only for callers who happened to slice perfectly.
//!
//! # Two authorities, one log, separate fields
//!
//! `model_deployment_events` carries both catalog-metadata transitions and the
//! health observations PR-C's single writer appends (#1681 D4). The fold keeps
//! them in separate fields — [`DeploymentFold::health`] versus `revision` /
//! `imported` / `retired` — for the same reason the schema keeps them in
//! separate tables: a deployment being throttled at 09:04 is not a change to
//! what that deployment *is*. Because health state is reconstructible from the
//! log alone, replaying it is a genuine check on `model_deployment_health`
//! rather than a second copy of it.
//!
//! # Unrecognized event kinds are reported, not skipped
//!
//! A log written by a newer build can contain a kind this build cannot type.
//! Dropping it silently would make two builds disagree about the same log
//! while both looked healthy; it is instead counted by name in
//! [`CatalogProjection::unrecognized_event_kinds`] and included in the
//! digest, following the `recommendation.rs` precedent of reporting excluded
//! identities rather than a quietly shorter answer.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::canonical_digest::canonical_json_digest_hex;

use super::{DeploymentEventKind, ModelDeploymentEvent};

/// What the event log says about one deployment.
///
/// Deliberately *not* the deployment row: the fold is a projection of the
/// audit log, and its whole value is being derivable without reading
/// `model_deployments` at all — which is what lets it be compared against
/// that table to detect a row that moved without an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentFold {
    pub deployment_id: String,
    /// The revision the most recent **catalog** event produced. Health events
    /// do not move it — see [`CatalogProjection::apply`].
    pub revision: i64,
    /// Id of the most recent event folded into this entry, of any kind.
    pub last_event_id: i64,
    /// `None` when the most recent event carried a kind this build cannot
    /// type.
    pub last_event_kind: Option<DeploymentEventKind>,
    pub event_count: usize,
    /// Whether the log has an `deployment_imported` event. A deployment whose
    /// log starts mid-life (`false`) is a real finding, not a rounding error.
    pub imported: bool,
    pub retired: bool,
    /// What the health events in the log say about how this deployment is
    /// behaving (#1681 D4). `None` until one lands — a deployment nothing has
    /// been observed about is not the same as one observed to be healthy.
    pub health: Option<HealthFold>,
}

/// The health half of a deployment's fold: the same state
/// `model_deployment_health` holds, reconstructed from the log alone.
///
/// Kept in its own field rather than flattened alongside `revision` and
/// `retired` because these are two authorities sharing one append-only table
/// (#1681 D1/D4). A projection that mixed them would let "the provider
/// throttled us at 09:04" read as a change to what the deployment *is*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthFold {
    /// `ok` / `cooldown` / `error`, derived from the event kind.
    pub state: &'static str,
    /// The cooldown the most recent health event recorded, as the writer wrote
    /// it. `None` when that event set none.
    pub cooldown_until: Option<String>,
    pub last_event_id: i64,
    pub last_event_kind: DeploymentEventKind,
    pub event_count: usize,
}

impl HealthFold {
    fn to_json(&self) -> Value {
        json!({
            "state": self.state,
            "cooldown_until": self.cooldown_until,
            "last_event_id": self.last_event_id,
            "last_event_kind": self.last_event_kind.as_str(),
            "event_count": self.event_count,
        })
    }
}

impl DeploymentFold {
    fn to_json(&self) -> Value {
        json!({
            "deployment_id": self.deployment_id,
            "revision": self.revision,
            "last_event_id": self.last_event_id,
            "last_event_kind": self.last_event_kind.map(|kind| kind.as_str()),
            "event_count": self.event_count,
            "imported": self.imported,
            "retired": self.retired,
            "health": self.health.as_ref().map(HealthFold::to_json),
        })
    }
}

/// The cooldown a health event's evidence recorded, if it recorded one.
///
/// Read out of the evidence JSON rather than inferred from the kind, because
/// the *instant* is the fact a reader needs and only the writer knows it.
/// Evidence that is not an object, or carries no readable `cooldown_until`,
/// yields `None` — a fold cannot invent a cooldown, and a health event written
/// by a newer build must still fold rather than panic.
fn evidence_cooldown_until(evidence: &str) -> Option<String> {
    serde_json::from_str::<Value>(evidence)
        .ok()?
        .get("cooldown_until")?
        .as_str()
        .map(str::to_string)
}

/// What [`CatalogProjection::apply`] did with an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    Applied,
    /// The event id was at or below the projection's watermark. Ignored — see
    /// the module note on why this is what makes the equivalence hold.
    AlreadyApplied,
}

/// The folded state of the catalog event log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogProjection {
    deployments: BTreeMap<String, DeploymentFold>,
    last_event_id: i64,
    unrecognized_event_kinds: BTreeMap<String, usize>,
}

impl CatalogProjection {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Fold an entire log from empty.
    ///
    /// Sorts by event id first: append order *is* id order in the store, but a
    /// projection whose answer depends on how a caller happened to query is
    /// not a projection.
    pub fn replay(events: &[ModelDeploymentEvent]) -> Self {
        let mut ordered: Vec<&ModelDeploymentEvent> = events.iter().collect();
        ordered.sort_by_key(|event| event.id);
        let mut projection = Self::empty();
        for event in ordered {
            projection.apply(event);
        }
        projection
    }

    /// Fold one more event.
    pub fn apply(&mut self, event: &ModelDeploymentEvent) -> ApplyOutcome {
        if event.id <= self.last_event_id {
            return ApplyOutcome::AlreadyApplied;
        }
        self.last_event_id = event.id;

        let kind = DeploymentEventKind::parse(&event.event_kind);
        if kind.is_none() {
            *self
                .unrecognized_event_kinds
                .entry(event.event_kind.clone())
                .or_insert(0) += 1;
        }

        let entry = self
            .deployments
            .entry(event.deployment_id.clone())
            .or_insert_with(|| DeploymentFold {
                deployment_id: event.deployment_id.clone(),
                revision: 0,
                last_event_id: 0,
                last_event_kind: None,
                event_count: 0,
                imported: false,
                retired: false,
                health: None,
            });

        entry.last_event_id = event.id;
        entry.last_event_kind = kind;
        entry.event_count += 1;
        match kind {
            Some(DeploymentEventKind::DeploymentImported) => {
                entry.revision = event.revision;
                entry.imported = true;
            }
            Some(DeploymentEventKind::DeploymentRetired) => {
                entry.revision = event.revision;
                entry.retired = true;
            }
            // An update after a retirement un-retires nothing: retirement is a
            // status transition the row itself records, and the fold reports
            // the log, not a guess about what the row now says.
            //
            // An untypeable kind still carries its revision: a newer build's
            // catalog transition is a catalog transition, and pretending its
            // revision did not happen would be a quieter lie than reporting the
            // kind as unrecognized.
            Some(DeploymentEventKind::DeploymentUpdated) | None => {
                entry.revision = event.revision;
            }
            // Health events (#1681 D4) say how the deployment is *behaving*,
            // never what it *is*. They deliberately do **not** set `revision`:
            // a health event carries the revision it *observed*, so a write
            // that raced a re-import would otherwise roll the projection's
            // catalog revision backwards — the log would then disagree with
            // `model_deployments` about a fact the health authority has no
            // business asserting.
            Some(
                health_kind @ (DeploymentEventKind::HealthServed
                | DeploymentEventKind::HealthCooldown
                | DeploymentEventKind::HealthError),
            ) => {
                let state = health_kind
                    .health_state()
                    .expect("a health event kind always has a health state");
                let count = entry
                    .health
                    .as_ref()
                    .map(|health| health.event_count)
                    .unwrap_or(0);
                entry.health = Some(HealthFold {
                    state,
                    cooldown_until: evidence_cooldown_until(&event.evidence),
                    last_event_id: event.id,
                    last_event_kind: health_kind,
                    event_count: count + 1,
                });
            }
        }

        ApplyOutcome::Applied
    }

    /// Fold a batch, in id order, skipping anything already folded.
    pub fn extend(&mut self, events: &[ModelDeploymentEvent]) {
        let mut ordered: Vec<&ModelDeploymentEvent> = events.iter().collect();
        ordered.sort_by_key(|event| event.id);
        for event in ordered {
            self.apply(event);
        }
    }

    pub fn deployments(&self) -> &BTreeMap<String, DeploymentFold> {
        &self.deployments
    }

    pub fn get(&self, deployment_id: &str) -> Option<&DeploymentFold> {
        self.deployments.get(deployment_id)
    }

    /// The highest event id folded so far — the watermark an incremental
    /// reader passes to `list_model_deployment_events_after`.
    pub fn last_event_id(&self) -> i64 {
        self.last_event_id
    }

    /// Event kinds this build could not type, by name and count.
    pub fn unrecognized_event_kinds(&self) -> &BTreeMap<String, usize> {
        &self.unrecognized_event_kinds
    }

    /// Canonical JSON of the whole projection — the comparison artifact the
    /// replay-equivalence assertion is made against.
    ///
    /// Hand-written rather than derived, following
    /// `db::eval_replay::canonical_eval_observations`: this is a fixture two
    /// independent fold paths are graded against, so a new field must be added
    /// here consciously rather than appear the moment somebody widens a
    /// struct.
    pub fn to_canonical_json(&self) -> Value {
        json!({
            "last_event_id": self.last_event_id,
            "deployments": Value::Array(
                self.deployments.values().map(DeploymentFold::to_json).collect()
            ),
            "unrecognized_event_kinds": Value::Object(
                self.unrecognized_event_kinds
                    .iter()
                    .map(|(kind, count)| (kind.clone(), json!(count)))
                    .collect()
            ),
        })
    }

    pub fn digest(&self) -> String {
        canonical_json_digest_hex(&self.to_canonical_json())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: i64, deployment_id: &str, revision: i64, kind: &str) -> ModelDeploymentEvent {
        ModelDeploymentEvent {
            id,
            deployment_id: deployment_id.to_string(),
            revision,
            event_kind: kind.to_string(),
            plan_digest: None,
            evidence: "{}".to_string(),
            created_at: "2026-08-11T00:00:00.000Z".to_string(),
        }
    }

    fn log() -> Vec<ModelDeploymentEvent> {
        vec![
            event(1, "env:extract", 1, "deployment_imported"),
            event(2, "env:summary", 1, "deployment_imported"),
            event(3, "env:extract", 2, "deployment_updated"),
            event(4, "env:reasoning", 1, "deployment_imported"),
            event(5, "env:summary", 2, "deployment_retired"),
            event(6, "env:extract", 3, "deployment_updated"),
        ]
    }

    fn health_event(
        id: i64,
        deployment_id: &str,
        revision: i64,
        kind: DeploymentEventKind,
        cooldown_until: Option<&str>,
    ) -> ModelDeploymentEvent {
        let mut event = event(id, deployment_id, revision, kind.as_str());
        event.evidence = json!({
            "outcome": "fixture",
            "state": kind.health_state(),
            "cooldown_until": cooldown_until,
        })
        .to_string();
        event
    }

    /// A log both authorities wrote into, which is the shape production
    /// produces: imports, a revision advance, and health observations
    /// interleaved.
    fn mixed_log() -> Vec<ModelDeploymentEvent> {
        vec![
            event(1, "env:extract", 1, "deployment_imported"),
            event(2, "env:summary", 1, "deployment_imported"),
            health_event(
                3,
                "env:extract",
                1,
                DeploymentEventKind::HealthCooldown,
                Some("2026-08-11T00:01:00.000Z"),
            ),
            event(4, "env:extract", 2, "deployment_updated"),
            health_event(5, "env:summary", 1, DeploymentEventKind::HealthError, None),
            health_event(6, "env:extract", 2, DeploymentEventKind::HealthServed, None),
        ]
    }

    // ─── discrimination 12 ───────────────────────────────────────────────────

    #[test]
    fn full_replay_equals_incremental_application() {
        let events = log();
        let full = CatalogProjection::replay(&events);

        let mut incremental = CatalogProjection::empty();
        for chunk in events.chunks(2) {
            incremental.extend(chunk);
        }

        assert_eq!(
            full.digest(),
            incremental.digest(),
            "a projection rebuilt from scratch must equal one grown a batch at a time, or the \
             catalog has two different truths depending on whether a process restarted"
        );
        assert_eq!(full, incremental);
    }

    #[test]
    fn incremental_application_survives_overlapping_and_repeated_batches() {
        // The realistic failure: a reader with an inclusive watermark, or a
        // retried batch after a crash. Without the id guard, `event_count` and
        // `revision` would drift on every overlap.
        let events = log();
        let full = CatalogProjection::replay(&events);

        let mut incremental = CatalogProjection::empty();
        incremental.extend(&events[0..4]);
        incremental.extend(&events[2..6]); // overlaps ids 3 and 4
        incremental.extend(&events[0..6]); // replays the whole log again

        assert_eq!(full.digest(), incremental.digest());
        assert_eq!(
            incremental.get("env:extract").map(|fold| fold.event_count),
            Some(3),
            "an event folded twice would inflate the count"
        );
    }

    #[test]
    fn re_applying_a_seen_event_reports_that_it_did_nothing() {
        let events = log();
        let mut projection = CatalogProjection::empty();
        assert_eq!(projection.apply(&events[0]), ApplyOutcome::Applied);
        assert_eq!(projection.apply(&events[0]), ApplyOutcome::AlreadyApplied);
        assert_eq!(projection.last_event_id(), 1);
    }

    #[test]
    fn a_shuffled_read_folds_to_the_same_state_as_an_ordered_one() {
        let ordered = log();
        let mut shuffled = ordered.clone();
        shuffled.reverse();
        shuffled.swap(0, 3);

        assert_eq!(
            CatalogProjection::replay(&ordered).digest(),
            CatalogProjection::replay(&shuffled).digest(),
            "the projection must not depend on how a caller happened to query the log"
        );
    }

    // ─── the fold actually says something ────────────────────────────────────

    #[test]
    fn the_fold_reports_each_deployments_latest_revision_and_lifecycle() {
        let projection = CatalogProjection::replay(&log());

        let extract = projection.get("env:extract").expect("extract folded");
        assert_eq!(extract.revision, 3);
        assert_eq!(extract.event_count, 3);
        assert!(extract.imported);
        assert!(!extract.retired);
        assert_eq!(
            extract.last_event_kind,
            Some(DeploymentEventKind::DeploymentUpdated)
        );

        let summary = projection.get("env:summary").expect("summary folded");
        assert!(summary.retired);
        assert_eq!(summary.revision, 2);

        assert_eq!(projection.last_event_id(), 6);
        assert_eq!(projection.deployments().len(), 3);
    }

    #[test]
    fn a_log_that_starts_mid_life_is_reported_as_never_imported() {
        // Not a rounding error: it means the import event is missing from an
        // append-only table, which is a finding.
        let projection =
            CatalogProjection::replay(&[event(9, "env:extract", 4, "deployment_updated")]);
        let fold = projection.get("env:extract").expect("folded");
        assert!(
            !fold.imported,
            "a deployment with no import event must not be reported as imported"
        );
        assert_eq!(fold.revision, 4);
    }

    #[test]
    fn an_unrecognized_event_kind_is_counted_by_name_not_silently_dropped() {
        let events = vec![
            event(1, "env:extract", 1, "deployment_imported"),
            event(2, "env:extract", 2, "deployment_teleported"),
        ];
        let projection = CatalogProjection::replay(&events);

        assert_eq!(
            projection
                .unrecognized_event_kinds()
                .get("deployment_teleported"),
            Some(&1),
            "a kind a newer build wrote must be visible, or two builds disagree about the same \
             log while both look healthy"
        );
        let fold = projection.get("env:extract").expect("folded");
        assert_eq!(fold.event_count, 2, "it still counts as an event");
        assert_eq!(fold.revision, 2, "and still carries its revision");
        assert_eq!(fold.last_event_kind, None);

        // And it changes the digest, so a replay comparison cannot pass by
        // both sides ignoring it identically-but-invisibly.
        let without = CatalogProjection::replay(&events[..1]);
        assert_ne!(projection.digest(), without.digest());
    }

    #[test]
    fn the_digest_moves_when_any_folded_field_moves() {
        let base = CatalogProjection::replay(&log());
        let mut more = log();
        more.push(event(7, "env:reasoning", 2, "deployment_retired"));
        assert_ne!(base.digest(), CatalogProjection::replay(&more).digest());
    }

    // ─── discrimination 12, extended over the health bundle ──────────────────

    #[test]
    fn full_replay_equals_incremental_application_over_health_events_too() {
        let events = mixed_log();
        let full = CatalogProjection::replay(&events);

        let mut incremental = CatalogProjection::empty();
        for chunk in events.chunks(2) {
            incremental.extend(chunk);
        }
        // And the two failure modes the id guard exists for, now with health
        // events in the stream: an inclusive watermark and a retried batch.
        incremental.extend(&events[3..6]);
        incremental.extend(&events[0..6]);

        assert_eq!(
            full.digest(),
            incremental.digest(),
            "health rows have to survive the same replay-equivalence property as catalog rows, or \
             a daemon that restarted disagrees with one that did not about which deployment is \
             cooling down"
        );
        assert_eq!(full, incremental);

        let extract = full.get("env:extract").expect("folded");
        let health = extract.health.as_ref().expect("health folded");
        assert_eq!(health.state, "ok", "the last health event served");
        assert_eq!(health.event_count, 2);
        assert_eq!(health.cooldown_until, None);
        assert_eq!(health.last_event_id, 6);
    }

    #[test]
    fn the_health_bundle_carries_the_cooldown_the_writer_recorded() {
        let projection = CatalogProjection::replay(&mixed_log()[..3]);
        let health = projection
            .get("env:extract")
            .expect("folded")
            .health
            .as_ref()
            .expect("health folded");
        assert_eq!(health.state, "cooldown");
        assert_eq!(
            health.cooldown_until.as_deref(),
            Some("2026-08-11T00:01:00.000Z"),
            "the instant is the fact a reader needs, and only the writer knows it"
        );
    }

    #[test]
    fn a_deployment_with_no_health_event_is_unobserved_not_healthy() {
        let projection = CatalogProjection::replay(&log());
        assert!(
            projection
                .get("env:reasoning")
                .expect("folded")
                .health
                .is_none(),
            "a deployment nothing has been observed about must not read as one observed to be \
             healthy — the resolver's answer differs"
        );
    }

    #[test]
    fn a_health_event_never_moves_the_catalog_revision() {
        // The race this rules out: a health write computed against revision 1
        // lands after a re-import advanced the row to 2. If health events set
        // the projection's revision, the fold would roll it back and disagree
        // with `model_deployments` about a fact the health authority has no
        // business asserting.
        let events = vec![
            event(1, "env:extract", 1, "deployment_imported"),
            event(2, "env:extract", 2, "deployment_updated"),
            health_event(3, "env:extract", 1, DeploymentEventKind::HealthError, None),
        ];
        let fold = CatalogProjection::replay(&events);
        let extract = fold.get("env:extract").expect("folded");
        assert_eq!(extract.revision, 2);
        assert_eq!(
            extract.health.as_ref().map(|health| health.state),
            Some("error")
        );
        assert_eq!(
            extract.event_count, 3,
            "it still counts as an event in the log"
        );
        assert_eq!(
            extract.last_event_kind,
            Some(DeploymentEventKind::HealthError),
            "and it is still the most recent event"
        );
    }

    #[test]
    fn the_digest_moves_when_a_health_event_lands() {
        let base = CatalogProjection::replay(&mixed_log());
        let mut more = mixed_log();
        more.push(health_event(
            7,
            "env:extract",
            2,
            DeploymentEventKind::HealthCooldown,
            Some("2026-08-11T00:05:00.000Z"),
        ));
        assert_ne!(
            base.digest(),
            CatalogProjection::replay(&more).digest(),
            "a comparison that could not see health events would pass while the two sides \
             disagreed about every cooldown"
        );
    }

    #[test]
    fn health_evidence_a_fold_cannot_read_yields_no_cooldown_rather_than_a_panic() {
        let mut event = health_event(
            1,
            "env:extract",
            1,
            DeploymentEventKind::HealthCooldown,
            None,
        );
        event.evidence = "not json".to_string();
        let projection = CatalogProjection::replay(&[event]);
        let health = projection
            .get("env:extract")
            .expect("folded")
            .health
            .as_ref()
            .expect("health folded");
        assert_eq!(health.state, "cooldown");
        assert_eq!(health.cooldown_until, None);
    }

    #[test]
    fn an_empty_log_folds_to_a_stable_empty_projection() {
        let empty = CatalogProjection::replay(&[]);
        assert_eq!(empty.digest(), CatalogProjection::empty().digest());
        assert_eq!(empty.last_event_id(), 0);
        assert!(empty.deployments().is_empty());
    }
}
