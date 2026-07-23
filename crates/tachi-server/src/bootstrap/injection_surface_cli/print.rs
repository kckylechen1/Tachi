use super::InjectionSurfaceReport;

pub(super) fn print_report(report: &InjectionSurfaceReport) {
    println!("Tachi injection-surface doctor");
    println!("  registry: {}", report.registry_path);
    if let Some(home) = &report.home {
        println!("  home: {home}");
    }
    println!(
        "  harnesses: {}  planes scanned: {}  unscanned: {}  findings: {}",
        report.summary.harnesses,
        report.summary.planes_scanned,
        report.summary.planes_unscanned,
        report.summary.findings
    );
    println!();
    println!("Plane accounting:");
    for account in &report.plane_accounts {
        match account.status.as_str() {
            "unscanned" => println!(
                "  UNSCANNED  {} / {}",
                account.harness_id, account.plane
            ),
            _ => {
                let path = account.path.as_deref().unwrap_or("-");
                println!(
                    "  scanned    {} / {}  ({path})",
                    account.harness_id, account.plane
                );
            }
        }
    }
    println!();
    if report.findings.is_empty() {
        println!("Findings: none");
        return;
    }
    println!("Findings:");
    for finding in &report.findings {
        println!(
            "  [{}] {} / {} / {}",
            finding.severity, finding.harness_id, finding.plane, finding.check_kind
        );
        println!("    evidence: {}", finding.evidence_path);
        println!("    remediation_owner: {}", finding.remediation_owner);
    }
}
