use super::*;

#[tokio::test]
async fn stuck_detection_no_warning_below_threshold() {
    let server = make_server();

    // First two identical calls: no warning, hard block far away.
    for i in 0..2 {
        let warn = server
            .check_rate_limit("save_memory", "hash-pre", "session-soft")
            .unwrap_or_else(|e| panic!("call {} should succeed: {:?}", i + 1, e));
        assert!(
            warn.is_none(),
            "call {} should not carry a stuck warning, got: {:?}",
            i + 1,
            warn
        );
    }
}

#[tokio::test]
async fn stuck_detection_emits_warning_from_third_call_through_seventh() {
    let server = make_server();

    // Calls 1 and 2: no warning.
    for _ in 0..2 {
        let warn = server
            .check_rate_limit("save_memory", "hash-warn", "session-soft-2")
            .expect("call should succeed");
        assert!(warn.is_none());
    }

    // Calls 3 through 7 (5 calls): each succeeds and carries a warning.
    // Hard block triggers on call 9 (default burst = 8 means stamps.len() >= 8).
    for upcoming in 3u64..=7 {
        let warn = server
            .check_rate_limit("save_memory", "hash-warn", "session-soft-2")
            .unwrap_or_else(|e| panic!("call {upcoming} should succeed: {:?}", e));
        let msg =
            warn.unwrap_or_else(|| panic!("call {upcoming} should carry a soft stuck warning"));
        assert!(
            msg.contains("stuck-detection"),
            "warning text should include the 'stuck-detection' tag, got: {msg}"
        );
        assert!(
            msg.contains("save_memory"),
            "warning should mention the tool name, got: {msg}"
        );
        assert!(
            msg.contains(&format!("called {upcoming} times")),
            "warning should report current count {upcoming}, got: {msg}"
        );
        assert!(
            msg.contains("tachi_unstick"),
            "warning should suggest tachi_unstick, got: {msg}"
        );
    }
}

#[tokio::test]
async fn stuck_detection_warning_disappears_at_hard_block() {
    let server = make_server();

    // Burn through 8 successful calls (calls 3..=7 carry warnings, calls 1,2,8 do not).
    // Wait — at call 8 (upcoming_count=8), upcoming_count == effective_burst(8),
    // so the soft-warning condition `upcoming < effective_burst` is false. Verify.
    for upcoming in 1u64..=8 {
        let warn = server
            .check_rate_limit("save_memory", "hash-hard", "session-hard")
            .unwrap_or_else(|e| panic!("call {upcoming} should succeed: {:?}", e));
        if (3..=7).contains(&upcoming) {
            assert!(
                warn.is_some(),
                "call {upcoming} should carry a soft warning"
            );
        } else {
            assert!(
                warn.is_none(),
                "call {upcoming} should NOT carry a soft warning, got: {:?}",
                warn
            );
        }
    }

    // 9th identical call → hard block.
    let err = server
        .check_rate_limit("save_memory", "hash-hard", "session-hard")
        .expect_err("9th identical call should hit the hard loop block");
    assert!(err.message.contains("Loop detected"));
}
