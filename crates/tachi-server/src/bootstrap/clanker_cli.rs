//! tachi#1735: sweep terminal Clanker run evidence
//! (`~/.cache/clanker/runs/<id>/`, machine-written by Clanker >= 0.4.7) into
//! the existing mirror-eval spine (#1066 tables: `mirror_eval_runs` /
//! `mirror_eval_observations`) so externally-dispatched lane runs
//! (codex/opencode/gemini/cursor relays) contribute carrier-observed quality
//! evidence to the #1675 ledger without a parallel store.
//!
//! ## Reuse, not reimplementation
//!
//! Every write goes through `memcore::register_mirror_eval_run` /
//! `memcore::record_mirror_eval_observation` — the SAME natural-key
//! idempotency and terminal-snapshot-conflict semantics the `tachi_agent_eval`
//! facade (`crate::agent_eval::mirror`) already relies on for host-native
//! subagents (#1066). This module owns exactly one thing those handlers
//! don't: turning a Clanker run directory's on-disk artifacts into the typed
//! `New*` structs, plus the terminal/live/malformed classification the
//! frozen contract requires before a row is ever attempted.
//!
//! ## Content-minimal by construction
//!
//! `result.md` carries free-text prose under `## final_message` / `## error`
//! headings. [`parse_result_frontmatter`] never reads past the first `## `
//! heading — the prose is never even extracted into a struct field, let
//! alone persisted. `events.jsonl` is counted (line count only), never
//! parsed for content. Every string field that IS persisted still runs
//! through the same secret-scrub `crate::agent_eval::mirror::handle_register`/
//! `handle_observe` already apply to caller-supplied text, for the same
//! defense-in-depth reason (a locally-sourced value — a cwd path, a model
//! id — can still coincidentally look secret-shaped).
//!
//! ## Terminal determination (leader-verified field intel, not spec text)
//!
//! A run is terminal IFF `result.md` exists AND its `- status:` bullet is
//! one of `done` / `error` / `killed`. Anything else — no `result.md` at
//! all (observed on this machine: a `rejected` telemetry-only run with zero
//! `result.md`), an unparseable frontmatter, or a non-terminal status value
//! — is treated as **live** and skipped, never guessed at. "Rather miss a
//! terminal run than fabricate one" per the dispatch packet's explicit
//! instruction.

use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tachi_bootstrap::cli::ClankerAction;

use memcore::{
    get_observation, get_run_by_native_child_id, record_mirror_eval_observation,
    register_mirror_eval_run, NewMirrorEvalObservation, NewMirrorEvalRun,
};

use super::open_cli_store;

/// `execution_origin` for every row this sweep registers — distinguishes
/// Clanker-relay evidence from the host-native subagent path
/// (`"host_native_subagent"`, #1066's own register callers).
const CLANKER_EXECUTION_ORIGIN: &str = "clanker_relay";

/// `frozen_contract_ref` constant. The sweep is a mechanical ingestion
/// pipeline, not a per-run frozen spec — Clanker's on-disk artifacts carry
/// no issue/contract reference to forward (see the source-facts key survey
/// in this module's tests), so every row cites the leaf that defined this
/// ingestion contract rather than fabricating a per-run one.
const CLANKER_FROZEN_CONTRACT_REF: &str = "kckylechen1/tachi#1735";

/// `lifecycle_owner`: Clanker (the relay daemon) owns spawn -> terminal for
/// these runs end to end; no host leader session supervises them live the
/// way a `tachi_agent_eval(action='register')` host-native caller does.
const CLANKER_LIFECYCLE_OWNER: &str = "clanker";

/// Natural-key namespace prefix. Contract: "natural key = clanker run id,
/// namespaced" — keeps a Clanker run id out of the bare `native_child_id`
/// namespace a host-native subagent caller might otherwise collide with.
const NATIVE_CHILD_ID_PREFIX: &str = "clanker:";

const SWEEP_REPORT_SCHEMA_VERSION: &str = "tachi.clanker_sweep.report.v1";

pub(super) async fn run_clanker_command(
    action: ClankerAction,
    global_db_path: &PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        ClankerAction::Sweep {
            runs_dir,
            limit,
            json,
        } => {
            let runs_dir = match runs_dir {
                Some(dir) => dir,
                None => default_runs_dir().ok_or(
                    "cannot determine default Clanker runs dir (pass --runs-dir; HOME unresolved)",
                )?,
            };
            let store = open_cli_store(global_db_path)?;
            let report = sweep_clanker_runs(store.connection(), &runs_dir, limit)
                .map_err(|e| format!("clanker sweep of {}: {e}", runs_dir.display()))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_sweep_summary(&report);
            }
            if report.needs_attention() {
                return Err(format!(
                    "clanker sweep: {} malformed run(s), {} conflicted run(s) — see detail above/in --json",
                    report.malformed_skipped, report.conflict_skipped
                )
                .into());
            }
            Ok(())
        }
    }
}

