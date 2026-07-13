//! Immutable build tickets and the queue's terminal states (#894 S2c item 2,
//! round-2).
//!
//! A ticket is the *request*: what source tree to build, with what command, and
//! who gets the result. It is written once and never rewritten — a mutable
//! ticket would let a caller change the source identity out from under a build
//! that is already running against a target dir chosen for the OLD identity,
//! which is precisely the cross-lineage poisoning the broker exists to prevent.
//!
//! Storage is the `hard_state` KV table — the same store the sticky claim gate
//! and the orchestrator use:
//!
//! - [`TICKET_NS`] (key = ticket id) — the immutable request. Written with
//!   `insert_state_if_absent` (a single `INSERT ... ON CONFLICT DO NOTHING`), so
//!   the second submit of an id never overwrites.
//! - [`STATUS_NS`] (key = ticket id) — the *mutable* lifecycle record: how many
//!   times the executor tried this ticket, and whether it has reached a terminal
//!   state ([`TicketState::Failed`] / [`TicketState::Cancelled`]). This is what
//!   keeps a ticket that can never run — a dirty seat, a checkout that always
//!   fails — from sitting at the head of a FIFO queue forever and starving every
//!   ticket behind it (round-2 review finding: there was no dead letter and no
//!   cancel, so one bad ticket wedged the machine's build queue).
//!
//! Splitting the two is deliberate: the *request* stays write-once (nobody can
//! re-point a source sha), while the *lifecycle* is allowed to move — which is
//! the only thing a queue can do with a ticket that keeps failing.

use memcore::MemoryStore;
use serde::{Deserialize, Serialize};

/// `hard_state` namespace for the immutable ticket payloads.
pub(crate) const TICKET_NS: &str = "build_ticket";
/// `hard_state` namespace for the mutable per-ticket lifecycle record.
pub(crate) const STATUS_NS: &str = "build_ticket_status";

/// How many times the executor may fail a ticket before it is dead-lettered.
///
/// Bounded, not infinite: an error that is going to happen is going to happen
/// three times too. The point of the retries is to survive a *transient* refusal
/// (the seat was dirty for a minute, a git lock was held); the point of the
/// bound is that a permanent one leaves the queue instead of blocking it.
pub(crate) const MAX_TICKET_ATTEMPTS: u32 = 3;

/// Which source tree a ticket wants built. This is the *identity* the target
/// generation is checked against — `head_sha` is the exact tree, `base_sha` is
/// what it claims to be branched from, and `repo_root` scopes both.
///
/// `repo_root` is a **repo identity**, not "whatever path the caller typed": the
/// CLI resolves it through `build_broker::repo::repo_identity` (git's common
/// dir → the main worktree root, canonicalized) before it ever reaches a ticket.
/// Two linked worktrees of one repo therefore produce the same `repo_root`, and
/// the queue can be filtered by it (a seat only ever runs its own repo's
/// tickets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SourceIdentity {
    pub repo_root: String,
    pub base_sha: String,
    pub head_sha: String,
}

/// What to run. Executed directly (no shell), so `program`/`args` are not a
/// shell-injection surface; `features` is carried separately so the broker can
/// reason about (and later, key targets on) feature sets rather than parsing
/// them back out of an arg vector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BuildCommand {
    pub program: String,
    pub args: Vec<String>,
    pub features: Vec<String>,
}

/// An immutable build ticket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BuildTicket {
    pub ticket_id: String,
    pub source: SourceIdentity,
    pub command: BuildCommand,
    /// The lease the result is attributed to (`exec_envs.env_id`).
    pub env_id: Option<String>,
    pub dispatch_id: Option<String>,
    pub created_at: String,
}

