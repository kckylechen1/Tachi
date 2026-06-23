use super::*;

#[test]
fn vault_unavailable_errors_allow_fallback() {
    assert!(vault_secret_unavailable("Secret not found: GH_TOKEN"));
    assert!(vault_secret_unavailable("Vault is locked"));
    assert!(vault_secret_unavailable(
        "Vault auto-locked. Call vault_unlock first."
    ));
    assert!(vault_secret_unavailable("Vault not initialized"));
    assert!(!vault_secret_unavailable("Vault decrypt failed"));
}

#[test]
fn env_gh_token_prefers_gh_token_and_falls_back() {
    let _guard = ENV_LOCK.lock().unwrap();
    let old_gh = std::env::var_os("GH_TOKEN");
    let old_github = std::env::var_os("GITHUB_TOKEN");
    std::env::remove_var("GH_TOKEN");
    std::env::remove_var("GITHUB_TOKEN");

    std::env::set_var("GITHUB_TOKEN", "github-token");
    assert_eq!(env_gh_token().as_deref(), Some("github-token"));
    std::env::set_var("GH_TOKEN", "gh-token");
    assert_eq!(env_gh_token().as_deref(), Some("gh-token"));
    std::env::set_var("GH_TOKEN", "   ");
    assert_eq!(env_gh_token().as_deref(), Some("github-token"));

    if let Some(v) = old_gh {
        std::env::set_var("GH_TOKEN", v);
    } else {
        std::env::remove_var("GH_TOKEN");
    }
    if let Some(v) = old_github {
        std::env::set_var("GITHUB_TOKEN", v);
    } else {
        std::env::remove_var("GITHUB_TOKEN");
    }
}

#[test]
fn preserve_gh_env_keeps_auth_proxy_and_platform_env() {
    use std::ffi::OsStr;

    let _guard = ENV_LOCK.lock().unwrap();
    let old_https = std::env::var_os("HTTPS_PROXY");
    let old_cert = std::env::var_os("SSL_CERT_FILE");
    let old_xdg = std::env::var_os("XDG_CONFIG_HOME");
    let old_git_ssh_command = std::env::var_os("GIT_SSH_COMMAND");
    let old_ssh_auth_sock = std::env::var_os("SSH_AUTH_SOCK");
    let old_gh = std::env::var_os("GH_TOKEN");
    let old_github = std::env::var_os("GITHUB_TOKEN");

    std::env::set_var("HTTPS_PROXY", "http://proxy.local:8080");
    std::env::set_var("SSL_CERT_FILE", "/tmp/test-ca.pem");
    std::env::set_var("XDG_CONFIG_HOME", "/tmp/test-xdg");
    std::env::set_var("GIT_SSH_COMMAND", "sh -c 'echo should-not-run'");
    std::env::set_var("SSH_AUTH_SOCK", "/tmp/test-ssh-agent.sock");
    std::env::remove_var("GH_TOKEN");
    std::env::set_var("GITHUB_TOKEN", "github-env-token");

    let mut cmd = Command::new("gh");
    cmd.env_clear();
    preserve_gh_env(&mut cmd);

    let envs: Vec<_> = cmd.get_envs().collect();
    let get = |name: &str| {
        envs.iter()
            .find(|(k, _)| *k == OsStr::new(name))
            .and_then(|(_, v)| *v)
            .map(|v| v.to_string_lossy().to_string())
    };

    assert_eq!(
        get("HTTPS_PROXY").as_deref(),
        Some("http://proxy.local:8080")
    );
    assert_eq!(get("SSL_CERT_FILE").as_deref(), Some("/tmp/test-ca.pem"));
    assert_eq!(get("XDG_CONFIG_HOME").as_deref(), Some("/tmp/test-xdg"));
    assert_eq!(
        get("SSH_AUTH_SOCK").as_deref(),
        Some("/tmp/test-ssh-agent.sock")
    );
    assert_eq!(get("GIT_SSH_COMMAND"), None);
    assert_eq!(get("GITHUB_TOKEN").as_deref(), Some("github-env-token"));

    if let Some(v) = old_https {
        std::env::set_var("HTTPS_PROXY", v);
    } else {
        std::env::remove_var("HTTPS_PROXY");
    }
    if let Some(v) = old_cert {
        std::env::set_var("SSL_CERT_FILE", v);
    } else {
        std::env::remove_var("SSL_CERT_FILE");
    }
    if let Some(v) = old_xdg {
        std::env::set_var("XDG_CONFIG_HOME", v);
    } else {
        std::env::remove_var("XDG_CONFIG_HOME");
    }
    if let Some(v) = old_git_ssh_command {
        std::env::set_var("GIT_SSH_COMMAND", v);
    } else {
        std::env::remove_var("GIT_SSH_COMMAND");
    }
    if let Some(v) = old_ssh_auth_sock {
        std::env::set_var("SSH_AUTH_SOCK", v);
    } else {
        std::env::remove_var("SSH_AUTH_SOCK");
    }
    if let Some(v) = old_gh {
        std::env::set_var("GH_TOKEN", v);
    } else {
        std::env::remove_var("GH_TOKEN");
    }
    if let Some(v) = old_github {
        std::env::set_var("GITHUB_TOKEN", v);
    } else {
        std::env::remove_var("GITHUB_TOKEN");
    }
}
