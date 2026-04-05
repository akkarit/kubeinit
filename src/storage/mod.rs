use anyhow::Result;
use tracing::info;

use crate::cmd;
use crate::config;

/// Configuration for the Longhorn installation.
#[derive(Debug)]
pub struct LonghornConfig {
    /// Longhorn Helm chart version. `None` means the version from versions.toml.
    pub version: Option<String>,
}

/// Install Longhorn distributed storage via Helm.
pub async fn install_longhorn(longhorn_config: &LonghornConfig) -> Result<()> {
    if !cmd::binary_exists("helm").await {
        anyhow::bail!("helm is required to install Longhorn");
    }

    let version = longhorn_config
        .version
        .as_deref()
        .unwrap_or(config::DEFAULT_LONGHORN_VERSION);

    info!("Installing Longhorn v{version}...");

    // Add the Longhorn Helm repo
    cmd::run(
        "helm",
        &["repo", "add", "longhorn", config::URL_LONGHORN_HELM_REPO],
    )
    .await?;
    cmd::run("helm", &["repo", "update"]).await?;

    // Create the longhorn-system namespace
    cmd::run(
        "kubectl",
        &["create", "namespace", "longhorn-system", "--dry-run=client", "-o", "yaml"],
    )
    .await
    .ok();
    cmd::run(
        "kubectl",
        &["apply", "-f", "-"],
    )
    .await
    .ok();

    // Install via Helm
    let version_trimmed = version.trim_start_matches('v');
    cmd::run(
        "helm",
        &[
            "install",
            "longhorn",
            "longhorn/longhorn",
            "--namespace",
            "longhorn-system",
            "--create-namespace",
            "--version",
            version_trimmed,
            "--set",
            "defaultSettings.defaultDataPath=/var/lib/longhorn",
            "--set",
            "persistence.defaultClassReplicaCount=1",
        ],
    )
    .await?;

    info!("Waiting for Longhorn to become ready...");
    cmd::run(
        "kubectl",
        &[
            "rollout", "status", "deployment",
            "-n", "longhorn-system",
            "longhorn-driver-deployer",
            "--timeout=300s",
        ],
    )
    .await?;

    info!("Longhorn v{version} installed successfully");
    Ok(())
}
