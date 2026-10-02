//! Real host metadata for an operator-adjudicated certification receipt.

use std::process::{Command, Output};

fn version_probe(host_os: &str) -> Result<(&'static str, &'static [&'static str]), String> {
    match host_os {
        "macos" => Ok(("sw_vers", &["-productVersion"])),
        "linux" => Ok(("uname", &["-sr"])),
        other => Err(format!("unsupported certification host OS: {other}")),
    }
}

fn observed_version(output: Output) -> Result<String, String> {
    if !output.status.success() {
        return Err(format!("host version probe failed: {}", output.status));
    }
    let version = String::from_utf8(output.stdout)
        .map_err(|error| format!("host version probe is not UTF-8: {error}"))?;
    let version = version.trim();
    if version.is_empty() {
        return Err("host version probe returned empty metadata".to_owned());
    }
    Ok(version.to_owned())
}

pub fn host_os_version() -> Result<String, String> {
    let (program, args) = version_probe(std::env::consts::OS)?;
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("cannot execute host version probe {program}: {error}"))?;
    observed_version(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_receipt_never_requires_macos_sw_vers() {
        assert_eq!(version_probe("linux").unwrap(), ("uname", &["-sr"][..]));
        assert_eq!(
            version_probe("macos").unwrap(),
            ("sw_vers", &["-productVersion"][..])
        );
        assert!(version_probe("windows").is_err());
        assert!(version_probe("unknown").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn failed_empty_and_invalid_metadata_cannot_be_receipts() {
        use std::os::unix::process::ExitStatusExt;
        let output = |status, stdout| Output {
            status: std::process::ExitStatus::from_raw(status),
            stdout,
            stderr: Vec::new(),
        };
        assert!(observed_version(output(1 << 8, b"not success".to_vec())).is_err());
        assert!(observed_version(output(0, b" \n".to_vec())).is_err());
        assert!(observed_version(output(0, vec![0xff])).is_err());
        assert_eq!(
            observed_version(output(0, b"Linux 6.1\n".to_vec())).unwrap(),
            "Linux 6.1"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn receipt_metadata_is_observed_on_the_actual_host() {
        let version = host_os_version().expect("real host version must be observable");
        assert!(!version.is_empty());
        #[cfg(target_os = "linux")]
        assert!(version.starts_with("Linux "));
    }
}
