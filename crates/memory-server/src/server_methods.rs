use super::*;

pub(super) struct ResolvedCallTarget {
    pub requested_id: String,
    pub resolved_id: String,
    pub requested_kind: String,
    pub resolution: Value,
}

impl MemoryServer {
    fn reserve_rate_limiter_entry_capacity(
        map: &mut HashMap<String, VecDeque<Instant>>,
        max_entries: usize,
        stale_cutoff: Instant,
        new_key: &str,
    ) {
        if max_entries == 0 || map.contains_key(new_key) {
            return;
        }

        if map.len() >= max_entries {
            map.retain(|_, deque| deque.back().is_some_and(|&t| t >= stale_cutoff));
        }

        while map.len() >= max_entries {
            let Some(oldest_key) = map
                .iter()
                .min_by_key(|(_, deque)| deque.back().copied().unwrap_or(stale_cutoff))
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            map.remove(&oldest_key);
        }
    }

    pub(super) fn set_tool_profile(&self, profile: Option<ToolProfile>) {
        self.agent_runtime_write().tool_profile = profile;
    }

    pub(super) fn active_tool_profile(&self) -> Option<ToolProfile> {
        self.agent_runtime_read().tool_profile
    }

    pub(crate) fn native_tool_visibility(&self) -> Vec<(String, String, bool)> {
        let env_patterns = std::env::var("TACHI_EXPOSED_TOOLS")
            .ok()
            .map(|raw| crate::profiles::parse_tool_patterns_csv(&raw))
            .filter(|patterns| !patterns.is_empty());
        let profile = self.active_tool_profile();
        let mut tools = self
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| {
                let name = tool.name.as_ref().to_string();
                let description = tool
                    .description
                    .as_ref()
                    .map(|text| crate::utils::compact_text_line(text.as_ref(), 96))
                    .unwrap_or_default();
                let visible =
                    crate::profiles::tool_visible(&name, profile, env_patterns.as_deref());
                (name, description, visible)
            })
            .collect::<Vec<_>>();
        tools.sort_by(|a, b| a.0.cmp(&b.0));
        tools
    }

    #[cfg(test)]
    pub(crate) fn rate_limiter_entry_counts_for_tests(&self) -> (usize, usize) {
        let rl = self.rate_limiter_lock();
        (rl.windows.len(), rl.bursts.len())
    }

    pub(super) fn enqueue_foundry_job(&self, item: FoundryMaintenanceItem) -> Result<(), String> {
        // Persist to DB first so the job survives process exit
        let persisted = memory_core::PersistedFoundryJob {
            spec: item.job.clone(),
            target_db: item.target_db.as_str().to_string(),
            named_project: item.named_project.clone(),
            path_prefix: item.path_prefix.clone(),
            memory_ids: item.memory_ids.clone(),
        };
        let persist_result = if let Some(ref db_path) = item.db_path {
            self.with_path_store(db_path, |store| {
                memory_core::insert_foundry_job(store.connection(), &persisted)
                    .map_err(|e| format!("persist foundry job: {e}"))
            })
        } else if let Some(ref project_name) = item.named_project {
            self.with_named_project_store(project_name, |store| {
                memory_core::insert_foundry_job(store.connection(), &persisted)
                    .map_err(|e| format!("persist foundry job: {e}"))
            })
        } else {
            self.with_store_for_scope(item.target_db, |store| {
                memory_core::insert_foundry_job(store.connection(), &persisted)
                    .map_err(|e| format!("persist foundry job: {e}"))
            })
        };
        if let Err(err) = persist_result {
            eprintln!("[foundry] failed to persist job {}: {err}", item.job.id);
        }

        self.foundry_lock()
            .foundry_stats
            .queued
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if self.foundry_lock().foundry_tx.try_send(item).is_err() {
            self.foundry_lock()
                .foundry_stats
                .queued
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            return Err("foundry maintenance worker unavailable".to_string());
        }
        Ok(())
    }

    /// Check if a project DB is available (static startup or hot-swapped).
    pub(super) fn has_project_db(&self) -> bool {
        if self.project_db_path.is_some() {
            return true;
        }
        // Check hot-swapped project DB
        self.hot_project_db
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// Hot-activate a project database on a running server.
    /// Called by `tachi_init_project_db` to make the created DB immediately usable.
    pub(super) fn activate_project_db(&self, db_path: PathBuf) -> Result<bool, String> {
        let db_str = db_path.to_str().ok_or_else(|| {
            format!(
                "Project DB path contains invalid UTF-8: {}",
                db_path.display()
            )
        })?;
        let project_label = db_path
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|os| os.to_str())
            .unwrap_or("project")
            .to_string();
        let store = MemoryStore::open_with_label(db_str, &project_label)
            .map_err(|e| format!("open project db: {e}"))?;
        let read_pool = ReadStorePool::open_read_only(db_str, configured_memory_read_pool_size())
            .map_err(|e| format!("open project read db: {e}"))?;
        let state = ProjectDbState {
            store: Arc::new(StdMutex::new(store)),
            read_pool,
            rw_gate: Arc::new(StdRwLock::new(())),
            db_path: Arc::new(db_path),
        };

        let mut guard = self
            .hot_project_db
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let was_none = guard.is_none();
        *guard = Some(state);
        Ok(was_none)
    }

    /// Run a write closure against the hot-swapped project DB.
    pub(super) fn with_hot_project_store<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let guard = self
            .hot_project_db
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let state = guard
            .as_ref()
            .ok_or_else(|| "No hot-swapped project database available".to_string())?;
        let _gate = write_or_recover(&state.rw_gate, "hot_project_rw_gate");
        let mut store = lock_or_recover(&state.store, "hot_project_store");
        f(&mut store)
    }

    /// Run a read closure against the hot-swapped project DB.
    pub(super) fn with_hot_project_store_read<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let guard = self
            .hot_project_db
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let state = guard
            .as_ref()
            .ok_or_else(|| "No hot-swapped project database available".to_string())?;
        let _gate = read_or_recover(&state.rw_gate, "hot_project_rw_gate");
        state.read_pool.with_store("hot_project_read_pool", f)
    }

    fn open_read_store(db_path: &PathBuf, label: &str) -> Result<MemoryStore, String> {
        let db_str = db_path.to_str().ok_or_else(|| {
            format!(
                "{} DB path contains invalid UTF-8: {}",
                label,
                db_path.display()
            )
        })?;
        MemoryStore::open_read_only(db_str).map_err(|e| format!("open {label} read store: {e}"))
    }

    pub(super) fn with_path_store<T>(
        &self,
        db_path: &PathBuf,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let db_str = db_path
            .to_str()
            .ok_or_else(|| format!("DB path contains invalid UTF-8: {}", db_path.display()))?;
        let label = db_path
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|os| os.to_str())
            .unwrap_or("path");
        let _gate = write_or_recover(&self.global_rw_gate, "path_db_rw_gate");
        let mut store = MemoryStore::open_with_label(db_str, label)
            .map_err(|e| format!("open path store {}: {e}", db_path.display()))?;
        f(&mut store)
    }

    pub(super) fn with_path_store_read<T>(
        &self,
        db_path: &PathBuf,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let _gate = read_or_recover(&self.global_rw_gate, "path_db_rw_gate");
        let mut store = Self::open_read_store(db_path, "path")?;
        f(&mut store)
    }

    pub(super) fn with_global_store<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let _gate = write_or_recover(&self.global_rw_gate, "global_rw_gate");
        let mut store = lock_or_recover(&self.global_store, "global_store");
        f(&mut store)
    }

    pub(super) fn with_global_store_read<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let _gate = read_or_recover(&self.global_rw_gate, "global_rw_gate");
        self.global_read_pool.with_store("global_read_pool", f)
    }

    /// Stamp "now" as the last MCP activity. Lock-free; called centrally from
    /// `ServerHandler::call_tool` on every tool invocation (including calls a
    /// stdio child forwards to this daemon).
    pub(crate) fn touch_activity(&self) {
        self.last_activity_ms.store(
            chrono::Utc::now().timestamp_millis(),
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    /// Shared handle to the last-activity clock, for the serve-loop idle reaper.
    pub(crate) fn activity_clock(&self) -> Arc<std::sync::atomic::AtomicI64> {
        Arc::clone(&self.last_activity_ms)
    }

    pub(super) fn with_project_store<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        // Hot-activated DB (e.g. tachi_init_project_db) overrides the boot-time store.
        if self
            .hot_project_db
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return self.with_hot_project_store(f);
        }
        if let Some(ref store_arc) = self.project_store {
            let gate = self
                .project_rw_gate
                .as_ref()
                .ok_or_else(|| "No project lock available".to_string())?;
            let _gate = write_or_recover(gate, "project_rw_gate");
            let mut store = lock_or_recover(store_arc, "project_store");
            return f(&mut store);
        }
        Err("No project database available".to_string())
    }

    pub(super) fn with_project_store_read<T>(
        &self,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        if self
            .hot_project_db
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return self.with_hot_project_store_read(f);
        }
        if let Some(ref read_pool) = self.project_read_pool {
            let gate = self
                .project_rw_gate
                .as_ref()
                .ok_or_else(|| "No project lock available".to_string())?;
            let _gate = read_or_recover(gate, "project_rw_gate");
            return read_pool.with_store("project_read_pool", f);
        }
        Err("No project database available".to_string())
    }

    pub(super) fn with_store_for_scope<T>(
        &self,
        scope: DbScope,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        match scope {
            DbScope::Global => self.with_global_store(f),
            DbScope::Project => self.with_project_store(f),
        }
    }

    pub(super) fn with_store_for_scope_read<T>(
        &self,
        scope: DbScope,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        match scope {
            DbScope::Global => self.with_global_store_read(f),
            DbScope::Project => self.with_project_store_read(f),
        }
    }

    /// Addressing convention for named-project recall:
    ///   * `<repo>/.tachi/memory.db` is the per-repo source of truth (real data).
    ///   * `~/.tachi/global/memory.db` is the machine-global store.
    ///   * `~/.tachi/projects/<name>/` is an addressing alias, not a data store.
    ///
    /// Resolution prefers the manifest-recorded repo-local path for a project so
    /// it does NOT depend on the Plan C symlink existing (which is Unix-only and
    /// can be stale). It falls back to the `~/.tachi/projects/<name>/` alias for
    /// backward compatibility (e.g. named stores like `wiki` that have no repo).
    pub(super) fn resolve_named_project_db_path(project_name: &str) -> Result<PathBuf, String> {
        // Guard: reject names that could escape the projects/ directory.
        if project_name.is_empty()
            || project_name.contains('/')
            || project_name.contains('\\')
            || project_name.contains("..")
            || project_name.starts_with('.')
        {
            return Err(format!("Invalid project name '{project_name}'"));
        }
        let safe_name = crate::utils::sanitize_safe_path_name(project_name);

        // Prefer the manifest-recorded repo-local DB for this project name. This
        // makes repo-local addressing primary and removes the hard dependency on
        // the Plan C symlink (which does not exist on non-Unix hosts).
        if let Some(repo_local) = Self::manifest_repo_local_db_for_project(&safe_name) {
            if repo_local.exists() {
                return Ok(repo_local);
            }
        }

        let db_path = crate::path_utils::plan_c_global_db_path(&safe_name);
        if !db_path.exists() {
            if db_path.is_symlink() {
                let target_str = match std::fs::read_link(&db_path) {
                    Ok(target) => target.display().to_string(),
                    Err(_) => "<unknown>".to_string(),
                };
                Err(format!(
                    "Project '{}' database symlink is broken: {} -> (target missing: {})",
                    project_name,
                    db_path.display(),
                    target_str
                ))
            } else {
                Err(format!(
                    "Project '{}' not found (expected DB at {})",
                    project_name,
                    db_path.display()
                ))
            }
        } else {
            Ok(db_path)
        }
    }

    /// Resolve a (sanitized) project name to a repo-local `<repo>/.tachi/memory.db`
    /// recorded in the manifest, if any. This is the symlink-independent primary
    /// addressing path: for each manifest entry shaped like `<repo>/.tachi/memory.db`,
    /// we recompute the repo's alias dir name (hashed, or its legacy un-hashed
    /// form for backward compatibility) and match it against `safe_name`.
    ///
    /// Returns the first matching repo-local path. Returns `None` when the
    /// manifest is missing/unreadable or no entry matches — callers then fall
    /// back to the `~/.tachi/projects/<name>/` alias.
    fn manifest_repo_local_db_for_project(safe_name: &str) -> Option<PathBuf> {
        // The runtime manifest lives at `<tachi_home>/manifest.json` (see
        // `bootstrap/serve.rs`, which uses `app_home == tachi_home()`).
        let manifest_path = crate::path_utils::tachi_home().join("manifest.json");
        let manifest = crate::manifest::Manifest::load(&manifest_path).ok()?;
        for entry in &manifest.dbs {
            let db_path = std::path::Path::new(&entry.path);
            // Only consider repo-local `<repo>/.tachi/memory.db` shapes.
            let Some(project_root) =
                crate::path_utils::plan_c_project_root_from_local_db(db_path)
            else {
                continue;
            };
            let matches = crate::path_utils::plan_c_dir_name_from_root(&project_root)
                .as_deref()
                == Some(safe_name)
                || crate::path_utils::plan_c_legacy_dir_name_from_root(&project_root)
                    .as_deref()
                    == Some(safe_name);
            if matches {
                return Some(db_path.to_path_buf());
            }
        }
        None
    }

    /// Open a named project's DB for a read-only operation.
    pub(super) fn with_named_project_store_read<T>(
        &self,
        project_name: &str,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let db_path = Self::resolve_named_project_db_path(project_name)?;
        // Use global rw_gate for reader concurrency protection (prevents schema swap while reading)
        let _gate = read_or_recover(&self.global_rw_gate, "named_project_rw_gate");
        let mut store =
            Self::open_read_store(&db_path, &format!("named-project:{}", project_name))?;
        f(&mut store)
    }

    /// Open a named project's DB for a write operation.
    pub(super) fn with_named_project_store<T>(
        &self,
        project_name: &str,
        f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let db_path = Self::resolve_named_project_db_path(project_name)?;
        let db_str = db_path.to_str().ok_or_else(|| {
            format!(
                "Project DB path contains invalid UTF-8: {}",
                db_path.display()
            )
        })?;

        // Use global rw_gate for write lock to avoid concurrent SQLITE_BUSY issues for named projects.
        // It's a coarse lock, but named project writes are fast and this prevents concurrent overlaps.
        let _gate = write_or_recover(&self.global_rw_gate, "named_project_rw_gate");
        let mut store = MemoryStore::open_with_label(db_str, project_name)
            .map_err(|e| format!("open named project store: {e}"))?;
        f(&mut store)
    }

    pub(super) fn resolve_write_scope(&self, requested: &str) -> (DbScope, Option<String>) {
        if requested == "global" {
            (DbScope::Global, None)
        } else if self.has_project_db() {
            (DbScope::Project, None)
        } else {
            (
                DbScope::Global,
                Some("No project DB available; saved to global".to_string()),
            )
        }
    }

    pub(super) fn get_capability(&self, cap_id: &str) -> Result<HubCapability, rmcp::ErrorData> {
        let mut found = None;
        if self.has_project_db() {
            found = self
                .with_project_store_read(|store| {
                    store
                        .hub_get(cap_id)
                        .map_err(|e| format!("hub get project: {e}"))
                })
                .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
        }
        if found.is_none() {
            found = self
                .with_global_store_read(|store| {
                    store
                        .hub_get(cap_id)
                        .map_err(|e| format!("hub get global: {e}"))
                })
                .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
        }
        found.ok_or_else(|| {
            rmcp::ErrorData::invalid_params(format!("Capability '{cap_id}' not found"), None)
        })
    }

    pub(super) fn resolve_active_capability_id(
        &self,
        cap_id: &str,
    ) -> Result<String, rmcp::ErrorData> {
        if self.has_project_db() {
            let route = self
                .with_project_store_read(|store| {
                    store
                        .hub_get_active_version_route(cap_id)
                        .map_err(|e| format!("hub route project: {e}"))
                })
                .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
            if let Some(target) = route {
                return Ok(target);
            }
        }

        let route = self
            .with_global_store_read(|store| {
                store
                    .hub_get_active_version_route(cap_id)
                    .map_err(|e| format!("hub route global: {e}"))
            })
            .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;

        Ok(route.unwrap_or_else(|| cap_id.to_string()))
    }

    pub(super) fn get_virtual_capability_bindings(
        &self,
        vc_id: &str,
    ) -> Result<(Vec<VirtualCapabilityBinding>, &'static str), String> {
        if self.has_project_db() {
            let bindings = self.with_project_store_read(|store| {
                store
                    .vc_list_bindings(vc_id)
                    .map_err(|e| format!("vc bindings project: {e}"))
            })?;
            if !bindings.is_empty() {
                return Ok((bindings, "project"));
            }
        }

        let bindings = self.with_global_store_read(|store| {
            store
                .vc_list_bindings(vc_id)
                .map_err(|e| format!("vc bindings global: {e}"))
        })?;
        Ok((bindings, "global"))
    }

    pub(super) fn resolve_virtual_capability_target(
        &self,
        vc_id: &str,
    ) -> Result<(String, Value), String> {
        let vc_cap = self.get_capability(vc_id).map_err(|e| format!("{e}"))?;

        if !vc_cap.cap_type.eq_ignore_ascii_case("virtual") {
            return Err(format!("Capability '{vc_id}' is not type 'virtual'"));
        }
        if !capability_callable(&vc_cap) {
            return Err(format!(
                "Virtual Capability '{}' is not callable (enabled={}, review_status={}, health_status={}).",
                vc_id, vc_cap.enabled, vc_cap.review_status, vc_cap.health_status
            ));
        }

        let (bindings, binding_db) = self.get_virtual_capability_bindings(vc_id)?;
        if bindings.is_empty() {
            return Err(format!("Virtual Capability '{vc_id}' has no bindings"));
        }

        let mut chosen: Option<String> = None;
        let mut candidates = Vec::new();

        for binding in bindings {
            let mut reason = None;
            let mut version = None;
            let mut cap_type = None;

            if !binding.enabled {
                reason = Some("binding_disabled".to_string());
            }

            let target_cap = match self.get_capability(&binding.capability_id) {
                Ok(cap) => {
                    version = Some(cap.version);
                    cap_type = Some(cap.cap_type.clone());
                    Some(cap)
                }
                Err(_) => {
                    reason = Some("target_missing".to_string());
                    None
                }
            };

            if reason.is_none() {
                if let Some(cap) = target_cap.as_ref() {
                    if !cap.cap_type.eq_ignore_ascii_case("mcp") {
                        reason = Some("target_not_mcp".to_string());
                    } else if let Some(pin) = binding.version_pin {
                        if cap.version != pin {
                            reason = Some("version_pin_mismatch".to_string());
                        }
                    }

                    if reason.is_none() && !capability_callable(cap) {
                        reason = Some("target_not_callable".to_string());
                    }
                }
            }

            if chosen.is_none() && reason.is_none() {
                chosen = Some(binding.capability_id.clone());
            }

            candidates.push(json!({
                "vc_id": binding.vc_id,
                "capability_id": binding.capability_id,
                "priority": binding.priority,
                "version_pin": binding.version_pin,
                "enabled": binding.enabled,
                "target_version": version,
                "target_type": cap_type,
                "selected": reason.is_none() && chosen.as_deref() == Some(binding.capability_id.as_str()),
                "status": reason.unwrap_or_else(|| "ok".to_string()),
                "metadata": binding.metadata,
            }));
        }

        let selected = chosen.ok_or_else(|| {
            format!(
                "Virtual Capability '{vc_id}' has no callable MCP binding. Inspect vc_resolve for candidate status."
            )
        })?;

        Ok((
            selected.clone(),
            json!({
                "id": vc_id,
                "binding_db": binding_db,
                "selected": selected,
                "candidates": candidates,
            }),
        ))
    }

    pub(super) fn resolve_call_target(
        &self,
        requested_id: &str,
    ) -> Result<ResolvedCallTarget, String> {
        if let Ok(cap) = self.get_capability(requested_id) {
            if cap.cap_type.eq_ignore_ascii_case("virtual") {
                let (resolved_id, resolution) =
                    self.resolve_virtual_capability_target(requested_id)?;
                return Ok(ResolvedCallTarget {
                    requested_id: requested_id.to_string(),
                    resolved_id,
                    requested_kind: "virtual".to_string(),
                    resolution,
                });
            }
        }

        let resolved_id = self
            .resolve_active_capability_id(requested_id)
            .map_err(|e| format!("{e}"))?;
        let requested_kind = if requested_id == resolved_id {
            "concrete"
        } else {
            "alias"
        };
        Ok(ResolvedCallTarget {
            requested_id: requested_id.to_string(),
            requested_kind: requested_kind.to_string(),
            resolved_id: resolved_id.clone(),
            resolution: json!({
                "id": requested_id,
                "selected": resolved_id,
                "kind": requested_kind,
            }),
        })
    }

    pub(super) fn record_capability_call_outcome(
        &self,
        cap_id: &str,
        success: bool,
        error_kind: Option<&str>,
    ) -> Result<(), String> {
        const OPEN_THRESHOLD: u32 = 3;

        if self.has_project_db() {
            let in_project = self.with_project_store_read(|store| {
                store
                    .hub_get(cap_id)
                    .map(|cap| cap.is_some())
                    .map_err(|e| format!("hub get project: {e}"))
            })?;
            if in_project {
                return self.with_project_store(|store| {
                    store
                        .hub_record_call_outcome(cap_id, success, error_kind, OPEN_THRESHOLD)
                        .map_err(|e| format!("hub call outcome project: {e}"))
                });
            }
        }

        self.with_global_store(|store| {
            store
                .hub_record_call_outcome(cap_id, success, error_kind, OPEN_THRESHOLD)
                .map_err(|e| format!("hub call outcome global: {e}"))
        })
    }

    pub(super) fn proxy_tool_exposure_mode_for_server(
        &self,
        server_name: &str,
    ) -> Result<McpToolExposureMode, rmcp::ErrorData> {
        let cap_id = format!("mcp:{server_name}");
        let cap = self.get_capability(&cap_id)?;
        let def: serde_json::Value = serde_json::from_str(&cap.definition).map_err(|e| {
            rmcp::ErrorData::internal_error(
                format!("bad definition for capability '{cap_id}': {e}"),
                None,
            )
        })?;
        Ok(resolve_mcp_tool_exposure(
            &def,
            self.tool_discovery.mcp_tool_exposure_mode,
        ))
    }

    pub(super) fn get_sandbox_policy_for_capability(&self, capability_id: &str) -> Option<Value> {
        if self.has_project_db() {
            match self.with_project_store(|store| {
                store
                    .get_sandbox_policy(capability_id)
                    .map_err(|e| format!("sandbox policy project: {e}"))
            }) {
                Ok(Some(policy)) => return Some(policy),
                Ok(None) => {}
                Err(e) => {
                    eprintln!(
                        "[sandbox] failed to read project policy for '{}': {}",
                        capability_id, e
                    );
                }
            }
        }

        match self.with_global_store(|store| {
            store
                .get_sandbox_policy(capability_id)
                .map_err(|e| format!("sandbox policy global: {e}"))
        }) {
            Ok(policy) => policy,
            Err(e) => {
                eprintln!(
                    "[sandbox] failed to read global policy for '{}': {}",
                    capability_id, e
                );
                None
            }
        }
    }

    pub(super) fn get_effective_sandbox_policy(
        &self,
        requested_capability_id: Option<&str>,
        resolved_capability_id: &str,
    ) -> (Option<Value>, String) {
        if let Some(policy) = self.get_sandbox_policy_for_capability(resolved_capability_id) {
            return (Some(policy), resolved_capability_id.to_string());
        }

        if let Some(requested_id) = requested_capability_id {
            if requested_id != resolved_capability_id {
                if let Some(policy) = self.get_sandbox_policy_for_capability(requested_id) {
                    return (Some(policy), requested_id.to_string());
                }
            }
        }

        (None, String::new())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_sandbox_exec_audit(
        &self,
        capability_id: &str,
        stage: &str,
        decision: &str,
        reason: Option<&str>,
        duration_ms: u64,
        tool_name: Option<&str>,
        error_kind: Option<&str>,
        metadata: &Value,
    ) {
        let timestamp = Utc::now().to_rfc3339();
        let metadata_json = serde_json::to_string(metadata).unwrap_or_else(|_| "{}".to_string());
        if let Err(e) = self.with_global_store(|store| {
            store
                .insert_sandbox_exec_audit(
                    &timestamp,
                    capability_id,
                    stage,
                    decision,
                    reason,
                    duration_ms,
                    tool_name,
                    error_kind,
                    &metadata_json,
                )
                .map_err(|err| format!("sandbox exec audit insert: {err}"))
        }) {
            eprintln!(
                "[sandbox] failed to record execution audit for '{}' (stage={}, decision={}): {}",
                capability_id, stage, decision, e
            );
        }
    }

    pub(super) fn register_skill_tool(&self, cap: &HubCapability) -> Result<String, String> {
        let _ = self.unregister_skill_tool(&cap.id);
        let (tool_name, tool) = build_skill_tool_from_cap(cap)?;
        {
            lock_or_recover(&self.tool_discovery.skill_tools, "skill_tools")
                .insert(tool_name.clone(), cap.id.clone());
            lock_or_recover(&self.tool_discovery.skill_tool_defs, "skill_tool_defs")
                .insert(tool_name.clone(), tool);
        }
        Ok(tool_name)
    }

    pub(super) fn unregister_skill_tool(&self, skill_id: &str) -> Result<Option<String>, String> {
        let removed_tool_name = {
            let mut skill_tools = lock_or_recover(&self.tool_discovery.skill_tools, "skill_tools");
            let tool_name = skill_tools
                .iter()
                .find(|(_, id)| id.as_str() == skill_id)
                .map(|(name, _)| name.clone());
            if let Some(ref name) = tool_name {
                skill_tools.remove(name);
                lock_or_recover(&self.tool_discovery.skill_tool_defs, "skill_tool_defs")
                    .remove(name);
            }
            tool_name
        };

        Ok(removed_tool_name)
    }

    pub(super) async fn call_skill_tool(
        &self,
        tool_name: &str,
        arguments: Option<rmcp::model::JsonObject>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
        let skill_id = lock_or_recover(&self.tool_discovery.skill_tools, "skill_tools")
            .get(tool_name)
            .cloned()
            .ok_or_else(|| {
                rmcp::ErrorData::invalid_params(
                    format!("Skill tool '{}' not found", tool_name),
                    None,
                )
            })?;

        let cap = self.get_capability(&skill_id)?;
        let def: Value = serde_json::from_str(&cap.definition).map_err(|e| {
            rmcp::ErrorData::invalid_params(format!("Invalid skill definition JSON: {e}"), None)
        })?;

        let args = arguments.unwrap_or_default();
        let prompt_template = def
            .get("prompt")
            .and_then(|v| v.as_str())
            .or_else(|| def.get("template").and_then(|v| v.as_str()))
            .unwrap_or("{{args_json}}");
        let prompt = render_skill_prompt_template(prompt_template, &args)
            .map_err(|e| rmcp::ErrorData::internal_error(format!("serialize args: {e}"), None))?;

        let output = if let Some(mock_response) = def.get("mock_response").and_then(|v| v.as_str())
        {
            mock_response.to_string()
        } else {
            let system = def
                .get("system")
                .and_then(|v| v.as_str())
                .unwrap_or("You are executing a reusable skill. Follow the instruction and produce the result.");
            let model = def.get("model").and_then(|v| v.as_str());
            let temperature = def
                .get("temperature")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.2) as f32;
            let max_tokens = def
                .get("max_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(1200) as u32;
            self.llm
                .call_extract_llm(system, &prompt, model, temperature, max_tokens)
                .await
                .map_err(|e| {
                    rmcp::ErrorData::internal_error(format!("skill execution failed: {e}"), None)
                })?
        };

        make_text_tool_result(&json!({
            "skill_id": skill_id,
            "tool_name": tool_name,
            "output": output
        }))
    }

    pub(super) async fn retry_dispatch(
        &self,
        tool_name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
        if lock_or_recover(&self.tool_discovery.skill_tools, "skill_tools").contains_key(tool_name)
        {
            let args_obj = arguments.map(|m| m.into_iter().collect::<rmcp::model::JsonObject>());
            return self.call_skill_tool(tool_name, args_obj).await;
        }

        if dlq_mutation_is_unsafe(tool_name, arguments.as_ref())
            || self.tool_router.has_route(tool_name)
        {
            return Err(rmcp::ErrorData::invalid_params(
                format!(
                    "Tool '{}' cannot be retried via DLQ because it is native or non-idempotent; retry the MCP call explicitly",
                    tool_name
                ),
                None,
            ));
        }

        let args_obj = arguments.map(|m| m.into_iter().collect::<rmcp::model::JsonObject>());

        if let Some((server_name, remote_tool)) = {
            let proxy_tools = lock_or_recover(&self.tool_discovery.proxy_tools, "proxy_tools");
            crate::server_handler::split_proxy_tool_name(
                tool_name,
                proxy_tools.keys().map(String::as_str),
            )
        } {
            let exposure_mode = self.proxy_tool_exposure_mode_for_server(&server_name)?;
            if exposure_mode == McpToolExposureMode::Gateway {
                return Err(rmcp::ErrorData::invalid_params(
                    format!(
                        "Direct proxy tool '{}' is disabled by tool_exposure=gateway for '{}'. Retry via hub_call.",
                        tool_name, server_name
                    ),
                    None,
                ));
            }
            return self
                .proxy_call_internal(&server_name, &remote_tool, args_obj)
                .await;
        }

        Err(rmcp::ErrorData::invalid_params(
            format!(
                "Native tool '{}' cannot be retried via DLQ — retry the MCP call directly",
                tool_name
            ),
            None,
        ))
    }

    // ─── Rate Limiter ────────────────────────────────────────────────────────

    /// Check rate limits before dispatching a tool call.
    ///
    /// Returns:
    /// - `Ok(None)`             — call is allowed, no warning
    /// - `Ok(Some(warning))`    — call is allowed but at/above soft-warn threshold
    ///                             (caller should append the warning to the tool result)
    /// - `Err(ErrorData)`       — hard rate-limit / loop block, do not dispatch
    ///
    /// Two independent checks:
    /// 1. **RPM limit**: sliding window of all calls per session (if RATE_LIMIT_RPM > 0)
    /// 2. **Burst limit**: detects repeated identical calls (same tool+args) within a
    ///    60-second window, indicating an agent is stuck in a loop. Soft warning at
    ///    `STUCK_SOFT_WARN_THRESHOLD`, hard block at `effective_burst`.
    pub(super) fn check_rate_limit(
        &self,
        tool_name: &str,
        args_hash: &str,
        session_id: &str,
    ) -> Result<Option<String>, rmcp::ErrorData> {
        let now = Instant::now();

        let overrides = {
            let rt = self.agent_runtime_read();
            rt.agent_profile
                .as_ref()
                .map(|p| (p.rate_limit_rpm, p.rate_limit_burst))
        };

        let mut rl = self.rate_limiter_lock();
        let (effective_rpm, effective_burst) = match overrides {
            Some((rpm_override, burst_override)) => (
                rpm_override.unwrap_or(rl.rpm),
                burst_override.unwrap_or(rl.burst),
            ),
            None => (rl.rpm, rl.burst),
        };

        // ── RPM check ────────────────────────────────────────────────────
        if effective_rpm > 0 {
            let windows = &mut rl.windows;

            Self::reserve_rate_limiter_entry_capacity(
                windows,
                RATE_LIMIT_MAX_SESSIONS,
                now - Duration::from_secs(120),
                session_id,
            );

            let window = windows.entry(session_id.to_string()).or_default();

            // Evict entries older than 60 seconds
            let cutoff = now - Duration::from_secs(60);
            while let Some(&front) = window.front() {
                if front < cutoff {
                    window.pop_front();
                } else {
                    break;
                }
            }

            if window.len() as u64 >= effective_rpm {
                let oldest = window.front().copied().unwrap_or(now);
                let retry_after = Duration::from_secs(60)
                    .checked_sub(now.duration_since(oldest))
                    .unwrap_or(Duration::from_secs(1));
                return Err(rmcp::ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_REQUEST,
                    format!(
                        "Rate limited: {} calls/min exceeded (limit={}). Retry in {:.0}s.",
                        window.len(),
                        effective_rpm,
                        retry_after.as_secs_f64()
                    ),
                    None,
                ));
            }

            window.push_back(now);
        }

        // ── Burst / loop detection ───────────────────────────────────────
        let mut soft_warning: Option<String> = None;
        if effective_burst > 0 {
            let burst_key = format!("{}:{}:{}", session_id, tool_name, args_hash);
            let bursts = &mut rl.bursts;

            Self::reserve_rate_limiter_entry_capacity(
                bursts,
                RATE_LIMIT_MAX_BURST_KEYS,
                now - RATE_LIMIT_BURST_WINDOW,
                &burst_key,
            );

            let stamps = bursts.entry(burst_key).or_default();

            // Evict entries outside the burst window
            let cutoff = now - RATE_LIMIT_BURST_WINDOW;
            while let Some(&front) = stamps.front() {
                if front < cutoff {
                    stamps.pop_front();
                } else {
                    break;
                }
            }

            if stamps.len() as u64 >= effective_burst {
                return Err(rmcp::ErrorData::new(
                    rmcp::model::ErrorCode::INVALID_REQUEST,
                    format!(
                        "Loop detected: tool '{}' called {} times with identical arguments within {}s (burst_limit={}). \
                         Stop before retrying the same path. Call tachi_progress_check with the current task, attempts, and latest error to get a debug checklist and ask_codex_prompt; search prior lessons with tachi_wiki_search or tachi_task_brief; if still blocked, ask another agent using that prompt.",
                        tool_name,
                        stamps.len() + 1,
                        RATE_LIMIT_BURST_WINDOW.as_secs(),
                        effective_burst
                    ),
                    None,
                ));
            }

            // Soft warning: this call (about to be recorded as #stamps.len()+1)
            // crosses the soft threshold but is still below the hard block.
            // Emit on every repeat from threshold up to (effective_burst - 1).
            let upcoming_count = stamps.len() as u64 + 1;
            if upcoming_count >= STUCK_SOFT_WARN_THRESHOLD && upcoming_count < effective_burst {
                soft_warning = Some(format!(
                    "⚠️ stuck-detection: tool '{}' has been called {} times with identical arguments within {}s. \
                     Hard block triggers at {} repeats. Consider calling tachi_progress_check with the current task / attempts / latest error, \
                     or searching prior solutions via tachi_wiki_search / tachi_task_brief before retrying the same path.",
                    tool_name,
                    upcoming_count,
                    RATE_LIMIT_BURST_WINDOW.as_secs(),
                    effective_burst
                ));
            }

            stamps.push_back(now);
        }

        Ok(soft_warning)
    }
}

