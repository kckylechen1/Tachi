use serde_json::Value;
use std::path::Path;

pub(super) fn write_text_file(path: &Path, text: &str) -> Result<(), String> {
    crate::utils::write_owner_only_file_atomic(path, text.as_bytes())
        .map_err(|e| format!("write {}: {e}", path.display()))
}

pub(super) fn render_poke_report_markdown(report: &Value) -> String {
    let status = report
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let run_id = report
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut out = format!("# Poke Smoke Report\n\n- run_id: `{run_id}`\n- status: `{status}`\n\n");
    out.push_str("## Probes\n\n");
    if let Some(probes) = report.get("probes").and_then(Value::as_array) {
        for probe in probes {
            let name = probe.get("name").and_then(Value::as_str).unwrap_or("probe");
            let status = probe
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let artifact = probe
                .get("artifact")
                .and_then(Value::as_str)
                .unwrap_or("(not written)");
            out.push_str(&format!("- `{name}`: `{status}` ({artifact})\n"));
        }
    }
    out
}

pub(super) fn print_poke_summary(report: &Value) {
    println!(
        "Poke smoke: {}",
        report
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    println!(
        "run_dir: {}",
        report
            .get("run_dir")
            .and_then(Value::as_str)
            .unwrap_or("(unknown)")
    );
    if let Some(probes) = report.get("probes").and_then(Value::as_array) {
        for probe in probes {
            println!(
                "- {}: {}",
                probe.get("name").and_then(Value::as_str).unwrap_or("probe"),
                probe
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
            );
        }
    }
}
