//! Immutable build tickets (#894 S2c item 2).
//!
//! A ticket is the *request*: what source tree to build, with what command, and
//! who gets the result. It is written once and never rewritten — a mutable
//! ticket would let a caller change the source identity out from under a build
//! that is already running against a target dir chosen for the OLD identity,
//! which is precisely the cross-lineage poisoning the broker exists to prevent.
//!
//! Storage is the `hard_state` KV table (namespace [`TICKET_NS`], key =
//! ticket id) — the same store the sticky claim gate and the orchestrator use.
//! Immutability is enforced by writing with `insert_state_if_absent` (a single
//! `INSERT ... ON CONFLICT DO NOTHING`): the second submit of an id never
//! overwrites, and a submit whose payload differs from what is stored is a hard
//! error rather than a silent no-op.

use memcore::MemoryStore;
use serde::{Deserialize, Serialize};

/// `hard_state` namespace for tickets.
pub(crate) const TICKET_NS: &str = "build_ticket";

/// Which source tree a ticket wants built. This is the *identity* the target
/// generation is checked against — `head_sha` is the exact tree, `base_sha` is
/// what it claims to be branched from, and `repo_root` scopes both.
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
}

/// A git object id: hex, abbreviated (>=7) up to a full sha-256 (64).
fn is_object_id(raw: &str) -> bool {
    (7..=64).contains(&raw.len()) && raw.chars().all(|c| c.is_ascii_hexdigit())
}

/// Submit a ticket. Write-once: re-submitting the *identical* ticket is an
/// idempotent no-op (a retrying caller must not be punished), but re-submitting
/// a *different* payload under an existing id is a hard error — tickets are
/// immutable (#894 S2c item 2).
pub(crate) fn submit_ticket(store: &MemoryStore, ticket: &BuildTicket) -> Result<(), String> {
    let value = serde_json::to_string(ticket).map_err(|e| format!("serialize ticket: {e}"))?;
    let inserted = store
        .insert_state_if_absent(TICKET_NS, &ticket.ticket_id, &value)
        .map_err(|e| format!("submit ticket {}: {e}", ticket.ticket_id))?;
    if inserted {
        return Ok(());
    }
    match load_ticket(store, &ticket.ticket_id)? {
        Some(existing) if existing == *ticket => Ok(()),
        Some(_) => Err(format!(
            "build ticket '{}' already exists with a different payload: tickets are immutable \
             (a mutable ticket could re-point the source identity after the executor already \
             chose a target dir for the old one) — submit a new ticket id (#894 S2c)",
            ticket.ticket_id
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

/// Every ticket on this machine, oldest first — the queue order.
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
