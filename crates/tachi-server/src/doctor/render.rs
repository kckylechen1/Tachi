use super::DoctorReport;

// ─── Rendering ───────────────────────────────────────────────────────────────

pub fn render_report(report: &DoctorReport) -> String {
    let mut lines = Vec::new();
    lines.push("tachi doctor v2".to_string());
    lines.push(format!("generated_at: {}", report.generated_at));
    lines.push(format!(
        "scanned_roots: {}",
        report.scanned_roots.join(", ")
    ));
    lines.push(format!(
        "summary: {} physical dbs, {} resolved aliases, {} unresolved paths, {} path appearances, {} memories, {} jobs",
        report.summary.total_databases,
        report.summary.resolved_aliases,
        report.summary.unresolved_paths,
        report.summary.path_appearances,
        report.summary.total_memories,
        report.summary.total_jobs,
    ));
    lines.push(format!(
        "  ✅ healthy={} 🟡 vec_missing={} 🟠 wal_orphan={} 🔴 corrupt={} 🟣 legacy={} ⚪ placeholder={} 📦 backup={}",
        report.summary.healthy,
        report.summary.vec_extension_missing,
        report.summary.wal_orphan,
        report.summary.corrupt,
        report.summary.legacy_schema,
        report.summary.placeholder,
        report.summary.backup,
    ));
    lines.push(String::new());

    if !report.physical_stores.is_empty() {
        lines.push("physical stores:".to_string());
        for store in &report.physical_stores {
            lines.push(format!(
                "  {} [{}] aliases={} open_path={} basis={} mutation_state={}",
                store.canonical_path,
                store.physical_id,
                store.aliases.len(),
                store.open_path,
                store.open_path_basis.as_str(),
                store.mutation_state.as_str()
            ));
            for alias in &store.aliases {
                lines.push(format!("    alias: {alias}"));
            }
            for sidecar_path in &store.sidecar_paths {
                lines.push(format!("    sidecar-visible: {sidecar_path}"));
            }
            if let Some(kind) = store.open_failure_kind {
                lines.push(format!("    inventory_failure={}", kind.as_str()));
            }
        }
        lines.push(String::new());
    }

    if !report.warnings.is_empty() {
        lines.push("warnings:".to_string());
        for warning in &report.warnings {
            lines.push(format!("  [!] {}: {}", warning.code, warning.message));
            lines.push(format!("      fix: {}", warning.remediation));
        }
        lines.push(String::new());
    }

    for f in &report.findings {
        let mem = f
            .mem_count
            .map(|n| n.to_string())
            .unwrap_or_else(|| "?".to_string());
        let vec = f
            .vec_rowid_count
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".to_string());
        let none_dom = f
            .none_domain_count
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".to_string());
        let cross_domain = f
            .cross_domain_suspect_count
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".to_string());
        lines.push(format!(
            "{} {} [{}] size={} mem={} vec={} <none>={} cross_domain_suspect={} jobs={}/c={}/s={}/f={}/p={} wal={} schema={} scope={}",
            f.classification.icon(),
            f.classification.as_str(),
            f.path,
            f.file_size,
            mem,
            vec,
            none_dom,
            cross_domain,
            f.jobs.total,
            f.jobs.completed,
            f.jobs.skipped,
            f.jobs.failed,
            f.jobs.pending,
            f.has_wal,
            f.schema_kind,
            f.scope_hint,
        ));
        if f.cross_domain_suspect_count.unwrap_or(0) > 0 {
            lines.push(format!(
                "    cross_domain_suspect sample ids: {}",
                f.cross_domain_suspect_sample.join(", ")
            ));
        }
        if let Some(err) = &f.error {
            lines.push(format!("    error: {err}"));
        }
    }

    if !report.auto_fix_actions.is_empty() {
        lines.push(String::new());
        lines.push("auto-fix actions:".to_string());
        for a in &report.auto_fix_actions {
            let dest = a.destination.as_deref().unwrap_or("-");
            lines.push(format!(
                "  [{}] {} {} -> {} ({})",
                a.outcome, a.action, a.path, dest, a.note
            ));
        }
    }

    lines.join("\n")
}