#[cfg(test)]
mod resolve_named_project_tests {
    use super::*;

    fn with_env_lock<F: FnOnce()>(f: F) {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        f();
    }

    fn restore_env(name: &str, value: Option<std::ffi::OsString>) {
        if let Some(v) = value {
            std::env::set_var(name, v);
        } else {
            std::env::remove_var(name);
        }
    }

    /// Change #2: named-project resolution prefers the manifest-recorded
    /// repo-local DB and does NOT depend on the Plan C symlink existing.
    #[test]
    fn resolve_prefers_manifest_repo_local_db_without_symlink() {
        with_env_lock(|| {
            let tmp = tempfile::tempdir().expect("tmp");
            let saved = std::env::var_os("TACHI_HOME");
            let tachi_home = tmp.path().join("home");
            std::fs::create_dir_all(&tachi_home).expect("home");
            std::env::set_var("TACHI_HOME", &tachi_home);

            // Repo-local source of truth — NO Plan C alias/symlink created.
            let repo = tmp.path().join("My Service");
            let local_db = repo.join(".tachi/memory.db");
            std::fs::create_dir_all(local_db.parent().unwrap()).expect("local parent");
            std::fs::write(&local_db, b"").expect("local db");

            // Record the repo-local DB in the manifest at <tachi_home>/manifest.json.
            let entry = serde_json::json!({
                "path": local_db.to_string_lossy(),
                "role": "project",
                "owner": "project:My_Service",
                "schema_kind": "tachi",
                "vec_enabled": false,
                "allow_write": true,
                "last_doctor_at": "1970-01-01T00:00:00Z",
                "last_classification": "healthy",
                "scope_hint": "tachi-other"
            });
            let manifest = serde_json::json!({
                "schema_version": 1,
                "generated_at": "1970-01-01T00:00:00Z",
                "comment": "test",
                "dbs": [entry]
            });
            std::fs::write(
                tachi_home.join("manifest.json"),
                serde_json::to_vec_pretty(&manifest).unwrap(),
            )
            .expect("write manifest");

            // The hashed alias name for this repo resolves to the repo-local DB
            // even though no ~/.tachi/projects/<name>/ alias exists on disk.
            let name = crate::path_utils::plan_c_dir_name_from_root(&repo).expect("name");
            let resolved =
                MemoryServer::resolve_named_project_db_path(&name).expect("resolve");
            assert_eq!(resolved, local_db);

            restore_env("TACHI_HOME", saved);
        });
    }
}