impl BuildTicket {
    /// Build a validated ticket. Validation is not decoration:
    ///
    /// - `head_sha`/`base_sha` must be **hex object ids**, because the executor
    ///   seat feeds `head_sha` straight to `git checkout --detach <sha>`. A ref
    ///   name (or anything with a `-`, or a path) there is an argument-injection
    ///   surface and a way to make the seat check out something other than what
    ///   the ticket claims. Refuse at the door.
    /// - `repo_root` must be an **absolute** path: it is the ticket's repo
    ///   identity (what the queue filter and the target-generation check compare
    ///   on) and it is handed to `git -C`. A relative path is neither an
    ///   identity (it floats with the caller's cwd) nor safe to shell out with.
    /// - a ticket with no `program` has nothing to serialize the executor seat
    ///   *for*.
    pub(crate) fn new(
        ticket_id: impl Into<String>,
        source: SourceIdentity,
        command: BuildCommand,
        env_id: Option<String>,
        dispatch_id: Option<String>,
    ) -> Result<Self, String> {
        let ticket_id = ticket_id.into();
        if ticket_id.trim().is_empty() {
            return Err("build ticket needs a ticket_id".to_string());
        }
        if source.repo_root.trim().is_empty() {
            return Err("build ticket needs a repo_root (repo identity)".to_string());
        }
        if !std::path::Path::new(&source.repo_root).is_absolute() {
            return Err(format!(
                "build ticket repo_root '{}' is not absolute: repo_root is the ticket's repo \
                 IDENTITY (the queue filter and the target-generation check compare on it, and \
                 git is invoked with it), and a relative path floats with the caller's cwd \
                 (#894 S2c)",
                source.repo_root
            ));
        }
        if !is_object_id(&source.head_sha) {
            return Err(format!(
                "build ticket head_sha '{}' is not a hex object id: the executor seat checks \
                 this out directly, so a ref name / flag / path here is an injection surface \
                 (#894 S2c)",
                source.head_sha
            ));
        }
        if !is_object_id(&source.base_sha) {
            return Err(format!(
                "build ticket base_sha '{}' is not a hex object id (#894 S2c)",
                source.base_sha
            ));
        }
        if command.program.trim().is_empty() {
            return Err("build ticket needs a command program".to_string());
        }
        Ok(BuildTicket {
            ticket_id,
            source,
            command,
            env_id,
            dispatch_id,
            created_at: chrono::Utc::now().to_rfc3339(),
        })
    }

    /// The fields that make this ticket *the same request* — what a re-submit is
    /// compared on (see [`submit_ticket`]).
    ///
    /// Deliberately excludes `created_at`: it is stamped `now` by [`new`], so a
    /// whole-struct comparison meant two calls with identical arguments were
    /// never equal and the idempotent path was **unreachable in production** —
    /// every retry of a submit hit the "different payload" error instead
    /// (round-2 review finding). It also excludes `env_id`/`dispatch_id`, which
    /// say who to *attribute* the result to, not what to build.
    fn identity(&self) -> (&str, &SourceIdentity, &BuildCommand) {
        (&self.ticket_id, &self.source, &self.command)
    }

    /// Same request? (id + repo + source shas + program/args/features.)
    pub(crate) fn same_request_as(&self, other: &BuildTicket) -> bool {
        self.identity() == other.identity()
    }
}

/// A git object id: hex, abbreviated (>=7) up to a full sha-256 (64).
fn is_object_id(raw: &str) -> bool {
    (7..=64).contains(&raw.len()) && raw.chars().all(|c| c.is_ascii_hexdigit())
}

/// Where a ticket is in its life. A ticket with no status row has never been
/// attempted; that is [`TicketState::Queued`] by omission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TicketState {
    /// Waiting for the seat (possibly after some failed attempts).
    Queued,
    /// Dead letter: the executor failed it [`MAX_TICKET_ATTEMPTS`] times. It is
    /// terminal — `run_next` will never pick it up again, so it cannot starve
    /// the tickets behind it.
    Failed,
    /// Terminal by operator decision (`tachi build cancel`).
    Cancelled,
}

impl TicketState {
    /// Terminal = out of the queue for good.
    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, TicketState::Failed | TicketState::Cancelled)
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            TicketState::Queued => "queued",
            TicketState::Failed => "failed",
            TicketState::Cancelled => "cancelled",
        }
    }
}

/// The mutable half of a ticket: attempts + terminal state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TicketStatus {
    pub ticket_id: String,
    pub state: TicketState,
    /// How many times the executor has taken this ticket and failed it.
    pub attempts: u32,
    /// Why it failed / was cancelled (the most recent reason).
    pub last_error: Option<String>,
    pub updated_at: String,
}

