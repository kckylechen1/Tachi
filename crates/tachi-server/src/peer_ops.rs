//! S1 local peer-publication broker (#1016) — the daemon-side logic behind the
//! `peer_query` tool.
//!
//! ## What this is (and is deliberately not)
//!
//! `peer_query` answers a small, exhaustively-whitelisted set of *nouns* about
//! a peer session on the SAME host, over a self-asserted-local trust boundary
//! (`same_host_loopback_v1`): identity is taken on the caller's word, and the
//! surface is strictly advisory/read-only. Every noun not on the whitelist
//! below is DENIED, never silently routed (sol invariant 2).
//!
//! ## v1 whitelist (owner ruling 2026-07-17, "observe-only v1 of the
//! `tachi_peer` facade")
//!
//! - `presence` (S1, #1049) — claims-table heartbeat board.
//! - `run` (this leaf) — resolves `target_session_client` to its active
//!   claim's `dispatch_id`, then reads that dispatch's run-dir
//!   `status.json` (the same safe projection `board`/`task_facade` already
//!   expose for a known dispatch_id — this noun only adds the
//!   session_client → dispatch_id resolution step) and tails its
//!   `progress.jsonl` as the "recent event ledger" the ruling asks for.
//!   Requires `target_session_client` — this noun addresses one specific
//!   peer, it is not a board scan.
//!
//! ## Deliberately NOT implemented in this leaf (flagged, not guessed)
//!
//! The ruling also names `checkpoint` and a generic "event ledger" reader.
//! Both hit the same structural gap on inspection: `session_claims` carries
//! no `project` field, checkpoint memory rows (`tachi_memory(action=
//! 'checkpoint')`) and `tachi_events` rows are BOTH project-routed (may live
//! in a per-project DB this daemon's `PeerPublicationRead::open_global`
//! cannot see — `event_ops::emit_event`'s `server.event_db_route(project)`),
//! and neither carries a `session_client`/`dispatch_id` correlator to resolve
//! "which project/rows belong to this peer" from a `target_session_client`
//! alone. Guessing "the caller's own current project" would risk leaking a
//! different project's checkpoints/events across the peer boundary — the
//! opposite of fail-closed. Landing this without a real correlator is out of
//! scope for this leaf; the `PeerNoun` whitelist below denies both nouns
//! rather than routing them through an unsafe guess.
//!
//! ## The read is structurally read-only, not read-only by convention
//!
//! The answer is produced through [`PeerPublicationRead`], which owns a
//! DEDICATED SQLite connection opened with `OpenFlags::SQLITE_OPEN_READ_ONLY`
//! (via [`memcore::MemoryStore::open_read_only`]). The type exposes only typed
//! projection methods — it never hands out a writable connection, and the
//! underlying handle rejects any write at the SQLite layer regardless. Two
//! independent guarantees, not one convention (sol invariant 1 / structural-
//! gate law).
//!
//! ## An expired row never masquerades as live
//!
//! The snapshot is read inside a single read-only transaction with the query
//! clock; the projection then re-applies the lazy TTL a SECOND time with the
//! render clock before serializing, so a claim that crossed the TTL horizon
//! between snapshot and render is emitted flagged `expired_during_render` and
//! excluded from the alive count — never counted as a live seat (sol invariant
//! 7). And a source that could not be read at all (DB locked, table missing,
//! cannot open) is reported `unavailable`, NEVER as an empty `count: 0` board
//! (sol invariant 3, the #1024 NULL lesson carried forward).

use chrono::{DateTime, Utc};
use memcore::MemoryStore;

use crate::claims_ops::{project_peer_presence, PeerPresenceProjection, CLAIM_TTL_SECONDS};
use crate::server_state::MemoryServer;
use tachi_params::PeerQueryParams;

/// The peer-publication response contract this module emits.
const PEER_PUBLICATION_CONTRACT: &str = "peer-publication/v1";

/// Cap on how many trailing `progress.jsonl` lines a `run` read surfaces as
/// "recent events" — mirrors [`claims_ops`]'s `PRESENCE_BOARD_DISPLAY_CAP`
/// idiom (bounded advisory tail, not an unbounded dump).
const RECENT_EVENTS_DISPLAY_CAP: usize = 20;

/// The exhaustive noun whitelist (sol invariant 2). Adding a variant here
/// is the ONLY way to make a new noun routable — an unlisted noun can never be
/// answered, only `denied`.
enum PeerNoun {
    Presence,
    Run,
}

impl PeerNoun {
    /// Parse a caller-supplied noun against the exhaustive whitelist. Returns
    /// `None` for ANY value outside the whitelist (`outcomes`, `sticky`,
    /// `handoff`, `checkpoint`, `events`, `memories`, …) — the caller turns
    /// `None` into a `denied` response. `checkpoint`/`events` are deliberately
    /// absent — see the module doc's "Deliberately NOT implemented" section.
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "presence" => Some(PeerNoun::Presence),
            "run" => Some(PeerNoun::Run),
            _ => None,
        }
    }
}

/// A dedicated, structurally read-only view onto a daemon's memory store, used
/// to answer peer-publication reads without any path to mutate the store.
pub(crate) struct PeerPublicationRead {
    store: MemoryStore,
}

impl PeerPublicationRead {
    /// Open a dedicated read-only connection at `db_path`
    /// (`OpenFlags::SQLITE_OPEN_READ_ONLY`). Any failure (file missing, not a
    /// valid store, schema too new) is a `source unavailable` condition — the
    /// caller must never present it as an empty board.
    fn open_at(db_path: &str) -> Result<Self, String> {
        let store = MemoryStore::open_read_only(db_path).map_err(|e| e.to_string())?;
        Ok(Self { store })
    }

