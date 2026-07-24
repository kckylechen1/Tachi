//! #1382 — the live GitHub side of server-verified human approver authority.
//!
//! The decision itself lives in `tachi_params::approver_authority`, which is
//! pure. This module is the only place that talks to GitHub for it: it
//! implements [`tachi_params::ApproverAuthorityProbe`] on top of the same
//! hardened `gh` invocation path `gh_ops` already uses (env-cleared command,
//! allowlisted environment, Vault-or-env token, token redaction on every
//! captured byte), and it loads the owner-controlled authorization policy
//! from server-process configuration.
//!
//! ## Trust root, stated plainly
//!
//! * **Who the principal is** comes from `GET /user` — the account the
//!   *active credential* authenticates. A caller-supplied name never
//!   participates.
//! * **What that principal may do** comes from `GET /repos/{owner}/{repo}`
//!   (whose `permissions` object GitHub scopes to the authenticated user)
//!   and, for delegated approvers, from
//!   `GET /orgs/{org}/teams/{slug}/memberships/{login}`. Both are read live
//!   on every issuance and again on every revalidation, which is what makes
//!   revocation take effect rather than expire.
//! * **Which teams are authorized** comes from the process environment (see
//!   [`load_policy_from_env`]). That is owner-controlled configuration a
//!   remote MCP caller cannot reach; it is exactly as trustworthy as the
//!   process environment of the daemon, and no more. Its canonical hash is
//!   the receipt's `authorization_revision`, so changing it invalidates
//!   every receipt issued under the old rules.
//!
//! ## Everything that goes wrong is a refusal
//!
//! Every method here returns `Result<_, AuthorityDenialV1>`. A DNS failure,
//! a TLS error, a timeout, an HTTP 403 rate limit, a 5xx, a body that will
//! not parse, or a missing credential all become
//! [`tachi_params::AuthorityDenialV1::AuthorityUnavailable`] — a named,
//! `Display`-able refusal. There is no code path in this file that turns a
//! failed probe into `false`, `None`, `default()`, or a skipped check.
//!
//! HTTP 404 on a team-membership probe is the one *decided* negative
//! (`NotMember`); it is recognized by a positive match on `HTTP 404` in the
//! captured stderr. Misreading a transport error as 404 could at worst
//! continue to the next authorized team, and continuing can only grant
//! authority via a team the principal is genuinely an active member of — it
//! cannot manufacture authority that live evidence does not support.
//!
//! ## Response shapes: verified, not assumed (2026-07-25, live GitHub)
//!
//! * `gh api <missing resource>` exits non-zero and writes
//!   `gh: Not Found (HTTP 404)` to stderr (checked on both a missing repo
//!   and a missing team membership) — that exact substring is what
//!   [`GhApiOutcome::NotFound`] keys on.
//! * `/user` carries `login` (string), `id` (integer), `node_id` (string),
//!   `type` (string).
//! * `/repos/{owner}/{repo}` carries `full_name` (string), `owner.login`
//!   (string), `owner.id` (integer), `owner.type` (string), and a
//!   `permissions` object with boolean `admin`/`maintain`/`push`/`triage`/
//!   `pull`.
//! * `repos/{owner}/{repo}/commits/refs/heads/<branch>` is accepted with the
//!   slash-bearing ref inline and answers with `sha`.
//!
//! ## Not wired yet
//!
//! #1077 owns the governed establishment/overturn transition that must call
//! [`authorize_governed_mutation`] at its mutation choke point and
//! [`revalidate_governed_mutation`] immediately before it writes. At this
//! commit no such transition exists in the crate (there is no precedent
//! *apply* path yet — `precedent_ops` and `precedent_candidate_ops` only
//! capture caller-supplied rulings as memory rows), so this module has no
//! production caller. This *module* is declared `pub` (in `lib.rs`) for the
//! same reason `exec_env_postflight` is: the gate must exist and be
//! reviewable before the path it gates is built. The choke-point functions
//! themselves are `pub(crate)`, not `pub`, because they take `&MemoryServer`
//! — itself `pub(crate)` — so #1077's caller must live inside this crate
//! regardless of what visibility these functions declare.

use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::Value;

