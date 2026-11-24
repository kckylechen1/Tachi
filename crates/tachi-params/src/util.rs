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
    "PYTHONPATH",
    "PYTHONSTARTUP",
    "PYTHONHOME",
    "PYTHONWARNINGS",
    "PERL5OPT",
    "PERL5LIB",
    "RUBYOPT",
    "RUBYLIB",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_EXEC_PATH",
    "GIT_TEMPLATE_DIR",
    "GIT_EXTERNAL_DIFF",
    "GIT_CONFIG",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_COUNT",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "RUSTC",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_RUSTC",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "CARGO_HOME",
    "JAVA_TOOL_OPTIONS",
    "_JAVA_OPTIONS",
    "JDK_JAVA_OPTIONS",
    "MAVEN_OPTS",
    "GRADLE_OPTS",
];

const EXECUTION_ENVIRONMENT_NAME_PREFIXES: &[&str] =
    &["LD_", "DYLD_", "GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_"];

pub fn is_execution_environment_name(name: &str) -> bool {
    EXECUTION_ENVIRONMENT_NAMES.contains(&name)
        || EXECUTION_ENVIRONMENT_NAME_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
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
