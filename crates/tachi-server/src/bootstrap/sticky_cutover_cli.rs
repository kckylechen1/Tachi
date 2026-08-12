#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sticky_cutover_receipt_uses_a_deterministic_sidecar() {
        let plan = std::path::Path::new("/tmp/sticky-cutover-plan.json");
        assert_eq!(
            receipt_path_for_plan(plan),
            std::path::Path::new("/tmp/sticky-cutover-plan.json.receipt")
        );
    }
}