fn default_runs_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".cache").join("clanker").join("runs"))
}

// ─── report shape ───────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone, Serialize)]
struct ClankerSweepReport {
    pub schema_version: String,
    pub runs_dir: String,
    pub scanned: usize,
    /// Newly registered AND newly observed in this sweep.
    pub ingested_new: usize,
    /// Already registered by a prior sweep, but this sweep completed the
    /// missing observation (e.g. a prior sweep was interrupted mid-run).
    pub ingested_completed: usize,
    /// Already registered AND already observed — zero writes this sweep.
    pub duplicate: usize,
    pub live_skipped: usize,
    pub malformed_skipped: usize,
    pub conflict_skipped: usize,
    pub detail: Vec<SweepDetail>,
}

impl ClankerSweepReport {
    fn new(runs_dir: &Path) -> Self {
        Self {
            schema_version: SWEEP_REPORT_SCHEMA_VERSION.to_string(),
            runs_dir: runs_dir.display().to_string(),
            ..Default::default()
        }
    }

    /// True when the sweep hit something an operator should look at:
    /// malformed source facts or a genuine register/observe conflict. A
    /// live (still-running) run dir is normal operation, never this.
    fn needs_attention(&self) -> bool {
        self.malformed_skipped > 0 || self.conflict_skipped > 0
    }

