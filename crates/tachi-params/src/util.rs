/// Stable hash function (FNV-1a). Deterministic across Rust toolchain versions,
/// unlike DefaultHasher which uses SipHash with randomized keys.
pub fn stable_hash(input: &str) -> String {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET;
    for byte in input.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{:016x}", hash)
}

/// Environment names that choose which code a process loads or which
/// executables it runs. A value read from a project file must never reach a
/// child under one of these names.
const EXECUTION_ENVIRONMENT_NAMES: &[&str] = &[
    "PATH",
    "FPATH",
    "CDPATH",
    "MANPATH",
    "INFOPATH",
    "SHELL",
    "ENV",
    "BASH_ENV",
    "PROMPT_COMMAND",
    "ZDOTDIR",
    "COMSPEC",
    "NODE_OPTIONS",
    "NODE_PATH",
    "NODE_REPL_EXTERNAL_MODULE",
    "NPM_CONFIG_SCRIPT_SHELL",
    "NPM_CONFIG_NODE_OPTIONS",
    "NPM_CONFIG_NODE_GYP",
    "NPM_CONFIG_USERCONFIG",
    "NPM_CONFIG_GLOBALCONFIG",
    "NPM_CONFIG_GIT",
    "PYTHONPATH",
    "PYTHONSTARTUP",
    "PYTHONHOME",
    "PYTHONWARNINGS",
    "PYTHONBREAKPOINT",
    "PERL5DB",
    "PERL5OPT",
    "PERL5LIB",
    "PERLLIB",
    "RUBYOPT",
    "RUBYLIB",
    "RUBYPATH",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_EXEC_PATH",
    "GIT_TEMPLATE_DIR",
    "GIT_EXTERNAL_DIFF",
    "GIT_CONFIG",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "GIT_PAGER",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "RUSTC",
    "RUSTDOC",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    // `CARGO` is forwarded to whatever runs it. `RUSTUP_TOOLCHAIN` makes a
    // rustup proxy pick a toolchain, and install a missing one; `RUSTUP_HOME`
    // moves where toolchains and their settings are looked up.
    "CARGO",
    "RUSTUP_TOOLCHAIN",
    "RUSTUP_HOME",
    "CARGO_BUILD_RUSTC",
    "CARGO_BUILD_RUSTDOC",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    "CARGO_HOME",
    // Cargo runs the credential provider a registry names.
    "CARGO_REGISTRY_CREDENTIAL_PROVIDER",
    "CARGO_REGISTRY_GLOBAL_CREDENTIAL_PROVIDERS",
    "JAVA_TOOL_OPTIONS",
    "_JAVA_OPTIONS",
    "JDK_JAVA_OPTIONS",
    "CLASSPATH",
    "MAVEN_OPTS",
    "GRADLE_OPTS",
];

const EXECUTION_ENVIRONMENT_NAME_PREFIXES: &[&str] =
    &["LD_", "DYLD_", "GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_"];

/// `CARGO_TARGET_<TRIPLE>_RUNNER` and `_LINKER` choose the programs a build
/// runs for that target. The prefix on its own is the ordinary target
/// directory and stays allowed.
const TARGET_SELECTOR_PREFIX: &str = "CARGO_TARGET_";
const TARGET_SELECTOR_SUFFIXES: &[&str] = &["_RUNNER", "_LINKER"];

pub fn is_execution_environment_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    EXECUTION_ENVIRONMENT_NAMES.contains(&upper.as_str())
        || EXECUTION_ENVIRONMENT_NAME_PREFIXES
            .iter()
            .any(|prefix| upper.starts_with(prefix))
        || (upper.starts_with(TARGET_SELECTOR_PREFIX)
            && TARGET_SELECTOR_SUFFIXES
                .iter()
                .any(|suffix| upper.ends_with(suffix)))
}

pub fn is_shell_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::is_execution_environment_name;

    #[test]
    fn execution_environment_names_match_case_insensitively() {
        for name in [
            "PATH",
            "Path",
            "COMSPEC",
            "ComSpec",
            "ld_preload",
            "Dyld_Insert_Libraries",
            "git_config_key_0",
        ] {
            assert!(is_execution_environment_name(name), "{name}");
        }
        assert!(!is_execution_environment_name("OPENAI_API_KEY"));
    }

    // Names that make a toolchain run a program the binding chose. Ordinary
    // toolchain settings are not selectors and stay allowed.
    #[test]
    fn execution_environment_names_cover_tool_selectors_only() {
        for name in [
            "RUSTDOC",
            "rustdoc",
            "CARGO_BUILD_RUSTDOC",
            "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
            "CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER",
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER",
            "NPM_CONFIG_SCRIPT_SHELL",
            "npm_config_script_shell",
            "GIT_CONFIG_PARAMETERS",
            "PERL5DB",
            // Forwarded to whatever runs `cargo`; a rustup proxy picks the
            // toolchain, and may install one; npm turns NODE_OPTIONS into
            // what its scripts load, and reads its configuration from these.
            "CARGO",
            "cargo",
            "RUSTUP_TOOLCHAIN",
            "RUSTUP_HOME",
            "NPM_CONFIG_NODE_OPTIONS",
            "npm_config_node_options",
            "NPM_CONFIG_NODE_GYP",
            "NPM_CONFIG_USERCONFIG",
            "NPM_CONFIG_GLOBALCONFIG",
            // npm runs `git` through NPM_CONFIG_GIT; Python, Node, Ruby, Perl
            // and the JVM load code from these; Git runs these as programs;
            // Cargo runs the credential provider a registry names.
            "NPM_CONFIG_GIT",
            "PYTHONBREAKPOINT",
            "NODE_REPL_EXTERNAL_MODULE",
            "RUBYPATH",
            "PERLLIB",
            "CLASSPATH",
            "GIT_EDITOR",
            "GIT_SEQUENCE_EDITOR",
            "GIT_PAGER",
            "CARGO_REGISTRY_CREDENTIAL_PROVIDER",
            "CARGO_REGISTRY_GLOBAL_CREDENTIAL_PROVIDERS",
        ] {
            assert!(is_execution_environment_name(name), "{name}");
        }
        // Ordinary settings. A download source and a JDK location do not
        // choose a program for this invocation.
        for name in [
            "NPM_CONFIG_REGISTRY",
            "NPM_CONFIG_CACHE",
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_JOBS",
            "CARGO_REGISTRY_DEFAULT",
            "RUSTUP_IO_THREADS",
            "RUSTUP_DIST_SERVER",
            "JAVA_HOME",
        ] {
            assert!(!is_execution_environment_name(name), "{name}");
        }
    }
}
