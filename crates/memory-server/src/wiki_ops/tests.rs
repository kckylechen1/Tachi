mod reference_validation_tests {
    use super::super::ingest::{
        validate_wiki_ingest_http_url, wiki_ingest_http_client, wiki_ingest_http_client_for_url,
        ValidatedWikiIngestHttpUrl,
    };
    use super::super::references::{validate_reference_format, validate_references};
    use std::net::SocketAddr;

    #[test]
    fn valid_reference_formats() {
        for ok in [
            "https://example.com",
            "file:///path/to/file.md",
            "/Users/foo/project/README.md",
            "C:\\Users\\foo\\file.txt",
            "#69",
            "repo#69",
            "owner/repo#69",
            "docs/wiki-references-spec.md",
        ] {
            assert!(validate_reference_format(ok).is_ok(), "expected ok: {ok}");
        }
    }

    #[test]
    fn invalid_reference_formats() {
        for bad in [
            "",
            "   ",
            "relative/path.md",
            "ftp://example.com",
            "just some text",
        ] {
            assert!(
                validate_reference_format(bad).is_err(),
                "expected err: {bad}"
            );
        }
    }

    #[test]
    fn batch_validation_reports_index() {
        let err = validate_references(&["#1".to_string(), "nope".to_string()]).unwrap_err();
        assert!(err.contains("references[1]"), "err: {err}");
    }

    #[tokio::test]
    async fn wiki_ingest_http_url_rejects_loopback_ip() {
        let err = validate_wiki_ingest_http_url("http://127.0.0.1:8080/source.md")
            .await
            .unwrap_err();
        assert!(err.contains("private or local"), "err: {err}");
    }

    #[tokio::test]
    async fn wiki_ingest_http_url_rejects_metadata_link_local_ip() {
        let err = validate_wiki_ingest_http_url("http://169.254.169.254/latest/meta-data")
            .await
            .unwrap_err();
        assert!(err.contains("private or local"), "err: {err}");
    }

    #[tokio::test]
    async fn wiki_ingest_http_url_rejects_ipv6_unique_local_ip() {
        let err = validate_wiki_ingest_http_url("http://[fd00::1]/source.md")
            .await
            .unwrap_err();
        assert!(err.contains("private or local"), "err: {err}");
    }

    #[tokio::test]
    async fn wiki_ingest_http_url_rejects_ipv4_mapped_ipv6_loopback() {
        let err = validate_wiki_ingest_http_url("http://[::ffff:127.0.0.1]/source.md")
            .await
            .unwrap_err();
        assert!(err.contains("private or local"), "err: {err}");
    }

    #[tokio::test]
    async fn wiki_ingest_http_url_accepts_public_ip_literal_without_dns_override() {
        let validated = validate_wiki_ingest_http_url("http://93.184.216.34/source.md")
            .await
            .expect("public IP literal should be allowed");
        assert!(validated.resolved_addrs.is_none());
    }

    #[test]
    fn wiki_ingest_http_client_accepts_validated_dns_override() {
        let validated = ValidatedWikiIngestHttpUrl {
            url: reqwest::Url::parse("https://example.com/source.md").unwrap(),
            resolved_addrs: Some(vec!["93.184.216.34:443".parse::<SocketAddr>().unwrap()]),
        };
        wiki_ingest_http_client_for_url(&validated).expect("client with DNS override should build");
    }

    #[tokio::test]
    async fn wiki_ingest_http_url_rejects_localhost_name() {
        let err = validate_wiki_ingest_http_url("https://localhost/source.md")
            .await
            .unwrap_err();
        assert!(err.contains("host is not allowed"), "err: {err}");
    }

    #[test]
    fn wiki_ingest_http_client_is_reused() {
        let first = wiki_ingest_http_client().expect("client should build");
        let second = wiki_ingest_http_client().expect("client should build");
        assert!(std::ptr::eq(first, second));
    }
}

#[cfg(test)]
mod wiki_search_filter_tests {
    use super::super::search::wiki_row_has_direct_match_signal;
    use serde_json::json;

    #[test]
    fn wiki_search_filter_rejects_vector_only_drift() {
        let vector_only = json!({
            "path": "/wiki/engineering/unrelated",
            "score": {"vector": 0.42, "fts": 0.0, "symbolic": 0.0, "final": 1.0},
            "relevance": 1.0,
        });
        assert!(!wiki_row_has_direct_match_signal(&vector_only));
    }

    #[test]
    fn wiki_search_filter_keeps_lexical_or_symbolic_hits() {
        let fts_hit = json!({
            "path": "/wiki/engineering/mcp",
            "score": {"vector": 0.1, "fts": 0.2, "symbolic": 0.0, "final": 1.0},
        });
        let symbolic_hit = json!({
            "path": "/wiki/engineering/mcp",
            "score": {"vector": 0.1, "fts": 0.0, "symbolic": 0.2, "final": 1.0},
        });
        assert!(wiki_row_has_direct_match_signal(&fts_hit));
        assert!(wiki_row_has_direct_match_signal(&symbolic_hit));
    }
}
