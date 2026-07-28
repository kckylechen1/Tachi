//! `tachi cards sync` / `tachi cards list` (tachi#1202 Phase-1 / tachi#992).
//!
//! Mirrors leader-authored dispatch cards (`~/.agents/dispatch-ledger/cards/*.md`
//! — markdown with an optional typed frontmatter declaration, NOT tracked in
//! this repo) into read-only `/cards/<seat>` rows in the GLOBAL memory DB, so any
//! agent with a Tachi connection can look up a seat's playbook without
//! filesystem access to the leader's home directory.
//! Typed cards distinguish model, harness, seat, and crew identities. A crew is
//! a small agent team composed of multiple collaborating seats; it is mirrored
//! for routing context but never projected as a single-seat prompt overlay.
//!
//! Deliberately distinct from the singular `tachi card` command (`cards.rs`
//! in this same directory), which projects Tachikoma dispatch-profile cards
//! from `tachi_task(action='profiles')` — unrelated data, unrelated source of
//! truth. Do not conflate the two "cards" concepts.
//!
//! # Frozen interface contract (owner-ratified, tachi#1202, three points all A)
//!
//! - Mirror row: GLOBAL db, wiki-class entry, `path = /cards/<seat>` (seat =
//!   the card's filename minus extension, e.g. `glm-5.2`, `codex-gpt56-sol`).
//!   `metadata` carries `{source_file, source: "dispatch-ledger",
//!   content_hash}` (this module also adds `authority: "advisory"`, typed
//!   declaration fields `card_id`/`card_kind`/`card_status`/`card_aliases`,
//!   and `counter_clauses_present`/`counter_clauses`; all are additive fields
//!   the contract's `metadata 含 {...}` wording permits). The mirror path
//!   deliberately remains filename-stem based for frozen v1 compatibility.
//! - Content changed → `revision` increments. Unchanged → idempotent no-op
//!   (no write at all — the DB layer would happily bump `revision` on ANY
//!   upsert call regardless of whether content changed, so the "unchanged"
//!   case is decided here, before ever calling the write channel, by
//!   comparing this run's `content_hash` against the stored one).
//! - Source file disappears → mirror row's lifecycle goes `archived`, never
//!   deleted (`memcore::db::archive_memory`, reused via the `archive_memory`
//!   MCP tool/CLI channel — never a raw `DELETE`).
//! - Counter-clause extraction: a section whose HEADING matches
//!   `/反制|必带|Counter/i` (case-insensitive), including that heading's list
//!   items / body text. No matching heading → no clauses, full stop — this
//!   deliberately never falls back to scanning inline/non-heading prose (a
//!   card that only expresses its counter-clause as inline bold text inside
//!   an unrelated heading, e.g. `wizard-sonnet.md`, is a known, spec-mandated
//!   gap: "无匹配 section 则视为无条款(零注入,不拿别的凑)").
//!
//! # Write channel
//!
//! Every mutating write (create, update, archive) goes through the same
//! daemon-forward-else-in-process channel `tachi remember` uses, invoking the
//! already-daemon-recognized `save_memory` / `archive_memory` tool names.
//! Archive uses the channel's fixed `operate`-profile variant because the
//! low-level tool is deliberately absent from the standard facade tray; that
//! override is session-local and cannot be selected through CLI input. This is
//! never a bespoke `cards_sync` RPC (which no running daemon would recognize
//! and which the "refuse in-process fallback to avoid duplicate writes"
//! guard in `tool_dispatch.rs` would then hard-fail on whenever any daemon is
//! up). `project_db` is always `None` here (GLOBAL only) and `scope` is
//! always the literal string `"global"` — never left to `remember`'s
//! `scope="project"` default — so the #1041 S1 write-affinity gate never
//! reroutes a mirror row into a bound project store (the gate is a no-op
//! whenever `target_db == DbScope::Global`, which an explicit `scope=global`
//! always resolves to, project-DB-bound or not).
//!
//! The pre-write snapshot read (needed to diff `content_hash` and to know
//! each existing row's `id`/`revision` for updates and archival) is NOT part
//! of that contract — it is this module's own read-side implementation
//! detail — so it goes directly through `MemoryServer::with_global_store_read`
//! instead of round-tripping the `list_memories` tool's slimmed JSON shape
//! (which omits `revision` entirely; see `shared_defs::slim_entry`).
//!
//! Every mutating write's resulting `revision` is derived arithmetically
//! (`prior.revision + 1`) from the SQL upsert's own
//! `revision = memories.revision + 1` clause (`memcore::db::memory_crud`)
//! rather than re-read from the DB after the call — a fresh insert's
//! revision is always exactly 1 (no `ON CONFLICT` triggered), and both the
//! `save_memory` update path and `archive_memory` share that identical
//! increment-by-one semantic.

use regex::Regex;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tachi_bootstrap::cli::CardsAction;

use super::super::print_pretty_json;
use super::tool_dispatch::{
    dispatch_cli_operate_tool_with_migration_authority, dispatch_cli_tool_with_migration_authority,
};
use crate::memory_ops::handle_archive_memory;
use crate::memory_search_ops::handle_save_memory;
use crate::tool_params::{ArchiveMemoryParams, SaveMemoryParams};

const CARDS_MIRROR_PATH_PREFIX: &str = "/cards";
const CARDS_METADATA_SOURCE: &str = "dispatch-ledger";
const SUPPORTED_CARD_KINDS: [&str; 4] = ["model", "harness", "seat", "crew"];

pub(super) async fn run_cards_command(
    action: CardsAction,
    db_path: &PathBuf,
    app_home: &PathBuf,
    schema_migration: &memcore::MigrationAuthority,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        CardsAction::Draft { input } => super::cards_governance::draft_command(&input),
        CardsAction::Review { input } => super::cards_governance::review_command(&input),
        CardsAction::Approve { input, dir } => super::cards_governance::approve_command(
            &input,
            dir.as_deref().unwrap_or(&default_cards_dir()),
        ),
        CardsAction::Apply {
            approval,
            evidence,
            dir,
        } => {
            let dir = dir.unwrap_or_else(default_cards_dir);
            let outcome = super::cards_governance::apply_command(&approval, &evidence, &dir)?;
            // Source commit is complete before mirror sync. A failed mirror is
            // a structured pending outcome; replay will not append again.
            let completion = match sync_one_card(
                &dir,
                &outcome.seat,
                db_path,
                app_home,
                schema_migration,
            )
            .await
            {
                Ok(row) => {
                    let server =
                        match crate::cli_client::build_in_process_server_with_migration_authority(
                            db_path,
                            None,
                            schema_migration.clone(),
                        ) {
                            Ok(server) => server,
                            Err(error) => return mirror_pending(&outcome, error.to_string()),
                        };
                    let current = match read_card_file(&dir, &outcome.seat) {
                        Ok(file) => file,
                        Err(error) => return mirror_pending(&outcome, error.to_string()),
                    };
                    match crate::dispatch_ops::resolve_exact_seat_card_readiness(
                        &server,
                        &outcome.seat,
                    ) {
                        Some(readiness)
                            if current.hash == outcome.source_hash
                                && row.content_hash == outcome.source_hash
                                && readiness.source_hash == outcome.source_hash
                                && readiness.complete_projection =>
                        {
                            print_pretty_json(
                                &json!({"schema_version":"tachi.cards.apply.v1","source_status":outcome.source_status,"seat":outcome.seat,"source_hash":outcome.source_hash,"mirror_status":"synced","mirror_content_hash":row.content_hash,"projection_status":"ready","readiness_source_hash":readiness.source_hash,"mirror_revision":readiness.mirror_revision,"counter_clause_hash":readiness.counter_clause_hash}),
                            )
                        }
                        _ => {
                            return mirror_pending(
                                &outcome,
                                "mirror/readiness/canonical hash mismatch or incomplete projection"
                                    .into(),
                            )
                        }
                    }
                }
                Err(error) => return mirror_pending(&outcome, error.to_string()),
            };
            completion
        }
        CardsAction::Sync { dir, json } => {
            let dir = dir.unwrap_or_else(default_cards_dir);
            let rows = sync_cards(&dir, db_path, app_home, schema_migration).await?;
            if json {
                print_pretty_json(&sync_rows_json(&rows))
            } else {
                print_sync_table(&rows);
                Ok(())
            }
        }
        CardsAction::List { json } => {
            let rows = list_mirror_rows(db_path, schema_migration)?;
            if json {
                print_pretty_json(&list_rows_json(&rows))
            } else {
                print_list_table(&rows);
                Ok(())
            }
        }
    }
}

