pub(super) fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

pub(super) fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}
