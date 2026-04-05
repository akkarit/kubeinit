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
    /// Enable Kubernetes Gateway API support.
    pub gateway_api: bool,
}

/// Install Cilium as the cluster CNI using the Cilium CLI.
///
/// If the `cilium` CLI is available it is preferred. Otherwise we fall back to
/// a Helm-based installation so the tool works on hosts that only have `helm`.
pub async fn install_cilium(cilium_config: &CiliumConfig) -> Result<()> {
    if cilium_config.gateway_api {
        install_gateway_api_crds().await?;
    }

    info!("Installing Cilium CNI...");

    if cmd::binary_exists("cilium").await {
        install_via_cli(cilium_config).await
    } else if cmd::binary_exists("helm").await {
        install_via_helm(cilium_config).await
    } else {
        anyhow::bail!(
            "Neither `cilium` CLI nor `helm` found. \
             Install one of them to proceed with CNI setup."
        );
    }
}

/// Install the Kubernetes Gateway API CRDs (experimental channel, which
/// includes all resources needed by Cilium).
async fn install_gateway_api_crds() -> Result<()> {
    let version = config::DEFAULT_GATEWAY_API_VERSION;
    let url = config::URL_GATEWAY_API_CRDS.replace("{version}", version);

    info!("Installing Gateway API CRDs v{version}...");
    cmd::run("kubectl", &["apply", "--server-side=true", "-f", &url]).await?;
    info!("Gateway API CRDs installed");
    Ok(())
}

async fn install_via_cli(cilium_config: &CiliumConfig) -> Result<()> {
    let mut args = vec!["install", "--set", "kubeProxyReplacement=true"];

    let ipam_flag = format!("ipam.operator.clusterPoolIPv4PodCIDRList={}", cilium_config.pod_cidr);
    args.extend(["--set", &ipam_flag]);

    if cilium_config.gateway_api {
        args.extend([
            "--set", "gatewayAPI.enabled=true",
            "--set", "gatewayAPI.hostNetwork.enabled=true",
        ]);
    }

    let version_flag;
    if let Some(ref v) = cilium_config.version {
        version_flag = v.trim_start_matches('v').to_string();
        args.extend(["--version", &version_flag]);
    }

    cmd::run("cilium", &args).await?;

    info!("Waiting for Cilium to become ready...");
    cmd::run("cilium", &["status", "--wait"]).await?;

    info!("Cilium CNI installed successfully");
    Ok(())
}

async fn install_via_helm(cilium_config: &CiliumConfig) -> Result<()> {
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
        cilium_config.pod_cidr
    );
    args.extend(["--set", &ipam_flag]);

    if cilium_config.gateway_api {
        args.extend([
            "--set", "gatewayAPI.enabled=true",
            "--set", "gatewayAPI.hostNetwork.enabled=true",
        ]);
    }

    let version_flag;
    if let Some(ref v) = cilium_config.version {
        version_flag = v.trim_start_matches('v').to_string();
        args.extend(["--version", &version_flag]);
    }

    cmd::run("helm", &args).await?;

    info!("Cilium CNI installed via Helm. Use `kubectl -n kube-system get pods -l app.kubernetes.io/name=cilium-agent` to check status.");
    Ok(())
}