use tachi_params::{
    resolve_verified_approver, revalidate_approval, ApprovalReceiptV1, ApprovalTargetV1,
    ApproverAuthorityProbe, ApproverAuthorizationPolicyV1, AuthorityDenialV1, AuthorizedTeamV1,
    CallerAssertedContextV1, CredentialContextV1, CurrentApprovalContextV1, RepoFactsV1,
    RepoPermissionLevelV1, RepoPermissionV1, RepoRevisionV1, TeamMembershipProbeV1,
    TeamMembershipV1, TeamRoleV1, VerifiedPrincipalV1,
};

use crate::gh_ops::{gh_api_command, gh_redact};
use crate::MemoryServer;

/// Wall-clock ceiling for a single GitHub probe. Without it there is no
/// timeout branch at all, only an indefinite hang at a mutation choke point;
/// the contract requires timeouts to be a named refusal.
const GH_PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// How often the spawned `gh` process is polled for exit.
const GH_PROBE_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Non-secret label recorded as the credential's `source`. It names the
/// resolution path (`gh_ops::resolve_gh_token`, which prefers the Vault
/// secret and falls back to `GH_TOKEN`/`GITHUB_TOKEN` in the environment),
/// **not** which of those two actually answered — that distinction is not
/// observable from `build_gh_command`'s return value, and inventing it would
/// be a claim this module cannot support. Credential *change* detection does
/// not depend on it; the fingerprint carries that.
const CREDENTIAL_SOURCE: &str = "gh_ops::resolve_gh_token";

fn unavailable(probe: &str, detail: impl Into<String>) -> AuthorityDenialV1 {
    AuthorityDenialV1::AuthorityUnavailable {
        probe: probe.to_string(),
        detail: detail.into(),
    }
}

// ─── path-segment validation ────────────────────────────────────────────────

/// Reject anything that could steer a `gh api` path somewhere other than the
/// intended resource. Owner/repo/org/team/login segments are drawn from
/// GitHub's own name grammar, which is a strict subset of this.
fn validate_path_segment(kind: &str, value: &str) -> Result<(), AuthorityDenialV1> {
    if value.is_empty() {
        return Err(unavailable("path_validation", format!("{kind} is empty")));
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(unavailable(
            "path_validation",
            format!("{kind} '{value}' contains characters outside [A-Za-z0-9._-]"),
        ));
    }
    if value == "." || value == ".." {
        return Err(unavailable(
            "path_validation",
            format!("{kind} '{value}' is a relative path element"),
        ));
    }
    Ok(())
}

/// `owner/repo`, validated one segment at a time so neither half can smuggle
/// a slash, a query string, or a traversal element into the API path.
fn split_repo(repo: &str) -> Result<(String, String), AuthorityDenialV1> {
    let mut parts = repo.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if parts.next().is_some() {
        return Err(unavailable(
            "path_validation",
            format!("repo '{repo}' is not exactly 'owner/name'"),
        ));
    }
    validate_path_segment("repo owner", owner)?;
    validate_path_segment("repo name", name)?;
    Ok((owner.to_string(), name.to_string()))
}

/// Git refs legitimately contain `/` (`refs/heads/main`), so this is looser
/// than [`validate_path_segment`] — but it still refuses traversal, leading
/// or doubled slashes, whitespace, and query/fragment characters.
fn validate_git_ref(git_ref: &str) -> Result<(), AuthorityDenialV1> {
    if git_ref.is_empty() {
        return Err(unavailable("path_validation", "git ref is empty"));
    }
    if git_ref.starts_with('/') || git_ref.ends_with('/') || git_ref.contains("//") {
        return Err(unavailable(
            "path_validation",
            format!("git ref '{git_ref}' has an empty path segment"),
        ));
    }
    if git_ref.contains("..") {
        return Err(unavailable(
            "path_validation",
            format!("git ref '{git_ref}' contains a traversal element"),
        ));
    }
    if !git_ref
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
    {
        return Err(unavailable(
            "path_validation",
            format!("git ref '{git_ref}' contains characters outside [A-Za-z0-9._/-]"),
        ));
    }
    Ok(())
}

// ─── the one gh invocation ──────────────────────────────────────────────────

