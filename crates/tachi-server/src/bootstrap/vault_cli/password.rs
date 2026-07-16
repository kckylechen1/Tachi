use std::io::BufRead;
use std::path::Path;

/// Message shown when an interactive password prompt is needed but no TTY is
/// available to prompt on. This replaces the raw `rpassword` I/O error
/// (`Device not configured (os error 6)` on macOS/Linux), which is the errno
/// for "failed to open /dev/tty" and gives the caller zero indication of
/// what to do instead. Kept as a shared constant so the CLI error text and
/// its tests can't drift apart.
pub(in crate::bootstrap) const NO_TTY_HINT: &str =
    "no TTY to prompt for vault password; use --keychain, --stdin-password or --password-file";

/// #1175 codex 3.2: the same hint, but for `vault init`. A bare
/// `--password-file` isn't sufficient there the way it is for unlock — init
/// needs something to check the password against, so it also requires
/// `--confirm-password-file` (see the dedicated error a few lines below this
/// hint's other call site). Built on demand rather than folded into the
/// shared `NO_TTY_HINT` constant so the unlock path's hint stays free of
/// init-only guidance.
fn no_tty_hint_for_init() -> String {
    format!("{NO_TTY_HINT} (vault init also requires --confirm-password-file)")
}

/// Whether we can prompt interactively for a password.
///
/// #1175 codex 3.1: this must probe the same channel `rpassword::prompt_password`
/// actually prompts on, not the process's own stdin. `rpassword` 7.5.4 opens
/// the *controlling terminal device* directly (`/dev/tty` on unix — see
/// `rpassword::unix::DEFAULT_INPUT_PATH` — `CONIN$`/`CONOUT$` on Windows), so
/// it ignores stdin redirection entirely: a process with stdin piped or
/// redirected from `/dev/null` but still attached to a controlling terminal
/// can successfully prompt on `/dev/tty` even though `stdin().is_terminal()`
/// is false, and (more rarely) an inherited TTY-shaped stdin with no real
/// controlling terminal would wrongly report "can prompt" under the old
/// stdin-based check. Opening `/dev/tty` here is the identical probe
/// `rpassword` performs internally, so success/failure here matches its real
/// behavior exactly instead of approximating it via a different fd.
///
/// Test builds only: overridable via `TACHI_TEST_FORCE_NO_TTY=1` so a test
/// can exercise the no-TTY error path deterministically. This seam remains
/// load-bearing even with the `/dev/tty` probe below: the test process's own
/// controlling-terminal state is whatever `cargo test`'s harness happens to
/// leave it as (present when run from an interactive shell, absent under most
/// CI runners) — not something a test can control — so the explicit override
/// is still the only deterministic way to force the no-TTY branch (mirrors
/// the `TACHI_TEST_ALLOW_KEYCHAIN_AUTO_UNLOCK` test-escape-hatch convention in
/// `provider_config.rs` — never compiled into a release build).
#[cfg(unix)]
fn can_prompt_interactively() -> bool {
    #[cfg(test)]
    if std::env::var_os("TACHI_TEST_FORCE_NO_TTY").is_some() {
        return false;
    }
    std::fs::File::open("/dev/tty").is_ok()
}

/// Non-unix fallback (this crate ships for macOS/Linux; Windows is not a
/// shipped target today). Kept stdin-based rather than probing `CONIN$`
/// directly since there is no build/test coverage on this platform to verify
/// a `CONIN$` open behaves the way `rpassword`'s windows backend expects.
#[cfg(not(unix))]
fn can_prompt_interactively() -> bool {
    #[cfg(test)]
    if std::env::var_os("TACHI_TEST_FORCE_NO_TTY").is_some() {
        return false;
    }
    std::io::IsTerminal::is_terminal(&std::io::stdin())
}

pub(in crate::bootstrap) fn read_vault_password(
    stdin_password: bool,
    keychain: bool,
    password_file: Option<&Path>,
    insecure_password_file: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let password = if keychain {
        crate::vault_crypto::read_password_from_macos_keychain()?
    } else if let Some(path) = password_file {
        read_password_file(path, insecure_password_file)?
    } else if stdin_password {
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf)?;
        buf.trim().to_string()
    } else if !can_prompt_interactively() {
        return Err(NO_TTY_HINT.into());
    } else {
        rpassword::prompt_password("Vault password: ")?
    };

    if password.is_empty() {
        return Err("Password cannot be empty".into());
    }
    Ok(password)
}

pub(super) fn read_vault_init_password(
    stdin_password: bool,
    keychain: bool,
    password_file: Option<&Path>,
    confirm_password_file: Option<&Path>,
    insecure_password_file: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let (mut password, mut confirm) = if stdin_password {
        let stdin = std::io::stdin();
        let mut stdin = stdin.lock();
        read_vault_init_password_stdin_lines(
            &mut stdin,
            confirm_password_file,
            insecure_password_file,
        )?
    } else if keychain || password_file.is_some() {
        let password = read_vault_password(false, keychain, password_file, insecure_password_file)?;
        let Some(path) = confirm_password_file else {
            return Err(
                "Non-interactive vault init requires --confirm-password-file. Use interactive `tachi vault init` or provide a separate confirmation file."
                    .into(),
            );
        };
        (password, read_password_file(path, insecure_password_file)?)
    } else if !can_prompt_interactively() {
        return Err(no_tty_hint_for_init().into());
    } else {
        let password = rpassword::prompt_password("New vault password: ")?;
        let confirm = rpassword::prompt_password("Confirm password: ")?;
        (password, confirm)
    };

    if password.is_empty() {
        crate::vault_crypto::zero_string(&mut password);
        crate::vault_crypto::zero_string(&mut confirm);
        return Err("Password cannot be empty".into());
    }
    if password != confirm {
        crate::vault_crypto::zero_string(&mut password);
        crate::vault_crypto::zero_string(&mut confirm);
        return Err("Passwords do not match".into());
    }
    crate::vault_crypto::zero_string(&mut confirm);
    Ok(password)
}

pub(super) fn read_vault_init_password_stdin_lines(
    reader: &mut impl BufRead,
    confirm_password_file: Option<&Path>,
    insecure_password_file: bool,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    let mut password = String::new();
    reader.read_line(&mut password)?;
    let password = password.trim().to_string();
    let confirm = if let Some(path) = confirm_password_file {
        read_password_file(path, insecure_password_file)?
    } else {
        let mut confirm = String::new();
        reader.read_line(&mut confirm)?;
        confirm.trim().to_string()
    };
    Ok((password, confirm))
}

pub(super) fn read_password_file(
    path: &Path,
    insecure_password_file: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read password file {}: {e}", path.display()))?;
    let password = raw.lines().next().unwrap_or_default().trim().to_string();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(path) {
            let mode = metadata.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                if insecure_password_file {
                    eprintln!(
                        "WARNING: password file {} is readable by group/other (mode {:o}); prefer 0600",
                        path.display(),
                        mode
                    );
                } else {
                    return Err(format!(
                        "Password file {} is readable by group/other (mode {:o}). Set permissions to 0600 or pass --insecure-password-file.",
                        path.display(),
                        mode
                    )
                    .into());
                }
            }
        }
    }

    Ok(password)
}