    /// Open a read-only view of this daemon's OWN global store (S1 is same-host,
    /// single daemon).
    pub(crate) fn open_global(server: &MemoryServer) -> Result<Self, String> {
        let path = server.global_db_path_buf();
        let path_str = path
            .to_str()
            .ok_or_else(|| format!("non-utf8 global db path: {}", path.display()))?;
        Self::open_at(path_str)
    }

    /// Read a presence snapshot through a SINGLE read-only transaction and
    /// project it into the peer-publication `result` shape.
    ///
    /// `now_query` is the snapshot clock used to filter live claims;
    /// `now_render` is the (>= `now_query`) render clock the projection uses to
    /// re-check the TTL a second time (see [`project_peer_presence`]).
    /// `target`, when `Some`, narrows the answer to that session's claim(s).
    ///
    /// This is the ONLY read path this type exposes. It borrows the connection
    /// immutably and never writes; combined with the read-only open flag, a
    /// write is impossible both by type and by the SQLite layer.
    fn read_presence(
        &self,
        target: Option<&str>,
        now_query: DateTime<Utc>,
        now_render: DateTime<Utc>,
        ttl_seconds: i64,
    ) -> Result<PeerPresenceProjection, String> {
        let conn = self.store.connection();
        // Single-transaction snapshot. `unchecked_transaction` needs only a
        // shared ref (the connection is never borrowed mutably here); on drop
        // it rolls back — there is nothing to commit on a read.
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let now_iso = now_query.to_rfc3339();
        let mut candidates =
            memcore::list_active_claims(&tx, &now_iso, ttl_seconds).map_err(|e| e.to_string())?;
        drop(tx);

        if let Some(target) = target {
            candidates.retain(|claim| claim.session_client.as_deref() == Some(target));
        }
        Ok(project_peer_presence(&candidates, now_render, ttl_seconds))
    }

    /// Resolve `target`'s (session_client) most-recently-heartbeated ACTIVE
    /// claim to its `dispatch_id`, through the same single read-only
    /// transaction / snapshot-clock discipline as [`Self::read_presence`].
    ///
    /// Returns `Ok(None)` — never a fabricated id — when `target` has no live
    /// claim, or its live claim(s) carry no `dispatch_id` (a claim made
    /// through the manual `claim`/`release` facade actions, never through
    /// `tachi_dispatch`, legitimately has none). The caller must present this
    /// as `empty`, not `unavailable` (sol invariant 3: the source WAS read
    /// successfully, there is just nothing to report).
    fn resolve_active_dispatch_id(
        &self,
        target: &str,
        now_query: DateTime<Utc>,
        ttl_seconds: i64,
    ) -> Result<Option<String>, String> {
        let conn = self.store.connection();
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let now_iso = now_query.to_rfc3339();
        let candidates =
            memcore::list_active_claims(&tx, &now_iso, ttl_seconds).map_err(|e| e.to_string())?;
        drop(tx);

        // `list_active_claims` orders newest-heartbeat-first (see
        // `project_peer_presence`'s doc comment), so the first match carrying
        // a dispatch_id is the freshest.
        Ok(candidates
            .into_iter()
            .find(|claim| {
                claim.session_client.as_deref() == Some(target) && claim.dispatch_id.is_some()
            })
            .and_then(|claim| claim.dispatch_id))
    }

    /// Test-only: attempt a raw write through the held connection to PROVE it is
    /// structurally read-only (the write must fail at the SQLite layer, and an
    /// independent read-write connection must observe the table unchanged).
    #[cfg(test)]
    fn attempt_write_for_test(&self, sql: &str) -> rusqlite::Result<usize> {
        self.store.connection().execute(sql, [])
    }
}

/// The presence read either produced a projection, or the source could not be
/// read at all. The two must NEVER collapse into one another — an unreadable
/// source is `unavailable`, not an empty board (sol invariant 3).
enum PresenceOutcome {
    Read(PeerPresenceProjection),
    Unavailable(String),
}

/// One of `run`'s two run-dir sub-sources (`status.json`, `progress.jsonl`
/// tail), read independently — one can succeed while the other is
/// unavailable, and that must stay visible rather than collapsing to a single
/// pass/fail bit (sol invariant 3, applied per-source).
enum RunSourceOutcome {
    Ok(serde_json::Value),
    Unavailable(String),
}

/// Outcome of a `noun=run` read for one `target`.
enum RunOutcome {
    /// The claims source itself (the addressing step) could not be read.
    ClaimsUnavailable(String),
    /// The claims source WAS read; `target` simply has no active claim
    /// carrying a `dispatch_id` right now. Never fabricated — `empty`, not
    /// `unavailable` (sol invariant 3).
    NoActiveDispatch,
    /// A `dispatch_id` was resolved; its run-dir sources were (independently)
    /// attempted.
    Found {
        dispatch_id: String,
        status: RunSourceOutcome,
        recent_events: RunSourceOutcome,
    },
}