fn mirror_pending(
    outcome: &super::cards_governance::ApplySourceOutcome,
    error: String,
) -> Result<(), Box<dyn std::error::Error>> {
    print_pretty_json(
        &json!({"schema_version":"tachi.cards.apply.v1","status":"source_applied_mirror_pending","source_status":outcome.source_status,"seat":outcome.seat,"source_hash":outcome.source_hash,"mirror_status":"pending","projection_status":"pending","error":error}),
    )?;
    Err("source applied; mirror/projection pending".into())
}

fn default_cards_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".agents/dispatch-ledger/cards")
}

fn mirror_path_for_seat(seat: &str) -> String {
    format!("{CARDS_MIRROR_PATH_PREFIX}/{seat}")
}

fn seat_from_mirror_path(path: &str) -> Option<String> {
    path.strip_prefix(&format!("{CARDS_MIRROR_PATH_PREFIX}/"))
        .map(str::to_string)
}

/// Filename (minus `.md`) becomes the seat id. `README.md` is the ledger's
/// own meta-doc (see `~/.agents/dispatch-ledger/cards/README.md` — "回写纪律
/// / 条目模板 / 路由索引"), not a model/vendor seat card, so it is excluded
/// even though it syntactically matches `*.md`.
fn seat_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    if stem.eq_ignore_ascii_case("readme") {
        return None;
    }
    Some(stem.to_string())
}

fn content_hash_hex(bytes: &[u8]) -> String {
    use blake2::{Blake2s256, Digest};
    let mut hasher = Blake2s256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn counter_clause_heading_regex() -> Regex {
    // Frozen contract regex (tachi#1202): case-insensitive match against a
    // markdown heading's own text, never the section body.
    Regex::new(r"(?i)反制|必带|Counter").expect("static counter-clause regex must compile")
}

/// Markdown ATX heading level (`#`=1 .. `######`=6), or `None` if `line` is
/// not a heading (`#foo` with no space after the hashes doesn't count).
fn heading_level(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|&ch| ch == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &trimmed[level..];
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(level)
    } else {
        None
    }
}

/// Extract every section whose heading matches the counter-clause regex,
/// concatenated in document order (a card may accrete a second `## 反制条款
/// (追加)` section over time; both are real, both are kept — see
/// `gemini-3.5-flash.md`'s two-heading shape). A section runs from its
/// matching heading up to (excluding) the next heading of equal-or-shallower
/// level, so nested subsections stay attached to their parent.
///
/// Returns `None` when zero headings match — the frozen contract's explicit
/// "无匹配 section 则视为无条款(零注入,不拿别的凑)": this function never
/// scans non-heading prose (inline bold counter-clause text under an
/// unrelated heading, e.g. `wizard-sonnet.md`, is a known accepted gap, not
/// a bug this function should paper over).
pub(super) fn extract_counter_clauses(text: &str) -> Option<String> {
    let re = counter_clause_heading_regex();
    let lines: Vec<&str> = text.lines().collect();
    let mut sections: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let Some(level) = heading_level(lines[i]) else {
            i += 1;
            continue;
        };
        if !re.is_match(lines[i]) {
            i += 1;
            continue;
        }
        let mut body = vec![lines[i].to_string()];
        let mut j = i + 1;
        while j < lines.len() {
            if let Some(next_level) = heading_level(lines[j]) {
                if next_level <= level {
                    break;
                }
            }
            body.push(lines[j].to_string());
            j += 1;
        }
        sections.push(body.join("\n").trim_end().to_string());
        i = j;
    }
    if sections.is_empty() {
        None
    } else {
        Some(sections.join("\n\n"))
    }
}

struct CardFile {
    seat: String,
    path: PathBuf,
    text: String,
    hash: String,
    counter_clauses: Option<String>,
    declaration: CardDeclaration,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CardDeclaration {
    card_id: Option<String>,
    kind: Option<String>,
    status: Option<String>,
    aliases: Vec<String>,
}

fn frontmatter_scalar(raw: &str) -> String {
    raw.trim()
        .trim_matches(|ch| ch == '"' || ch == '\'')
        .to_string()
}

fn frontmatter_list(raw: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let raw = raw.trim();
    if !(raw.starts_with('[') && raw.ends_with(']')) {
        return Err("card frontmatter aliases must be an inline YAML list".into());
    }
    let inner = &raw[1..raw.len() - 1];
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }
    let aliases = inner.split(',').map(frontmatter_scalar).collect::<Vec<_>>();
    if aliases.iter().any(String::is_empty) {
        return Err("card frontmatter aliases contain an empty value".into());
    }
    Ok(aliases)
}

