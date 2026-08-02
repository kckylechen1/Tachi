use super::*;
use tokio::io::AsyncReadExt;

#[derive(Debug, Clone)]
pub(super) struct ValidatedWikiIngestHttpUrl {
    pub(super) url: reqwest::Url,
    pub(super) sanitized_source: String,
    pub(super) resolved_addrs: Option<Vec<SocketAddr>>,
}

struct WikiIngestSource {
    content: String,
    durable_source: String,
}

struct WikiIngestPostFetch {
    content: String,
    durable_source: String,
    topic: Option<String>,
    update_related: bool,
}

/// `tachi_home` is the caller's server-bound home directory
/// (`MemoryServer::tachi_home_dir()`), already resolved through the
/// canonical `TACHI_HOME` → `SIGIL_HOME` → `TACHI_APP_HOME` → workspace →
/// `~/.tachi` precedence chain.
///
/// #1096 leaf-2a round-2 (codex C3-wiki): the first pass here replaced the
/// pre-#1096 allow-list — which read `TACHI_HOME` and `SIGIL_HOME`
/// independently and admitted BOTH roots when both were set — with just the
/// funnel's single resolved winner. That is a narrowing, not a widening: a
/// deployment with `TACHI_HOME=/A` and `SIGIL_HOME=/B` set simultaneously
/// used to allow local ingest from files under `/B` (the funnel picks `/A`
/// as `tachi_home`, but `/B` was still on the pre-#1096 allow-list); the
/// first-pass rewrite silently rejected `/B` files it used to accept. This
/// version restores the union: every one of `TACHI_HOME`/`SIGIL_HOME`/
/// `TACHI_APP_HOME` that is independently set (even the ones the funnel's
/// precedence didn't pick as `tachi_home`) is still an allow-list root,
/// alongside the resolved `tachi_home` and cwd. See
/// `ingest_local_file_allowed_tests::admits_union_of_all_three_home_env_roots`
/// below for the regression this closes.
///
/// Returns the canonicalized path (and the allow-list root it was admitted
/// under, when there is one) on success. #1566: callers must reuse this
/// canonical form as the durable ingest source identity instead of
/// canonicalizing a second time (or worse, persisting the raw un-canonicalized
/// argument) — otherwise two different spellings of the same file (relative
/// vs. absolute, or a path containing `..`) end up recorded as two different
/// `evidence_refs_v1` identities for what is actually one file on disk.
struct WikiIngestLocalFileCanonical {
    /// Absolute, canonicalized path. This is always what actually gets
    /// opened for read — never the (possibly root-relative) identity below.
    absolute: PathBuf,
    /// The allow-list root `absolute` was admitted under, if admission went
    /// through the allow-list rather than the
    /// `TACHI_WIKI_INGEST_ALLOW_ANY_LOCAL_FILE` escape hatch (which has no
    /// notion of "the" root — the whole point of the escape hatch is to
    /// bypass root membership). #1566-2/r3: callers use this to *try* to
    /// render the durable identity relative to the root — but only when the
    /// result lands in `WIKI_INGEST_RELATIVE_REF_PREFIXES`'s vocabulary (see
    /// `wiki_ingest_local_file_identity`), so a `docs/...`-shaped repo file
    /// still classifies as `SourceKindV1::CanonicalDoc`
    /// (`tachi_params::classify_wiki_reference`) after canonicalization even
    /// when the raw input was already absolute, without turning every other
    /// admitted file's identity into a root-relative (and root-choice- and
    /// cwd-sensitive) string.
    matched_root: Option<PathBuf>,
}

fn wiki_ingest_local_file_canonical(
    source_path: &Path,
    tachi_home: &Path,
) -> Option<WikiIngestLocalFileCanonical> {
    let allow_any = std::env::var("TACHI_WIKI_INGEST_ALLOW_ANY_LOCAL_FILE")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        });

    let canonical_source = std::fs::canonicalize(source_path).ok();
    if allow_any {
        // Preserve the escape hatch even when canonicalization fails (e.g.
        // the path doesn't exist yet) — the subsequent file open surfaces
        // the real I/O error; this bypass must not itself become that error.
        return Some(WikiIngestLocalFileCanonical {
            absolute: canonical_source.unwrap_or_else(|| source_path.to_path_buf()),
            matched_root: None,
        });
    }
    let canonical_source = canonical_source?;

    let cwd = std::env::current_dir().ok();
    let mut roots = Vec::new();
    if let Some(cwd) = cwd {
        roots.push(cwd);
    }
    roots.push(tachi_home.to_path_buf());
    // Union, not narrowing: also admit whichever of the funnel's three home
    // keys are independently set as raw env, even the ones the funnel's
    // precedence didn't pick as the winning `tachi_home` above. This is what
    // restores the pre-#1096 TACHI_HOME+SIGIL_HOME union behavior (see the
    // function doc comment) while extending it to the funnel's third key.
    for env_key in ["TACHI_HOME", "SIGIL_HOME", "TACHI_APP_HOME"] {
        if let Ok(path) = std::env::var(env_key) {
            if !path.trim().is_empty() {
                roots.push(PathBuf::from(path));
            }
        }
    }
    // Preserved from the pre-#1096 behavior: the bare `~/.tachi` was always
    // an allow-list root regardless of TACHI_HOME/SIGIL_HOME/TACHI_APP_HOME
    // overrides, so keep it even when `tachi_home` resolved elsewhere —
    // narrowing this allow-list is out of scope for a pure plumbing change.
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".tachi"));
    }

    // `find`, not `any`: we need to keep *which* root matched so the caller
    // can render the identity relative to it. #1566 r3: root selection is
    // NOT just a function of `roots`' fixed push order, `source_path`, and
    // the filesystem — `roots` itself is a function of the process's current
    // `cwd` (pushed first) and its `TACHI_HOME`/`SIGIL_HOME`/`TACHI_APP_HOME`
    // env vars, both of which vary per-invocation (a daemon can be launched
    // from different working directories across restarts). When `cwd`
    // happens to be an ancestor of the "real" root (e.g. `cwd=$HOME` and
    // `tachi_home=$HOME/.tachi`), `cwd` wins the `find` ahead of `tachi_home`
    // on some invocations and not others, so *which* root matched can
    // genuinely differ across cwds for the same file. `wiki_ingest_local_file_identity`'s
    // vocabulary gate is what makes the *rendered identity* resilient to
    // that: an outer/accidental root produces a relative string that (almost
    // always) does not start with `docs/`/`skill/`, so it falls back to the
    // canonical absolute path — the one value that is a pure function of the
    // filesystem and `source_path` alone, independent of `roots`/cwd/env.
    // The one case this does not close: an outer root that itself has a
    // `docs`/`skill` child sitting exactly on the same path the inner root's
    // `docs`/`skill` child would produce (i.e. `docs`/`skill` independently
    // registered as a home root one level apart) can still render two
    // different vocabulary-shaped relatives for the same file — a
    // misconfiguration, not something this function defends against.
    roots
        .into_iter()
        .filter_map(|root| std::fs::canonicalize(root).ok())
        .find(|root| canonical_source.starts_with(root))
        .map(|matched_root| WikiIngestLocalFileCanonical {
            absolute: canonical_source,
            matched_root: Some(matched_root),
        })
}

/// Repo-relative prefixes recognized as a "legal, typed" reference shape by
/// this codebase's two existing closed vocabularies:
/// `wiki_ops::references::validate_reference_format` (what a hand-supplied
/// `references[]` entry must match to be accepted as a repo-relative
/// reference at all — see that function for the authoritative list) and
/// `tachi_params::classify_wiki_reference` (which further tags a `docs/`
/// entry, though not `skill/`, as `SourceKindV1::CanonicalDoc`). Neither
/// function exports these prefixes as a reusable constant, so this array is
/// kept in sync with them by hand — update all three together.
///
/// #1566 r3: `wiki_ingest_local_file_identity` only renders a canonicalized
/// local ingest source relative to its matched allow-list root when doing so
/// produces a string that starts with one of these prefixes; every other
/// file keeps its canonical absolute form. See that function's doc for why
/// unconditional relativization (the r2 shape) was unsound.
const WIKI_INGEST_RELATIVE_REF_PREFIXES: [&str; 4] = ["docs/", "docs\\", "skill/", "skill\\"];