    fn record(&mut self, run_id: String, outcome: RunOutcome) {
        match outcome {
            RunOutcome::IngestedNew => self.ingested_new += 1,
            RunOutcome::IngestedCompleted => self.ingested_completed += 1,
            RunOutcome::Duplicate => self.duplicate += 1,
            RunOutcome::Live(reason) => {
                self.live_skipped += 1;
                self.detail.push(SweepDetail::new(run_id, "live", reason));
            }
            RunOutcome::Malformed(reason) => {
                self.malformed_skipped += 1;
                self.detail
                    .push(SweepDetail::new(run_id, "malformed", reason));
            }
            RunOutcome::Conflict(reason) => {
                self.conflict_skipped += 1;
                self.detail
                    .push(SweepDetail::new(run_id, "conflict", reason));
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct SweepDetail {
    pub run_id: String,
    pub category: String,
    pub reason: String,
}

impl SweepDetail {
    fn new(run_id: String, category: &str, reason: String) -> Self {
        Self {
            run_id,
            category: category.to_string(),
            reason,
        }
    }
}

fn print_sweep_summary(report: &ClankerSweepReport) {
    println!("Clanker sweep: {}", report.runs_dir);
    println!(
        "  scanned={} ingested_new={} ingested_completed={} duplicate={} live_skipped={} malformed_skipped={} conflict_skipped={}",
        report.scanned,
        report.ingested_new,
        report.ingested_completed,
        report.duplicate,
        report.live_skipped,
        report.malformed_skipped,
        report.conflict_skipped,
    );
    for entry in &report.detail {
        if entry.category == "malformed" || entry.category == "conflict" {
            println!("  ! {} [{}] {}", entry.run_id, entry.category, entry.reason);
        }
    }
}

// ─── sweep core (pure-ish: one Connection, one directory tree) ─────────────

enum RunOutcome {
    IngestedNew,
    IngestedCompleted,
    Duplicate,
    Live(String),
    Malformed(String),
    Conflict(String),
}

fn sweep_clanker_runs(
    conn: &Connection,
    runs_dir: &Path,
    limit: Option<usize>,
) -> Result<ClankerSweepReport, Box<dyn std::error::Error>> {
    let mut report = ClankerSweepReport::new(runs_dir);

    let mut entries: Vec<PathBuf> = match fs::read_dir(runs_dir) {
        Ok(read_dir) => read_dir
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => {
            return Err(format!("cannot read runs dir {}: {err}", runs_dir.display()).into())
        }
    };
    entries.sort();
    if let Some(limit) = limit {
        entries.truncate(limit);
    }

    for dir in entries {
        report.scanned += 1;
        let run_id = match dir.file_name().and_then(|n| n.to_str()) {
            Some(name) => name.to_string(),
            None => {
                report.record(
                    dir.display().to_string(),
                    RunOutcome::Malformed("run directory name is not valid UTF-8".to_string()),
                );
                continue;
            }
        };
        let outcome =
            classify_and_ingest(conn, &dir, &run_id).map_err(|e| format!("run {run_id}: {e}"))?;
        report.record(run_id, outcome);
    }

    Ok(report)
}

fn classify_and_ingest(
    conn: &Connection,
    dir: &Path,
    run_id: &str,
) -> Result<RunOutcome, Box<dyn std::error::Error>> {
    let result_md_path = dir.join("result.md");
    let frontmatter = match fs::read_to_string(&result_md_path) {
        Ok(text) => parse_result_frontmatter(&text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // Leader-verified field intel: a terminal run CAN lack
            // result.md (an observed telemetry-only `rejected` run on this
            // machine). Uncertain terminal state -> live, never guessed.
            return Ok(RunOutcome::Live(
                "no result.md; uncertain terminal state, treated as live".to_string(),
            ));
        }
        Err(err) => {
            return Ok(RunOutcome::Malformed(format!(
                "result.md unreadable: {err}"
            )))
        }
    };

    let Some(status) = frontmatter.status.as_deref().map(str::to_lowercase) else {
        return Ok(RunOutcome::Malformed(
            "result.md has no '- status:' field".to_string(),
        ));
    };
    if !matches!(status.as_str(), "done" | "error" | "killed") {
        return Ok(RunOutcome::Live(format!(
            "result.md status '{status}' is not terminal"
        )));
    }

    let telemetry_path = dir.join("telemetry.json");
    let telemetry_text = match fs::read_to_string(&telemetry_path) {
        Ok(text) => text,
        Err(err) => {
            return Ok(RunOutcome::Malformed(format!(
                "telemetry.json unreadable: {err}"
            )))
        }
    };
    let telemetry: ClankerTelemetry = match serde_json::from_str(&telemetry_text) {
        Ok(value) => value,
        Err(err) => {
            return Ok(RunOutcome::Malformed(format!(
                "telemetry.json parse error: {err}"
            )))
        }
    };

    let native_child_id = scrub(format!("{NATIVE_CHILD_ID_PREFIX}{run_id}"));

    // Pre-fetched ONLY to categorize the report (new / completed / duplicate)
    // — never to decide whether to call `register_mirror_eval_run` /
    // `record_mirror_eval_observation` below. Both of those ALWAYS get
    // called, every sweep, so memcore's own natural-key idempotency-or-
    // explicit-conflict check (not a second, weaker copy of it here) is the
    // single authority on whether a row is a safe replay or a genuine
    // conflict — reuse, not reimplementation, per the frozen contract.
    let was_registered_before = get_run_by_native_child_id(conn, &native_child_id)
        .map_err(|e| format!("lookup existing run: {e}"))?
        .is_some();

    let new_run = build_new_run(&telemetry, &native_child_id);
    let run = match register_mirror_eval_run(conn, &new_run) {
        Ok(run) => run,
        Err(err) => return Ok(RunOutcome::Conflict(format!("register: {err}"))),
    };
    let eval_run_id = run.eval_run_id;

    let was_observed_before = get_observation(conn, &eval_run_id)
        .map_err(|e| format!("lookup existing observation: {e}"))?
        .is_some();

    let new_observation =
        build_new_observation(&telemetry, &frontmatter, &eval_run_id, &status, run_id, dir);
    match record_mirror_eval_observation(conn, &new_observation) {
        Ok(_) => Ok(match (was_registered_before, was_observed_before) {
            (false, _) => RunOutcome::IngestedNew,
            (true, false) => RunOutcome::IngestedCompleted,
            (true, true) => RunOutcome::Duplicate,
        }),
        Err(err) => Ok(RunOutcome::Conflict(format!("observe: {err}"))),
    }
}

fn build_new_run(telemetry: &ClankerTelemetry, native_child_id: &str) -> NewMirrorEvalRun {
    NewMirrorEvalRun {
        frozen_contract_ref: CLANKER_FROZEN_CONTRACT_REF.to_string(),
        execution_origin: CLANKER_EXECUTION_ORIGIN.to_string(),
        lifecycle_owner: CLANKER_LIFECYCLE_OWNER.to_string(),
        harness: scrub_opt(non_empty(telemetry.host.clone())),
        native_child_id: Some(native_child_id.to_string()),
        requested_profile: scrub_opt(non_empty(telemetry.profile_id.clone())),
        requested_model: scrub_opt(non_empty(telemetry.requested_model.clone())),
        requested_agent: scrub_opt(non_empty(
            telemetry
                .requested_lane
                .clone()
                .or_else(|| telemetry.lane.clone()),
        )),
    }
}

fn build_new_observation(
    telemetry: &ClankerTelemetry,
    frontmatter: &ResultFrontmatter,
    eval_run_id: &str,
    status_fallback: &str,
    run_id: &str,
    run_dir: &Path,
) -> NewMirrorEvalObservation {
    // Carrier terminal_reason IS the terminal_outcome this schema already
    // has a column for (#1066's `mirror_eval_observations.terminal_outcome`)
    // — no new column, reuse per the frozen contract's "do not re-implement".
    // Fall back to result.md's own status bullet only when telemetry omits
    // terminal_reason (never leave the required column empty).
    let terminal_outcome =
        non_empty(telemetry.terminal_reason.clone()).unwrap_or_else(|| status_fallback.to_string());

    let cost_tokens = telemetry
        .prompt_usage
        .as_ref()
        .and_then(|p| p.total_tokens)
        .or_else(|| telemetry.session_usage.as_ref().and_then(|s| s.used));

    let cost_usd = telemetry.session_usage.as_ref().and_then(|s| {
        s.cost.as_ref().and_then(|c| {
            let currency_is_usd = c
                .currency
                .as_deref()
                .map(|cur| cur.eq_ignore_ascii_case("usd"))
                .unwrap_or(true);
            if currency_is_usd {
                c.amount
            } else {
                None
            }
        })
    });

    let mut artifacts = Vec::new();
    if let Some(cwd) = non_empty(frontmatter.cwd.clone()) {
        artifacts.push(scrub(format!("cwd:{cwd}")));
    }
    if let Some(count) = count_jsonl_lines(&run_dir.join("events.jsonl")) {
        artifacts.push(format!("events:{count}_lines"));
    }
    // `touched_files` / `plan_final`: named in the source-facts contract as
    // "where present" — best-effort, counts/refs only, never seen on this
    // machine's real run dirs (surveyed as fixture reference), so these
    // branches are exercised only by synthetic test fixtures below.
    if let Some(count) = count_json_array_entries(&run_dir.join("touched_files.json")) {
        artifacts.push(format!("touched_files:{count}"));
    }
    if run_dir.join("plan_final.json").exists() || run_dir.join("plan_final.md").exists() {
        artifacts.push("plan_final:present".to_string());
    }

    NewMirrorEvalObservation {
        eval_run_id: eval_run_id.to_string(),
        terminal_outcome: scrub(terminal_outcome),
        duration_ms: telemetry.duration_ms,
        cost_tokens,
        cost_usd,
        // Opaque ref, never the host-local absolute filesystem path — a ref
        // per the "content-minimal: ids, metrics, enum states, digests,
        // refs" contract clause, not a copy of `result.md`'s own body.
        result_ref: Some(format!("clanker_run:{run_id}#result.md")),
        artifacts,
        effective_model: scrub_opt(non_empty(telemetry.observed_model.clone())),
        effective_backend: scrub_opt(non_empty(telemetry.backend.clone())),
        effective_harness: scrub_opt(non_empty(telemetry.transport.clone())),
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|v| {
        let trimmed = v.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

/// Same scrub `crate::agent_eval::mirror` applies to every caller-supplied
/// string on the tool-facing register/observe path (codex round-2 finding
/// #4d) — pure and deterministic, so a legitimate value scrubs to itself
/// every time and idempotent replay/lookup keys stay stable.
fn scrub(text: String) -> String {
    crate::memory_search_ops::scrub_secrets(&text).0
}

fn scrub_opt(text: Option<String>) -> Option<String> {
    text.map(scrub)
}

fn count_jsonl_lines(path: &Path) -> Option<u64> {
    let file = fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    Some(reader.lines().map_while(Result::ok).count() as u64)
}

fn count_json_array_entries(path: &Path) -> Option<u64> {
    let text = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    value.as_array().map(|arr| arr.len() as u64)
}

// ─── result.md frontmatter (bullet list ONLY — never body prose) ──────────

#[derive(Debug, Default, Clone)]
struct ResultFrontmatter {
    status: Option<String>,
    #[allow(dead_code)]
    // parsed for completeness/tests; not yet persisted separately from `lane` telemetry fields
    lane: Option<String>,
    cwd: Option<String>,
}

/// Parses ONLY the leading `- key: value` bullet list `result.md` writes
/// before its first `## ` heading (`status`/`lane`/`run_dir`/`cwd`). Stops
/// at the first `## ` heading unconditionally — the free-text body under
/// `## final_message` / `## error` / any review-table heading is never
/// read into a field, so it structurally cannot leak into a persisted row
/// (tachi#1735 discriminator 2).
fn parse_result_frontmatter(text: &str) -> ResultFrontmatter {
    let mut fm = ResultFrontmatter::default();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            break;
        }
        let Some(rest) = trimmed.strip_prefix("- ") else {
            continue;
        };
        let Some((key, value)) = rest.split_once(':') else {
            continue;
        };
        let value = value.trim().to_string();
        match key.trim() {
            "status" => fm.status = Some(value),
            "lane" => fm.lane = Some(value),
            "cwd" => fm.cwd = Some(value),
            _ => {}
        }
    }
    fm
}

// ─── telemetry.json (permissive: every field optional, type mismatch =
// malformed) ─────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone, Deserialize)]
struct ClankerTelemetry {
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    lane: Option<String>,
    #[serde(default)]
    requested_lane: Option<String>,
    #[serde(default)]
    profile_id: Option<String>,
    #[serde(default)]
    requested_model: Option<String>,
    #[serde(default)]
    observed_model: Option<String>,
    #[serde(default)]
    backend: Option<String>,
    #[serde(default)]
    transport: Option<String>,
    #[serde(default)]
    terminal_reason: Option<String>,
    #[serde(default)]
    duration_ms: Option<u64>,
    #[serde(default)]
    prompt_usage: Option<PromptUsage>,
    #[serde(default)]
    session_usage: Option<SessionUsage>,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct PromptUsage {
    #[serde(default, rename = "totalTokens")]
    total_tokens: Option<u64>,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct SessionUsage {
    #[serde(default)]
    used: Option<u64>,
    #[serde(default)]
    cost: Option<SessionCost>,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct SessionCost {
    #[serde(default)]
    amount: Option<f64>,
    #[serde(default)]
    currency: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use memcore::MemoryStore;

    fn test_store() -> MemoryStore {
        let db_path = crate::utils::test_fixture_path(format!(
            "clanker-sweep-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        MemoryStore::open(db_path.to_str().expect("utf8 fixture path")).expect("open test store")
    }

    fn fixture_runs_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn write_run(
        runs_dir: &Path,
        run_id: &str,
        result_md: Option<&str>,
        telemetry_json: Option<&str>,
        events_jsonl: Option<&str>,
    ) {
        let dir = runs_dir.join(run_id);
        fs::create_dir_all(&dir).expect("create run dir");
        if let Some(body) = result_md {
            fs::write(dir.join("result.md"), body).expect("write result.md");
        }
        if let Some(body) = telemetry_json {
            fs::write(dir.join("telemetry.json"), body).expect("write telemetry.json");
        }
        if let Some(body) = events_jsonl {
            fs::write(dir.join("events.jsonl"), body).expect("write events.jsonl");
        }
    }

    fn done_result_md(run_id: &str, lane: &str, cwd: &str) -> String {
        format!(
            "# clanker run {run_id}\n\n- status: done\n- lane: {lane}\n- run_dir: /whatever/{run_id}\n- cwd: {cwd}\n\n## final_message\n\nSECRET-MARKER-DO-NOT-PERSIST this is transcript prose.\n"
        )
    }

    fn done_telemetry(observed_model: &str) -> String {
        format!(
            r#"{{
                "host": "claude",
                "requested_lane": "codex",
                "lane": "codex",
                "backend": "codex",
                "transport": "acp-stdio",
                "profile_id": "codex-review",
                "requested_model": "gpt-5.5",
                "observed_model": "{observed_model}",
                "duration_ms": 199462,
                "terminal_reason": "done",
                "prompt_usage": {{"totalTokens": 90471}},
                "session_usage": {{"used": 90471, "cost": {{"amount": 0.42, "currency": "USD"}}}}
            }}"#
        )
    }

    // ── Discriminator 1: re-sweep idempotent ───────────────────────────

    #[test]
    fn resweep_is_idempotent_zero_duplicate_rows() {
        let store = test_store();
        let runs = fixture_runs_dir();
        write_run(
            runs.path(),
            "codex-aaa111",
            Some(&done_result_md("codex-aaa111", "codex", "/repo")),
            Some(&done_telemetry("openai/gpt-5.5")),
            Some("{\"event\":1}\n{\"event\":2}\n"),
        );

        let first = sweep_clanker_runs(store.connection(), runs.path(), None).expect("first sweep");
        assert_eq!(first.ingested_new, 1);
        assert_eq!(first.duplicate, 0);

        let second =
            sweep_clanker_runs(store.connection(), runs.path(), None).expect("second sweep");
        assert_eq!(second.ingested_new, 0, "re-sweep must insert zero new rows");
        assert_eq!(
            second.duplicate, 1,
            "re-sweep must report the run as all-duplicate"
        );

        let run_count: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM mirror_eval_runs", [], |r| r.get(0))
            .unwrap();
        let obs_count: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM mirror_eval_observations", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(run_count, 1, "exactly one run row after two sweeps");
        assert_eq!(obs_count, 1, "exactly one observation row after two sweeps");
    }

    // ── register conflict: memcore's own idempotency/conflict check is the
    // single authority — this module never pre-decides "already registered"
    // and skips calling it, so a genuine content drift under the same
    // natural key is caught, not silently absorbed. ─────────────────────

    #[test]
    fn register_conflict_is_loud_skip_not_silently_absorbed() {
        let store = test_store();
        let runs = fixture_runs_dir();

        // Seed a row under the SAME natural key this run would use, but
        // with different content (a different requested_profile) — the
        // kind of drift memcore's own natural-key check must catch.
        let native_child_id = "clanker:codex-conflict1".to_string();
        let conflicting = NewMirrorEvalRun {
            frozen_contract_ref: CLANKER_FROZEN_CONTRACT_REF.to_string(),
            execution_origin: CLANKER_EXECUTION_ORIGIN.to_string(),
            lifecycle_owner: CLANKER_LIFECYCLE_OWNER.to_string(),
            harness: Some("claude".to_string()),
            native_child_id: Some(native_child_id),
            requested_profile: Some("some-other-profile".to_string()),
            requested_model: None,
            requested_agent: None,
        };
        register_mirror_eval_run(store.connection(), &conflicting)
            .expect("seed conflicting registration");

        write_run(
            runs.path(),
            "codex-conflict1",
            Some(&done_result_md("codex-conflict1", "codex", "/repo")),
            // `done_telemetry`'s profile_id ("codex-review") differs from
            // the seeded "some-other-profile" -> a genuine content mismatch
            // under the same native_child_id.
            Some(&done_telemetry("openai/gpt-5.5")),
            None,
        );

        let report = sweep_clanker_runs(store.connection(), runs.path(), None).expect("sweep");
        assert_eq!(report.conflict_skipped, 1);
        assert_eq!(report.ingested_new, 0);
        assert!(
            report.needs_attention(),
            "a register conflict must make the sweep report partial"
        );
        assert!(report
            .detail
            .iter()
            .any(|d| d.run_id == "codex-conflict1" && d.category == "conflict"));

        let run_count: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM mirror_eval_runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            run_count, 1,
            "a rejected conflicting register must never mint a second row"
        );
    }

    // ── Discriminator 2: secret/content-negative ────────────────────────

    #[test]
    fn ingested_rows_never_carry_result_md_or_events_prose() {
        let store = test_store();
        let runs = fixture_runs_dir();
        let marker = "SECRET-MARKER-DO-NOT-PERSIST";
        write_run(
            runs.path(),
            "codex-bbb222",
            Some(&done_result_md("codex-bbb222", "codex", "/repo")),
            Some(&done_telemetry("openai/gpt-5.5")),
            Some(&format!("{{\"prompt\":\"{marker} in an event line\"}}\n")),
        );

        let report = sweep_clanker_runs(store.connection(), runs.path(), None).expect("sweep");
        assert_eq!(report.ingested_new, 1);

        let run_id: String = store
            .connection()
            .query_row("SELECT eval_run_id FROM mirror_eval_runs", [], |r| r.get(0))
            .unwrap();
        let view = memcore::get_mirror_eval_run_view(store.connection(), Some(&run_id), None)
            .unwrap()
            .expect("run view");
        let rendered = serde_json::to_string(&serde_json::json!({
            "run": format!("{:?}", view.run),
            "observation": format!("{:?}", view.observation),
        }))
        .unwrap();
        assert!(
            !rendered.contains(marker),
            "planted marker prose must never reach a persisted mirror-eval field: {rendered}"
        );
        assert!(
            !rendered.contains("final_message"),
            "result.md section headings must never leak into persisted fields"
        );
    }

    // ── Discriminator 3: killed/error lands with terminal_reason; live skipped ──

    #[test]
    fn error_run_lands_with_its_terminal_reason() {
        let store = test_store();
        let runs = fixture_runs_dir();
        let result_md = "# clanker run gemini-err1\n\n- status: error\n- lane: gemini\n- run_dir: /x\n- cwd: /repo\n\n## error\n\nsome connector error text\n";
        let telemetry = r#"{"host":"claude","lane":"gemini","backend":"gemini","transport":"acp-stdio","observed_model":"gemini-3.6-flash-high","terminal_reason":"error","duration_ms":1681}"#;
        write_run(
            runs.path(),
            "gemini-err1",
            Some(result_md),
            Some(telemetry),
            None,
        );

        let report = sweep_clanker_runs(store.connection(), runs.path(), None).expect("sweep");
        assert_eq!(report.ingested_new, 1);

        let terminal_outcome: String = store
            .connection()
            .query_row(
                "SELECT terminal_outcome FROM mirror_eval_observations",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(terminal_outcome, "error");
    }

    #[test]
    fn killed_run_lands_with_killed_terminal_reason() {
        let store = test_store();
        let runs = fixture_runs_dir();
        let result_md =
            "# clanker run codex-killed1\n\n- status: killed\n- lane: codex\n- run_dir: /x\n- cwd: /repo\n";
        let telemetry = r#"{"host":"claude","lane":"codex","backend":"codex","transport":"acp-stdio","observed_model":"openai/gpt-5.5","terminal_reason":"killed","duration_ms":500}"#;
        write_run(
            runs.path(),
            "codex-killed1",
            Some(result_md),
            Some(telemetry),
            None,
        );

        let report = sweep_clanker_runs(store.connection(), runs.path(), None).expect("sweep");
        assert_eq!(report.ingested_new, 1);
        let terminal_outcome: String = store
            .connection()
            .query_row(
                "SELECT terminal_outcome FROM mirror_eval_observations",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(terminal_outcome, "killed");
    }

    #[test]
    fn live_run_dir_is_skipped_and_counted_no_result_md() {
        let store = test_store();
        let runs = fixture_runs_dir();
        // Observed real shape: a terminal (rejected) telemetry-only run
        // with NO result.md at all. Per the explicit dispatch instruction,
        // absence of result.md is uncertain -> live, never guessed.
        write_run(
            runs.path(),
            "opencode-live1",
            None,
            Some(r#"{"host":"claude","lane":"opencode","terminal_reason":"rejected"}"#),
            None,
        );

        let report = sweep_clanker_runs(store.connection(), runs.path(), None).expect("sweep");
        assert_eq!(report.live_skipped, 1);
        assert_eq!(report.ingested_new, 0);
        assert_eq!(report.scanned, 1);
        assert!(!report.needs_attention(), "a live skip is not an error");

        let run_count: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM mirror_eval_runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(run_count, 0, "a live run must never mint a row");
    }

    #[test]
    fn live_run_dir_is_skipped_and_counted_non_terminal_status() {
        let store = test_store();
        let runs = fixture_runs_dir();
        let result_md =
            "# clanker run codex-running1\n\n- status: running\n- lane: codex\n- run_dir: /x\n- cwd: /repo\n";
        write_run(runs.path(), "codex-running1", Some(result_md), None, None);

        let report = sweep_clanker_runs(store.connection(), runs.path(), None).expect("sweep");
        assert_eq!(report.live_skipped, 1);
        assert!(!report.needs_attention());
    }

    // ── Discriminator 4: malformed telemetry.json -> loud skip, sweep continues ──

    #[test]
    fn malformed_telemetry_is_loud_skip_and_sweep_continues_with_partial_exit() {
        let store = test_store();
        let runs = fixture_runs_dir();
        write_run(
            runs.path(),
            "codex-bad1",
            Some(&done_result_md("codex-bad1", "codex", "/repo")),
            Some("{ this is not valid json"),
            None,
        );
        write_run(
            runs.path(),
            "codex-good1",
            Some(&done_result_md("codex-good1", "codex", "/repo")),
            Some(&done_telemetry("openai/gpt-5.5")),
            None,
        );

        let report = sweep_clanker_runs(store.connection(), runs.path(), None).expect("sweep");
        assert_eq!(report.scanned, 2);
        assert_eq!(
            report.malformed_skipped, 1,
            "malformed telemetry.json must be counted, not silently dropped"
        );
        assert_eq!(
            report.ingested_new, 1,
            "the well-formed sibling run must still be ingested"
        );
        assert!(
            report.needs_attention(),
            "a malformed run must make the sweep report partial"
        );
        assert!(report
            .detail
            .iter()
            .any(|d| d.run_id == "codex-bad1" && d.category == "malformed"));
    }

    #[test]
    fn missing_telemetry_on_a_terminal_run_is_malformed_not_silently_skipped() {
        let store = test_store();
        let runs = fixture_runs_dir();
        write_run(
            runs.path(),
            "codex-notelemetry",
            Some(&done_result_md("codex-notelemetry", "codex", "/repo")),
            None,
            None,
        );

        let report = sweep_clanker_runs(store.connection(), runs.path(), None).expect("sweep");
        assert_eq!(report.malformed_skipped, 1);
        assert!(report.needs_attention());
    }

    // ── Discriminator 5: observed_model provider/model shape; bare/missing -> unknown ──

    #[test]
    fn bare_observed_model_is_stored_verbatim_never_fabricated_and_resolves_unknown() {
        let store = test_store();
        let runs = fixture_runs_dir();
        // Real observed shape on this machine: no provider prefix at all.
        write_run(
            runs.path(),
            "gemini-bare1",
            Some(&done_result_md("gemini-bare1", "gemini", "/repo")),
            Some(&done_telemetry("gemini-3.6-flash-high")),
            None,
        );

        let report = sweep_clanker_runs(store.connection(), runs.path(), None).expect("sweep");
        assert_eq!(report.ingested_new, 1);

        let effective_model: String = store
            .connection()
            .query_row(
                "SELECT effective_model FROM mirror_eval_observations",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            effective_model, "gemini-3.6-flash-high",
            "bare model name persists verbatim, never gains a fabricated provider prefix"
        );
        assert_eq!(
            tachi_dispatch::model_lineage_id(Some(&effective_model), tachi_dispatch::UNKNOWN_IDENTITY),
            tachi_dispatch::UNKNOWN_IDENTITY,
            "a bare (no '/') model string must resolve to unknown identity downstream, never a guessed provider"
        );
    }

    #[test]
    fn missing_observed_model_ingests_with_unknown_identity_marking() {
        let store = test_store();
        let runs = fixture_runs_dir();
        let telemetry = r#"{"host":"claude","lane":"codex","backend":"codex","transport":"acp-stdio","terminal_reason":"done","duration_ms":100}"#;
        write_run(
            runs.path(),
            "codex-noident1",
            Some(&done_result_md("codex-noident1", "codex", "/repo")),
            Some(telemetry),
            None,
        );

        let report = sweep_clanker_runs(store.connection(), runs.path(), None).expect("sweep");
        assert_eq!(
            report.ingested_new, 1,
            "a missing observed_model must still ingest the row (never a skip)"
        );

        let effective_model: Option<String> = store
            .connection()
            .query_row(
                "SELECT effective_model FROM mirror_eval_observations",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(effective_model, None);
        assert_eq!(
            tachi_dispatch::model_lineage_id(
                effective_model.as_deref(),
                tachi_dispatch::UNKNOWN_IDENTITY
            ),
            tachi_dispatch::UNKNOWN_IDENTITY
        );
    }

    #[test]
    fn full_provider_model_shape_persists_verbatim() {
        let store = test_store();
        let runs = fixture_runs_dir();
        write_run(
            runs.path(),
            "opencode-full1",
            Some(&done_result_md("opencode-full1", "opencode", "/repo")),
            Some(&done_telemetry("anthropic/claude-haiku-4-5")),
            None,
        );

        let report = sweep_clanker_runs(store.connection(), runs.path(), None).expect("sweep");
        assert_eq!(report.ingested_new, 1);
        let effective_model: String = store
            .connection()
            .query_row(
                "SELECT effective_model FROM mirror_eval_observations",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(effective_model, "anthropic/claude-haiku-4-5");
        assert_ne!(
            tachi_dispatch::model_lineage_id(
                Some(&effective_model),
                tachi_dispatch::UNKNOWN_IDENTITY
            ),
            tachi_dispatch::UNKNOWN_IDENTITY
        );
    }

    // ── frontmatter parser: never reads past the first heading ─────────

    #[test]
    fn frontmatter_parser_stops_before_first_heading() {
        let text = "# clanker run x\n\n- status: done\n- lane: codex\n- cwd: /repo\n\n## final_message\n\n- status: LIES\nshould never be read\n";
        let fm = parse_result_frontmatter(text);
        assert_eq!(fm.status.as_deref(), Some("done"));
        assert_eq!(fm.lane.as_deref(), Some("codex"));
        assert_eq!(fm.cwd.as_deref(), Some("/repo"));
    }

    // ── best-effort optional artifacts (not present in real samples surveyed) ──

    #[test]
    fn touched_files_and_plan_final_are_best_effort_refs_when_present() {
        let store = test_store();
        let runs = fixture_runs_dir();
        let dir = runs.path().join("codex-artifacts1");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("result.md"),
            done_result_md("codex-artifacts1", "codex", "/repo"),
        )
        .unwrap();
        fs::write(dir.join("telemetry.json"), done_telemetry("openai/gpt-5.5")).unwrap();
        fs::write(
            dir.join("touched_files.json"),
            r#"["a.rs", "b.rs", "c.rs"]"#,
        )
        .unwrap();
        fs::write(
            dir.join("plan_final.json"),
            r#"{"plan": "irrelevant content"}"#,
        )
        .unwrap();

        let report = sweep_clanker_runs(store.connection(), runs.path(), None).expect("sweep");
        assert_eq!(report.ingested_new, 1);

        let artifacts_json: String = store
            .connection()
            .query_row("SELECT artifacts FROM mirror_eval_observations", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(artifacts_json.contains("touched_files:3"));
        assert!(artifacts_json.contains("plan_final:present"));
        assert!(
            !artifacts_json.contains("irrelevant content"),
            "plan_final content must never be persisted, only a presence ref"
        );
    }

    // ── limit ────────────────────────────────────────────────────────

    #[test]
    fn limit_caps_scanned_run_dirs_deterministically() {
        let store = test_store();
        let runs = fixture_runs_dir();
        write_run(
            runs.path(),
            "codex-a",
            Some(&done_result_md("codex-a", "codex", "/repo")),
            Some(&done_telemetry("openai/gpt-5.5")),
            None,
        );
        write_run(
            runs.path(),
            "codex-b",
            Some(&done_result_md("codex-b", "codex", "/repo")),
            Some(&done_telemetry("openai/gpt-5.5")),
            None,
        );

        let report = sweep_clanker_runs(store.connection(), runs.path(), Some(1)).expect("sweep");
        assert_eq!(report.scanned, 1);
    }

    // ── missing runs dir entirely: zero-scan, not an error ──────────────

    #[test]
    fn missing_runs_dir_is_zero_scan_not_an_error() {
        let store = test_store();
        let missing = fixture_runs_dir().path().join("does-not-exist");
        let report = sweep_clanker_runs(store.connection(), &missing, None).expect("sweep");
        assert_eq!(report.scanned, 0);
        assert!(!report.needs_attention());
    }
}