/// Read `run_dir/progress.jsonl`'s trailing [`RECENT_EVENTS_DISPLAY_CAP`]
/// lines as the "recent event ledger" the #1016 v1 ruling asks for.
/// `progress.jsonl` mirrors `trajectory.jsonl`'s event stream 1:1
/// (`dispatch_v2::append_trajectory_event`) — lifecycle markers
/// (`event`/`dispatch_id`/`stage`/`timestamp`, occasionally a short `error`),
/// never raw prompt/transcript content, so this is safe at the same
/// advisory-projection trust level as `status.json`'s own fields. Malformed
/// lines are skipped, never surfaced as a parse error (best-effort tail, not
/// a strict log reader).
fn read_recent_progress_events(status: &serde_json::Value) -> RunSourceOutcome {
    let Some(run_dir) = status.get("run_dir").and_then(serde_json::Value::as_str) else {
        return RunSourceOutcome::Unavailable(
            "run status projection carried no run_dir".to_string(),
        );
    };
    let progress_path = std::path::Path::new(run_dir).join("progress.jsonl");
    let raw = match std::fs::read_to_string(&progress_path) {
        Ok(raw) => raw,
        Err(err) => {
            return RunSourceOutcome::Unavailable(format!(
                "read {}: {err}",
                progress_path.display()
            ))
        }
    };
    // Newest-first while capping, then restore chronological (oldest-first)
    // order — the same shape a caller reading the raw file top-to-bottom
    // would see for its trailing window.
    let mut tail: Vec<serde_json::Value> = raw
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .take(RECENT_EVENTS_DISPLAY_CAP)
        .collect();
    tail.reverse();
    RunSourceOutcome::Ok(serde_json::Value::Array(tail))
}

/// `noun=run` entry point: resolve `target` to a `dispatch_id` through the
/// claims source, then independently read that dispatch's `status.json` (via
/// the SAME safe projection `board`/`task_facade` already expose for a known
/// dispatch_id — this only adds the session_client resolution step) and
/// `progress.jsonl` tail.
fn read_run(
    server: &MemoryServer,
    target: &str,
    now_query: DateTime<Utc>,
    ttl_seconds: i64,
) -> RunOutcome {
    let read = match PeerPublicationRead::open_global(server) {
        Ok(read) => read,
        Err(err) => return RunOutcome::ClaimsUnavailable(err),
    };
    let dispatch_id = match read.resolve_active_dispatch_id(target, now_query, ttl_seconds) {
        Ok(Some(id)) => id,
        Ok(None) => return RunOutcome::NoActiveDispatch,
        Err(err) => return RunOutcome::ClaimsUnavailable(err),
    };
    let status = match crate::dispatch_ops::collect_run_task_for_server(server, &dispatch_id) {
        Some(value) => RunSourceOutcome::Ok(value),
        None => RunSourceOutcome::Unavailable(format!(
            "run_dir/status.json not found for dispatch_id {dispatch_id}"
        )),
    };
    let recent_events = match &status {
        RunSourceOutcome::Ok(value) => read_recent_progress_events(value),
        RunSourceOutcome::Unavailable(_) => {
            RunSourceOutcome::Unavailable("run status unreadable; progress not attempted".into())
        }
    };
    RunOutcome::Found {
        dispatch_id,
        status,
        recent_events,
    }
}

/// Advisory "how to actually get a reply" pointer, attached to every `run`
/// answer that has a `target`. MCP is client→server — nothing here can
/// interrupt a live peer turn, so the only honest answer mechanism is the
/// EXISTING sticky facade (#964), used as-is (this noun does not own sticky
/// CAS or reimplement it). Deliberately hedged: the peer-addressing spine
/// (`session_client`) and sticky's own `to`/seat identity chain
/// (`sticky_ops::identity::resolve_caller_agent_id`) are NOT yet unified —
/// asserting `to=<session_client>` always resolves the same seat would be
/// overclaiming a linkage that doesn't exist yet.
fn turn_boundary_callback(target: &str) -> serde_json::Value {
    serde_json::json!({
        "mechanism": "tachi_memory(action='sticky_leave', to=<peer's agent seat>)",
        "note": format!(
            "peers only answer at their own turn boundary (MCP is client->server, \
             never interrupt-capable); leave a sticky note for an async reply. \
             session_client ('{target}') and sticky's seat identity are separate \
             addressing spaces today — resolve the peer's seat name if you don't \
             already know it (#1016 follow-up unifies them)."
        ),
    })
}

/// `peer_query` entry point. Never fails the tool call: every failure mode
/// (denied noun, unreadable source) is a structured envelope, not an `Err`.
pub(crate) fn handle_peer_query(
    server: &MemoryServer,
    params: PeerQueryParams,
) -> Result<String, String> {
    let now_query = Utc::now();
    let snapshot_at = now_query.to_rfc3339();
    let target = params
        .target_session_client
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // sol invariant 2: exhaustive noun whitelist — anything not on the
    // whitelist is denied before any source is touched.
    let noun = params.noun.trim().to_ascii_lowercase();
    let envelope = match PeerNoun::parse(&noun) {
        None => {
            let answered_at = Utc::now().to_rfc3339();
            denied_envelope(&noun, target.as_deref(), &snapshot_at, &answered_at)
        }
        Some(PeerNoun::Presence) => {
            let read = PeerPublicationRead::open_global(server);
            let now_render = Utc::now();
            let answered_at = now_render.to_rfc3339();
            let outcome = match read {
                Ok(read) => match read.read_presence(
                    target.as_deref(),
                    now_query,
                    now_render,
                    CLAIM_TTL_SECONDS,
                ) {
                    Ok(projection) => PresenceOutcome::Read(projection),
                    Err(err) => PresenceOutcome::Unavailable(err),
                },
                Err(err) => PresenceOutcome::Unavailable(err),
            };
            presence_envelope(target.as_deref(), &snapshot_at, &answered_at, outcome)
        }
        Some(PeerNoun::Run) => {
            let answered_at = Utc::now().to_rfc3339();
            match target.as_deref() {
                None => target_required_envelope(&noun, &snapshot_at, &answered_at),
                Some(t) => {
                    let outcome = read_run(server, t, now_query, CLAIM_TTL_SECONDS);
                    run_envelope(t, &snapshot_at, &answered_at, outcome)
                }
            }
        }
    };

    serde_json::to_string(&envelope).map_err(|e| e.to_string())
}

