//! Receipt-based Wiki/Guide delivery on the existing briefing surface.
//! Receipts describe content the caller still retains, never successful use.

use crate::copilot_ops::{guide_applies_to, rank_applicable_guides, GuideApplicabilityContext};
use crate::tool_params::{
    derive_effective_knowledge_artifact, SearchMemoryParams, StoreRef, TachiMemoryParams,
    WikiApplicabilityStatusV1, WikiReadPlan,
};
use crate::wiki_ops::{
    attach_wiki_provenance, knowledge_store_in_plan, read_knowledge_entry_for_plan,
    search_wiki_rows_for_plan,
};
use crate::MemoryServer;
use memcore::MemoryEntry;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

const MAX_RETAINED: usize = 64;
const MAX_REQUEST_BYTES: usize = 64 * 1024;
const CONTENT_BYTES: usize = 32 * 1024;
const SUMMARY_CHARS: usize = 600;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Unit {
    Summary,
    #[default]
    Body,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Version {
    store: StoreRef,
    id: String,
    unit: Unit,
    revision: i64,
    digest: String,
}

impl Version {
    fn key(&self) -> (StoreRef, String, Unit) {
        (self.store.clone(), self.id.clone(), self.unit)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    context_epoch: String,
    scope: String,
    entries: Vec<Version>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    context_epoch: String,
    #[serde(default)]
    unit: Unit,
    retained: Option<Manifest>,
    repo: Option<String>,
    task_type: Option<String>,
    profile: Option<String>,
    stage: Option<String>,
}

fn bounded_string(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn is_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

impl Request {
    pub(super) fn parse(params: &TachiMemoryParams) -> Result<Option<Self>, String> {
        let Some(value) = params
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("knowledge_context"))
        else {
            return Ok(None);
        };
        if serde_json::to_vec(value)
            .map_err(|error| error.to_string())?
            .len()
            > MAX_REQUEST_BYTES
        {
            return Err("knowledge_context exceeds 64 KiB".to_string());
        }
        let request: Self = serde_json::from_value(value.clone())
            .map_err(|error| format!("invalid knowledge_context: {error}"))?;
        if !bounded_string(&request.context_epoch, 128)
            || [
                &request.repo,
                &request.task_type,
                &request.profile,
                &request.stage,
            ]
            .into_iter()
            .flatten()
            .any(|value| !bounded_string(value, 256))
        {
            return Err("knowledge_context requires a nonempty epoch (max 128 bytes) and bounded applicability fields (max 256 bytes)".to_string());
        }
        if let Some(manifest) = &request.retained {
            if !bounded_string(&manifest.context_epoch, 128)
                || !is_digest(&manifest.scope)
                || manifest.entries.len() > MAX_RETAINED
            {
                return Err(
                    "invalid knowledge_context retained envelope (maximum 64 entries)".to_string(),
                );
            }
            let mut seen = HashSet::new();
            for version in &manifest.entries {
                if !bounded_string(&version.id, 256)
                    || version.revision < 0
                    || !is_digest(&version.digest)
                    || !seen.insert(version.key())
                    || matches!(&version.store, StoreRef::NamedProject { project } if !bounded_string(project, 256))
                {
                    return Err(
                        "invalid or duplicate knowledge_context retained version".to_string()
                    );
                }
            }
        }
        if params.include_archived
            || params.include_superseded
            || params.as_of.is_some()
            || params
                .scope
                .as_deref()
                .is_some_and(|scope| scope.eq_ignore_ascii_case("memory"))
        {
            return Err("knowledge_context requires current active Wiki/Guide content; historical flags and scope=memory are incompatible".to_string());
        }
        Ok(Some(request))
    }

    fn applicability<'a>(
        &'a self,
        params: &'a TachiMemoryParams,
        project: Option<&'a str>,
    ) -> GuideApplicabilityContext<'a> {
        GuideApplicabilityContext {
            project,
            repo: self.repo.as_deref(),
            domain: params.domain.as_deref(),
            task_type: self.task_type.as_deref(),
            profile: self.profile.as_deref(),
            stage: self.stage.as_deref(),
        }
    }
}

