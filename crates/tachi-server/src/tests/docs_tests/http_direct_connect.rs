//! Frozen assertions for #732 HTTP direct-connect cookbook.

#[test]
fn http_direct_connect_doc_covers_acceptance_surface() {
    let body = include_str!("../../../../../docs/engineering/architecture/http-direct-connect.md");
    for needle in [
        "#732",
        "127.0.0.1",
        "loopback-trust-v1",
        "X-Tachi-Project",
        "tachiProject",
        "X-Tachi-Profile",
        "re-initialize",
        "-32000",
        "stdio",
        "admin",
        "#495",
        "http_direct_connect_header_identity",
        "auth_posture",
    ] {
        assert!(
            body.contains(needle),
            "http-direct-connect.md must mention {needle}"
        );
    }
}

#[test]
fn install_points_at_http_cookbook_and_health_check() {
    let body = include_str!("../../../../../docs/INSTALL.md");
    assert!(
        body.contains("http-direct-connect.md"),
        "INSTALL must link the HTTP cookbook"
    );
    assert!(
        body.contains("auth_posture") || body.contains("loopback"),
        "INSTALL must mention loopback/auth posture"
    );
    assert!(
        body.contains("/health"),
        "INSTALL must mention /health reconnect probe"
    );
}

#[test]
fn library_identity_doc_points_at_http_cookbook() {
    let body =
        include_str!("../../../../../docs/engineering/architecture/library-identity-runtime.md");
    assert!(
        body.contains("http-direct-connect.md"),
        "library-identity-runtime.md must link HTTP cookbook"
    );
}