/// Parse the declaration fields Tachi needs without introducing a second
/// YAML implementation as a routing authority. Cards without frontmatter are
/// accepted as legacy seat cards; once a leading `---` is present, the typed
/// identity is strict and must be complete.
fn parse_card_declaration(text: &str) -> Result<CardDeclaration, Box<dyn std::error::Error>> {
    let mut lines = text.lines();
    if lines.next() != Some("---") {
        return Ok(CardDeclaration::default());
    }

    let mut fields = BTreeMap::new();
    let mut closed = false;
    for line in lines {
        if line == "---" {
            closed = true;
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let (key, value) = trimmed
            .split_once(':')
            .ok_or("malformed card frontmatter line")?;
        if fields
            .insert(key.trim().to_string(), value.trim().to_string())
            .is_some()
        {
            return Err(format!("duplicate card frontmatter field {}", key.trim()).into());
        }
    }
    if !closed {
        return Err("unterminated card frontmatter".into());
    }

    let card_id = frontmatter_scalar(
        fields
            .get("card_id")
            .ok_or("typed card frontmatter is missing card_id")?,
    );
    let kind = frontmatter_scalar(
        fields
            .get("kind")
            .ok_or("typed card frontmatter is missing kind")?,
    );
    let status = frontmatter_scalar(
        fields
            .get("status")
            .ok_or("typed card frontmatter is missing status")?,
    );
    if !SUPPORTED_CARD_KINDS.contains(&kind.as_str()) {
        return Err(format!("invalid card kind {kind}").into());
    }
    if !matches!(
        status.as_str(),
        "active" | "experimental" | "degraded" | "retired"
    ) {
        return Err(format!("invalid card status {status}").into());
    }
    if !card_id.starts_with(&format!("{kind}/")) {
        return Err(format!("card_id {card_id} does not match kind {kind}").into());
    }
    let aliases = match fields.get("aliases") {
        Some(raw) => frontmatter_list(raw)?,
        None => Vec::new(),
    };

    Ok(CardDeclaration {
        card_id: Some(card_id),
        kind: Some(kind),
        status: Some(status),
        aliases,
    })
}

fn declaration_metadata_matches(metadata: &Value, declaration: &CardDeclaration) -> bool {
    let aliases_match = metadata
        .get("card_aliases")
        .and_then(Value::as_array)
        .map(|values| {
            values.len() == declaration.aliases.len()
                && values
                    .iter()
                    .zip(&declaration.aliases)
                    .all(|(stored, expected)| stored.as_str() == Some(expected.as_str()))
        })
        .unwrap_or(false);
    metadata.get("card_id").and_then(Value::as_str) == declaration.card_id.as_deref()
        && metadata.get("card_kind").and_then(Value::as_str) == declaration.kind.as_deref()
        && metadata.get("card_status").and_then(Value::as_str) == declaration.status.as_deref()
        && aliases_match
}

fn read_card_file(dir: &Path, seat: &str) -> Result<CardFile, Box<dyn std::error::Error>> {
    // The seat was governance-validated before this boundary. Do not scan the
    // directory: unrelated malformed cards cannot affect targeted recovery.
    let path = dir.join(format!("{seat}.md"));
    let metadata = std::fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(format!(
            "target card {} must be a regular non-symlink file",
            path.display()
        )
        .into());
    }
    let bytes = std::fs::read(&path)?;
    let text = String::from_utf8(bytes.clone())
        .map_err(|_| format!("card {} is not strict UTF-8", path.display()))?;
    let declaration = parse_card_declaration(&text)
        .map_err(|e| format!("card {} has invalid frontmatter: {e}", path.display()))?;
    Ok(CardFile {
        seat: seat.to_string(),
        path,
        hash: content_hash_hex(&bytes),
        counter_clauses: extract_counter_clauses(&text),
        text,
        declaration,
    })
}

fn scan_card_files(dir: &Path) -> Result<Vec<CardFile>, Box<dyn std::error::Error>> {
    let read_dir =
        std::fs::read_dir(dir).map_err(|e| format!("read cards dir {}: {e}", dir.display()))?;
    let mut files = Vec::new();
    for entry in read_dir {
        let entry = entry?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let Some(seat) = seat_from_filename(&path) else {
            continue;
        };
        let bytes =
            std::fs::read(&path).map_err(|e| format!("read card file {}: {e}", path.display()))?;
        let text = String::from_utf8(bytes.clone())
            .map_err(|_| format!("card file {} is not strict UTF-8", path.display()))?;
        let hash = content_hash_hex(&bytes);
        let counter_clauses = extract_counter_clauses(&text);
        let declaration = parse_card_declaration(&text)
            .map_err(|e| format!("card file {} has invalid frontmatter: {e}", path.display()))?;
        files.push(CardFile {
            seat,
            path,
            text,
            hash,
            counter_clauses,
            declaration,
        });
    }
    files.sort_by(|a, b| a.seat.cmp(&b.seat));
    Ok(files)
}

/// Direct-store snapshot of every current `/cards/<seat>` mirror row
/// (archived included — needed to tell "already archived, still missing" from
/// "freshly archived this run" and to un-archive a row whose file reappears).
/// Keyed by seat; if duplicates ever exist for one seat (should not happen by
/// construction — `id` is stable per seat once created) the highest-revision
/// row wins.
fn read_existing_mirrors(
    db_path: &PathBuf,
    schema_migration: &memcore::MigrationAuthority,
) -> Result<BTreeMap<String, memcore::MemoryEntry>, Box<dyn std::error::Error>> {
    let server = crate::cli_client::build_in_process_server_with_migration_authority(
        db_path,
        None,
        schema_migration.clone(),
    )?;
    let entries: Vec<memcore::MemoryEntry> = server
        .with_global_store_read(|store| {
            store
                .list_by_path(CARDS_MIRROR_PATH_PREFIX, 1000, true)
                .map_err(|e| e.to_string())
        })
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let mut map: BTreeMap<String, memcore::MemoryEntry> = BTreeMap::new();
    for entry in entries {
        let Some(seat) = seat_from_mirror_path(&entry.path) else {
            continue;
        };
        let replace = map
            .get(&seat)
            .map(|existing| entry.revision > existing.revision)
            .unwrap_or(true);
        if replace {
            map.insert(seat, entry);
        }
    }
    Ok(map)
}

#[derive(Debug)]
pub(super) struct CardSyncRow {
    seat: String,
    status: &'static str,
    revision: i64,
    content_hash: String,
}

async fn sync_cards(
    dir: &Path,
    db_path: &PathBuf,
    app_home: &PathBuf,
    schema_migration: &memcore::MigrationAuthority,
) -> Result<Vec<CardSyncRow>, Box<dyn std::error::Error>> {
    sync_cards_selected(dir, None, true, db_path, app_home, schema_migration).await
}

/// Targeted post-apply mirror update. It intentionally does not run the
/// missing-file archival pass: applying one card cannot adjudicate unrelated
/// cards' lifecycle.
async fn sync_one_card(
    dir: &Path,
    seat: &str,
    db_path: &PathBuf,
    app_home: &PathBuf,
    schema_migration: &memcore::MigrationAuthority,
) -> Result<CardSyncRow, Box<dyn std::error::Error>> {
    let rows =
        sync_cards_selected(dir, Some(seat), false, db_path, app_home, schema_migration).await?;
    rows.into_iter()
        .find(|r| r.seat == seat)
        .ok_or_else(|| format!("target card {seat}.md was not mirrored").into())
}