/// What a single `gh api` call produced. `NotFound` is separated from
/// `Failed` so a team-membership probe can report a decided "not a member"
/// while every other failure stays an availability refusal.
enum GhApiOutcome {
    Body { stdout: String, token: String },
    NotFound { stderr: String },
    Failed { detail: String },
}

/// Run `gh api <path>` with a hard deadline, returning redacted output.
///
/// Bounded-output assumption, stated because it is load-bearing: the four
/// endpoints this module calls (`/user`, `/repos/{o}/{r}`, a team membership,
/// and a single commit) each return a few kilobytes, comfortably inside the
/// OS pipe buffer, so the child can exit without a concurrent reader. If a
/// response ever did exceed the buffer the child would block, the deadline
/// would fire, and the call would become an availability refusal — the
/// failure direction is closed, not open.
fn run_gh_api(server: &MemoryServer, path: &str) -> GhApiOutcome {
    let (mut cmd, token) = match gh_api_command(server, &[path]) {
        Ok(pair) => pair,
        // `build_gh_command` failures name a binary or a secret *name*, never
        // a secret value.
        Err(err) => {
            return GhApiOutcome::Failed {
                detail: format!("could not build the `gh` command: {err}"),
            }
        }
    };

    if token.trim().is_empty() {
        return GhApiOutcome::Failed {
            detail: "no explicit GitHub credential is available (Vault `GH_TOKEN`, or \
                     `GH_TOKEN`/`GITHUB_TOKEN` in the daemon environment). Approval authority \
                     must be bound to a credential context this gate can identify, and a `gh` \
                     keyring session cannot be pinned, so this is a refusal rather than an \
                     unpinned approval"
                .to_string(),
        };
    }

    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            return GhApiOutcome::Failed {
                detail: format!("could not spawn `gh`: {err}"),
            }
        }
    };

    let deadline = Instant::now() + GH_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return GhApiOutcome::Failed {
                        detail: format!(
                            "`gh api {path}` did not finish within {}s and was killed",
                            GH_PROBE_TIMEOUT.as_secs()
                        ),
                    };
                }
                std::thread::sleep(GH_PROBE_POLL_INTERVAL);
            }
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return GhApiOutcome::Failed {
                    detail: format!("could not wait on `gh`: {err}"),
                };
            }
        }
    }

    let output = match child.wait_with_output() {
        Ok(output) => output,
        Err(err) => {
            return GhApiOutcome::Failed {
                detail: format!("could not read `gh` output: {err}"),
            }
        }
    };

    let stdout = gh_redact(&String::from_utf8_lossy(&output.stdout), &token);
    let stderr = gh_redact(&String::from_utf8_lossy(&output.stderr), &token);

    if output.status.success() {
        return GhApiOutcome::Body { stdout, token };
    }
    if stderr.contains("HTTP 404") {
        return GhApiOutcome::NotFound { stderr };
    }
    GhApiOutcome::Failed {
        detail: format!(
            "`gh api {path}` failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            stderr.chars().take(500).collect::<String>()
        ),
    }
}

/// Fetch and parse a JSON body, or refuse. Any non-success outcome — including
/// a 404 — is an availability refusal here; only
/// [`GhApproverAuthorityProbe::team_membership`] treats 404 as a decided
/// negative, and it calls [`run_gh_api`] directly for exactly that reason.
fn gh_api_json(
    server: &MemoryServer,
    probe: &str,
    path: &str,
) -> Result<(Value, String), AuthorityDenialV1> {
    match run_gh_api(server, path) {
        GhApiOutcome::Body { stdout, token } => {
            let value: Value = serde_json::from_str(&stdout).map_err(|err| {
                unavailable(
                    probe,
                    format!("`gh api {path}` returned unparseable JSON: {err}"),
                )
            })?;
            Ok((value, token))
        }
        GhApiOutcome::NotFound { stderr } => Err(unavailable(
            probe,
            format!("`gh api {path}` returned HTTP 404: {stderr}"),
        )),
        GhApiOutcome::Failed { detail } => Err(unavailable(probe, detail)),
    }
}

fn field_str(
    value: &Value,
    probe: &str,
    path: &str,
    key: &str,
) -> Result<String, AuthorityDenialV1> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| unavailable(probe, format!("`{path}` response has no string `{key}`")))
}