fn digest(value: &Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).expect("JSON value serializes"))
    )
}

fn scope(server: &MemoryServer, params: &TachiMemoryParams, project: Option<&str>) -> String {
    digest(&json!({
        "protocol": "wiki-guide-context-v1",
        "home": server.tachi_home_dir(),
        "global": server.global_db_path_buf(),
        "bound": server.project_db_path_buf(),
        "explicit_project": params.project,
        "effective_project": project,
        "role": params.agent_role.as_deref().map(str::trim).filter(|role| !role.is_empty()),
        "include_test": params.include_test,
    }))
}

pub(super) fn is_knowledge_path(path: &str) -> bool {
    ["/wiki", "/guide"].iter().any(|root| {
        path == *root
            || path
                .strip_prefix(root)
                .is_some_and(|tail| tail.starts_with('/'))
    })
}

fn payload(entry: &MemoryEntry, store: &StoreRef, unit: Unit) -> Value {
    let mut content = json!({
        "kind": if entry.is_guide() { "guide" } else { "wiki" },
        "path": entry.path,
        "topic": entry.topic,
        "summary": entry.summary.chars().take(SUMMARY_CHARS).collect::<String>(),
        "summary_truncated": entry.summary.chars().count() > SUMMARY_CHARS,
        "valid_from": entry.valid_from,
        "valid_until": entry.valid_until,
    });
    if unit == Unit::Body {
        content["text"] = json!(entry.text);
    }
    attach_wiki_provenance(&mut content, entry, store);
    content
}

fn version(entry: &MemoryEntry, store: &StoreRef, unit: Unit, content: &Value) -> Version {
    Version {
        store: store.clone(),
        id: entry.id.clone(),
        unit,
        revision: entry.revision,
        digest: digest(content),
    }
}

struct Reader<'a> {
    server: &'a MemoryServer,
    params: &'a TachiMemoryParams,
    applicability: GuideApplicabilityContext<'a>,
    wiki_plan: WikiReadPlan,
    guide_plan: WikiReadPlan,
    rules: Option<(String, Vec<(String, String)>)>,
    now: String,
}

impl Reader<'_> {
    fn read(&self, store: &StoreRef, id: &str) -> Result<Option<MemoryEntry>, String> {
        // GuideFederated admits legacy playbooks, but never legacy Wiki rows.
        let Some(entry) = read_knowledge_entry_for_plan(self.server, &self.guide_plan, store, id)?
        else {
            return Ok(None);
        };
        let guide_root = entry.path == "/guide" || entry.path.starts_with("/guide/");
        if !guide_root && !knowledge_store_in_plan(&self.wiki_plan, store) {
            return Ok(None);
        }
        let effective =
            derive_effective_knowledge_artifact(&entry.metadata, &entry.path, &entry.scope);
        let allowed = self.rules.as_ref().is_none_or(|(role, rules)| {
            memcore::db::evaluate_sandbox_access(rules, role, &entry.path, "read").0
        });
        if !is_knowledge_path(&entry.path)
            || memcore::is_test_memory_excluded(
                &entry,
                self.params.path_prefix.as_deref(),
                self.params.include_test,
            )
            || !memcore::db::is_user_facing_wiki_entry(&entry)
            || entry.path == "/wiki/drafts"
            || entry.path.starts_with("/wiki/drafts/")
            || entry.archived
            || !effective.lifecycle.is_default_retrievable()
            || effective.applicability_status == WikiApplicabilityStatusV1::Malformed
            || !allowed
            || !memcore::temporal_normalize::entry_valid_at(
                &entry.valid_from,
                entry.valid_until.as_deref(),
                &entry.timestamp,
                &self.now,
            )
            || ((entry.is_guide() || !effective.applies_to.is_empty())
                && !guide_applies_to(&entry, store, &self.applicability))
        {
            return Ok(None);
        }
        Ok(Some(entry))
    }

    fn read_selected(&self, store: &StoreRef, id: &str) -> Result<Option<MemoryEntry>, String> {
        self.read(store, id).map(|entry| {
            entry.filter(|entry| {
                self.params.path_prefix.as_deref().is_none_or(|prefix| {
                    entry.path == prefix
                        || entry
                            .path
                            .strip_prefix(prefix)
                            .is_some_and(|tail| tail.starts_with('/'))
                })
            })
        })
    }
}