async fn sync_cards_selected(
    dir: &Path,
    selected_seat: Option<&str>,
    archive_missing: bool,
    db_path: &PathBuf,
    app_home: &PathBuf,
    schema_migration: &memcore::MigrationAuthority,
) -> Result<Vec<CardSyncRow>, Box<dyn std::error::Error>> {
    let files = match selected_seat {
        Some(seat) => vec![read_card_file(dir, seat)?],
        None => scan_card_files(dir)?,
    };
    let mut existing = read_existing_mirrors(db_path, schema_migration)?;
    let mut rows = Vec::with_capacity(files.len());

    for scanned_file in &files {
        // Full sync cooperates with governed apply on the same per-card lock.
        // The pre-lock directory scan is discovery only: after acquiring the
        // lock, re-read the source and retain the lock through mirror write.
        // Targeted post-apply sync already runs while ApplySourceOutcome owns
        // this lock, so it must not reacquire it.
        let _source_lock = if selected_seat.is_none() {
            Some(super::cards_governance::lock(&scanned_file.path)?)
        } else {
            None
        };
        let locked_file = if _source_lock.is_some() {
            Some(read_card_file(dir, &scanned_file.seat)?)
        } else {
            None
        };
        let file = locked_file.as_ref().unwrap_or(scanned_file);
        let prior = existing.remove(&file.seat);

        if let Some(prior_entry) = &prior {
            let metadata = &prior_entry.metadata;
            let fully_unchanged = prior_entry
                .metadata
                .get("content_hash")
                .and_then(Value::as_str)
                == Some(file.hash.as_str())
                // save_memory's established text contract trims surrounding
                // whitespace; compare against that deterministic projection
                // while content_hash continues to receipt the exact bytes.
                && prior_entry.text == file.text.trim()
                && metadata.get("source").and_then(Value::as_str) == Some(CARDS_METADATA_SOURCE)
                && metadata.get("authority").and_then(Value::as_str) == Some("advisory")
                && metadata.get("source_file").and_then(Value::as_str)
                    == Some(file.path.to_string_lossy().as_ref())
                && declaration_metadata_matches(metadata, &file.declaration)
                && metadata
                    .get("counter_clauses_present")
                    .and_then(Value::as_bool)
                    == Some(file.counter_clauses.is_some())
                && metadata.get("counter_clauses").and_then(Value::as_str)
                    == file.counter_clauses.as_deref();
            if fully_unchanged && !prior_entry.archived {
                rows.push(CardSyncRow {
                    seat: file.seat.clone(),
                    status: "unchanged",
                    revision: prior_entry.revision,
                    content_hash: file.hash.clone(),
                });
                continue;
            }
        }

        let mut metadata = json!({
            "source_file": file.path.display().to_string(),
            "source": CARDS_METADATA_SOURCE,
            "content_hash": file.hash,
            "authority": "advisory",
            "card_id": file.declaration.card_id,
            "card_kind": file.declaration.kind,
            "card_status": file.declaration.status,
            "card_aliases": file.declaration.aliases,
            "counter_clauses_present": file.counter_clauses.is_some(),
        });
        // Always insert this key — even when there's no current clause —
        // rather than omitting it. Updates route through
        // `merge_patch_metadata` (entry.rs), which starts from the PRIOR
        // metadata object and only overwrites keys present in the incoming
        // one; omitting the key here would let a stale `counter_clauses`
        // string from an earlier revision survive under a now-false
        // `counter_clauses_present`. An explicit `Null` is a key that IS
        // present, so the merge overwrites the old value instead of
        // skipping it — the reader sees no clause text, not old text.
        if let Some(obj) = metadata.as_object_mut() {
            obj.insert(
                "counter_clauses".to_string(),
                match file.counter_clauses.as_ref() {
                    Some(clauses) => json!(clauses),
                    None => Value::Null,
                },
            );
        }

        let mut args = serde_json::Map::new();
        args.insert("text".into(), json!(file.text.clone()));
        args.insert("path".into(), json!(mirror_path_for_seat(&file.seat)));
        args.insert("scope".into(), json!("global"));
        args.insert("category".into(), json!("wiki"));
        args.insert("topic".into(), json!(file.seat.clone()));
        args.insert(
            "summary".into(),
            json!(format!("Dispatch ledger lane card: {}", file.seat)),
        );
        args.insert("force".into(), json!(true));
        args.insert("metadata".into(), metadata);
        if let Some(prior_entry) = &prior {
            args.insert("id".into(), json!(prior_entry.id.clone()));
        }

        dispatch_cli_tool_with_migration_authority(
            "save_memory",
            args,
            db_path,
            None,
            app_home,
            schema_migration,
            |server, args_map| {
                Box::pin(async move {
                    let params: SaveMemoryParams = serde_json::from_value(Value::Object(args_map))
                        .map_err(|e| format!("invalid save_memory args: {e}"))?;
                    handle_save_memory(&server, params).await
                })
            },
        )
        .await?;

        let revision = prior.as_ref().map(|entry| entry.revision + 1).unwrap_or(1);
        rows.push(CardSyncRow {
            seat: file.seat.clone(),
            status: if prior.is_some() {
                "updated"
            } else {
                "created"
            },
            revision,
            content_hash: file.hash.clone(),
        });
        if selected_seat.is_some() {
            let verified = read_existing_mirrors(db_path, schema_migration)?
                .remove(&file.seat)
                .ok_or("targeted mirror row missing after write")?;
            let m = &verified.metadata;
            if verified.archived
                || verified.revision != revision
                || verified.text != file.text.trim()
                || m.get("content_hash").and_then(Value::as_str) != Some(file.hash.as_str())
                || m.get("source").and_then(Value::as_str) != Some(CARDS_METADATA_SOURCE)
                || m.get("authority").and_then(Value::as_str) != Some("advisory")
                || m.get("source_file").and_then(Value::as_str)
                    != Some(file.path.to_string_lossy().as_ref())
                || !declaration_metadata_matches(m, &file.declaration)
                || m.get("counter_clauses_present").and_then(Value::as_bool)
                    != Some(file.counter_clauses.is_some())
                || m.get("counter_clauses").and_then(Value::as_str)
                    != file.counter_clauses.as_deref()
            {
                return Err("targeted mirror verification diverged".into());
            }
        }
    }

    // Everything left in `existing` has a mirror row but no file this run.
    // Archive it (never delete). A row that was ALREADY archived and is
    // still missing is left alone: `archive_memory` is a DB-level no-op on
    // an already-archived id, and re-reporting a stale transition every run
    // would be noise, not new information about THIS sync.
    if archive_missing {
        for (seat, entry) in existing {
            if entry.archived {
                continue;
            }
            let mut args = serde_json::Map::new();
            args.insert("id".into(), json!(entry.id.clone()));

            dispatch_cli_operate_tool_with_migration_authority(
                "archive_memory",
                args,
                db_path,
                None,
                app_home,
                schema_migration,
                |server, args_map| {
                    Box::pin(async move {
                        let params: ArchiveMemoryParams =
                            serde_json::from_value(Value::Object(args_map))
                                .map_err(|e| format!("invalid archive_memory args: {e}"))?;
                        handle_archive_memory(&server, params).await
                    })
                },
            )
            .await?;

            rows.push(CardSyncRow {
                seat,
                status: "archived",
                revision: entry.revision + 1,
                content_hash: entry
                    .metadata
                    .get("content_hash")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }

    rows.sort_by(|a, b| a.seat.cmp(&b.seat));
    Ok(rows)
}

fn sync_rows_json(rows: &[CardSyncRow]) -> Value {
    json!({
        "schema_version": "tachi.cards.sync.v1",
        "rows": rows
            .iter()
            .map(|row| json!({
                "seat": row.seat,
                "status": row.status,
                "revision": row.revision,
                "content_hash": row.content_hash,
            }))
            .collect::<Vec<_>>(),
    })
}

fn print_sync_table(rows: &[CardSyncRow]) {
    println!("Dispatch-Ledger Cards Sync");
    println!("{:<28} {:<10} {:<8}", "seat", "status", "revision");
    for row in rows {
        println!("{:<28} {:<10} {:<8}", row.seat, row.status, row.revision);
    }
}

struct MirrorListRow {
    seat: String,
    kind: String,
    revision: i64,
    // Mapped from `MemoryEntry::timestamp`: `handle_save_memory` stamps this
    // to `Utc::now()` on every actual write (create or update) and this
    // module never overrides it — so on a row that only ever changes via
    // this sync path, `timestamp` IS "when this mirror row was last
    // written", which is exactly "updated_at" for this table's purposes.
    updated_at: String,
    counter_clauses_present: bool,
    archived: bool,
}

fn list_mirror_rows(
    db_path: &PathBuf,
    schema_migration: &memcore::MigrationAuthority,
) -> Result<Vec<MirrorListRow>, Box<dyn std::error::Error>> {
    let existing = read_existing_mirrors(db_path, schema_migration)?;
    Ok(existing
        .into_iter()
        .map(|(seat, entry)| MirrorListRow {
            seat,
            kind: entry
                .metadata
                .get("card_kind")
                .and_then(Value::as_str)
                .unwrap_or("seat")
                .to_string(),
            revision: entry.revision,
            updated_at: entry.timestamp,
            counter_clauses_present: entry
                .metadata
                .get("counter_clauses_present")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            archived: entry.archived,
        })
        .collect())
}

fn list_rows_json(rows: &[MirrorListRow]) -> Value {
    json!({
        "schema_version": "tachi.cards.list.v1",
        "rows": rows
            .iter()
            .map(|row| json!({
                "seat": row.seat,
                "kind": row.kind,
                "revision": row.revision,
                "updated_at": row.updated_at,
                "counter_clauses_present": row.counter_clauses_present,
                "archived": row.archived,
            }))
            .collect::<Vec<_>>(),
    })
}

fn print_list_table(rows: &[MirrorListRow]) {
    println!("Dispatch-Ledger Cards (mirror rows)");
    println!(
        "{:<28} {:<9} {:<6} {:<9} {:<28}",
        "seat", "kind", "rev", "counter", "updated_at"
    );
    for row in rows {
        let counter = if row.counter_clauses_present {
            "yes"
        } else {
            "no"
        };
        let seat_label = if row.archived {
            format!("{} (archived)", row.seat)
        } else {
            row.seat.clone()
        };
        println!(
            "{:<28} {:<9} {:<6} {:<9} {:<28}",
            seat_label, row.kind, row.revision, counter, row.updated_at
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── extract_counter_clauses: pure-function fixtures mirroring real
    // ── ~/.agents/dispatch-ledger/cards/*.md shapes (tachi#1202 recon).

    #[test]
    fn extracts_single_matching_heading_with_list_items() {
        let text = "\
## 定位

Some prose.

## 反制条款(派工 prompt 必带)
- 锁定 commit SHA
- solo review, no delegation

## 证据 pin
unrelated section after
";
        let clauses = extract_counter_clauses(text).expect("should find a match");
        assert!(clauses.contains("反制条款"));
        assert!(clauses.contains("锁定 commit SHA"));
        assert!(clauses.contains("solo review, no delegation"));
        assert!(!clauses.contains("证据 pin"));
        assert!(!clauses.contains("unrelated section after"));
    }

    #[test]
    fn frontmatter_does_not_mask_counter_clause_sections() {
        let text = "\
---
card_id: harness/codex-collaboration
kind: harness
status: active
harness_id: codex
aliases: [codex-cli, codex-app-server]
---

# Codex collaboration harness

## 反制条款
- use the live protocol schema

## Evidence
- unrelated
";
        let clauses = extract_counter_clauses(text).expect("should find clauses after frontmatter");
        assert!(clauses.contains("use the live protocol schema"));
        assert!(!clauses.contains("card_id"));
        assert!(!clauses.contains("unrelated"));

        let declaration = parse_card_declaration(text).expect("typed declaration");
        assert_eq!(
            declaration.card_id.as_deref(),
            Some("harness/codex-collaboration")
        );
        assert_eq!(declaration.kind.as_deref(), Some("harness"));
        assert_eq!(declaration.status.as_deref(), Some("active"));
        assert_eq!(declaration.aliases, ["codex-cli", "codex-app-server"]);
    }

    #[test]
    fn concatenates_multiple_matching_headings_in_document_order() {
        // Mirrors gemini-3.5-flash.md's shape: two independent `反制条款`-ish
        // headings in one file. Contract doesn't pick "first match" —
        // concatenate both so later accretions aren't silently dropped.
        let text = "\
## 反制条款(派工 prompt 必须带)
- first clause

## unrelated middle section
- not a clause

## 反制条款(追加)
- second clause
";
        let clauses = extract_counter_clauses(text).expect("should find matches");
        assert!(clauses.contains("first clause"));
        assert!(clauses.contains("second clause"));
        assert!(!clauses.contains("not a clause"));
        // Document order: first section's text precedes second's.
        assert!(clauses.find("first clause") < clauses.find("second clause"));
    }

    #[test]
    fn matches_biguo_dai_without_requiring_the_bixudai_variant() {
        // composer-2.5.md-style: heading text is "必须带" (必+须+带), which
        // does NOT contain the contiguous substring "必带" — only the
        // "反制条款" alternative in the regex should save this heading.
        let text = "## 反制条款(派工 prompt 必须带)\n- clause\n";
        assert!(extract_counter_clauses(text).is_some());

        // A heading with NEITHER "反制" NOR contiguous "必带" NOR "Counter"
        // must not match at all.
        let no_match = "## 派工 prompt 必须带\n- clause\n";
        assert!(extract_counter_clauses(no_match).is_none());
    }

    #[test]
    fn returns_none_when_no_heading_matches_even_with_inline_bold_prose() {
        // wizard-sonnet.md-style: the ONLY counter-clause expression is
        // inline bold prose inside an unrelated dated heading, never its own
        // matching heading. Frozen contract: zero injection, not a scrounge.
        let text = "\
## 2026-07-13 全天复盘

**反制条款(派单包必带,即日生效)**:
- 动状态字段的单必须先枚举
";
        assert!(extract_counter_clauses(text).is_none());
    }

    #[test]
    fn returns_none_for_a_card_with_zero_counter_clause_mentions() {
        // grok-4.5.md-style null fixture.
        let text = "# grok-4.5\n\n## 状态:未校准(白卡)\n\n## 证据pin\n(待积累)\n";
        assert!(extract_counter_clauses(text).is_none());
    }

    #[test]
    fn nested_subheading_stays_attached_to_matching_parent_section() {
        let text = "\
## 反制条款(必带)
- top-level clause

### nested detail
- still part of the same section

## 下一节
- not part of it
";
        let clauses = extract_counter_clauses(text).expect("should match");
        assert!(clauses.contains("top-level clause"));
        assert!(clauses.contains("still part of the same section"));
        assert!(!clauses.contains("not part of it"));
    }

    #[test]
    fn seat_from_filename_excludes_readme_case_insensitively() {
        assert_eq!(
            seat_from_filename(Path::new("/x/glm-5.2.md")),
            Some("glm-5.2".to_string())
        );
        assert_eq!(
            seat_from_filename(Path::new("/x/codex-gpt56-sol.md")),
            Some("codex-gpt56-sol".to_string())
        );
        assert_eq!(seat_from_filename(Path::new("/x/README.md")), None);
        assert_eq!(seat_from_filename(Path::new("/x/readme.md")), None);
    }

    #[test]
    fn content_hash_is_stable_and_change_sensitive() {
        let a = content_hash_hex(b"hello world");
        let b = content_hash_hex(b"hello world");
        let c = content_hash_hex(b"hello world!");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn mirror_path_roundtrip() {
        assert_eq!(mirror_path_for_seat("glm-5.2"), "/cards/glm-5.2");
        assert_eq!(
            seat_from_mirror_path("/cards/glm-5.2"),
            Some("glm-5.2".to_string())
        );
        assert_eq!(seat_from_mirror_path("/notes/2026-07-17"), None);
    }

    // ── Integration: full sync pipeline against a tempdir fixture dir + a
    // ── tempdir GLOBAL db, exercising the judge-test transition matrix:
    // ── two cards → created ×2; rerun → unchanged (revision holds); edit one
    // ── → updated (revision+1); delete one → archived (row persists).

    fn write_fixture(dir: &Path, seat: &str, body: &str) {
        std::fs::write(dir.join(format!("{seat}.md")), body).expect("write fixture card");
    }

    fn app_home_and_db(temp: &Path) -> (PathBuf, PathBuf) {
        let app_home = temp.join("home");
        std::fs::create_dir_all(&app_home).expect("app_home dir");
        let db_path = app_home.join("global").join("memory.db");
        std::fs::create_dir_all(db_path.parent().expect("global dir")).expect("global dir");
        (app_home, db_path)
    }

    fn find_row<'a>(rows: &'a [CardSyncRow], seat: &str) -> &'a CardSyncRow {
        rows.iter()
            .find(|row| row.seat == seat)
            .unwrap_or_else(|| panic!("no row for seat {seat} in {rows:?}"))
    }

    #[tokio::test]
    async fn sync_persists_typed_declaration_metadata_idempotently() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (app_home, db_path) = app_home_and_db(temp.path());
        let cards_dir = temp.path().join("cards");
        std::fs::create_dir_all(&cards_dir).expect("cards dir");
        let schema_migration = memcore::MigrationAuthority::Deny;
        write_fixture(
            &cards_dir,
            "grok-cli",
            "---\ncard_id: harness/grok-cli\nkind: harness\nstatus: degraded\nharness_id: grok-cli\naliases: [grok-acp]\n---\n\n# Grok CLI\n\n## 反制条款\n- probe before dispatch\n",
        );

        let first = sync_cards(&cards_dir, &db_path, &app_home, &schema_migration)
            .await
            .expect("first typed sync");
        assert_eq!(find_row(&first, "grok-cli").status, "created");
        let mirrors = read_existing_mirrors(&db_path, &schema_migration).expect("read mirrors");
        let metadata = &mirrors["grok-cli"].metadata;
        assert_eq!(metadata["card_id"], json!("harness/grok-cli"));
        assert_eq!(metadata["card_kind"], json!("harness"));
        assert_eq!(metadata["card_status"], json!("degraded"));
        assert_eq!(metadata["card_aliases"], json!(["grok-acp"]));

        let second = sync_cards(&cards_dir, &db_path, &app_home, &schema_migration)
            .await
            .expect("second typed sync");
        assert_eq!(find_row(&second, "grok-cli").status, "unchanged");
        assert_eq!(find_row(&second, "grok-cli").revision, 1);
    }

    #[tokio::test]
    async fn sync_accepts_crew_as_a_typed_agent_team() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (app_home, db_path) = app_home_and_db(temp.path());
        let cards_dir = temp.path().join("cards");
        std::fs::create_dir_all(&cards_dir).expect("cards dir");
        let schema_migration = memcore::MigrationAuthority::Deny;
        write_fixture(
            &cards_dir,
            "kimi-crew",
            "---\ncard_id: crew/oc-kimi-crew\nkind: crew\nstatus: active\nrole: implementer-formation\nmodel_family: kimi-k3\nharness_id: clanker/opencode\n---\n\n# Kimi Crew\n\nA small agent team.\n",
        );

        let rows = sync_cards(&cards_dir, &db_path, &app_home, &schema_migration)
            .await
            .expect("crew cards are a supported typed identity");
        assert_eq!(find_row(&rows, "kimi-crew").status, "created");

        let mirrors = read_existing_mirrors(&db_path, &schema_migration).expect("read mirrors");
        let metadata = &mirrors["kimi-crew"].metadata;
        assert_eq!(metadata["card_id"], json!("crew/oc-kimi-crew"));
        assert_eq!(metadata["card_kind"], json!("crew"));
        assert_eq!(metadata["card_status"], json!("active"));

        let listed = list_mirror_rows(&db_path, &schema_migration).expect("list mirrors");
        let crew = listed
            .iter()
            .find(|row| row.seat == "kimi-crew")
            .expect("crew row is listed");
        assert_eq!(crew.kind, "crew");
        let listed_json = list_rows_json(&listed);
        assert_eq!(listed_json["rows"][0]["kind"], json!("crew"));
    }

    #[tokio::test]
    async fn sync_full_transition_matrix() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (app_home, db_path) = app_home_and_db(temp.path());
        let cards_dir = temp.path().join("cards");
        std::fs::create_dir_all(&cards_dir).expect("cards dir");
        let schema_migration = memcore::MigrationAuthority::Deny;

        write_fixture(
            &cards_dir,
            "wizard-sonnet",
            "## 反制条款(派单包必带)\n- always pwd-check the worktree\n",
        );
        write_fixture(&cards_dir, "grok-4.5", "## 状态:未校准(白卡)\n(待积累)\n");

        // 1) Two fixture cards → two `created` rows, revision 1 each.
        let rows = sync_cards(&cards_dir, &db_path, &app_home, &schema_migration)
            .await
            .expect("first sync");
        assert_eq!(rows.len(), 2, "{rows:?}");
        let wizard_row = find_row(&rows, "wizard-sonnet");
        assert_eq!(wizard_row.status, "created");
        assert_eq!(wizard_row.revision, 1);
        assert_eq!(
            wizard_row.content_hash,
            content_hash_hex(
                "## 反制条款(派单包必带)\n- always pwd-check the worktree\n".as_bytes()
            )
        );
        let grok_row = find_row(&rows, "grok-4.5");
        assert_eq!(grok_row.status, "created");
        assert_eq!(grok_row.revision, 1);

        // Mirror rows carry the counter-clause presence signal correctly.
        let mirrors = read_existing_mirrors(&db_path, &schema_migration).expect("read mirrors");
        assert!(
            mirrors["wizard-sonnet"]
                .metadata
                .get("counter_clauses_present")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            "wizard-sonnet has a matching '## 反制条款' heading"
        );
        assert!(
            !mirrors["grok-4.5"]
                .metadata
                .get("counter_clauses_present")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            "grok-4.5 fixture has no counter-clause heading at all"
        );

        // 2) Rerun with no file changes → both `unchanged`, revision holds.
        let rows2 = sync_cards(&cards_dir, &db_path, &app_home, &schema_migration)
            .await
            .expect("rerun sync");
        assert_eq!(find_row(&rows2, "wizard-sonnet").status, "unchanged");
        assert_eq!(find_row(&rows2, "wizard-sonnet").revision, 1);
        assert_eq!(find_row(&rows2, "grok-4.5").status, "unchanged");
        assert_eq!(find_row(&rows2, "grok-4.5").revision, 1);

        // 3) Edit one card's content → that row `updated` (revision+1); the
        // untouched sibling stays `unchanged` at its prior revision.
        write_fixture(
            &cards_dir,
            "wizard-sonnet",
            "## 反制条款(派单包必带)\n- always pwd-check the worktree\n- NEW: also verify db identity\n",
        );
        let rows3 = sync_cards(&cards_dir, &db_path, &app_home, &schema_migration)
            .await
            .expect("edit sync");
        let edited = find_row(&rows3, "wizard-sonnet");
        assert_eq!(edited.status, "updated");
        assert_eq!(edited.revision, 2);
        assert_eq!(find_row(&rows3, "grok-4.5").status, "unchanged");
        assert_eq!(find_row(&rows3, "grok-4.5").revision, 1);

        // 4) Delete one card's file → its mirror row is `archived`, not
        // removed: still present in the DB with archived=true. The untouched
        // sibling still reports (from the per-file loop) as `unchanged`.
        std::fs::remove_file(cards_dir.join("grok-4.5.md")).expect("remove fixture");
        let rows4 = sync_cards(&cards_dir, &db_path, &app_home, &schema_migration)
            .await
            .expect("archive sync");
        assert_eq!(rows4.len(), 2, "{rows4:?}");
        assert_eq!(find_row(&rows4, "wizard-sonnet").status, "unchanged");
        let grok_after = find_row(&rows4, "grok-4.5");
        assert_eq!(grok_after.status, "archived");
        assert_eq!(grok_after.revision, 2);

        let mirrors_after =
            read_existing_mirrors(&db_path, &schema_migration).expect("read mirrors after archive");
        let grok_entry = mirrors_after
            .get("grok-4.5")
            .expect("archived row must still exist, not be deleted");
        assert!(grok_entry.archived, "grok-4.5 mirror row must be archived");
        assert_eq!(grok_entry.revision, 2);

        // Rerun again with the file still absent: no repeat `archived`
        // report (already-settled state is not re-announced every run).
        let rows5 = sync_cards(&cards_dir, &db_path, &app_home, &schema_migration)
            .await
            .expect("second post-delete sync");
        assert!(
            rows5.iter().all(|row| row.seat != "grok-4.5"),
            "an already-archived, still-missing seat should not reappear in the report: {rows5:?}"
        );
    }

    // Regression for tachi#1202 CONCERN: `merge_patch_metadata` (entry.rs)
    // rebuilds an update's metadata from the PRIOR object and only
    // overwrites keys present in the incoming one. A card that drops its
    // counter-clause heading between syncs must not leave the OLD clause
    // text reachable under a now-false `counter_clauses_present` — the key
    // must be explicitly cleared (`Null`), not omitted.
    #[tokio::test]
    async fn sync_clears_stale_counter_clauses_on_removal() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (app_home, db_path) = app_home_and_db(temp.path());
        let cards_dir = temp.path().join("cards");
        std::fs::create_dir_all(&cards_dir).expect("cards dir");
        let schema_migration = memcore::MigrationAuthority::Deny;

        // 1) First sync: card has a matching counter-clause heading.
        write_fixture(
            &cards_dir,
            "wizard-sonnet",
            "## 反制条款(派单包必带)\n- always pwd-check the worktree\n",
        );
        let rows = sync_cards(&cards_dir, &db_path, &app_home, &schema_migration)
            .await
            .expect("first sync");
        assert_eq!(find_row(&rows, "wizard-sonnet").status, "created");

        let mirrors = read_existing_mirrors(&db_path, &schema_migration).expect("read mirrors");
        let metadata = &mirrors["wizard-sonnet"].metadata;
        assert_eq!(
            metadata
                .get("counter_clauses_present")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert!(
            metadata
                .get("counter_clauses")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .contains("pwd-check"),
            "first sync must record the clause text: {metadata:?}"
        );

        // 2) Second sync: same card, heading removed — this is an update
        // (is_patch=true in build_save_entry), the transition under test.
        write_fixture(
            &cards_dir,
            "wizard-sonnet",
            "## 状态\nno counter-clause heading here anymore\n",
        );
        let rows2 = sync_cards(&cards_dir, &db_path, &app_home, &schema_migration)
            .await
            .expect("removal sync");
        assert_eq!(find_row(&rows2, "wizard-sonnet").status, "updated");

        let mirrors2 = read_existing_mirrors(&db_path, &schema_migration).expect("read mirrors");
        let metadata2 = &mirrors2["wizard-sonnet"].metadata;
        assert_eq!(
            metadata2
                .get("counter_clauses_present")
                .and_then(Value::as_bool),
            Some(false),
            "presence flag must flip to false: {metadata2:?}"
        );
        assert!(
            !metadata2
                .get("counter_clauses")
                .map(|value| value.is_string())
                .unwrap_or(false),
            "stale clause text must not survive as a string value: {metadata2:?}"
        );
        let dump = metadata2.to_string();
        assert!(
            !dump.contains("pwd-check"),
            "old clause text must not be reachable anywhere in the mirrored metadata: {dump}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn targeted_sync_ignores_unrelated_malformed_and_symlink_cards() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let (app_home, db_path) = app_home_and_db(temp.path());
        let cards = temp.path().join("cards");
        std::fs::create_dir(&cards).unwrap();
        write_fixture(&cards, "target", "## Counter\n- exact clause\n");
        std::fs::write(cards.join("bad.md"), [0xff, 0xfe]).unwrap();
        symlink(cards.join("bad.md"), cards.join("linked.md")).unwrap();
        let row = sync_one_card(
            &cards,
            "target",
            &db_path,
            &app_home,
            &memcore::MigrationAuthority::Deny,
        )
        .await
        .unwrap();
        assert_eq!(row.seat, "target");
        assert_eq!(
            row.content_hash,
            content_hash_hex(b"## Counter\n- exact clause\n")
        );
        let mirrors = read_existing_mirrors(&db_path, &memcore::MigrationAuthority::Deny).unwrap();
        assert_eq!(
            mirrors.len(),
            1,
            "targeted sync must neither load nor archive unrelated seats"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn full_sync_honors_apply_lock_and_never_writes_its_unlocked_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let (app_home, db_path) = app_home_and_db(temp.path());
        let cards = temp.path().join("cards");
        std::fs::create_dir(&cards).unwrap();
        let card = cards.join("target.md");
        std::fs::write(&card, "## Counter\n- source A\n").unwrap();
        sync_cards(
            &cards,
            &db_path,
            &app_home,
            &memcore::MigrationAuthority::Deny,
        )
        .await
        .unwrap();

        std::fs::write(&card, "## Counter\n- source B\n").unwrap();
        let apply_lock = super::super::cards_governance::lock(&card).unwrap();
        assert!(
            sync_cards(
                &cards,
                &db_path,
                &app_home,
                &memcore::MigrationAuthority::Deny,
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("lock"),
            "full sync must not proceed from a pre-lock source snapshot"
        );
        let mirrors = read_existing_mirrors(&db_path, &memcore::MigrationAuthority::Deny).unwrap();
        assert!(mirrors["target"].text.contains("source A"));
        drop(apply_lock);

        sync_cards(
            &cards,
            &db_path,
            &app_home,
            &memcore::MigrationAuthority::Deny,
        )
        .await
        .unwrap();
        let mirrors = read_existing_mirrors(&db_path, &memcore::MigrationAuthority::Deny).unwrap();
        assert!(mirrors["target"].text.contains("source B"));
    }

    #[tokio::test]
    async fn governed_append_mirror_and_prompt_share_the_accepted_source_hash() {
        use tachi_params::{
            draft_lane_card, hash_bytes, hash_json, review_lane_card, ApprovalArtifact,
            DraftRequest, EvidenceRelation, EvidenceSnapshot, EvidenceState, LaneAuthority,
            ReviewDecision, ReviewRequest, TachiDispatchParams, GOVERNANCE_VERSION,
        };

        let temp = tempfile::tempdir().unwrap();
        let (app_home, db_path) = app_home_and_db(temp.path());
        let cards = temp.path().join("cards");
        std::fs::create_dir(&cards).unwrap();
        let source = b"# reviewed-seat\n";
        std::fs::write(cards.join("reviewed-seat.md"), source).unwrap();

        let evidence = vec![EvidenceSnapshot {
            id: "run-42".into(),
            subject_role: "reviewer".into(),
            subject_vendor: "openai".into(),
            subject_agent: Some("codex".into()),
            source_ref: "dispatch/run-42".into(),
            source_kind: "dispatch_run".into(),
            immutable_revision: "sha256:42".into(),
            assertion_hash: "blake2:42".into(),
            relation: EvidenceRelation::Supports,
            state: EvidenceState::Current,
        }];
        let draft = draft_lane_card(DraftRequest {
            schema_version: GOVERNANCE_VERSION.into(),
            seat: "reviewed-seat".into(),
            role: "reviewer".into(),
            vendor: "openai".into(),
            agent: Some("codex".into()),
            author: "author".into(),
            observed_at: "2026-07-19".into(),
            observed_failure_or_capability: "Caught a stale source.".into(),
            recurrence_context: "Repeated in two reviewed runs.".into(),
            counter_clause: "Recheck the canonical source hash before writing.".into(),
            authority: LaneAuthority::LaneOperationalEvidence,
            evidence: evidence.clone(),
        })
        .unwrap();
        let review = review_lane_card(ReviewRequest {
            schema_version: GOVERNANCE_VERSION.into(),
            draft: draft.clone(),
            reviewer: "independent-reviewer".into(),
            decision: ReviewDecision::Accepted,
            notes: "accepted".into(),
        })
        .unwrap();
        let append = draft.append_markdown.as_bytes().to_vec();
        let result = [source.as_slice(), append.as_slice()].concat();
        let mut approval = ApprovalArtifact {
            schema_version: GOVERNANCE_VERSION.into(),
            draft,
            review,
            leader: "leader".into(),
            decision: "approved".into(),
            source_hash: hash_bytes(source),
            source_byte_len: source.len() as u64,
            append_offset: source.len() as u64,
            append_bytes_hash: hash_bytes(&append),
            append_bytes: append,
            expected_result_hash: hash_bytes(&result),
            approval_hash: String::new(),
        };
        approval.approval_hash = hash_json(&approval).unwrap();
        let approval_path = temp.path().join("approval.json");
        let evidence_path = temp.path().join("evidence.json");
        std::fs::write(&approval_path, serde_json::to_vec(&approval).unwrap()).unwrap();
        std::fs::write(&evidence_path, serde_json::to_vec(&evidence).unwrap()).unwrap();

        let outcome =
            super::super::cards_governance::apply_command(&approval_path, &evidence_path, &cards)
                .unwrap();
        let row = sync_one_card(
            &cards,
            "reviewed-seat",
            &db_path,
            &app_home,
            &memcore::MigrationAuthority::Deny,
        )
        .await
        .unwrap();
        let server = crate::cli_client::build_in_process_server_with_migration_authority(
            &db_path,
            None,
            memcore::MigrationAuthority::Deny,
        )
        .unwrap();
        let readiness =
            crate::dispatch_ops::resolve_exact_seat_card_readiness(&server, "reviewed-seat")
                .unwrap();
        assert_eq!(outcome.source_hash, hash_bytes(&result));
        assert_eq!(row.content_hash, outcome.source_hash);
        assert_eq!(readiness.source_hash, outcome.source_hash);
        assert!(readiness.complete_projection);

        let params: TachiDispatchParams = serde_json::from_value(json!({
            "agent": "codex",
            "profile": "reviewed-seat",
            "task": "review the bounded change",
            "auto_capability_bundle": false
        }))
        .unwrap();
        let prompt = crate::dispatch_ops::assemble_prompt(&server, &params).await;
        assert!(
            prompt.contains("Recheck the canonical source hash before writing."),
            "accepted clause must project from the mirror into the prompt: {prompt}"
        );
    }

    #[test]
    fn scan_card_files_excludes_readme_and_non_markdown() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_fixture(temp.path(), "glm-5.2", "## 反制条款\n- clause\n");
        std::fs::write(
            temp.path().join("README.md"),
            "# Dispatch Ledger — Lane Cards\n",
        )
        .expect("write readme");
        std::fs::write(temp.path().join("ledger.jsonl"), "{}\n").expect("write non-md file");

        let files = scan_card_files(temp.path()).expect("scan");
        let seats: Vec<&str> = files.iter().map(|f| f.seat.as_str()).collect();
        assert_eq!(seats, vec!["glm-5.2"]);
    }
}