/// Submit a ticket. Write-once: re-submitting the *same request* is an
/// idempotent no-op (a retrying caller must not be punished), but re-submitting
/// a *different* request under an existing id is a hard error — tickets are
/// immutable (#894 S2c item 2).
///
/// "Same request" is [`BuildTicket::same_request_as`] — identity fields only.
/// Comparing the whole struct (as round-1 did) compared `created_at` too, which
/// `BuildTicket::new` stamps with the wall clock: the idempotent branch could
/// then only ever be reached by re-submitting a ticket value that had been kept
/// alive in memory, never by a fresh process retrying the same command. In
/// production it was dead code and every retry was an error.
///
/// A re-submit that matches the request but carries different attribution
/// (`env_id` / `dispatch_id`) is accepted as idempotent, and the STORED ticket's
/// attribution stands — a ticket is immutable, so the first submit wins. That is
/// warned about rather than silently swallowed.
pub(crate) fn submit_ticket(store: &MemoryStore, ticket: &BuildTicket) -> Result<(), String> {
    let value = serde_json::to_string(ticket).map_err(|e| format!("serialize ticket: {e}"))?;
    let inserted = store
        .insert_state_if_absent(TICKET_NS, &ticket.ticket_id, &value)
        .map_err(|e| format!("submit ticket {}: {e}", ticket.ticket_id))?;
    if inserted {
        return Ok(());
    }
    match load_ticket(store, &ticket.ticket_id)? {
        Some(existing) if existing.same_request_as(ticket) => {
            if existing.env_id != ticket.env_id || existing.dispatch_id != ticket.dispatch_id {
                tracing::warn!(
                    ticket = %ticket.ticket_id,
                    stored_env_id = ?existing.env_id,
                    resubmitted_env_id = ?ticket.env_id,
                    "build ticket re-submitted with different attribution; the stored ticket is \
                     immutable and its attribution stands"
                );
            }
            Ok(())
        }
        Some(existing) => Err(format!(
            "build ticket '{}' already exists as a DIFFERENT request: tickets are immutable (a \
             mutable ticket could re-point the source identity after the executor already chose \
             a target dir for the old one). Stored: {} @ {} `{} {}`; submitted: {} @ {} `{} {}`. \
             Submit a new ticket id (#894 S2c)",
            ticket.ticket_id,
            existing.source.repo_root,
            existing.source.head_sha,
            existing.command.program,
            existing.command.args.join(" "),
            ticket.source.repo_root,
            ticket.source.head_sha,
            ticket.command.program,
            ticket.command.args.join(" "),
        )),
        None => Err(format!(
            "build ticket '{}' collided on insert but could not be read back",
            ticket.ticket_id
        )),
    }
}

/// Load a ticket by id.
pub(crate) fn load_ticket(
    store: &MemoryStore,
    ticket_id: &str,
) -> Result<Option<BuildTicket>, String> {
    let row = store
        .get_state_kv(TICKET_NS, ticket_id)
        .map_err(|e| format!("load ticket {ticket_id}: {e}"))?;
    match row {
        None => Ok(None),
        Some((json, _version)) => serde_json::from_str(&json)
            .map(Some)
            .map_err(|e| format!("decode ticket {ticket_id}: {e}")),
    }
}

/// Every ticket on this machine, oldest first — the queue order (all repos).
pub(crate) fn list_tickets(store: &MemoryStore) -> Result<Vec<BuildTicket>, String> {
    let mut tickets = store
        .list_state(TICKET_NS)
        .map_err(|e| format!("list tickets: {e}"))?
        .into_iter()
        .map(|row| {
            serde_json::from_str::<BuildTicket>(&row.value_json)
                .map_err(|e| format!("decode ticket {}: {e}", row.key))
        })
        .collect::<Result<Vec<_>, _>>()?;
    // FIFO by submission time. `created_at` is an RFC3339 stamp, so this is a
    // parsed-instant comparison, not a string sort — a lexicographic compare of
    // mixed-offset timestamps ("+08:00" vs "Z") orders them wrong.
    tickets.sort_by_key(|t| {
        chrono::DateTime::parse_from_rfc3339(&t.created_at)
            .map(|dt| dt.timestamp_micros())
            .unwrap_or(i64::MAX)
    });
    Ok(tickets)
}

/// Read a ticket's lifecycle record. `None` = never attempted, never cancelled.
pub(crate) fn load_status(
    store: &MemoryStore,
    ticket_id: &str,
) -> Result<Option<TicketStatus>, String> {
    let row = store
        .get_state_kv(STATUS_NS, ticket_id)
        .map_err(|e| format!("load ticket status {ticket_id}: {e}"))?;
    match row {
        None => Ok(None),
        Some((json, _version)) => serde_json::from_str(&json)
            .map(Some)
            .map_err(|e| format!("decode ticket status {ticket_id}: {e}")),
    }
}

/// The effective state of a ticket (a missing status row = `Queued`).
pub(crate) fn ticket_state(store: &MemoryStore, ticket_id: &str) -> Result<TicketState, String> {
    Ok(load_status(store, ticket_id)?
        .map(|s| s.state)
        .unwrap_or(TicketState::Queued))
}