/// Selection happens before delivery deduplication; retained entries are also
/// checked by exact identity, independently of the current query/top-k.
pub(super) async fn build(
    server: &MemoryServer,
    params: &TachiMemoryParams,
    request: &Request,
    project: Option<&str>,
    wiki_params: &SearchMemoryParams,
    wiki_rows: &[Value],
    mut warnings: Vec<String>,
) -> Result<Value, String> {
    let current_scope = scope(server, params, project);
    let reset_reason = match &request.retained {
        None => Some("no_manifest"),
        Some(held) if held.context_epoch != request.context_epoch => Some("context_epoch_changed"),
        Some(held) if held.scope != current_scope => Some("scope_changed"),
        Some(_) => None,
    };
    let wiki_plan = WikiReadPlan::from_project(params.project.as_deref())?;
    let guide_plan = if params.project.is_some() {
        wiki_plan.clone()
    } else {
        WikiReadPlan::GuideFederated
    };
    let rules = match params
        .agent_role
        .as_deref()
        .map(str::trim)
        .filter(|role| !role.is_empty())
    {
        Some(role) => Some((
            role.to_string(),
            server.with_global_store_read(|db| {
                db.list_sandbox_rules_for_role(role)
                    .map_err(|error| error.to_string())
            })?,
        )),
        None => None,
    };
    let reader = Reader {
        server,
        params,
        applicability: request.applicability(params, project),
        wiki_plan,
        guide_plan,
        rules,
        now: wiki_params
            .as_of
            .clone()
            .expect("knowledge retrieval has one current time"),
    };
    let mut selected = Vec::new();
    let mut seen = HashSet::new();
    let wiki_limit = params.top_k.clamp(1, if params.compact { 3 } else { 5 });
    for row in wiki_rows {
        let identity: Result<(StoreRef, String), _> =
            serde_json::from_value(json!([row["store"], row["id"]]));
        match identity {
            Ok((store, id)) => match reader.read_selected(&store, &id) {
                Ok(Some(entry))
                    if entry.path != "/guide"
                        && !entry.path.starts_with("/guide/")
                        && seen.insert((store.clone(), id.clone())) =>
                {
                    selected.push((entry, store));
                    if selected.len() >= wiki_limit {
                        break;
                    }
                }
                Ok(_) => {}
                Err(error) => warnings.push(format!("knowledge read unavailable: {error}")),
            },
            Err(error) => warnings.push(format!("invalid Wiki result identity: {error}")),
        }
    }

    // Use indexed task retrieval, not a timestamp-limited list of recent guides.
    // Applicability is applied to this bounded candidate pool before final top-k.
    if params
        .path_prefix
        .as_deref()
        .is_none_or(|prefix| prefix == "/guide" || prefix.starts_with("/guide/"))
    {
        let mut search = wiki_params.clone();
        search.path_prefix = Some(
            params
                .path_prefix
                .clone()
                .unwrap_or_else(|| "/guide".to_string()),
        );
        search.top_k = 50;
        search.agent_role = params.agent_role.clone();
        match search_wiki_rows_for_plan(server, search, &reader.guide_plan, None, false).await {
            Ok(result) => {
                let mut candidates = Vec::new();
                for row in result.rows {
                    let identity: Result<(StoreRef, String), _> =
                        serde_json::from_value(json!([row["store"], row["id"]]));
                    if let Ok((store, id)) = identity {
                        match reader.read_selected(&store, &id) {
                            Ok(Some(entry)) => candidates.push((entry, store)),
                            Ok(None) => {}
                            Err(error) => warnings.push(format!("Guide read unavailable: {error}")),
                        }
                    }
                }
                let limit = params.top_k.clamp(1, if params.compact { 3 } else { 5 });
                for (_, entry, store) in rank_applicable_guides(
                    candidates,
                    &reader.applicability,
                    &wiki_params.query,
                    limit,
                ) {
                    if seen.insert((store.clone(), entry.id.clone())) {
                        selected.push((entry, store));
                    }
                }
            }
            Err(error) => warnings.push(format!("Guide recall unavailable: {error}")),
        }
    }

    let mut confirmed = Vec::new();
    let mut withdrawn = Vec::new();
    let mut unverified = Vec::new();
    if let Some(held) = request.retained.as_ref().filter(|_| reset_reason.is_none()) {
        for previous in &held.entries {
            // Use one snapshot per selected entry for both validation and
            // delivery. A later read must not withdraw a version and then
            // send an older snapshot of that same entry in this packet.
            let current = selected
                .iter()
                .find(|(entry, store)| store == &previous.store && entry.id == previous.id)
                .map(|(entry, _)| Ok(Some(entry.clone())))
                .unwrap_or_else(|| reader.read(&previous.store, &previous.id));
            match current {
                Ok(Some(entry)) => {
                    let content = payload(&entry, &previous.store, previous.unit);
                    if version(&entry, &previous.store, previous.unit, &content) == *previous {
                        confirmed.push(previous.clone());
                    } else {
                        withdrawn.push(json!({"version": previous, "reason": "changed"}));
                    }
                }
                Ok(None) => {
                    withdrawn.push(json!({"version": previous, "reason": "not_retrievable"}))
                }
                Err(error) => unverified.push(json!({"version": previous, "reason": error})),
            }
        }
    }
    let mut units = Vec::new();
    let mut unchanged = Vec::new();
    let mut delivered = Vec::new();
    let mut body_deferred = Vec::new();
    let mut remaining = CONTENT_BYTES - 2; // JSON array delimiters.
    for (entry, store) in selected {
        let mut unit = request.unit;
        let mut content = payload(&entry, &store, unit);
        let mut current = version(&entry, &store, unit, &content);
        if confirmed.contains(&current) {
            unchanged.push(json!(current));
            continue;
        }
        let mut delivery = json!({"version": current, "content": content});
        let separator_bytes = usize::from(!units.is_empty());
        if delivery.to_string().len() + separator_bytes > remaining && unit == Unit::Body {
            body_deferred.push(json!({"store": store, "id": entry.id, "reason": "content_budget"}));
            unit = Unit::Summary;
            content = payload(&entry, &store, unit);
            current = version(&entry, &store, unit, &content);
            if confirmed.contains(&current) {
                unchanged.push(json!(current));
                continue;
            }
            delivery = json!({"version": current, "content": content});
        }
        let bytes = delivery.to_string().len() + separator_bytes;
        if bytes > remaining {
            warnings.push(format!(
                "knowledge unit deferred by content budget: {}",
                entry.id
            ));
            continue;
        }
        remaining -= bytes;
        delivered.push(current);
        units.push(delivery);
    }
    // Prefer freshly delivered units when the bounded receipt window is full.
    delivered.extend(confirmed);
    let mut manifest_keys = HashSet::new();
    delivered.retain(|item| manifest_keys.insert(item.key()));
    delivered.truncate(MAX_RETAINED);
    if scope(server, params, project) != current_scope {
        return Err("project binding changed during knowledge briefing; retry".to_string());
    }
    Ok(json!({
        "protocol": "wiki-guide-context-v1",
        "reset_reason": reset_reason,
        "units": units,
        "unchanged": unchanged,
        "withdrawn": withdrawn,
        "unverified": unverified,
        "body_deferred": body_deferred,
        "incomplete": !warnings.is_empty() || !unverified.is_empty() || !body_deferred.is_empty(),
        "warnings": warnings,
        "content_budget_bytes": CONTENT_BYTES,
        "manifest": Manifest { context_epoch: request.context_epoch.clone(), scope: current_scope, entries: delivered },
    }))
}
