/// Default batch size when `FOUNDRY_DISTILL_BATCH_SIZE` is unset.
pub(crate) const DEFAULT_GROUPS_PER_BATCH: usize = 6;
pub(crate) const DEFAULT_PROCESSED_SCAN_LIMIT: usize = 10_000;
pub(crate) const DEFAULT_CANDIDATE_SCAN_LIMIT: usize = 5_000;
pub(crate) const MAX_DISTILL_SCAN_LIMIT: usize = 50_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DistillBackend {
    ClaudeCli,
    RawApi,
}

impl DistillBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            DistillBackend::ClaudeCli => "claude_cli",
            DistillBackend::RawApi => "raw_api",
        }
    }
}

/// Resolve distill backend from `FOUNDRY_DISTILL_BACKEND`.
/// Defaults to `raw_api` so daemon runs do not require Claude Code CLI.
pub(crate) fn resolve_distill_backend() -> DistillBackend {
    match std::env::var("FOUNDRY_DISTILL_BACKEND")
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("claude_cli" | "claude-cli" | "cli") => DistillBackend::ClaudeCli,
        Some("raw_api" | "raw-api" | "rawapi" | "api") => DistillBackend::RawApi,
        None | Some(_) => DistillBackend::RawApi,
    }
}

/// Strip agent-internal noise from raw session text before passing to the LLM.
/// Removes thinking traces, tool JSON blobs, SYSTEM_MEMORY injections, and
/// image references that contaminate distillation quality.
pub fn scrub_agent_noise(text: &str) -> String {
    // Patterns to strip line-by-line
    let noise_line_prefixes: &[&str] = &[
        "**Prioritizing",
        "**Refining",
        "**Analyzing",
        "I'm now focusing",
        "I'm now zeroing",
        "<SYSTEM-RETRIEVED-MEMORY",
        "<image name=",
    ];
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let trimmed = line.trim_start();
        // Skip lines that are pure tool JSON
        if (trimmed.starts_with('{') || trimmed.starts_with('['))
            && (trimmed.contains("\"SearchPath\"")
                || trimmed.contains("\"file_path\"")
                || trimmed.contains("\"todos\"")
                || trimmed.contains("\"prompt\""))
        {
            continue;
        }
        if noise_line_prefixes.iter().any(|p| trimmed.starts_with(p)) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    // Collapse runs of blank lines into at most two
    let mut prev_blank = false;
    let mut result = String::with_capacity(out.len());
    for line in out.lines() {
        let is_blank = line.trim().is_empty();
        if is_blank && prev_blank {
            continue;
        }
        prev_blank = is_blank;
        result.push_str(line);
        result.push('\n');
    }
    result
}

pub(crate) fn resolve_batch_size() -> usize {
    std::env::var("FOUNDRY_DISTILL_BATCH_SIZE")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| (1..=20).contains(&n))
        .unwrap_or(DEFAULT_GROUPS_PER_BATCH)
}

pub(crate) fn resolve_scan_limit(env_key: &str, default: usize) -> usize {
    std::env::var(env_key)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| (1..=MAX_DISTILL_SCAN_LIMIT).contains(&n))
        .unwrap_or(default)
}

pub(crate) fn resolve_processed_scan_limit() -> usize {
    resolve_scan_limit(
        "FOUNDRY_DISTILL_PROCESSED_SCAN_LIMIT",
        DEFAULT_PROCESSED_SCAN_LIMIT,
    )
}

pub(crate) fn resolve_candidate_scan_limit() -> usize {
    resolve_scan_limit(
        "FOUNDRY_DISTILL_CANDIDATE_SCAN_LIMIT",
        DEFAULT_CANDIDATE_SCAN_LIMIT,
    )
}

/// Minimum bucket size before we ask the LLM to distill — matches the
/// scheduler's threshold so we don't stitch tiny noisy groups.
pub(crate) const MIN_BUCKET_SIZE: usize = 3;

/// Max characters of the batch user payload before dispatching. If the
/// serialized JSON exceeds this, groups are dropped from the tail to stay
/// within the limit and avoid token overflow on the model side.
pub(crate) const MAX_BATCH_PAYLOAD_CHARS: usize = 60_000;