fn field_u64(value: &Value, probe: &str, path: &str, key: &str) -> Result<u64, AuthorityDenialV1> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| unavailable(probe, format!("`{path}` response has no integer `{key}`")))
}

/// A permission bit GitHub omitted is a bit it did not grant. The enclosing
/// `permissions` object being absent, by contrast, is an unparseable
/// response and refuses in [`GhApproverAuthorityProbe::repo_facts`].
fn permission_bit(permissions: &Value, key: &str) -> bool {
    permissions
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ─── the probe ──────────────────────────────────────────────────────────────

/// Live GitHub implementation of [`ApproverAuthorityProbe`].
pub struct GhApproverAuthorityProbe<'a> {
    server: &'a MemoryServer,
}

impl<'a> GhApproverAuthorityProbe<'a> {
    /// `pub(crate)`, not `pub`: `MemoryServer` is itself `pub(crate)`
    /// (`crates/tachi-server/src/lib.rs`'s `pub(crate) use
    /// server_state::{..., MemoryServer, ...}`), so a wider visibility here
    /// would be unreachable from outside the crate anyway and trips
    /// the rustc `private_interfaces` lint.
    pub(crate) fn new(server: &'a MemoryServer) -> Self {
        Self { server }
    }
}

impl ApproverAuthorityProbe for GhApproverAuthorityProbe<'_> {
    fn authenticated_principal(&self) -> Result<VerifiedPrincipalV1, AuthorityDenialV1> {
        let probe = "authenticated_principal";
        let (value, token) = gh_api_json(self.server, probe, "user")?;
        Ok(VerifiedPrincipalV1 {
            login: field_str(&value, probe, "user", "login")?,
            user_id: field_u64(&value, probe, "user", "id")?,
            node_id: field_str(&value, probe, "user", "node_id")?,
            account_type: field_str(&value, probe, "user", "type")?,
            credential_context: CredentialContextV1 {
                source: CREDENTIAL_SOURCE.to_string(),
                credential_fingerprint: tachi_params::credential_fingerprint(&token),
            },
            verified_at: now_rfc3339(),
        })
    }

    fn repo_facts(&self, repo: &str) -> Result<RepoFactsV1, AuthorityDenialV1> {
        let probe = "repo_facts";
        let (owner, name) = split_repo(repo)?;
        let path = format!("repos/{owner}/{name}");
        let (value, _token) = gh_api_json(self.server, probe, &path)?;

        let owner_obj = value
            .get("owner")
            .ok_or_else(|| unavailable(probe, format!("`{path}` response has no `owner`")))?;
        let permissions = value.get("permissions").ok_or_else(|| {
            unavailable(
                probe,
                format!(
                    "`{path}` response has no `permissions` object; the authenticated \
                     principal's live permission could not be read"
                ),
            )
        })?;

        Ok(RepoFactsV1 {
            full_name: field_str(&value, probe, &path, "full_name")?,
            owner_login: field_str(owner_obj, probe, &path, "login")?,
            owner_id: field_u64(owner_obj, probe, &path, "id")?,
            owner_type: field_str(owner_obj, probe, &path, "type")?,
            permissions: RepoPermissionV1 {
                admin: permission_bit(permissions, "admin"),
                maintain: permission_bit(permissions, "maintain"),
                push: permission_bit(permissions, "push"),
                triage: permission_bit(permissions, "triage"),
                pull: permission_bit(permissions, "pull"),
            },
            observed_at: now_rfc3339(),
        })
    }

    fn team_membership(
        &self,
        org: &str,
        team_slug: &str,
        login: &str,
    ) -> Result<TeamMembershipProbeV1, AuthorityDenialV1> {
        let probe = "team_membership";
        validate_path_segment("org", org)?;
        validate_path_segment("team slug", team_slug)?;
        validate_path_segment("login", login)?;
        let path = format!("orgs/{org}/teams/{team_slug}/memberships/{login}");

        match run_gh_api(self.server, &path) {
            GhApiOutcome::NotFound { .. } => Ok(TeamMembershipProbeV1::NotMember {
                org: org.to_string(),
                team_slug: team_slug.to_string(),
                login: login.to_string(),
            }),
            GhApiOutcome::Failed { detail } => Err(unavailable(probe, detail)),
            GhApiOutcome::Body { stdout, .. } => {
                let value: Value = serde_json::from_str(&stdout).map_err(|err| {
                    unavailable(probe, format!("`{path}` returned unparseable JSON: {err}"))
                })?;
                Ok(TeamMembershipProbeV1::Member(TeamMembershipV1 {
                    org: org.to_string(),
                    team_slug: team_slug.to_string(),
                    login: login.to_string(),
                    state: field_str(&value, probe, &path, "state")?,
                    role: field_str(&value, probe, &path, "role")?,
                    observed_at: now_rfc3339(),
                }))
            }
        }
    }

    fn repo_revision(
        &self,
        repo: &str,
        git_ref: &str,
    ) -> Result<RepoRevisionV1, AuthorityDenialV1> {
        let probe = "repo_revision";
        let (owner, name) = split_repo(repo)?;
        validate_git_ref(git_ref)?;
        let path = format!("repos/{owner}/{name}/commits/{git_ref}");
        let (value, _token) = gh_api_json(self.server, probe, &path)?;
        Ok(RepoRevisionV1 {
            repo: repo.to_string(),
            git_ref: git_ref.to_string(),
            commit_sha: field_str(&value, probe, &path, "sha")?,
            verified_at: now_rfc3339(),
        })
    }
}