fn write_status(store: &MemoryStore, status: &TicketStatus) -> Result<(), String> {
    let value =
        serde_json::to_string(status).map_err(|e| format!("serialize ticket status: {e}"))?;
    store
        .set_state(STATUS_NS, &status.ticket_id, &value)
        .map_err(|e| format!("write ticket status {}: {e}", status.ticket_id))?;
    Ok(())
}

/// Book a failed attempt against a ticket, dead-lettering it once it has burned
/// [`MAX_TICKET_ATTEMPTS`].
///
/// This is what makes a doomed ticket *leave* the queue. Round-1 had no such
/// path: `run_next` always took the oldest pending ticket, and any error that
/// was not "slot busy" (a dirty seat refusing the checkout, a git failure, a
/// missing sha) propagated out with the ticket still pending — so the very next
/// `run_next` took the same ticket, failed the same way, and every ticket behind
/// it starved. Forever.
///
/// Returns the status row as it now stands (so the caller can log/report the
/// dead letter).
pub(crate) fn record_failed_attempt(
    store: &MemoryStore,
    ticket_id: &str,
    error: &str,
) -> Result<TicketStatus, String> {
    let previous = load_status(store, ticket_id)?;
    // A terminal ticket is not re-opened by a late failure report.
    if let Some(status) = previous.as_ref() {
        if status.state.is_terminal() {
            return Ok(status.clone());
        }
    }
    let attempts = previous.map(|s| s.attempts).unwrap_or(0) + 1;
    let state = if attempts >= MAX_TICKET_ATTEMPTS {
        TicketState::Failed
    } else {
        TicketState::Queued
    };
    let status = TicketStatus {
        ticket_id: ticket_id.to_string(),
        state,
        attempts,
        last_error: Some(truncate(error, 1_000)),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    write_status(store, &status)?;
    Ok(status)
}

/// Outcome of [`cancel_ticket`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CancelOutcome {
    /// It was queued; it is now terminal and will never be picked up.
    Cancelled,
    /// It was already terminal (failed/cancelled) — idempotent no-op.
    AlreadyTerminal { state: TicketState },
    /// No such ticket.
    NotFound,
}

/// Cancel a queued ticket — a terminal state the executor never picks up.
///
/// Refused (as an error, not a silent no-op) when the ticket already has a
/// receipt: the build ran, and "cancelling" a finished build would only mean
/// lying about it. The *running* case is refused by the caller
/// ([`super::cancel_queued_ticket`]), which can see the executor slot: killing a
/// live cargo is `build abandon`'s job, and only after its process is dead.
pub(crate) fn cancel_ticket(
    store: &MemoryStore,
    ticket_id: &str,
    reason: &str,
) -> Result<CancelOutcome, String> {
    if load_ticket(store, ticket_id)?.is_none() {
        return Ok(CancelOutcome::NotFound);
    }
    if let Some(status) = load_status(store, ticket_id)? {
        if status.state.is_terminal() {
            return Ok(CancelOutcome::AlreadyTerminal {
                state: status.state,
            });
        }
    }
    let attempts = load_status(store, ticket_id)?
        .map(|s| s.attempts)
        .unwrap_or(0);
    write_status(
        store,
        &TicketStatus {
            ticket_id: ticket_id.to_string(),
            state: TicketState::Cancelled,
            attempts,
            last_error: Some(truncate(reason, 1_000)),
            updated_at: chrono::Utc::now().to_rfc3339(),
        },
    )?;
    Ok(CancelOutcome::Cancelled)
}

/// Every ticket that has reached a terminal state without a receipt — the dead
/// letters. `repo` filters to one repo identity when given.
pub(crate) fn terminal_tickets(
    store: &MemoryStore,
    repo: Option<&str>,
) -> Result<Vec<(BuildTicket, TicketStatus)>, String> {
    let mut out = Vec::new();
    for ticket in list_tickets(store)? {
        if !super::repo::same_repo(&ticket.source.repo_root, repo) {
            continue;
        }
        if let Some(status) = load_status(store, &ticket.ticket_id)? {
            if status.state.is_terminal() {
                out.push((ticket, status));
            }
        }
    }
    Ok(out)
}

fn truncate(raw: &str, max: usize) -> String {
    if raw.len() <= max {
        return raw.to_string();
    }
    let end = (0..=max)
        .rev()
        .find(|i| raw.is_char_boundary(*i))
        .unwrap_or(0);
    format!("{}…", &raw[..end])
}