/// Render a locally-admitted canonical path as the string persisted for both
/// `metadata.ingest_source` and `evidence_refs_v1[].ref` — always the exact
/// same value for both, so there is one identity, not two.
///
/// #1566-2/r3: when the path was admitted under an allow-list root AND
/// rendering it relative to that root falls inside
/// `WIKI_INGEST_RELATIVE_REF_PREFIXES` (e.g. a canonicalized
/// `/repo/docs/x.md` under allow-list root `/repo` becomes `docs/x.md`), use
/// that relative form — this is what lets
/// `tachi_params::classify_wiki_reference`'s `docs/`-prefix check keep
/// recognizing an ingested repo doc as `SourceKindV1::CanonicalDoc` after
/// canonicalization (canonicalizing a `docs/...`-relative input turns it
/// absolute, which would otherwise silently and permanently lose that
/// classification, and desync it from non-canonicalized `docs/...` refs
/// written elsewhere, which do classify).
///
/// Every other case keeps the canonical **absolute** path — this is the r3
/// fix over the r2 shape, which relativized unconditionally. Unconditional
/// relativization broke identity uniqueness three ways a cross-vendor review
/// caught: (1) a file sitting directly under an allow-list root (not inside
/// a `docs/`/`skill/` subdirectory) rendered as a bare filename, so two
/// different repos' same-named root file (or any two files in different
/// allow-list roots that share a leaf name) collided on one identity; (2)
/// when an allow-list root is nested inside another (e.g. `~/.tachi` under
/// `$HOME`, both admitted roots), which one `find` matches first depends on
/// the process's cwd — a value that a daemon's own restarts can vary — so
/// the *same file on disk* could render two different identities purely
/// because of an unrelated cwd change; (3) an arbitrary relative shape (e.g.
/// `notes/a.md`, or the bare filename from (1)) is not a legal reference per
/// `wiki_ops::references::validate_reference_format`'s closed vocabulary —
/// this ingest path does not itself run `durable_source` through that
/// validator (`memcore::db::ValidatedReferenceMutation::evidence` only
/// bounds/normalizes it), so an unconditionally-relativized identity could
/// silently write a ref shape into `evidence_refs_v1` that a caller manually
/// supplying the same string to `references[]` would have had rejected.
/// Gating relativization on the same vocabulary those two functions already
/// recognize keeps every rendered identity inside the legal shape space,
/// restores uniqueness (the canonical absolute path is unique by
/// construction — `std::fs::canonicalize` resolves symlinks/`.`/`..` to one
/// string per file), and removes the cwd sensitivity for every file that
/// does not land in the vocabulary (the absolute path does not depend on
/// which root matched or what the process's cwd was). It is not a complete
/// fix for (2) in the narrow case where the *outer* accidental root also
/// independently produces a vocabulary-shaped relative for the same file
/// (an outer root's own `docs`/`skill` child coinciding with the inner
/// root's) — see the `find, not any` comment in
/// `wiki_ingest_local_file_canonical` above for that residual.
///
/// Fails closed on non-UTF-8 paths rather than lossily substituting U+FFFD
/// replacement characters into a stored identity (#1566 N1) — a lossy
/// rewrite is a fail-open silent corruption of the identity this whole
/// function exists to keep exact.
fn wiki_ingest_local_file_identity(
    canonical: &WikiIngestLocalFileCanonical,
) -> Result<String, String> {
    let absolute = canonical
        .absolute
        .to_str()
        .ok_or_else(|| "wiki ingest source path is not valid UTF-8".to_string())?;

    let relative_in_vocabulary = canonical.matched_root.as_ref().and_then(|root| {
        canonical
            .absolute
            .strip_prefix(root)
            .ok()
            .and_then(|relative| relative.to_str())
            .filter(|relative| {
                WIKI_INGEST_RELATIVE_REF_PREFIXES
                    .iter()
                    .any(|prefix| relative.starts_with(prefix))
            })
    });

    Ok(relative_in_vocabulary.unwrap_or(absolute).to_string())
}

async fn source_for_path(tachi_home: &Path, source: String) -> Result<WikiIngestSource, String> {
    if source.starts_with("http://") || source.starts_with("https://") {
        let validated = validate_wiki_ingest_http_url(&source).await?;
        let client = wiki_ingest_http_client_for_url(&validated)?;
        let durable_source = validated.sanitized_source.clone();
        let response = client
            .get(validated.url)
            .send()
            .await
            .map_err(|e| format!("fetch source URL: {}", e.without_url()))?;
        if !response.status().is_success() {
            return Err(format!(
                "fetch source URL failed with status {}",
                response.status()
            ));
        }
        let content = read_limited_wiki_http_response(response).await?;
        Ok(WikiIngestSource {
            content,
            durable_source,
        })
    } else {
        let path = Path::new(&source);
        let Some(canonical) = wiki_ingest_local_file_canonical(path, tachi_home) else {
            return Err(
                "local wiki ingest is restricted to the current workspace or TACHI_HOME; set TACHI_WIKI_INGEST_ALLOW_ANY_LOCAL_FILE=1 to override"
                    .to_string(),
            );
        };
        // #1566-1: persist the canonicalized identity (not the raw `source`
        // argument) as `durable_source`, so two spellings of the same file
        // (relative vs. absolute, or a path containing `..`) land on the
        // same `evidence_refs_v1` ref. #1566 r3: identity is the canonical
        // absolute path — `wiki_ingest_local_file_identity` only substitutes
        // a root-relative rendering when that rendering falls inside
        // `WIKI_INGEST_RELATIVE_REF_PREFIXES`'s vocabulary; that gate is what
        // keeps the dedupe invariant above holding for the relative form
        // too (both spellings resolve to the same canonical path, select the
        // same matched root, and the vocabulary check is a pure function of
        // that root-relative string, so it agrees for both), and it does not
        // introduce a cwd dependency into identity: for every file the gate
        // rejects, the value falls back to the canonical absolute path,
        // which depends only on the filesystem and `source_path`, not on
        // which root matched or the process's cwd. #1566-3: bound it before
        // the file read, so an oversized path never reaches disk I/O.
        let durable_source =
            bound_wiki_ingest_durable_source(wiki_ingest_local_file_identity(&canonical)?)?;
        let content = read_limited_wiki_local_file(&canonical.absolute).await?;
        Ok(WikiIngestSource {
            content,
            durable_source,
        })
    }
}

/// Bound a would-be `durable_source` (persisted as `evidence_refs_v1[].ref` /
/// `metadata.ingest_source`) to the same limit `memcore` enforces on
/// evidence-ref storage. Fail-closed: reject rather than silently truncate,
/// so a caller never observes a saved identity shorter than what they typed.
/// #1566-2/3: shared by both the HTTP (`sanitized_wiki_ingest_source`) and
/// local-file branches so the bound is checked once, in one place, ahead of
/// any network or filesystem I/O the source would otherwise trigger.
fn bound_wiki_ingest_durable_source(source: String) -> Result<String, String> {
    if source.len() > memcore::db::MAX_REFERENCE_BYTES {
        return Err(format!(
            "wiki ingest source identity exceeds {} byte limit",
            memcore::db::MAX_REFERENCE_BYTES
        ));
    }
    Ok(source)
}

async fn read_limited_wiki_local_file(path: &Path) -> Result<String, String> {
    // Check metadata before opening/allocating the body when the filesystem
    // provides a trustworthy size. The bounded reader below remains mandatory
    // because the file can grow or have an unknown size between these steps.
    if tokio::fs::metadata(path)
        .await
        .ok()
        .is_some_and(|metadata| metadata.len() > WIKI_INGEST_SOURCE_MAX_BYTES as u64)
    {
        return Err(format!(
            "source file exceeds {} byte limit",
            WIKI_INGEST_SOURCE_MAX_BYTES
        ));
    }

    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("read source file: {e}"))?;
    read_limited_wiki_reader(file, "source file").await
}