/// The `target` block, identical across every status: same-host locality and
/// the self-asserted-local trust label the S1 contract fixes.
fn target_block(session_client: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "locality": "same_host",
        "session_client": session_client,
        "identity_assurance": "self_asserted_local",
    })
}

/// A `denied` envelope: the noun was not on the exhaustive whitelist. No source
/// is listed (none was consulted) and `result` is empty — the response can
/// never carry another noun's payload keys (sol kill-test 6).
fn denied_envelope(
    noun: &str,
    target: Option<&str>,
    snapshot_at: &str,
    answered_at: &str,
) -> serde_json::Value {
    serde_json::json!({
        "contract": PEER_PUBLICATION_CONTRACT,
        "status": "denied",
        "target": target_block(target),
        "snapshot": {
            "snapshot_at": snapshot_at,
            "answered_at": answered_at,
            "sources": [],
        },
        "result": {},
        "errors": [ {
            "noun": noun,
            "code": "noun_not_whitelisted",
            "message": format!(
                "peer_query noun '{noun}' is not routable; the exhaustive whitelist is: presence, run"
            ),
        } ],
    })
}

/// A `denied` envelope specific to a noun that requires `target_session_client`
/// (it addresses one specific peer, not a board scan) when the caller omitted
/// it. Distinct error code from `noun_not_whitelisted` — the noun IS routable,
/// the call is just missing the addressing field it needs.
fn target_required_envelope(noun: &str, snapshot_at: &str, answered_at: &str) -> serde_json::Value {
    serde_json::json!({
        "contract": PEER_PUBLICATION_CONTRACT,
        "status": "denied",
        "target": target_block(None),
        "snapshot": {
            "snapshot_at": snapshot_at,
            "answered_at": answered_at,
            "sources": [],
        },
        "result": {},
        "errors": [ {
            "noun": noun,
            "code": "target_required",
            "message": format!(
                "peer_query noun '{noun}' requires target_session_client — it addresses one specific peer, not a board scan"
            ),
        } ],
    })
}

/// A `run` envelope: resolves `target` to a `dispatch_id` via the claims
/// source, then reads that dispatch's `status.json`/`progress.jsonl`
/// independently. Overall `status` mirrors the `run_status` source
/// specifically (an unreadable `progress` tail alone does not flip the whole
/// answer to `unreachable` — it is a secondary, independently-reported
/// source, same idiom as presence's `expired_during_render` staying visible
/// rather than collapsing the whole board).
fn run_envelope(
    target: &str,
    snapshot_at: &str,
    answered_at: &str,
    outcome: RunOutcome,
) -> serde_json::Value {
    match outcome {
        RunOutcome::ClaimsUnavailable(err) => serde_json::json!({
            "contract": PEER_PUBLICATION_CONTRACT,
            "status": "unreachable",
            "target": target_block(Some(target)),
            "snapshot": {
                "snapshot_at": snapshot_at,
                "answered_at": answered_at,
                "sources": [ { "name": "claims", "state": "unavailable", "as_of": serde_json::Value::Null } ],
            },
            "result": {},
            "errors": [ {
                "name": "claims",
                "code": "source_unavailable",
                "message": err,
            } ],
        }),
        RunOutcome::NoActiveDispatch => serde_json::json!({
            "contract": PEER_PUBLICATION_CONTRACT,
            "status": "ok",
            "target": target_block(Some(target)),
            "snapshot": {
                "snapshot_at": snapshot_at,
                "answered_at": answered_at,
                "sources": [ { "name": "claims", "state": "empty", "as_of": serde_json::Value::Null } ],
            },
            "result": {
                "dispatch_id": serde_json::Value::Null,
                "status": serde_json::Value::Null,
                "recent_events": serde_json::Value::Null,
            },
            "errors": [],
            "turn_boundary_callback": turn_boundary_callback(target),
        }),
        RunOutcome::Found {
            dispatch_id,
            status,
            recent_events,
        } => {
            let (status_state, status_value, status_err) = match status {
                RunSourceOutcome::Ok(value) => ("ok", Some(value), None),
                RunSourceOutcome::Unavailable(err) => ("unavailable", None, Some(err)),
            };
            let (events_state, events_value, events_err) = match recent_events {
                RunSourceOutcome::Ok(value) => {
                    let is_empty = value.as_array().map(Vec::is_empty).unwrap_or(true);
                    (if is_empty { "empty" } else { "ok" }, Some(value), None)
                }
                RunSourceOutcome::Unavailable(err) => ("unavailable", None, Some(err)),
            };
            let overall_status = if status_state == "unavailable" {
                "unreachable"
            } else {
                "ok"
            };
            let mut errors = Vec::new();
            if let Some(err) = status_err {
                errors.push(serde_json::json!({
                    "name": "run_status",
                    "code": "source_unavailable",
                    "message": err,
                }));
            }
            if let Some(err) = events_err {
                errors.push(serde_json::json!({
                    "name": "progress",
                    "code": "source_unavailable",
                    "message": err,
                }));
            }
            serde_json::json!({
                "contract": PEER_PUBLICATION_CONTRACT,
                "status": overall_status,
                "target": target_block(Some(target)),
                "snapshot": {
                    "snapshot_at": snapshot_at,
                    "answered_at": answered_at,
                    "sources": [
                        { "name": "claims", "state": "ok", "as_of": serde_json::Value::Null },
                        { "name": "run_status", "state": status_state, "as_of": serde_json::Value::Null },
                        { "name": "progress", "state": events_state, "as_of": serde_json::Value::Null },
                    ],
                },
                "result": {
                    "dispatch_id": dispatch_id,
                    "status": status_value,
                    "recent_events": events_value,
                },
                "errors": errors,
                "turn_boundary_callback": turn_boundary_callback(target),
            })
        }
    }
}

