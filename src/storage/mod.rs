use std::fmt;
use std::str::FromStr;

use anyhow::Result;
use tracing::info;

use crate::cmd;
use crate::config;

/// Supported storage backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageBackend {
    /// OpenEBS RawFile LocalPV — lightweight loop-mounted local volumes (default).
    Rawfile,
    /// Longhorn — distributed block storage (requires open-iscsi).
    Longhorn,
}

impl FromStr for StorageBackend {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "rawfile" | "rawfile-localpv" => Ok(Self::Rawfile),
            "longhorn" => Ok(Self::Longhorn),
            _ => anyhow::bail!(
                "unknown storage backend '{s}': expected 'rawfile' or 'longhorn'"
            ),
        }
    }
}

impl fmt::Display for StorageBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rawfile => write!(f, "rawfile"),
            Self::Longhorn => write!(f, "longhorn"),
        }
    }
}

/// Configuration for storage installation.
#[derive(Debug)]
pub struct StorageConfig {
    /// Which storage backend to deploy.
    pub backend: StorageBackend,
    /// Helm chart version override. `None` uses the compiled-in default.
    pub version: Option<String>,
}

// ── Shared helpers ──────────────────────────────────────────────────────────

/// Remove the control-plane NoSchedule taint so workloads (storage, user pods)
/// can be scheduled on single-node or small clusters, then verify removal.
async fn ensure_control_plane_schedulable() -> Result<()> {
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

// ── RawFile LocalPV (default) ───────────────────────────────────────────────

/// Install OpenEBS RawFile LocalPV via Helm.
async fn install_rawfile(version: &str) -> Result<()> {
    info!("Installing RawFile LocalPV v{version}...");

    cmd::run(
        "helm",
        &["repo", "add", "rawfile-localpv", config::URL_RAWFILE_LOCALPV_HELM_REPO],
    )
    .await?;
    cmd::run("helm", &["repo", "update"]).await?;

    let version_trimmed = version.trim_start_matches('v');
    cmd::run(
        "helm",
        &[
            "install",
            "rawfile-localpv",
            "rawfile-localpv/rawfile-localpv",
            "--namespace",
            "openebs",
            "--create-namespace",
            "--version",
            version_trimmed,
            "--set",
            "storageClasses[0].name=rawfile-localpv",
            "--set",
            "storageClasses[0].enabled=true",
            "--set",
            "storageClasses[0].isDefault=true",
            "--set",
            "storageClasses[0].reclaimPolicy=Delete",
            "--set",
            "storageClasses[0].volumeBindingMode=WaitForFirstConsumer",
            "--set",
            "storageClasses[0].allowVolumeExpansion=true",
            "--set",
            "storageClasses[0].fsType=ext4",
        ],
    )
    .await?;

    info!("Waiting for RawFile LocalPV to become ready...");
    cmd::run(
        "kubectl",
        &[
            "rollout", "status", "daemonset",
            "-n", "openebs",
            "rawfile-localpv-node",
            "--timeout=300s",
        ],
    )
    .await?;

    info!("RawFile LocalPV v{version} installed successfully");
    Ok(())
}

// ── Longhorn (alternative) ──────────────────────────────────────────────────

/// Install the open-iscsi host dependency required by Longhorn and ensure
/// the iscsid service is running.
async fn install_iscsi() -> Result<()> {
    let active = cmd::run_output("systemctl", &["is-active", "iscsid"])
        .await
        .unwrap_or_default();
    if active.trim() == "active" {
        info!("iscsid is already running");
        return Ok(());
    }

    info!("Installing open-iscsi (Longhorn dependency)...");
    cmd::run_privileged("apt-get", &["update", "-qq"]).await?;
    cmd::run_privileged(
        "apt-get",
        &["install", "-y", "-qq", "open-iscsi"],
    )
    .await?;

    cmd::run_privileged("systemctl", &["enable", "--now", "iscsid"]).await?;

    let check = cmd::run_output("systemctl", &["is-active", "iscsid"])
        .await
        .unwrap_or_default();
    if check.trim() != "active" {
        anyhow::bail!("iscsid failed to start after installing open-iscsi");
    }
    info!("open-iscsi installed and iscsid is running");
    Ok(())
}

/// Install Longhorn distributed block storage via Helm.
async fn install_longhorn(version: &str) -> Result<()> {
    info!("Installing Longhorn v{version}...");

    cmd::run(
        "helm",
        &["repo", "add", "longhorn", config::URL_LONGHORN_HELM_REPO],
    )
    .await?;
    cmd::run("helm", &["repo", "update"]).await?;

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

// ── Public entry point ──────────────────────────────────────────────────────

/// Install the selected storage backend.
pub async fn install_storage(storage_config: &StorageConfig) -> Result<()> {
    if !cmd::binary_exists("helm").await {
        anyhow::bail!("helm is required to install storage");
    }

    ensure_control_plane_schedulable().await?;

    let (default_version, label) = match storage_config.backend {
        StorageBackend::Rawfile => (config::DEFAULT_RAWFILE_LOCALPV_VERSION, "RawFile LocalPV"),
        StorageBackend::Longhorn => (config::DEFAULT_LONGHORN_VERSION, "Longhorn"),
    };

    let version = storage_config
        .version
        .as_deref()
        .unwrap_or(default_version);

    info!("Storage backend: {label}");

    match storage_config.backend {
        StorageBackend::Rawfile => {
            install_rawfile(version).await?;
        }
        StorageBackend::Longhorn => {
            install_iscsi().await?;
            install_longhorn(version).await?;
        }
    }

    Ok(())
}
