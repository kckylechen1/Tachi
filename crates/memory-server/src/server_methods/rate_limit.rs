use crate::server_state::{
    MemoryServer, RATE_LIMIT_BURST_WINDOW, RATE_LIMIT_MAX_BURST_KEYS, RATE_LIMIT_MAX_SESSIONS,
    STUCK_SOFT_WARN_THRESHOLD,
};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

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

    #[cfg(test)]
    pub(crate) fn rate_limiter_entry_counts_for_tests(&self) -> (usize, usize) {
        let rl = self.rate_limiter_lock();
        (rl.windows.len(), rl.bursts.len())
    }

    pub(crate) fn check_rate_limit(
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