// ─── owner-controlled policy ────────────────────────────────────────────────

/// `TACHI_APPROVER_POLICY_ID` — owner label carried into the authorization
/// revision.
pub const ENV_POLICY_ID: &str = "TACHI_APPROVER_POLICY_ID";
/// `TACHI_APPROVER_ALLOW_REPO_OWNER` — `true`/`false`.
pub const ENV_ALLOW_REPO_OWNER: &str = "TACHI_APPROVER_ALLOW_REPO_OWNER";
/// `TACHI_APPROVER_TEAMS` — comma-separated `org/team:role` entries.
pub const ENV_TEAMS: &str = "TACHI_APPROVER_TEAMS";
/// `TACHI_APPROVER_REQUIRED_PERMISSION` — `admin` | `maintain` | `push`.
pub const ENV_REQUIRED_PERMISSION: &str = "TACHI_APPROVER_REQUIRED_PERMISSION";
/// `TACHI_APPROVER_MAX_RECEIPT_AGE_SECS`.
pub const ENV_MAX_RECEIPT_AGE_SECS: &str = "TACHI_APPROVER_MAX_RECEIPT_AGE_SECS";
/// `TACHI_APPROVER_FUTURE_SKEW_SECS`.
pub const ENV_FUTURE_SKEW_SECS: &str = "TACHI_APPROVER_FUTURE_SKEW_SECS";

const DEFAULT_POLICY_ID: &str = "tachi/governed-precedent-gate";

fn policy_unusable(detail: impl Into<String>) -> AuthorityDenialV1 {
    AuthorityDenialV1::PolicyUnusable {
        detail: detail.into(),
    }
}