pub(super) async fn read_limited_wiki_reader<R>(
    mut reader: R,
    label: &str,
) -> Result<String, String>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut body = Vec::new();
    {
        let mut limited = (&mut reader).take(WIKI_INGEST_SOURCE_MAX_BYTES as u64);
        limited
            .read_to_end(&mut body)
            .await
            .map_err(|e| format!("read {label}: {e}"))?;
    }
    debug_assert!(body.len() <= WIKI_INGEST_SOURCE_MAX_BYTES);

    if body.len() == WIKI_INGEST_SOURCE_MAX_BYTES {
        let mut overflow_probe = [0_u8; 1];
        let overflow_len = reader
            .read(&mut overflow_probe)
            .await
            .map_err(|e| format!("read {label}: {e}"))?;
        if overflow_len != 0 {
            return Err(format!(
                "{label} exceeds {} byte limit",
                WIKI_INGEST_SOURCE_MAX_BYTES
            ));
        }
    }

    String::from_utf8(body).map_err(|e| format!("read {label} as UTF-8: {e}"))
}

#[cfg(test)]
pub(crate) async fn handle_wiki_ingest_post_fetch_for_test(
    server: &MemoryServer,
    raw_source: &str,
    content: String,
    topic: Option<String>,
    update_related: bool,
) -> Result<String, String> {
    let durable_source = validate_wiki_ingest_http_url(raw_source)
        .await?
        .sanitized_source;
    handle_wiki_ingest_post_fetch(
        server,
        WikiIngestPostFetch {
            content,
            durable_source,
            topic,
            update_related,
        },
    )
    .await
}

pub(super) fn wiki_ingest_http_client_for_url(
    validated: &ValidatedWikiIngestHttpUrl,
) -> Result<reqwest::Client, String> {
    let Some(resolved_addrs) = validated.resolved_addrs.as_deref() else {
        return wiki_ingest_http_client().cloned();
    };
    let host = validated
        .url
        .host_str()
        .ok_or_else(|| "wiki ingest source URL must include a host".to_string())?;
    crate::ensure_tls_provider();
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(StdDuration::from_secs(30))
        .resolve_to_addrs(host, resolved_addrs)
        // SECURITY: same bypass as research_ops's fetch client (tachi#530
        // T3 review) — tachi-server's reqwest carries the `system-proxy`
        // feature, so without `.no_proxy()` a configured/system proxy would
        // do the real DNS resolution and connect, letting it rebind the
        // validated host to a private/local address and bypass the
        // `validate_wiki_ingest_http_url` SSRF guard entirely.
        .no_proxy()
        .build()
        .map_err(|e| format!("build source URL client: {e}"))
}

