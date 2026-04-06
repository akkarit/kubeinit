use anyhow::Result;
use tracing::info;

use crate::cmd;
use crate::config;

/// Configuration for the OpenEBS LocalPV Provisioner installation.
#[derive(Debug)]
pub struct StorageConfig {
    /// OpenEBS LocalPV Helm chart version. `None` means the version from versions.toml.
    pub version: Option<String>,
}

/// Remove the control-plane NoSchedule taint so workloads (storage, user pods)
/// can be scheduled on single-node or small clusters, then verify removal.
async fn ensure_control_plane_schedulable() -> Result<()> {
    // Use kubectl to get the node name — more reliable than the hostname
    // command, which may not be installed or may return a value that doesn't
    // match the Kubernetes node name.
    let hostname = cmd::run_output(
        "kubectl",
        &["get", "nodes", "-o", "jsonpath={.items[0].metadata.name}"],
    )
    .await
    .unwrap_or_default();
    if hostname.is_empty() {
        anyhow::bail!("Could not determine node name for taint removal");
    }

    info!("Removing NoSchedule taint from {hostname}...");
    // The command succeeds even if the taint is already absent (trailing '-').
    cmd::run(
        "kubectl",
        &[
            "taint", "nodes", &hostname,
            "node-role.kubernetes.io/control-plane:NoSchedule-",
        ],
    )
    .await
    .ok();

    // Verify the taint is actually gone.
    let taints = cmd::run_output(
        "kubectl",
        &[
            "get", "node", &hostname,
            "-o", "jsonpath={.spec.taints}",
        ],
    )
    .await
    .unwrap_or_default();

    if taints.contains("NoSchedule") {
        anyhow::bail!(
            "Control-plane NoSchedule taint still present on {hostname} after removal attempt. \
             Storage pods would not be schedulable."
        );
    }

    info!("Verified: no NoSchedule taint on {hostname}");
    Ok(())
}

/// Install OpenEBS Dynamic LocalPV Provisioner via Helm.
pub async fn install_storage(storage_config: &StorageConfig) -> Result<()> {
    if !cmd::binary_exists("helm").await {
        anyhow::bail!("helm is required to install OpenEBS LocalPV Provisioner");
    }

    ensure_control_plane_schedulable().await?;

    let version = storage_config
        .version
        .as_deref()
        .unwrap_or(config::DEFAULT_OPENEBS_LOCALPV_VERSION);

    info!("Installing OpenEBS LocalPV Provisioner v{version}...");

    // Add the OpenEBS LocalPV Helm repo
    cmd::run(
        "helm",
        &["repo", "add", "openebs-localpv", config::URL_OPENEBS_LOCALPV_HELM_REPO],
    )
    .await?;
    cmd::run("helm", &["repo", "update"]).await?;

    // Install via Helm
    let version_trimmed = version.trim_start_matches('v');
    cmd::run(
        "helm",
        &[
            "install",
            "openebs-localpv",
            "openebs-localpv/localpv-provisioner",
            "--namespace",
            "openebs",
            "--create-namespace",
            "--version",
            version_trimmed,
            "--set",
            "storageClass.isDefaultClass=true",
        ],
    )
    .await?;

    info!("Waiting for OpenEBS LocalPV Provisioner to become ready...");
    cmd::run(
        "kubectl",
        &[
            "rollout", "status", "deployment",
            "-n", "openebs",
            "openebs-localpv-localpv-provisioner",
            "--timeout=300s",
        ],
    )
    .await?;

    info!("OpenEBS LocalPV Provisioner v{version} installed successfully");
    Ok(())
}