fn env_value(key: &str) -> Option<String> {
    std::env::var(key).ok().and_then(|raw| {
        let trimmed = raw.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

/// Explicit boolean vocabulary. An unrecognized value is a configuration
/// error, not a silent `false` and certainly not a silent `true`.
fn parse_bool(key: &str, raw: &str) -> Result<bool, AuthorityDenialV1> {
    match raw.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => Err(policy_unusable(format!(
            "{key} must be one of true/false/1/0/yes/no/on/off, got '{other}'"
        ))),
    }
}

/// Parse `org/team:role` — the role suffix is mandatory, because defaulting
/// it would silently pick a delegation strength the owner never wrote down.
pub fn parse_authorized_team(entry: &str) -> Result<AuthorizedTeamV1, AuthorityDenialV1> {
    let (path, role) = entry.rsplit_once(':').ok_or_else(|| {
        policy_unusable(format!(
            "{ENV_TEAMS} entry '{entry}' must be 'org/team:member' or 'org/team:maintainer'"
        ))
    })?;
    let (org, team_slug) = path.split_once('/').ok_or_else(|| {
        policy_unusable(format!(
            "{ENV_TEAMS} entry '{entry}' must name the team as 'org/team'"
        ))
    })?;
    let required_role = match role.trim().to_ascii_lowercase().as_str() {
        "member" => TeamRoleV1::Member,
        "maintainer" => TeamRoleV1::Maintainer,
        other => {
            return Err(policy_unusable(format!(
                "{ENV_TEAMS} entry '{entry}' has role '{other}'; only 'member' and 'maintainer' \
                 are recognized"
            )))
        }
    };
    let org = org.trim();
    let team_slug = team_slug.trim();
    if org.is_empty() || team_slug.is_empty() {
        return Err(policy_unusable(format!(
            "{ENV_TEAMS} entry '{entry}' has an empty org or team slug"
        )));
    }
    Ok(AuthorizedTeamV1 {
        org: org.to_string(),
        team_slug: team_slug.to_string(),
        required_role,
    })
}

fn parse_permission_level(raw: &str) -> Result<RepoPermissionLevelV1, AuthorityDenialV1> {
    match raw.to_ascii_lowercase().as_str() {
        "admin" => Ok(RepoPermissionLevelV1::Admin),
        "maintain" => Ok(RepoPermissionLevelV1::Maintain),
        "push" => Ok(RepoPermissionLevelV1::Push),
        other => Err(policy_unusable(format!(
            "{ENV_REQUIRED_PERMISSION} must be admin/maintain/push, got '{other}'. Read and \
             triage access are deliberately not expressible as an approval floor"
        ))),
    }
}

fn parse_secs(key: &str, raw: &str) -> Result<i64, AuthorityDenialV1> {
    raw.parse::<i64>().map_err(|err| {
        policy_unusable(format!("{key} must be an integer number of seconds: {err}"))
    })
}

/// Build the owner-authorized policy from the daemon's environment.
///
/// Every recognized variable is optional, but a *present and malformed* value
/// is a hard refusal rather than a fall-back to the default: quietly
/// substituting a default for a value the owner did mean to set is exactly
/// the silent degradation this contract forbids. The resulting policy is
/// validated (including the receipt-lifetime ceilings) before it is returned.
pub fn load_policy_from_env() -> Result<ApproverAuthorizationPolicyV1, AuthorityDenialV1> {
    build_policy(PolicyInputs {
        policy_id: env_value(ENV_POLICY_ID),
        allow_repository_owner: env_value(ENV_ALLOW_REPO_OWNER),
        teams: env_value(ENV_TEAMS),
        required_permission: env_value(ENV_REQUIRED_PERMISSION),
        max_receipt_age_secs: env_value(ENV_MAX_RECEIPT_AGE_SECS),
        future_skew_tolerance_secs: env_value(ENV_FUTURE_SKEW_SECS),
    })
}

/// The raw, already-read configuration strings. Separating this from the
/// environment read is what lets the whole parse-and-validate composition be
/// tested without mutating process state.
#[derive(Debug, Clone, Default)]
pub struct PolicyInputs {
    pub policy_id: Option<String>,
    pub allow_repository_owner: Option<String>,
    pub teams: Option<String>,
    pub required_permission: Option<String>,
    pub max_receipt_age_secs: Option<String>,
    pub future_skew_tolerance_secs: Option<String>,
}

/// Parse and validate an authorization policy from raw configuration
/// strings. Absent values take documented defaults; **present** values that
/// do not parse are refusals, never silent fallbacks.
pub fn build_policy(
    inputs: PolicyInputs,
) -> Result<ApproverAuthorizationPolicyV1, AuthorityDenialV1> {
    let policy_id = inputs
        .policy_id
        .unwrap_or_else(|| DEFAULT_POLICY_ID.to_string());

    // The only default in this function that widens rather than narrows, and
    // it is called out deliberately: unset means "the verified repository
    // owner may approve", which is the ratified trust root's primary basis
    // and the narrowest non-empty policy that exists. It grants nothing to a
    // principal GitHub does not confirm is the owner, and it authorizes no
    // delegate at all — `authorized_teams` still defaults to empty.
    //
    // The justification is the ratified decision itself, NOT convenience:
    // #1382 names verified repository ownership as a trust root, so enabling
    // it by default enacts that decision rather than requiring an operator to
    // switch the decision on. Defaulting to `false` would not be "safer" —
    // a policy that authorizes nobody is a loud refusal at `validate()`, and
    // that crate's own note (`tachi_params::approver_authority`, at the
    // `validate` PolicyUnusable arm) calls such a policy *safe* precisely
    // because it fails closed. Choosing `true` trades none of that away; it
    // only declines to make the ratified trust root opt-in.
    let allow_repository_owner = match inputs.allow_repository_owner.as_deref() {
        Some(raw) => parse_bool(ENV_ALLOW_REPO_OWNER, raw)?,
        None => true,
    };

    let mut authorized_teams = Vec::new();
    if let Some(raw) = inputs.teams.as_deref() {
        for entry in raw.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            authorized_teams.push(parse_authorized_team(entry)?);
        }
    }

    let required_permission = match inputs.required_permission.as_deref() {
        Some(raw) => parse_permission_level(raw)?,
        None => RepoPermissionLevelV1::Admin,
    };

    let max_receipt_age_secs = match inputs.max_receipt_age_secs.as_deref() {
        Some(raw) => parse_secs(ENV_MAX_RECEIPT_AGE_SECS, raw)?,
        None => 300,
    };

    let future_skew_tolerance_secs = match inputs.future_skew_tolerance_secs.as_deref() {
        Some(raw) => parse_secs(ENV_FUTURE_SKEW_SECS, raw)?,
        None => 60,
    };

    let policy = ApproverAuthorizationPolicyV1 {
        policy_id,
        allow_repository_owner,
        authorized_teams,
        required_permission,
        max_receipt_age_secs,
        future_skew_tolerance_secs,
    };
    policy.validate()?;
    Ok(policy)
}

// ─── the choke-point entry points ───────────────────────────────────────────

/// Issue an approval receipt for a governed mutation, or refuse loudly.
///
/// `caller_asserted` is recorded on the receipt and hashed into it; it is
/// never consulted when deciding authority.
///
/// `pub(crate)`, not `pub`: it takes `&MemoryServer`, which is itself
/// `pub(crate)`, so a wider visibility would be unreachable from outside the
/// crate and trips the rustc `private_interfaces` lint.
///
/// `#[allow(dead_code)]`: this is one of the two genuinely uncalled
/// choke-point entry points in this module (see "Not wired yet" above) —
/// #1077's establishment/overturn transition is the intended production
/// caller, not yet built. Same shape as
/// `lesson_forge_ops::storage::persist_pending_lesson_candidate`. Flagged
/// here rather than silently suppressed at the module level.
#[allow(dead_code)]
pub(crate) fn authorize_governed_mutation(
    server: &MemoryServer,
    policy: &ApproverAuthorizationPolicyV1,
    target: &ApprovalTargetV1,
    caller_asserted: &CallerAssertedContextV1,
) -> Result<ApprovalReceiptV1, AuthorityDenialV1> {
    let probe = GhApproverAuthorityProbe::new(server);
    resolve_verified_approver(&probe, policy, target, caller_asserted, chrono::Utc::now())
}

/// Revalidate an approval immediately before a governed mutation writes.
///
/// `Ok(())` is the only outcome that may be followed by a write. Every
/// refusal — a revoked permission, a team removal, a credential swap, a
/// moved branch, an edited proposal, an unreachable GitHub — leaves the
/// caller with an [`AuthorityDenialV1`] and nothing mutated.
///
/// `pub(crate)`, not `pub`: it takes `&MemoryServer`, which is itself
/// `pub(crate)`, so a wider visibility would be unreachable from outside the
/// crate and trips the rustc `private_interfaces` lint.
///
/// `#[allow(dead_code)]`: the other of the two genuinely uncalled
/// choke-point entry points — see [`authorize_governed_mutation`]'s note.
#[allow(dead_code)]
pub(crate) fn revalidate_governed_mutation(
    server: &MemoryServer,
    policy: &ApproverAuthorizationPolicyV1,
    receipt: &ApprovalReceiptV1,
    current: &CurrentApprovalContextV1,
) -> Result<(), AuthorityDenialV1> {
    let probe = GhApproverAuthorityProbe::new(server);
    revalidate_approval(&probe, policy, receipt, current, chrono::Utc::now())
}

#[cfg(test)]
mod tests;