pub(super) fn wiki_ingest_http_client() -> Result<&'static reqwest::Client, String> {
    WIKI_INGEST_HTTP_CLIENT
        .get_or_init(|| {
            crate::ensure_tls_provider();
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(StdDuration::from_secs(30))
                // SECURITY: see the sibling `.no_proxy()` call above — same
                // proxy-bypass hole, same fix, for the no-DNS-override path.
                .no_proxy()
                .build()
                .map_err(|e| format!("build source URL client: {e}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

pub(super) async fn validate_wiki_ingest_http_url(
    source: &str,
) -> Result<ValidatedWikiIngestHttpUrl, String> {
    let url = reqwest::Url::parse(source).map_err(|e| format!("parse source URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("wiki ingest only supports http:// and https:// source URLs".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("wiki ingest source URLs must not include credentials".to_string());
    }

    // #1566-3: bound the durable source identity here, before the DNS
    // resolution (`lookup_host`) below and before any caller can reach the
    // actual fetch. An oversized URL must fail here with zero network I/O
    // and zero downstream LLM calls, not after a wasted round trip.
    let sanitized_source = sanitized_wiki_ingest_source(&url)?;

    let host = url
        .host_str()
        .ok_or_else(|| "wiki ingest source URL must include a host".to_string())?;
    if host.eq_ignore_ascii_case("localhost") || host.to_ascii_lowercase().ends_with(".localhost") {
        return Err("wiki ingest source URL host is not allowed".to_string());
    }
    let ip_literal = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = ip_literal.parse::<IpAddr>() {
        reject_blocked_wiki_ingest_ip(ip)?;
        return Ok(ValidatedWikiIngestHttpUrl {
            sanitized_source,
            url,
            resolved_addrs: None,
        });
    }

    let port = url
        .port_or_known_default()
        .ok_or_else(|| "wiki ingest source URL has no usable port".to_string())?;
    let mut resolved_addrs = Vec::new();
    let mut resolved_any = false;
    for addr in lookup_host((host, port))
        .await
        .map_err(|e| format!("resolve source URL host: {e}"))?
    {
        resolved_any = true;
        reject_blocked_wiki_ingest_ip(addr.ip())?;
        resolved_addrs.push(addr);
    }
    if !resolved_any {
        return Err("wiki ingest source URL host resolved to no addresses".to_string());
    }

    Ok(ValidatedWikiIngestHttpUrl {
        sanitized_source,
        url,
        resolved_addrs: Some(resolved_addrs),
    })
}

/// Derive the durable, credential-stripped source identity for an ingested
/// URL, bounded to `memcore`'s `MAX_REFERENCE_BYTES` (fail-closed — see
/// `bound_wiki_ingest_durable_source`). #1566-2: this used to return an
/// unbounded `String`; a sufficiently long query string or path could
/// produce a `durable_source` wider than what `memcore::memory_crud` accepts
/// as an evidence-ref target, which meant the *save*, not the fetch, was the
/// first place an oversized ingest source would fail — after already paying
/// for the network round trip.
fn sanitized_wiki_ingest_source(url: &reqwest::Url) -> Result<String, String> {
    let mut sanitized = url.clone();
    // Credentials are rejected above. Keep this defensive clearing local to
    // the derived durable representation so a future caller cannot persist
    // URL userinfo even if validation is accidentally reordered.
    let _ = sanitized.set_username("");
    let _ = sanitized.set_password(None);
    sanitized.set_query(None);
    sanitized.set_fragment(None);
    bound_wiki_ingest_durable_source(sanitized.to_string())
}

fn reject_blocked_wiki_ingest_ip(ip: IpAddr) -> Result<(), String> {
    if wiki_ingest_ip_is_blocked(ip) {
        Err("wiki ingest source URL resolves to a private or local address".to_string())
    } else {
        Ok(())
    }
}

fn wiki_ingest_ip_is_blocked(ip: IpAddr) -> bool {
    is_private_or_local_ip(ip)
}

pub(super) async fn read_limited_wiki_http_response(
    mut response: reqwest::Response,
) -> Result<String, String> {
    if response
        .content_length()
        .is_some_and(|len| len > WIKI_INGEST_SOURCE_MAX_BYTES as u64)
    {
        return Err(format!(
            "source response exceeds {} byte limit",
            WIKI_INGEST_SOURCE_MAX_BYTES
        ));
    }

    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("read source response: {e}"))?
    {
        if body.len().saturating_add(chunk.len()) > WIKI_INGEST_SOURCE_MAX_BYTES {
            return Err(format!(
                "source response exceeds {} byte limit",
                WIKI_INGEST_SOURCE_MAX_BYTES
            ));
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|e| format!("read source response as UTF-8: {e}"))
}

fn string_list_from_value(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::trim))
                .filter(|item| !item.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

fn derive_ingest_fallback(source: &str, topic_hint: Option<&str>, content: &str) -> Value {
    let title = topic_hint
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            content
                .lines()
                .find(|line| !line.trim().is_empty())
                .map(|line| {
                    line.trim()
                        .trim_start_matches('#')
                        .trim()
                        .chars()
                        .take(80)
                        .collect()
                })
        })
        .unwrap_or_else(|| {
            Path::new(source)
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("ingested-source")
                .to_string()
        });
    let keywords = topic_hint
        .map(|topic| {
            topic
                .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let entities = topic_hint
        .filter(|topic| !topic.trim().is_empty())
        .map(|topic| vec![topic.trim().to_string()])
        .unwrap_or_default();
    json!({
        "title": title,
        "topic": topic_hint.unwrap_or("ingest"),
        "summary": content.chars().take(100).collect::<String>(),
        "keywords": keywords,
        "entities": entities,
    })
}

async fn extract_ingest_metadata(
    server: &MemoryServer,
    source: &str,
    topic_hint: Option<&str>,
    content: &str,
) -> Result<(Value, Option<tachi_llm::PersistedModelInvocationReceiptV1>), String> {
    let system = "Extract wiki ingestion metadata. Return JSON only with keys: title, topic, summary, keywords, entities.";
    let user = format!(
        "Source: {source}\nTopic hint: {}\n\nContent:\n{}",
        topic_hint.unwrap_or(""),
        content.chars().take(8000).collect::<String>()
    );
    match server
        .llm
        .call_extract_llm_with_receipt(system, &user, None, 0.2, 800)
        .await
    {
        Ok(response)
            if response.invocation.completion_status()
                == tachi_llm::CompletionStatusV1::Truncated =>
        {
            Err(tachi_llm::LLM_OUTPUT_TRUNCATED.to_string())
        }
        Ok(response) => {
            let payload = tachi_llm::LlmClient::extract_json_payload(&response.value)
                .map_err(|error| format!("wiki ingest metadata parse failed: {error}"))?;
            let value = serde_json::from_str::<Value>(payload)
                .map_err(|error| format!("wiki ingest metadata parse failed: {error}"))?;
            if !value.is_object() {
                return Err("wiki ingest metadata parse failed: expected a JSON object".to_string());
            }
            Ok((value, Some(response.invocation)))
        }
        Err(_) => Ok((derive_ingest_fallback(source, topic_hint, content), None)),
    }
}

/// Persist a new ingest entry only after it has claimed any active predecessor.
/// A false supersession CAS means another writer already owns that predecessor,
/// so the new entry must not become a competing wiki candidate.
#[derive(Clone)]
struct TrustedExistingModelInvocationReceipt(Value);

impl TrustedExistingModelInvocationReceipt {
    /// This constructor is deliberately private and accepts only a row read
    /// from the replacement transaction. Caller/model metadata never reaches
    /// this preservation seam.
    fn from_existing_row(entry: &MemoryEntry) -> Option<Self> {
        crate::provenance::trusted_existing_model_invocation(&entry.metadata).map(Self)
    }

    fn attach_exactly(&self, mut metadata: Value) -> Result<Value, memcore::MemoryError> {
        let metadata_obj = metadata.as_object_mut().ok_or_else(|| {
            memcore::MemoryError::InvalidArg(
                "wiki ingest metadata must be an object before receipt preservation".to_string(),
            )
        })?;
        let provenance = metadata_obj
            .get_mut("provenance")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| {
                memcore::MemoryError::InvalidArg(
                    "wiki ingest provenance must be an object before receipt preservation"
                        .to_string(),
                )
            })?;
        provenance.insert("model_invocation".to_string(), self.0.clone());
        Ok(metadata)
    }
}

fn persist_wiki_ingest_entry(
    store: &mut MemoryStore,
    entry: &MemoryEntry,
    reference_appends: &[memcore::db::ValidatedReferenceMutation],
    new_invocation: Option<&tachi_llm::PersistedModelInvocationReceiptV1>,
    related_edges: &[memcore::MemoryEdge],
) -> Result<Vec<String>, String> {
    store
        .with_immutable_supersession_transaction(|replacement| {
            let old_entries = replacement
                .list_active_wiki_ingest_predecessors(&entry.path, &entry.topic)?
                .into_iter()
                .filter(|existing| existing.id != entry.id)
                .collect::<Vec<_>>();
            let trusted_existing_receipt = old_entries
                .iter()
                .find_map(TrustedExistingModelInvocationReceipt::from_existing_row);
            let mut replacement_entry = entry.clone();
            replacement_entry.metadata = match trusted_existing_receipt {
                Some(receipt) => receipt.attach_exactly(replacement_entry.metadata)?,
                None => match new_invocation {
                    Some(invocation) => crate::provenance::attach_model_invocation(
                        replacement_entry.metadata,
                        invocation,
                    )
                    .map_err(memcore::MemoryError::InvalidArg)?,
                    None => replacement_entry.metadata,
                },
            };
            let metadata_patch = replacement_entry
                .metadata
                .as_object()
                .cloned()
                .unwrap_or_default();
            for old_entry in &old_entries {
                replacement.claim_immutable_supersession(&old_entry.id, &replacement_entry.id)?;
            }
            replacement.upsert_with_validated_reference_mutations(
                &replacement_entry,
                &metadata_patch,
                reference_appends,
            )?;
            for old_entry in &old_entries {
                replacement.archive_claimed_source(&old_entry.id)?;
            }
            let mut committed_related_ids = Vec::new();
            let normalized_entities = replacement_entry
                .entities
                .iter()
                .map(|entity| entity.trim().to_ascii_lowercase())
                .filter(|entity| !entity.is_empty())
                .collect::<HashSet<_>>();
            for edge in related_edges {
                if old_entries
                    .iter()
                    .any(|predecessor| predecessor.id == edge.target_id)
                    || edge.target_id == replacement_entry.id
                {
                    continue;
                }
                let Some(target) = replacement.get_memory(&edge.target_id)? else {
                    continue;
                };
                if !replacement.memory_is_active_unsuperseded(&edge.target_id)?
                    || !is_ordinary_related_wiki_entry(&target)
                        .map_err(memcore::MemoryError::InvalidArg)?
                    || !entry_shares_normalized_entity(&target, &normalized_entities)
                {
                    continue;
                }
                replacement.add_edge(edge).map_err(|error| {
                    memcore::MemoryError::Internal(format!("wiki ingest edge: {error}"))
                })?;
                committed_related_ids.push(edge.target_id.clone());
            }
            Ok(committed_related_ids)
        })
        .map_err(|e| format!("wiki ingest refused: {e}"))
}

pub(crate) async fn handle_wiki_ingest(
    server: &MemoryServer,
    params: TachiWikiIngestParams,
) -> Result<String, String> {
    let TachiWikiIngestParams {
        source,
        topic,
        update_related,
    } = params;
    let WikiIngestSource {
        content,
        durable_source,
    } = source_for_path(&server.tachi_home_dir(), source).await?;
    handle_wiki_ingest_post_fetch(
        server,
        WikiIngestPostFetch {
            content,
            durable_source,
            topic,
            update_related,
        },
    )
    .await
}

async fn handle_wiki_ingest_post_fetch(
    server: &MemoryServer,
    source: WikiIngestPostFetch,
) -> Result<String, String> {
    let WikiIngestPostFetch {
        content,
        durable_source,
        topic,
        update_related,
    } = source;
    if content.trim().is_empty() {
        append_wiki_log(
            server,
            "ingest",
            &format!("{} | skipped empty source", durable_source),
        );
        return serde_json::to_string(&json!({
            "status": "skipped",
            "reason": "empty_source",
            "source": durable_source,
        }))
        .map_err(|e| format!("serialize wiki_ingest: {e}"));
    }

    let (metadata, model_invocation) =
        extract_ingest_metadata(server, &durable_source, topic.as_deref(), &content).await?;
    let title = metadata
        .get("title")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("Ingested Source")
        .to_string();
    let topic = metadata
        .get("topic")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .or(topic.clone())
        .unwrap_or_else(|| "ingest".to_string());
    let summary = metadata
        .get("summary")
        .and_then(Value::as_str)
        .map(|value| value.chars().take(120).collect::<String>())
        .unwrap_or_else(|| content.chars().take(100).collect());
    let mut keywords = string_list_from_value(metadata.get("keywords"));
    if !keywords.iter().any(|keyword| keyword == "ingest") {
        keywords.push("ingest".to_string());
    }
    let entities = string_list_from_value(metadata.get("entities"));
    let path = format!("/wiki/general/{}", sanitize_safe_path_name(&topic));
    let id = uuid::Uuid::new_v4().to_string();
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let evidence_refs_v1 =
        build_evidence_refs_v1(std::slice::from_ref(&durable_source), &timestamp);
    let reference_appends = evidence_refs_v1
        .into_iter()
        .map(|reference| {
            let target_kind = reference
                .target_kind
                .map(serde_json::to_value)
                .transpose()
                .map_err(|error| format!("serialize wiki ingest target kind: {error}"))?
                .and_then(|value| value.as_str().map(str::to_string));
            memcore::db::ValidatedReferenceMutation::evidence(
                reference.target_ref,
                reference.captured_at,
                target_kind,
            )
            .map_err(|error| format!("validate wiki ingest reference: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut artifact_metadata =
        build_candidate_knowledge_artifact_fields(&path, "unspecified", &json!({}));
    if let Some(object) = artifact_metadata.as_object_mut() {
        object.extend(
            json!({
            "wiki": true,
            "wiki_title": title,
            "ingest_source": durable_source.clone(),
            "allow_cross_project": true,
            // #1072 fix-round (#1215 BUG 6): `wiki_ingest` used to upsert
            // straight into `/wiki/general/...` with no lifecycle/authority
            // marker at all, so `derive_wiki_lifecycle`'s no-marker default
            // (`Active`, kept for pre-#1072 back-compat on entries that
            // predate the lifecycle vocabulary) silently promoted arbitrary
            // fetched URL/file content to reviewed truth — a bypass named
            // explicitly in the cross-vendor review ("ingest writers").
            // Ingested content is unreviewed by construction (no approval
            // step exists here); stamp it `pending_review` honestly, same
            // vocabulary `wiki_layer_metadata` stamps for the MCP write
            // path, with the ingest source recorded as its typed evidence ref.
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        );
    }
    let metadata = crate::provenance::inject_provenance(
        server,
        artifact_metadata,
        "wiki_ingest",
        "wiki_ingest",
        Some("global"),
        crate::server_state::DbScope::Project,
        json!({"source": durable_source.clone()}),
    );
    let entry = MemoryEntry {
        id: id.clone(),
        path: path.clone(),
        summary: summary.clone(),
        text: content.clone(),
        importance: 0.8,
        timestamp: timestamp.clone(),
        valid_from: String::new(),
        valid_until: None,
        category: "experience".to_string(),
        topic: topic.clone(),
        keywords,
        persons: Vec::new(),
        entities: entities.clone(),
        location: String::new(),
        source: "wiki".to_string(),
        scope: "general".to_string(),
        archived: false,
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        metadata,
        vector: None,
        retention_policy: Some("permanent".to_string()),
        domain: Some("wiki".to_string()),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    };

    let mut related = Vec::new();
    let mut related_edges = Vec::new();
    if update_related {
        related = find_related_by_entities(server, "wiki", &entities, &id, 10)
            .map_err(|error| format!("wiki ingest related lookup: {error}"))?;
        for related_entry in &related {
            let Some(target_id) = related_entry.get("id").and_then(Value::as_str) else {
                continue;
            };
            related_edges.push(memcore::MemoryEdge {
                source_id: id.clone(),
                target_id: target_id.to_string(),
                relation: "references".to_string(),
                weight: 0.6,
                metadata: json!({
                    "wiki_ingest": true,
                    "shared_entities": entities.clone(),
                }),
                created_at: Utc::now().to_rfc3339(),
                valid_from: String::new(),
                valid_to: None,
            });
        }
    }

    let committed_related_ids =
        server.with_named_project_store_identity_checked("wiki", |store| {
            persist_wiki_ingest_entry(
                store,
                &entry,
                &reference_appends,
                model_invocation.as_ref(),
                &related_edges,
            )
        })?;
    let committed_related_ids = committed_related_ids
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    related.retain(|entry| {
        entry
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| committed_related_ids.contains(id))
    });

    // #1413 concern 1: bust the shared (global) recall cache AFTER every
    // content-changing write in this ingest has committed — the entry upsert
    // (+ optional supersede/archive of the prior wiki entry) above AND the
    // related-edge writes in the `update_related` loop just above, since those
    // edges can surface via graph-expanded searches. This runs only on the
    // fully-successful path: an edge write failure `return Err(e)`-bails before
    // reaching here, so failure propagation is retained. The invalidator
    // re-takes the global write gate via `with_global_store`, so it stays OUT
    // of every `with_named_project_store` closure above — never inside one.
    let _ = crate::memory_search_ops::invalidate_recall_cache_after_write(server, "wiki_ingest");

    server.enqueue_enrichment(crate::enrichment::build_enrichment_item(
        &entry,
        true,
        false,
        DbScope::Project,
        Some("wiki".to_string()),
        None,
        None,
        None,
        1,
    ));

    append_wiki_log(
        server,
        "ingest",
        &format!("{} | created {} at {}", durable_source, id, path),
    );

    serde_json::to_string(&json!({
        "status": "created",
        "id": id,
        "path": path,
        "source": durable_source,
        "title": title,
        "summary": summary,
        "related_entries": related,
    }))
    .map_err(|e| format!("serialize wiki_ingest: {e}"))
}

#[cfg(test)]
mod ingest_local_file_allowed_tests {
    use super::wiki_ingest_local_file_canonical;
    use crate::test_support::EnvRestore;

    /// #1096 leaf-2a round-2 (codex C3-wiki): RED against the first-pass
    /// implementation, which passed only the funnel-resolved `tachi_home`
    /// (the single precedence winner) as an allow-list root. With
    /// `TACHI_HOME=/A` and `SIGIL_HOME=/B` both set, the funnel resolves
    /// `tachi_home` to `/A`; the first-pass code then rejected a file under
    /// `/B` that the pre-#1096 implementation used to accept. This asserts
    /// the union: a file under the LOSING key's root is still allowed.
    #[test]
    fn admits_union_of_all_three_home_env_roots() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tachi_home_dir = tempfile::tempdir().expect("tachi_home tempdir");
        let sigil_home_dir = tempfile::tempdir().expect("sigil_home tempdir");
        let app_home_dir = tempfile::tempdir().expect("app_home tempdir");

        let tachi_file = tachi_home_dir.path().join("under-tachi-home.md");
        let sigil_file = sigil_home_dir.path().join("under-sigil-home.md");
        let app_file = app_home_dir.path().join("under-app-home.md");
        std::fs::write(&tachi_file, "tachi").expect("write tachi fixture");
        std::fs::write(&sigil_file, "sigil").expect("write sigil fixture");
        std::fs::write(&app_file, "app").expect("write app fixture");

        let _tachi_env = EnvRestore::set_path("TACHI_HOME", tachi_home_dir.path());
        let _sigil_env = EnvRestore::set_path("SIGIL_HOME", sigil_home_dir.path());
        let _app_env = EnvRestore::set_path("TACHI_APP_HOME", app_home_dir.path());
        // `TACHI_WIKI_INGEST_ALLOW_ANY_LOCAL_FILE` would short-circuit the
        // allow-list entirely and defeat this test's whole point.
        let _allow_any_off = EnvRestore::remove("TACHI_WIKI_INGEST_ALLOW_ANY_LOCAL_FILE");

        // The funnel picks TACHI_HOME as the resolved winner passed in here,
        // matching what `MemoryServer::tachi_home_dir()` would resolve to.
        let resolved_tachi_home = tachi_home_dir.path();

        assert!(
            wiki_ingest_local_file_canonical(&tachi_file, resolved_tachi_home).is_some(),
            "file under the resolved (winning) TACHI_HOME must be allowed"
        );
        assert!(
            wiki_ingest_local_file_canonical(&sigil_file, resolved_tachi_home).is_some(),
            "file under the losing key SIGIL_HOME must still be allowed (union, not narrowing)"
        );
        assert!(
            wiki_ingest_local_file_canonical(&app_file, resolved_tachi_home).is_some(),
            "file under the losing key TACHI_APP_HOME must still be allowed (union, not narrowing)"
        );
    }
}

#[cfg(test)]
mod ingest_source_identity_tests {
    use super::{bound_wiki_ingest_durable_source, build_evidence_refs_v1, source_for_path};
    use crate::test_support::{CwdRestore, EnvRestore};
    use tachi_params::SourceKindV1;

    /// #1566-1: the same file, ingested through two different spellings of
    /// its path (relative-with-`..` vs. absolute), must produce the exact
    /// same `durable_source` identity — because that string is what lands in
    /// both `metadata.ingest_source` and `evidence_refs_v1[].ref`. Before
    /// this fix, `source_for_path`'s local branch persisted the raw `source`
    /// argument verbatim (post allow-list check, which canonicalized
    /// separately and threw the result away), so the two spellings produced
    /// two different refs for one file.
    #[tokio::test]
    async fn local_ingest_dedupes_source_identity_across_path_spellings() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tachi_home_dir = tempfile::tempdir().expect("tachi_home tempdir");
        let sub_dir = tachi_home_dir.path().join("sub");
        std::fs::create_dir_all(&sub_dir).expect("create sub dir");
        let target_file = sub_dir.join("page.md");
        std::fs::write(&target_file, "# Page\n").expect("write fixture");

        let _tachi_env = EnvRestore::set_path("TACHI_HOME", tachi_home_dir.path());
        let _allow_any_off = EnvRestore::remove("TACHI_WIKI_INGEST_ALLOW_ANY_LOCAL_FILE");

        let absolute_spelling = target_file.to_string_lossy().into_owned();
        let roundabout_spelling = sub_dir
            .join("..")
            .join("sub")
            .join("page.md")
            .to_string_lossy()
            .into_owned();
        assert_ne!(
            absolute_spelling, roundabout_spelling,
            "test fixture must actually exercise two distinct spellings"
        );

        let via_absolute = source_for_path(tachi_home_dir.path(), absolute_spelling)
            .await
            .expect("absolute spelling should ingest");
        let via_roundabout = source_for_path(tachi_home_dir.path(), roundabout_spelling)
            .await
            .expect("`..`-containing spelling should ingest");

        assert_eq!(
            via_absolute.durable_source, via_roundabout.durable_source,
            "two spellings of the same file must canonicalize to one durable_source identity"
        );
    }

    /// #1566-2: canonicalizing a `docs/...`-relative input turns it
    /// absolute, which would otherwise silently drop the resulting
    /// `evidence_refs_v1` ref's `SourceKindV1::CanonicalDoc` classification
    /// (`tachi_params::classify_wiki_reference` only recognizes a literal
    /// `docs/` prefix, and an absolute canonical path never has one) — and
    /// would desync the same file's identity depending on which spelling
    /// ingested it. Asserts both invariants hold together: (a) an absolute
    /// spelling and a `docs/`-relative spelling of the same file still
    /// dedupe to one `durable_source` (the item-1 invariant), and (b) that
    /// shared identity is still classified `CanonicalDoc` (item 2 / B2).
    #[tokio::test]
    async fn local_ingest_of_docs_path_keeps_canonical_doc_classification() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let repo_root = tempfile::tempdir().expect("repo root tempdir");
        let docs_dir = repo_root.path().join("docs");
        std::fs::create_dir_all(&docs_dir).expect("create docs dir");
        let target_file = docs_dir.join("x.md");
        std::fs::write(&target_file, "# X\n").expect("write fixture");

        // `wiki_ingest_local_file_canonical`'s roots list pushes cwd first,
        // ahead of `tachi_home`/the env-var roots/`~/.tachi` — pin cwd to
        // `repo_root` so it is deterministically the matched root, and clear
        // the other roots so none of them can spuriously prefix-match first.
        let _cwd = CwdRestore::set(repo_root.path());
        let tachi_home_dir = tempfile::tempdir().expect("tachi_home tempdir");
        let _tachi_env = EnvRestore::remove("TACHI_HOME");
        let _sigil_env = EnvRestore::remove("SIGIL_HOME");
        let _app_env = EnvRestore::remove("TACHI_APP_HOME");
        let _allow_any_off = EnvRestore::remove("TACHI_WIKI_INGEST_ALLOW_ANY_LOCAL_FILE");

        let absolute_spelling = target_file.to_string_lossy().into_owned();
        let relative_spelling = "docs/x.md".to_string();

        let via_absolute = source_for_path(tachi_home_dir.path(), absolute_spelling)
            .await
            .expect("absolute spelling should ingest");
        let via_relative = source_for_path(tachi_home_dir.path(), relative_spelling)
            .await
            .expect("docs/-relative spelling should ingest");

        assert_eq!(
            via_absolute.durable_source, via_relative.durable_source,
            "absolute and docs/-relative spellings of the same file must dedupe to one identity"
        );
        assert_eq!(
            via_absolute.durable_source, "docs/x.md",
            "durable_source should be rendered relative to the matched cwd root, not absolute"
        );

        let evidence_refs = build_evidence_refs_v1(
            std::slice::from_ref(&via_absolute.durable_source),
            "2026-01-01T00:00:00.000Z",
        );
        assert_eq!(
            evidence_refs[0].target_kind,
            Some(SourceKindV1::CanonicalDoc),
            "canonicalized docs/ file must still classify as CanonicalDoc, not silently lose target_kind"
        );
    }

    /// #1566 r3 (cold-review point 1, real gap): the coverage above only
    /// exercises `matched_root` coming from cwd. Prove the *other* matching
    /// path — `matched_root` coming from the `tachi_home` argument while cwd
    /// is a completely unrelated directory outside the repo — still renders
    /// a `docs/`-relative identity and still classifies `CanonicalDoc`. This
    /// is the shape a daemon actually hits: launched from an arbitrary cwd,
    /// with `TACHI_HOME` as the stable configured root.
    #[tokio::test]
    async fn local_ingest_of_docs_path_via_tachi_home_root_outside_cwd_keeps_canonical_doc_classification(
    ) {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let repo_root = tempfile::tempdir().expect("repo root tempdir");
        let docs_dir = repo_root.path().join("docs");
        std::fs::create_dir_all(&docs_dir).expect("create docs dir");
        let target_file = docs_dir.join("x.md");
        std::fs::write(&target_file, "# X\n").expect("write fixture");

        // cwd is deliberately outside `repo_root` so it can never
        // prefix-match `target_file` — the only candidate root that can
        // admit it is the `tachi_home` argument itself.
        let cwd_outside = tempfile::tempdir().expect("unrelated cwd tempdir");
        let _cwd = CwdRestore::set(cwd_outside.path());
        let _tachi_env = EnvRestore::remove("TACHI_HOME");
        let _sigil_env = EnvRestore::remove("SIGIL_HOME");
        let _app_env = EnvRestore::remove("TACHI_APP_HOME");
        let _allow_any_off = EnvRestore::remove("TACHI_WIKI_INGEST_ALLOW_ANY_LOCAL_FILE");

        let absolute_spelling = target_file.to_string_lossy().into_owned();
        let via_absolute = source_for_path(repo_root.path(), absolute_spelling)
            .await
            .expect("absolute spelling should ingest via the tachi_home root");

        assert_eq!(
            via_absolute.durable_source, "docs/x.md",
            "matched_root came from the tachi_home argument (cwd is outside the repo), \
             and must still render docs/-relative"
        );

        let evidence_refs = build_evidence_refs_v1(
            std::slice::from_ref(&via_absolute.durable_source),
            "2026-01-01T00:00:00.000Z",
        );
        assert_eq!(
            evidence_refs[0].target_kind,
            Some(SourceKindV1::CanonicalDoc),
            "docs/ file admitted via the tachi_home root (not cwd) must still classify CanonicalDoc"
        );
    }

    /// #1566 r3 (cold-review point 1, real gap): before r3, a file admitted
    /// directly under an allow-list root (not inside a `docs/`/`skill/`
    /// subdirectory) rendered as a bare filename — the exact shape
    /// `write_wiki_ingest_source` in `tests/wiki_tests/ingest.rs` uses for
    /// its fixtures (`home.temp_home.join(".tachi").join(name)`, admitted
    /// directly under the `.tachi` root), and the shape that would let two
    /// different repos' same-named root file (or `allow_cross_project`
    /// letting two projects' same-named file) collide on one identity. Prove
    /// such a file keeps its canonical absolute identity instead.
    #[tokio::test]
    async fn local_ingest_of_non_vocabulary_root_file_keeps_absolute_identity() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let repo_root = tempfile::tempdir().expect("repo root tempdir");
        let target_file = repo_root.path().join("README.md");
        std::fs::write(&target_file, "# Readme\n").expect("write fixture");

        let _cwd = CwdRestore::set(repo_root.path());
        let tachi_home_dir = tempfile::tempdir().expect("tachi_home tempdir");
        let _tachi_env = EnvRestore::remove("TACHI_HOME");
        let _sigil_env = EnvRestore::remove("SIGIL_HOME");
        let _app_env = EnvRestore::remove("TACHI_APP_HOME");
        let _allow_any_off = EnvRestore::remove("TACHI_WIKI_INGEST_ALLOW_ANY_LOCAL_FILE");

        let absolute_spelling = target_file.to_string_lossy().into_owned();
        let via_absolute = source_for_path(tachi_home_dir.path(), absolute_spelling)
            .await
            .expect("absolute spelling should ingest");

        let expected_canonical = std::fs::canonicalize(&target_file)
            .expect("canonicalize test fixture")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            via_absolute.durable_source, expected_canonical,
            "a root-direct file outside docs/skill must keep its canonical absolute identity"
        );
        assert_ne!(
            via_absolute.durable_source, "README.md",
            "must never collapse to a bare filename — two different repos' same-named \
             root file would collide on one identity"
        );
    }

    /// #1566 r3 (cold-review point 2, real gap): the same absolute file,
    /// ingested once with cwd matching it as the root and once with cwd
    /// pointed somewhere unrelated (so the `tachi_home` argument matches
    /// instead), must render the exact same identity. This is the realistic
    /// form daemon-restart cwd drift takes: `TACHI_HOME` is a stable
    /// configuration value, but a process's cwd is not, and the pre-r3
    /// unconditional-relativization shape rendered a different string
    /// depending on which of the two admitted the file.
    #[tokio::test]
    async fn local_ingest_of_docs_path_is_identical_across_differing_cwd() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let repo_root = tempfile::tempdir().expect("repo root tempdir");
        let docs_dir = repo_root.path().join("docs");
        std::fs::create_dir_all(&docs_dir).expect("create docs dir");
        let target_file = docs_dir.join("x.md");
        std::fs::write(&target_file, "# X\n").expect("write fixture");
        let absolute_spelling = target_file.to_string_lossy().into_owned();

        let _tachi_env = EnvRestore::remove("TACHI_HOME");
        let _sigil_env = EnvRestore::remove("SIGIL_HOME");
        let _app_env = EnvRestore::remove("TACHI_APP_HOME");
        let _allow_any_off = EnvRestore::remove("TACHI_WIKI_INGEST_ALLOW_ANY_LOCAL_FILE");

        // cwd matches the file directly (repo_root itself wins the `find`).
        let identity_cwd_matches = {
            let _cwd = CwdRestore::set(repo_root.path());
            let tachi_home_dir = tempfile::tempdir().expect("tachi_home tempdir");
            source_for_path(tachi_home_dir.path(), absolute_spelling.clone())
                .await
                .expect("ingest via cwd-matched root")
                .durable_source
        };

        // cwd is unrelated to the file; only the `tachi_home` argument
        // (still `repo_root`) matches.
        let identity_tachi_home_matches = {
            let cwd_outside = tempfile::tempdir().expect("unrelated cwd tempdir");
            let _cwd = CwdRestore::set(cwd_outside.path());
            source_for_path(repo_root.path(), absolute_spelling.clone())
                .await
                .expect("ingest via tachi_home-matched root")
                .durable_source
        };

        assert_eq!(
            identity_cwd_matches, identity_tachi_home_matches,
            "the same file's identity must not depend on whether cwd or tachi_home \
             supplied the matched root"
        );
        assert_eq!(identity_cwd_matches, "docs/x.md");
    }

    /// #1566-2: `bound_wiki_ingest_durable_source` is the shared fail-closed
    /// gate both the HTTP and local branches route through. Directly
    /// unit-test its boundary since constructing an actual >4096-byte
    /// canonicalized filesystem path is impractical (macOS `PATH_MAX`/
    /// `NAME_MAX` are far smaller than the evidence-ref limit).
    #[test]
    fn bound_rejects_source_over_max_reference_bytes() {
        let oversized = "x".repeat(memcore::db::MAX_REFERENCE_BYTES + 1);
        let err = bound_wiki_ingest_durable_source(oversized).unwrap_err();
        assert!(err.contains("byte limit"), "err: {err}");

        let exactly_at_limit = "x".repeat(memcore::db::MAX_REFERENCE_BYTES);
        assert!(bound_wiki_ingest_durable_source(exactly_at_limit).is_ok());
    }

    /// #1566-3: the local branch must reject an oversized source identity
    /// before it ever attempts to open/read the file. This uses the
    /// `TACHI_WIKI_INGEST_ALLOW_ANY_LOCAL_FILE` escape hatch with a
    /// nonexistent path so the *only* way this test can pass is if the
    /// length bound is enforced ahead of the read — a raw file-open of this
    /// path would fail with a "no such file" I/O error, not a byte-limit
    /// error, if the ordering regressed.
    #[tokio::test]
    async fn local_ingest_rejects_oversized_source_before_reading_file() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let _allow_any = EnvRestore::set("TACHI_WIKI_INGEST_ALLOW_ANY_LOCAL_FILE", "1");
        let tachi_home_dir = tempfile::tempdir().expect("tachi_home tempdir");
        let oversized_source = format!(
            "/nonexistent/{}",
            "x".repeat(memcore::db::MAX_REFERENCE_BYTES)
        );

        let err = source_for_path(tachi_home_dir.path(), oversized_source)
            .await
            .expect_err("oversized local source must be rejected");
        assert!(err.contains("byte limit"), "err: {err}");
        assert!(
            !err.contains("read source file"),
            "must fail on the length bound, not a filesystem read: {err}"
        );
    }
}

#[cfg(test)]
mod immutable_supersession_tests {
    use super::*;

    fn wiki_entry(id: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: format!("/wiki/general/{id}"),
            summary: format!("wiki summary {id}"),
            text: format!("wiki body {id}"),
            importance: 0.7,
            timestamp: chrono::Utc::now().to_rfc3339(),
            valid_from: String::new(),
            valid_until: None,
            category: "experience".to_string(),
            topic: id.to_string(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "wiki".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            metadata: serde_json::json!({"wiki": true}),
            vector: None,
            retention_policy: Some("permanent".to_string()),
            domain: Some("wiki".to_string()),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn replacement_selects_current_active_predecessor_inside_transaction() {
        let temp = tempfile::tempdir().expect("wiki immutable-edge tempdir");
        let db_path = temp.path().join("wiki.db");
        let mut store = MemoryStore::open(db_path.to_str().expect("utf8 db path"))
            .expect("open wiki test store");
        let old = wiki_entry("old-wiki-entry");
        let canonical = wiki_entry("canonical-wiki-entry");
        let mut candidate = wiki_entry("fresh-wiki-candidate");
        candidate.path = canonical.path.clone();
        candidate.topic = canonical.topic.clone();
        store.upsert(&old).expect("seed old wiki entry");
        store.upsert(&canonical).expect("seed canonical wiki entry");
        assert!(store
            .supersede_memory(&old.id, &canonical.id)
            .expect("seed immutable predecessor edge"));

        persist_wiki_ingest_entry(&mut store, &candidate, &[], None, &[])
            .expect("replacement must select and claim the current active winner");
        let old_after = store
            .get_with_options(&old.id, true)
            .expect("read old wiki entry")
            .expect("old wiki entry remains");
        assert!(
            !old_after.archived,
            "historical predecessor must not be rewritten"
        );
        assert_eq!(
            store
                .supersession_target(&old.id)
                .expect("read predecessor edge"),
            Some(Some(canonical.id.clone())),
            "historical predecessor edge must remain immutable"
        );
        assert_eq!(
            store
                .supersession_target(&canonical.id)
                .expect("read current winner edge"),
            Some(Some(candidate.id.clone())),
            "replacement must claim the winner observed in its transaction"
        );
        assert!(store.get(&candidate.id).expect("read candidate").is_some());
    }

    #[test]
    fn replacement_supersedes_same_topic_wiki_predecessor_at_legacy_path() {
        let temp = tempfile::tempdir().expect("wiki same-topic replacement tempdir");
        let db_path = temp.path().join("wiki.db");
        let mut store = MemoryStore::open(db_path.to_str().expect("utf8 db path"))
            .expect("open wiki test store");
        let mut legacy = wiki_entry("legacy-topic-path");
        legacy.path = "/wiki/general/trendlock-legacy".to_string();
        legacy.topic = "trendlock".to_string();
        legacy.domain = None;
        let mut replacement = wiki_entry("current-topic-path");
        replacement.path = "/wiki/general/trendlock".to_string();
        replacement.topic = legacy.topic.clone();
        let mut guide = wiki_entry("same-topic-guide");
        guide.path = "/guide/global/trendlock".to_string();
        guide.topic = legacy.topic.clone();
        guide.category = "guide".to_string();
        store
            .upsert(&legacy)
            .expect("seed legacy Wiki predecessor without a domain tag");
        store.upsert(&guide).expect("seed same-topic Guide");

        persist_wiki_ingest_entry(&mut store, &replacement, &[], None, &[])
            .expect("same-topic Wiki predecessor must be replaced atomically");

        let legacy_after = store
            .get_with_options(&legacy.id, true)
            .expect("read legacy predecessor")
            .expect("legacy predecessor remains auditable");
        assert!(legacy_after.archived);
        assert_eq!(
            store
                .supersession_target(&legacy.id)
                .expect("read legacy predecessor supersession"),
            Some(Some(replacement.id.clone())),
            "legacy same-topic predecessor must not remain active"
        );
        assert!(store
            .get(&replacement.id)
            .expect("read replacement")
            .is_some());
        let guide_after = store
            .get(&guide.id)
            .expect("read same-topic Guide")
            .expect("same-topic Guide remains");
        assert!(!guide_after.archived);
        assert_eq!(
            store
                .supersession_target(&guide.id)
                .expect("read Guide supersession"),
            Some(None),
            "Wiki ingest must not classify a Guide as its predecessor"
        );
    }

    #[test]
    fn replacement_stays_canonical_when_an_unrelated_jaccard_candidate_exists() {
        let temp = tempfile::tempdir().expect("wiki replacement jaccard tempdir");
        let db_path = temp.path().join("wiki.db");
        let mut store = MemoryStore::open(db_path.to_str().expect("utf8 db path"))
            .expect("open wiki test store");
        let mut old = wiki_entry("old-replacement-target");
        old.text = "The predecessor has deliberately unrelated content.".to_string();
        let mut near_duplicate = wiki_entry("unrelated-near-duplicate");
        near_duplicate.text =
            "Canonical ingest content must remain the active replacement winner.".to_string();
        let mut replacement = wiki_entry("fresh-replacement");
        replacement.path = old.path.clone();
        replacement.topic = old.topic.clone();
        replacement.text = near_duplicate.text.clone();
        store.upsert(&old).expect("seed predecessor");
        store.upsert(&near_duplicate).expect("seed near duplicate");

        persist_wiki_ingest_entry(&mut store, &replacement, &[], None, &[])
            .expect("persist canonical replacement");

        assert_eq!(
            store
                .supersession_target(&replacement.id)
                .expect("read replacement supersession"),
            Some(None),
            "replacement must not be inserted as a generic Jaccard loser"
        );
        assert_eq!(
            store
                .supersession_target(&old.id)
                .expect("read predecessor supersession"),
            Some(Some(replacement.id.clone()))
        );
    }

    #[test]
    fn replacement_does_not_relate_to_the_predecessor_it_archives() {
        let temp = tempfile::tempdir().expect("wiki replacement relation tempdir");
        let db_path = temp.path().join("wiki.db");
        let mut store = MemoryStore::open(db_path.to_str().expect("utf8 db path"))
            .expect("open wiki test store");
        let old = wiki_entry("related-predecessor");
        let mut replacement = wiki_entry("fresh-related-replacement");
        replacement.path = old.path.clone();
        replacement.topic = old.topic.clone();
        store.upsert(&old).expect("seed predecessor");
        let edge = memcore::MemoryEdge {
            source_id: replacement.id.clone(),
            target_id: old.id.clone(),
            relation: "references".to_string(),
            weight: 0.6,
            metadata: serde_json::json!({"wiki_ingest": true}),
            created_at: chrono::Utc::now().to_rfc3339(),
            valid_from: String::new(),
            valid_to: None,
        };

        let committed_related =
            persist_wiki_ingest_entry(&mut store, &replacement, &[], None, &[edge])
                .expect("persist replacement without stale relation");

        assert!(committed_related.is_empty());
        let edges = store
            .get_edges(&replacement.id, "outgoing", Some("references"))
            .expect("read replacement edges");
        assert!(
            edges.iter().all(|edge| edge.target_id != old.id),
            "the archived predecessor is not a truthful related target"
        );
    }

    #[test]
    fn replacement_revalidates_related_corpus_lifecycle_and_entities_in_transaction() {
        let temp = tempfile::tempdir().expect("wiki related revalidation tempdir");
        let db_path = temp.path().join("wiki.db");
        let mut store = MemoryStore::open(db_path.to_str().expect("utf8 db path"))
            .expect("open wiki test store");

        let mut replacement = wiki_entry("fresh-related-candidate");
        replacement.entities = vec!["SharedEntity".to_string()];

        let mut valid = wiki_entry("active-related-target");
        valid.path = "/wiki/general/active-related-target".to_string();
        valid.entities = vec!["sharedentity".to_string()];

        let mut rem_draft = wiki_entry("wiki-rem:pending-related-target");
        rem_draft.path = "/wiki/drafts/rem-pending-related-target".to_string();
        rem_draft.entities = vec!["SharedEntity".to_string()];
        rem_draft.metadata = serde_json::json!({
            "wiki": true,
            "lifecycle": "pending_review",
            "rem": {
                "producer": "weekly_wiki_evolver",
                "operation_id": rem_draft.id.clone(),
                "operation_status": "pending_sources"
            }
        });

        let mut pending = wiki_entry("pending-related-target");
        pending.path = "/wiki/general/pending-related-target".to_string();
        pending.entities = vec!["SharedEntity".to_string()];
        pending.metadata = serde_json::json!({"wiki": true, "lifecycle": "pending_review"});

        let mut entity_drifted = wiki_entry("entity-drifted-related-target");
        entity_drifted.path = "/wiki/general/entity-drifted-related-target".to_string();
        entity_drifted.entities = vec!["NoLongerShared".to_string()];

        store
            .with_immutable_supersession_transaction(|operation| {
                operation
                    .insert_rem_operation_if_absent(&rem_draft)
                    .map(|_| ())
            })
            .expect("seed REM related candidate");
        for entry in [&valid, &pending, &entity_drifted] {
            store.upsert(entry).expect("seed related candidate");
        }
        let edges = [&valid, &rem_draft, &pending, &entity_drifted]
            .into_iter()
            .map(|target| memcore::MemoryEdge {
                source_id: replacement.id.clone(),
                target_id: target.id.clone(),
                relation: "references".to_string(),
                weight: 0.6,
                metadata: serde_json::json!({"wiki_ingest": true}),
                created_at: chrono::Utc::now().to_rfc3339(),
                valid_from: String::new(),
                valid_to: None,
            })
            .collect::<Vec<_>>();

        let committed = persist_wiki_ingest_entry(&mut store, &replacement, &[], None, &edges)
            .expect("persist replacement with transactionally revalidated relations");

        assert_eq!(committed, vec![valid.id.clone()]);
        let persisted = store
            .get_edges(&replacement.id, "outgoing", Some("references"))
            .expect("read committed related edges");
        assert_eq!(
            persisted
                .into_iter()
                .map(|edge| edge.target_id)
                .collect::<Vec<_>>(),
            vec![valid.id]
        );
    }
}
