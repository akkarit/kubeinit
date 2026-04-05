use std::process::Stdio;

use anyhow::{Context, Result, bail};
use tokio::process::Command;
use tracing::{debug, info};

/// Returns `true` when the current process is running as root (uid 0).
pub fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

/// Run a command that requires root privileges. If the current user is not root
/// the command is automatically prefixed with `sudo`.
pub async fn run_privileged(program: &str, args: &[&str]) -> Result<()> {
    if is_root() {
        run(program, args).await
    } else {
        let mut sudo_args: Vec<&str> = vec![program];
        sudo_args.extend_from_slice(args);
        run("sudo", &sudo_args).await
    }
}

/// Like [`run_privileged`] but captures stdout.
pub async fn run_privileged_output(program: &str, args: &[&str]) -> Result<String> {
    if is_root() {
        run_output(program, args).await
    } else {
        let mut sudo_args: Vec<&str> = vec![program];
        sudo_args.extend_from_slice(args);
        run_output("sudo", &sudo_args).await
    }
}

/// Verify that we can obtain root privileges (either already root or `sudo`
/// works). Call this early so the user gets a clear error instead of a failure
/// halfway through installation.
pub async fn ensure_privilege() -> Result<()> {
    if is_root() {
        return Ok(());
    }

    info!("Not running as root — checking sudo access...");
    let status = Command::new("sudo")
        .args(["-v"])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .context("failed to execute sudo")?;

    if !status.success() {
        bail!("sudo access is required to install dependencies");
    }

    info!("sudo access confirmed");
    Ok(())
}

/// Run a command, streaming its output to the terminal. Returns an error if the
/// process exits with a non-zero status code.
pub async fn run(program: &str, args: &[&str]) -> Result<()> {
    info!("Running: {} {}", program, args.join(" "));

    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .with_context(|| format!("failed to execute {program}"))?;

    if !status.success() {
        bail!("{program} exited with status {status}");
    }

    Ok(())
}

/// Run a command and capture its stdout as a `String`.
pub async fn run_output(program: &str, args: &[&str]) -> Result<String> {
    debug!("Running (capture): {} {}", program, args.join(" "));

    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .await
        .with_context(|| format!("failed to execute {program}"))?;

    if !output.status.success() {
        bail!("{program} exited with status {}", output.status);
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Check whether a binary is present on `$PATH`.
pub async fn binary_exists(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success())
}
