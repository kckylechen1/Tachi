use super::*;

pub(in crate::dispatch_profile) fn sum_matrix_samples(rows: &[AgentPerformanceMatrixRow]) -> u32 {
    rows.iter().map(|row| row.samples).sum()
}

pub(in crate::dispatch_profile) fn sum_matrix_failures(rows: &[AgentPerformanceMatrixRow]) -> u32 {
    rows.iter().map(|row| row.failure_count).sum()
}

pub(in crate::dispatch_profile) fn weighted_matrix_rate<F>(
    rows: &[AgentPerformanceMatrixRow],
    value: F,
) -> Option<f64>
where
    F: Fn(&AgentPerformanceMatrixRow) -> Option<f64>,
{
    let mut weighted_sum = 0.0;
    let mut samples = 0_u32;
    for row in rows {
        let Some(value) = value(row) else {
            continue;
        };
        weighted_sum += value * row.samples as f64;
        samples += row.samples;
    }
    (samples > 0).then(|| round4(weighted_sum / samples as f64))
}

pub(in crate::dispatch_profile) fn summarize_matrix_rows(
    rows: &[AgentPerformanceMatrixRow],
) -> Value {
    json!({
        "samples": sum_matrix_samples(rows),
        "success_rate": weighted_matrix_rate(rows, |row| row.success_rate),
        "useful_rate": weighted_matrix_rate(rows, |row| row.useful_rate),
        "verification_rate": weighted_matrix_rate(rows, |row| Some(row.verification_rate)),
        "failure_count": sum_matrix_failures(rows),
        "human_override_rate": weighted_matrix_rate(rows, |row| Some(row.human_override_rate)),
        "avg_retry_count": weighted_matrix_rate(rows, |row| Some(row.avg_retry_count)),
        "avg_latency_ms": weighted_matrix_rate(rows, |row| row.avg_latency_ms),
        "avg_cost_usd": weighted_matrix_rate(rows, |row| row.avg_cost_usd),
        "avg_quality_score": weighted_matrix_rate(rows, |row| row.avg_quality_score),
    })
}
