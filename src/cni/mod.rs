use anyhow::Result;
use tracing::info;

use crate::cmd;
use crate::config;

/// Configuration for the Cilium CNI installation.
#[derive(Debug)]
pub struct CiliumConfig {
    /// Cilium Helm chart version. `None` means latest.
    pub version: Option<String>,
    /// Pod CIDR to configure in Cilium.
    pub pod_cidr: String,
}

/// Install Cilium as the cluster CNI using the Cilium CLI.
///
/// If the `cilium` CLI is available it is preferred. Otherwise we fall back to
/// a Helm-based installation so the tool works on hosts that only have `helm`.
pub async fn install_cilium(config: &CiliumConfig) -> Result<()> {
    info!("Installing Cilium CNI...");

    if cmd::binary_exists("cilium").await {
        install_via_cli(config).await
    } else if cmd::binary_exists("helm").await {
        install_via_helm(config).await
    } else {
        anyhow::bail!(
            "Neither `cilium` CLI nor `helm` found. \
             Install one of them to proceed with CNI setup."
        );
    }
}

async fn install_via_cli(config: &CiliumConfig) -> Result<()> {
    let mut args = vec!["install", "--set", "kubeProxyReplacement=true"];

    let ipam_flag = format!("ipam.operator.clusterPoolIPv4PodCIDRList={}", config.pod_cidr);
    args.extend(["--set", &ipam_flag]);

    let version_flag;
    if let Some(ref v) = config.version {
        version_flag = v.trim_start_matches('v').to_string();
        args.extend(["--version", &version_flag]);
    }

    cmd::run("cilium", &args).await?;

    info!("Waiting for Cilium to become ready...");
    cmd::run("cilium", &["status", "--wait"]).await?;

    info!("Cilium CNI installed successfully");
    Ok(())
}

async fn install_via_helm(config: &CiliumConfig) -> Result<()> {
    // Add the Cilium Helm repo
    cmd::run(
        "helm",
        &["repo", "add", "cilium", config::URL_CILIUM_HELM_REPO],
    )
    .await?;
    cmd::run("helm", &["repo", "update"]).await?;

    let mut args = vec![
        "install",
        "cilium",
        "cilium/cilium",
        "--namespace",
        "kube-system",
        "--set",
        "kubeProxyReplacement=true",
    ];

    let ipam_flag = format!(
        "ipam.operator.clusterPoolIPv4PodCIDRList={}",
        config.pod_cidr
    );
    args.extend(["--set", &ipam_flag]);

    let version_flag;
    if let Some(ref v) = config.version {
        version_flag = v.trim_start_matches('v').to_string();
        args.extend(["--version", &version_flag]);
    }

    cmd::run("helm", &args).await?;

    info!("Cilium CNI installed via Helm. Use `kubectl -n kube-system get pods -l app.kubernetes.io/name=cilium-agent` to check status.");
    Ok(())
}
