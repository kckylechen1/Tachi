use std::io::BufRead;
use std::path::Path;

pub(in crate::bootstrap) fn read_vault_password(
    stdin_password: bool,
    keychain: bool,
    password_file: Option<&Path>,
    insecure_password_file: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let password = if keychain {
        if !cfg!(target_os = "macos") {
            return Err(
                "--keychain is only supported on macOS; use --password-file on Linux/Windows"
                    .into(),
            );
        }
        let output = std::process::Command::new("security")
            .args([
                "find-generic-password",
                "-s",
                "tachi-vault",
                "-a",
                "default",
                "-w",
            ])
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "Failed to read from Keychain: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        String::from_utf8(output.stdout)?.trim().to_string()
    } else if let Some(path) = password_file {
        read_password_file(path, insecure_password_file)?
    } else if stdin_password {
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf)?;
        buf.trim().to_string()
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