/// A presence envelope. `ok`/`empty` only when the source was actually read;
/// an unreadable source is `unavailable` with an empty `result` — never a
/// `count: 0` board (sol invariant 3).
fn presence_envelope(
    target: Option<&str>,
    snapshot_at: &str,
    answered_at: &str,
    outcome: PresenceOutcome,
) -> serde_json::Value {
    match outcome {
        PresenceOutcome::Read(projection) => {
            let alive = projection
                .board
                .get("count")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let had_any_candidate = alive > 0 || !projection.expired_during_render.is_empty();
            // `empty` is legal ONLY when the read succeeded and there were zero
            // rows of any kind; if rows existed but all expired during render,
            // that is `ok` with a zero alive count, not `empty`.
            let source_state = if had_any_candidate { "ok" } else { "empty" };
            serde_json::json!({
                "contract": PEER_PUBLICATION_CONTRACT,
                "status": "ok",
                "target": target_block(target),
                "snapshot": {
                    "snapshot_at": snapshot_at,
                    "answered_at": answered_at,
                    "sources": [ {
                        "name": "presence",
                        "state": source_state,
                        "as_of": projection.as_of,
                    } ],
                },
                "result": {
                    "board": projection.board,
                    "expired_during_render": projection.expired_during_render,
                },
                "errors": [],
            })
        }
        PresenceOutcome::Unavailable(err) => serde_json::json!({
            "contract": PEER_PUBLICATION_CONTRACT,
            "status": "unreachable",
            "target": target_block(target),
            "snapshot": {
                "snapshot_at": snapshot_at,
                "answered_at": answered_at,
                "sources": [ {
                    "name": "presence",
                    "state": "unavailable",
                    "as_of": serde_json::Value::Null,
                } ],
            },
            "result": {},
            "errors": [ {
                "name": "presence",
                "code": "source_unavailable",
                "message": err,
            } ],
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memcore::{ClaimState, SessionClaim};

    fn ts(iso: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(iso).unwrap().to_utc()
    }

    /// A claim whose `heartbeat_at` is `iso`. `session_client` lets a test
    /// target-filter.
    fn claim(claim_id: &str, session_client: &str, heartbeat_iso: &str) -> SessionClaim {
        SessionClaim {
            claim_id: claim_id.to_string(),
            session_client: Some(session_client.to_string()),
            issue_ref: Some(format!("org/repo#{claim_id}")),
            flow_id: None,
            dispatch_id: None,
            branch: "feat/x".to_string(),
            declared_file_scope: None,
            state: ClaimState::Active,
            release_reason: None,
            created_at: heartbeat_iso.to_string(),
            heartbeat_at: heartbeat_iso.to_string(),
            released_at: None,
        }
    }

    /// Recursively collect every object KEY that appears anywhere in `value`.
    fn all_keys(value: &serde_json::Value, out: &mut std::collections::BTreeSet<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (k, v) in map {
                    out.insert(k.clone());
                    all_keys(v, out);
                }
            }
            serde_json::Value::Array(items) => {
                for v in items {
                    all_keys(v, out);
                }
            }
            _ => {}
        }
    }

    /// Nouns of the OTHER (non-presence) publication surfaces S1 must never
    /// leak — the minimal version of sol kill-test 6. If any of these ever
    /// appears as a response key, presence answering has bled another noun's
    /// payload into the envelope.
    const FORBIDDEN_NOUN_KEYS: &[&str] = &[
        "memories",
        "memory",
        "vault",
        "secrets",
        "credentials",
        "precedents",
        "outcomes",
        "sticky",
        "handoff",
        "checkpoint",
    ];

    fn assert_no_forbidden_keys(envelope: &serde_json::Value) {
        let mut keys = std::collections::BTreeSet::new();
        all_keys(envelope, &mut keys);
        for forbidden in FORBIDDEN_NOUN_KEYS {
            assert!(
                !keys.contains(*forbidden),
                "peer-publication envelope must never carry another noun's key '{forbidden}': keys={keys:?}"
            );
        }
    }

    // ── ① structural read-only: no write path, and the handle rejects writes ─

    #[test]
    fn peer_read_connection_is_structurally_read_only_table_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("peer.db");
        let db_str = db.to_str().unwrap();

        // Seed one claim through a read-write store, then drop it.
        {
            let store = MemoryStore::open(db_str).unwrap();
            memcore::insert_claim(
                store.connection(),
                &memcore::NewSessionClaim {
                    claim_id: "seed-1".to_string(),
                    session_client: Some("codex".to_string()),
                    issue_ref: Some("org/repo#1".to_string()),
                    flow_id: None,
                    dispatch_id: None,
                    branch: "feat/x".to_string(),
                    declared_file_scope: None,
                    created_at: Utc::now().to_rfc3339(),
                },
            )
            .unwrap();
        }

        let read = PeerPublicationRead::open_at(db_str).unwrap();

        // The projection read works…
        let now = Utc::now();
        let projection = read
            .read_presence(None, now, now, CLAIM_TTL_SECONDS)
            .expect("read-only presence read must succeed");
        assert_eq!(
            projection.board["count"].as_u64().unwrap(),
            1,
            "the seeded claim must be visible through the read-only view"
        );

        // …but a write through the SAME handle fails at the SQLite layer.
        let write = read.attempt_write_for_test(
            "INSERT INTO session_claims (claim_id, branch, state, created_at, heartbeat_at) \
             VALUES ('injected', '', 'active', '2026-07-12T00:00:00Z', '2026-07-12T00:00:00Z')",
        );
        assert!(
            write.is_err(),
            "a write through the peer read-only connection must be rejected structurally"
        );

        // And an independent read-write connection confirms the table is
        // unchanged — the failed write left no row behind.
        let verify = MemoryStore::open(db_str).unwrap();
        let count: i64 = verify
            .connection()
            .query_row("SELECT COUNT(*) FROM session_claims", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "no row may be written through the read-only view");
    }

    // ── ② source unavailable (bad/missing DB) is never an empty count:0 board ─

    #[test]
    fn missing_db_is_unavailable_not_empty() {
        // Opening a nonexistent path is a source-unavailable condition.
        let err = PeerPublicationRead::open_at("/nonexistent/path/to/peer.db");
        assert!(err.is_err(), "opening a missing DB must fail");

        let envelope = presence_envelope(
            None,
            "2026-07-12T00:00:00Z",
            "2026-07-12T00:00:01Z",
            PresenceOutcome::Unavailable(err.err().unwrap()),
        );
        assert_eq!(envelope["status"], "unreachable");
        let source = &envelope["snapshot"]["sources"][0];
        assert_eq!(
            source["state"], "unavailable",
            "an unreadable source must be `unavailable`, never `empty`"
        );
        // sol invariant 3: no count:0 board masquerading for an unreadable source.
        assert!(
            envelope["result"].get("board").is_none(),
            "unavailable result must not carry a board (no fabricated count:0): {envelope}"
        );
        assert_no_forbidden_keys(&envelope);
    }

    #[test]
    fn genuinely_empty_is_empty_not_unavailable() {
        // Read succeeded, zero candidates → `empty` with a real count:0 board.
        let projection = project_peer_presence(&[], ts("2026-07-12T00:00:00Z"), CLAIM_TTL_SECONDS);
        let envelope = presence_envelope(
            None,
            "2026-07-12T00:00:00Z",
            "2026-07-12T00:00:00Z",
            PresenceOutcome::Read(projection),
        );
        assert_eq!(envelope["status"], "ok");
        assert_eq!(envelope["snapshot"]["sources"][0]["state"], "empty");
        assert_eq!(envelope["result"]["board"]["count"], 0);
    }

    // ── ③ six live claims → 5-row board + count 6 + overflow 1 ───────────────

    #[test]
    fn six_live_claims_yield_five_rows_count_six_overflow_one() {
        let now = ts("2026-07-12T12:00:00Z");
        // All six heartbeats are well within TTL of `now`.
        let candidates: Vec<SessionClaim> = (0..6)
            .map(|i| {
                claim(
                    &format!("c{i}"),
                    &format!("sess-{i}"),
                    "2026-07-12T11:59:00Z",
                )
            })
            .collect();

        let projection = project_peer_presence(&candidates, now, CLAIM_TTL_SECONDS);
        assert_eq!(projection.board["count"].as_u64().unwrap(), 6);
        assert_eq!(projection.board["items"].as_array().unwrap().len(), 5);
        assert_eq!(projection.board["overflow"].as_u64().unwrap(), 1);
        assert!(projection.expired_during_render.is_empty());

        let envelope = presence_envelope(
            None,
            "2026-07-12T12:00:00Z",
            "2026-07-12T12:00:00Z",
            PresenceOutcome::Read(projection),
        );
        assert_eq!(envelope["snapshot"]["sources"][0]["state"], "ok");
        assert_no_forbidden_keys(&envelope);
    }

    // ── ④ TTL boundary: 1s-left alive, past expiry gone, render-window expiry ─

    #[test]
    fn ttl_boundary_one_second_left_is_alive() {
        // TTL = 1800s; heartbeat 1799s before render → 1s of life left.
        let now = ts("2026-07-12T12:00:00Z");
        let one_second_left = claim("c", "sess", "2026-07-12T11:30:01Z");
        let projection = project_peer_presence(&[one_second_left], now, 1800);
        assert_eq!(projection.board["count"].as_u64().unwrap(), 1);
        assert!(projection.expired_during_render.is_empty());
    }

    #[test]
    fn ttl_render_window_expiry_is_flagged_not_counted_alive() {
        // The candidate was fresh at snapshot time but its heartbeat is now
        // past the TTL horizon at render time (1801s ago, TTL 1800s).
        let now_render = ts("2026-07-12T12:00:00Z");
        let expired = claim("c", "sess", "2026-07-12T11:29:59Z");
        let projection = project_peer_presence(&[expired], now_render, 1800);

        assert_eq!(
            projection.board["count"].as_u64().unwrap(),
            0,
            "an expired-during-render claim must NOT be counted as alive"
        );
        assert_eq!(projection.board["items"].as_array().unwrap().len(), 0);
        assert_eq!(projection.expired_during_render.len(), 1);
        assert_eq!(
            projection.expired_during_render[0]["expired_during_render"], true,
            "the render-window expiry must be explicitly flagged, never presented as live"
        );
        assert!(
            projection.as_of.is_none(),
            "as_of has no alive row to report"
        );
    }

    #[test]
    fn ttl_exact_horizon_is_still_alive_not_off_by_one() {
        // Heartbeat exactly TTL seconds ago: `is_claim_stale` uses `> ttl`, so
        // exactly-at-the-horizon is still alive.
        let now = ts("2026-07-12T12:00:00Z");
        let at_horizon = claim("c", "sess", "2026-07-12T11:30:00Z");
        let projection = project_peer_presence(&[at_horizon], now, 1800);
        assert_eq!(projection.board["count"].as_u64().unwrap(), 1);
        assert!(projection.expired_during_render.is_empty());
    }

    // ── ⑤ noun whitelist: outcomes → denied; storefront never leaks keys ─────

    #[test]
    fn non_presence_noun_is_denied() {
        assert!(PeerNoun::parse("outcomes").is_none());
        assert!(PeerNoun::parse("sticky").is_none());
        assert!(PeerNoun::parse("handoff").is_none());
        assert!(PeerNoun::parse("memories").is_none());
        assert!(matches!(
            PeerNoun::parse("presence"),
            Some(PeerNoun::Presence)
        ));

        let envelope = denied_envelope(
            "outcomes",
            Some("codex"),
            "2026-07-12T00:00:00Z",
            "2026-07-12T00:00:00Z",
        );
        assert_eq!(envelope["status"], "denied");
        assert!(
            !envelope["errors"].as_array().unwrap().is_empty(),
            "a denied noun must carry an explaining error"
        );
        assert_eq!(envelope["errors"][0]["code"], "noun_not_whitelisted");
        // No source was consulted for a denied noun.
        assert!(envelope["snapshot"]["sources"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn storefront_response_never_contains_other_noun_keys() {
        // Denied envelope for a forbidden noun…
        let denied = denied_envelope(
            "outcomes",
            None,
            "2026-07-12T00:00:00Z",
            "2026-07-12T00:00:00Z",
        );
        assert_no_forbidden_keys(&denied);

        // …and a fully-populated presence-ok envelope. Even with a live board,
        // the response key set must never include another noun's payload key.
        let now = ts("2026-07-12T12:00:00Z");
        let projection = project_peer_presence(
            &[claim("c", "sess", "2026-07-12T11:59:00Z")],
            now,
            CLAIM_TTL_SECONDS,
        );
        let ok = presence_envelope(
            Some("sess"),
            "2026-07-12T12:00:00Z",
            "2026-07-12T12:00:00Z",
            PresenceOutcome::Read(projection),
        );
        assert_no_forbidden_keys(&ok);
    }

    // ── target filtering: single-seat narrowing ──────────────────────────────

    #[test]
    fn target_filter_narrows_to_a_single_seat() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("peer.db");
        let db_str = db.to_str().unwrap();
        {
            let store = MemoryStore::open(db_str).unwrap();
            for (id, sess) in [("a", "codex"), ("b", "claude-code")] {
                memcore::insert_claim(
                    store.connection(),
                    &memcore::NewSessionClaim {
                        claim_id: id.to_string(),
                        session_client: Some(sess.to_string()),
                        issue_ref: Some(format!("org/repo#{id}")),
                        flow_id: None,
                        dispatch_id: None,
                        branch: "feat/x".to_string(),
                        declared_file_scope: None,
                        created_at: Utc::now().to_rfc3339(),
                    },
                )
                .unwrap();
            }
        }
        let read = PeerPublicationRead::open_at(db_str).unwrap();
        let now = Utc::now();

        let all = read
            .read_presence(None, now, now, CLAIM_TTL_SECONDS)
            .unwrap();
        assert_eq!(all.board["count"].as_u64().unwrap(), 2, "board sees both");

        let one = read
            .read_presence(Some("codex"), now, now, CLAIM_TTL_SECONDS)
            .unwrap();
        assert_eq!(
            one.board["count"].as_u64().unwrap(),
            1,
            "target filter narrows to the single seat"
        );
        assert_eq!(
            one.board["items"][0]["session_client"], "codex",
            "the narrowed row is the requested seat"
        );
    }

    // ── noun=run: target-required, empty vs unavailable, real read ───────────

    /// Build a `MemoryServer` whose global DB lives at
    /// `<tmp>/global/memory.db` — the exact shape `runs_dir_for_server`
    /// special-cases to resolve `<tmp>/runs` as the dispatch run-dir root
    /// (`dispatch_ops::board::paths::runs_dir_for_server`), so a dispatch_id
    /// resolved through this server's claims table can be paired with a real
    /// `runs/<dispatch_id>/status.json` on disk.
    fn run_test_server(tmp: &std::path::Path) -> crate::MemoryServer {
        let global_db = tmp.join("global").join("memory.db");
        crate::MemoryServer::new(global_db, None).expect("test memory server")
    }

    #[test]
    fn run_noun_without_target_is_target_required_not_noun_not_whitelisted() {
        // On origin/main (no `run` variant) this same call returns `denied` /
        // `noun_not_whitelisted` (the noun itself isn't parsed). Post-fix it
        // parses fine and is denied for a DIFFERENT, more specific reason —
        // a real behavioral flip through the actual `handle_peer_query` entry
        // point, not a compile-time difference.
        let dir = tempfile::tempdir().unwrap();
        let server = run_test_server(dir.path());
        let body = handle_peer_query(
            &server,
            PeerQueryParams {
                target_session_client: None,
                noun: "run".to_string(),
            },
        )
        .unwrap();
        let envelope: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(envelope["status"], "denied");
        assert_eq!(envelope["errors"][0]["code"], "target_required");
        assert_no_forbidden_keys(&envelope);
    }

    #[test]
    fn run_noun_with_no_active_claim_is_empty_not_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let server = run_test_server(dir.path());
        // No claim seeded at all for "codex".
        let body = handle_peer_query(
            &server,
            PeerQueryParams {
                target_session_client: Some("codex".to_string()),
                noun: "run".to_string(),
            },
        )
        .unwrap();
        let envelope: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(envelope["status"], "ok");
        assert_eq!(envelope["snapshot"]["sources"][0]["name"], "claims");
        assert_eq!(envelope["snapshot"]["sources"][0]["state"], "empty");
        assert_eq!(envelope["result"]["dispatch_id"], serde_json::Value::Null);
        assert!(
            envelope["turn_boundary_callback"]["mechanism"]
                .as_str()
                .unwrap()
                .contains("sticky_leave"),
            "no-active-dispatch answer must still point at the sticky callback: {envelope}"
        );
        assert_no_forbidden_keys(&envelope);
    }

    #[test]
    fn run_noun_resolves_dispatch_id_and_reads_status_and_progress_tail() {
        let dir = tempfile::tempdir().unwrap();
        let global_db = dir.path().join("global").join("memory.db");
        let server = crate::MemoryServer::new(global_db.clone(), None).expect("server");

        // Seed an active claim carrying a dispatch_id for "codex".
        {
            let store = memcore::MemoryStore::open(global_db.to_str().unwrap()).unwrap();
            memcore::insert_claim(
                store.connection(),
                &memcore::NewSessionClaim {
                    claim_id: "claim-run-1".to_string(),
                    session_client: Some("codex".to_string()),
                    issue_ref: Some("org/repo#1".to_string()),
                    flow_id: None,
                    dispatch_id: Some("d-run-123".to_string()),
                    branch: "feat/x".to_string(),
                    declared_file_scope: None,
                    created_at: Utc::now().to_rfc3339(),
                },
            )
            .unwrap();
        }

        // Write a real run-dir: <tmp>/runs/d-run-123/{status.json,progress.jsonl}
        let run_dir = dir.path().join("runs").join("d-run-123");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("status.json"),
            serde_json::json!({
                "dispatch_id": "d-run-123",
                "task": "fix the thing",
                "agent": "codex",
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            run_dir.join("progress.jsonl"),
            "{\"event\":\"a\",\"timestamp\":\"t1\"}\n\
             {\"event\":\"b\",\"timestamp\":\"t2\"}\n\
             {\"event\":\"c\",\"timestamp\":\"t3\"}\n",
        )
        .unwrap();

        let body = handle_peer_query(
            &server,
            PeerQueryParams {
                target_session_client: Some("codex".to_string()),
                noun: "run".to_string(),
            },
        )
        .unwrap();
        let envelope: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(envelope["status"], "ok", "envelope: {envelope}");
        assert_eq!(envelope["result"]["dispatch_id"], "d-run-123");
        assert_eq!(
            envelope["result"]["status"]["dispatch_id"], "d-run-123",
            "run_status projection must carry through: {envelope}"
        );
        let events = envelope["result"]["recent_events"].as_array().unwrap();
        assert_eq!(
            events.len(),
            3,
            "all three progress lines fit under the cap"
        );
        assert_eq!(events[0]["event"], "a");
        assert_eq!(events[2]["event"], "c");
        assert_no_forbidden_keys(&envelope);
    }

    #[test]
    fn run_noun_missing_run_dir_is_unreachable_for_run_status_source() {
        let dir = tempfile::tempdir().unwrap();
        let global_db = dir.path().join("global").join("memory.db");
        let server = crate::MemoryServer::new(global_db.clone(), None).expect("server");
        {
            let store = memcore::MemoryStore::open(global_db.to_str().unwrap()).unwrap();
            memcore::insert_claim(
                store.connection(),
                &memcore::NewSessionClaim {
                    claim_id: "claim-run-2".to_string(),
                    session_client: Some("codex".to_string()),
                    issue_ref: Some("org/repo#2".to_string()),
                    flow_id: None,
                    dispatch_id: Some("d-missing-456".to_string()),
                    branch: "feat/y".to_string(),
                    declared_file_scope: None,
                    created_at: Utc::now().to_rfc3339(),
                },
            )
            .unwrap();
        }
        // No run_dir written on disk for d-missing-456.

        let body = handle_peer_query(
            &server,
            PeerQueryParams {
                target_session_client: Some("codex".to_string()),
                noun: "run".to_string(),
            },
        )
        .unwrap();
        let envelope: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(envelope["status"], "unreachable", "envelope: {envelope}");
        let sources = envelope["snapshot"]["sources"].as_array().unwrap();
        let run_status_source = sources
            .iter()
            .find(|s| s["name"] == "run_status")
            .expect("run_status source listed");
        assert_eq!(run_status_source["state"], "unavailable");
        assert!(
            !envelope["errors"].as_array().unwrap().is_empty(),
            "an unreadable run_status source must carry an explaining error: {envelope}"
        );
        assert_no_forbidden_keys(&envelope);
    }

    #[test]
    fn checkpoint_and_events_nouns_remain_denied() {
        // Documents the deliberate #1016 leaf gap (module doc): both hit a
        // project-routing / missing-correlator wall on inspection and are
        // NOT routed rather than guessed.
        assert!(PeerNoun::parse("checkpoint").is_none());
        assert!(PeerNoun::parse("events").is_none());
        assert!(PeerNoun::parse("event_ledger").is_none());
    }
}
