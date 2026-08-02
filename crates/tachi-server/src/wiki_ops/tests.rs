mod reference_validation_tests {
    use super::super::ingest::{
        read_limited_wiki_http_response, read_limited_wiki_reader, validate_wiki_ingest_http_url,
        wiki_ingest_http_client, wiki_ingest_http_client_for_url, ValidatedWikiIngestHttpUrl,
    };
    use super::super::references::{validate_reference_format, validate_references};
    use axum::{body::Body, response::Response, routing::get, Router};
    use futures::stream;
    use std::convert::Infallible;
    use std::io::Cursor;
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
            "docs/engineering/architecture/wiki-references-spec.md",
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
            sanitized_source: "https://example.com/source.md".to_string(),
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

    #[tokio::test]
    async fn wiki_ingest_http_url_rejects_userinfo() {
        let err = validate_wiki_ingest_http_url("https://user:password@93.184.216.34/source.md")
            .await
            .unwrap_err();
        assert!(err.contains("must not include credentials"), "err: {err}");
    }

    #[tokio::test]
    async fn wiki_ingest_signed_url_stays_request_usable_but_durable_source_is_scrubbed() {
        let signed = "https://93.184.216.34:8443/wiki/page.md?X-Amz-Signature=secret-token&expires=123#secret-fragment";
        let validated = validate_wiki_ingest_http_url(signed)
            .await
            .expect("signed URL should remain fetchable after validation");

        assert_eq!(validated.url.as_str(), signed);
        assert_eq!(
            validated.url.query(),
            Some("X-Amz-Signature=secret-token&expires=123")
        );
        assert_eq!(validated.url.fragment(), Some("secret-fragment"));
        assert_eq!(
            validated.sanitized_source,
            "https://93.184.216.34:8443/wiki/page.md"
        );
        assert!(!validated.sanitized_source.contains("secret-token"));
        assert!(!validated.sanitized_source.contains("secret-fragment"));
        assert!(validated
            .sanitized_source
            .contains("93.184.216.34:8443/wiki/page.md"));
    }

    /// #1566-2/3: an oversized durable-source identity must be rejected by
    /// `validate_wiki_ingest_http_url` itself, before DNS resolution
    /// (`lookup_host`) or the actual fetch ever run. The host here uses the
    /// RFC 2606 `.invalid` TLD, which is reserved to never resolve — if the
    /// length bound regressed to run *after* DNS resolution, this test would
    /// fail with a "resolve source URL host" error (or hang on a real
    /// lookup) instead of the length error asserted below.
    #[tokio::test]
    async fn wiki_ingest_http_url_rejects_oversized_source_before_dns_resolution() {
        let oversized_path = "a".repeat(memcore::db::MAX_REFERENCE_BYTES + 1);
        let oversized_url = format!("https://wiki-ingest-1566-hardening.invalid/{oversized_path}");

        let err = validate_wiki_ingest_http_url(&oversized_url)
            .await
            .expect_err("oversized source URL must be rejected");

        assert!(err.contains("byte limit"), "err: {err}");
        assert!(
            !err.contains("resolve source URL host"),
            "must fail on the length bound before DNS resolution, not after: {err}"
        );
    }

    #[tokio::test]
    async fn wiki_ingest_fallback_reader_accepts_exact_source_cap() {
        let reader = Cursor::new(vec![b'x'; super::super::WIKI_INGEST_SOURCE_MAX_BYTES]);
        let content = read_limited_wiki_reader(reader, "source file")
            .await
            .expect("fallback reader must accept exact-cap EOF");

        assert_eq!(content.len(), super::super::WIKI_INGEST_SOURCE_MAX_BYTES);
    }

    #[tokio::test]
    async fn wiki_ingest_fallback_reader_refuses_source_cap_plus_one() {
        let reader = Cursor::new(vec![b'x'; super::super::WIKI_INGEST_SOURCE_MAX_BYTES + 1]);
        let error = read_limited_wiki_reader(reader, "source file")
            .await
            .expect_err("fallback reader must detect growth past metadata size");

        assert_eq!(
            error,
            format!(
                "source file exceeds {} byte limit",
                super::super::WIKI_INGEST_SOURCE_MAX_BYTES
            )
        );
    }

    #[tokio::test]
    async fn wiki_ingest_chunked_http_reader_refuses_more_than_source_cap() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind chunked source fixture");
        let port = listener
            .local_addr()
            .expect("chunked source fixture address")
            .port();
        let first_chunk = vec![b'x'; super::super::WIKI_INGEST_SOURCE_MAX_BYTES];
        let app = Router::new().route(
            "/source.md",
            get(move || {
                let first_chunk = first_chunk.clone();
                async move {
                    let chunks = stream::iter([
                        Ok::<_, Infallible>(first_chunk),
                        Ok::<_, Infallible>(vec![b'y']),
                    ]);
                    Response::new(Body::from_stream(chunks))
                }
            }),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve chunked source fixture");
        });

        crate::ensure_tls_provider();
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("build chunked source client");
        let response = client
            .get(format!("http://127.0.0.1:{port}/source.md"))
            .send()
            .await
            .expect("fetch chunked source fixture");
        let error = read_limited_wiki_http_response(response)
            .await
            .expect_err("chunked oversized HTTP source must be refused");
        task.abort();
        let _ = task.await;

        assert_eq!(
            error,
            format!(
                "source response exceeds {} byte limit",
                super::super::WIKI_INGEST_SOURCE_MAX_BYTES
            )
        );
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
