use std::env::consts::ARCH;

use anyhow::{Result, bail};
use tracing::info;

use crate::cmd;
use crate::config;

/// Map Rust `std::env::consts::ARCH` to the naming convention used by
/// Kubernetes and most Go-based release artifacts.
fn go_arch() -> Result<&'static str> {
    match ARCH {
        "x86_64" => Ok("amd64"),
        "aarch64" => Ok("arm64"),
        _ => bail!("Unsupported architecture: {ARCH}"),
    }
}

/// Expand `{version}` and `{arch}` placeholders in a URL template.
fn expand_url(template: &str, version: &str, arch: &str) -> String {
    template
        .replace("{version}", version)
        .replace("{arch}", arch)
}

/// Install all dependencies required to bootstrap a cluster.
///
/// Binaries that are already present on `$PATH` are skipped.
pub async fn install_all(kubernetes_version: Option<&str>, cilium_version: Option<&str>) -> Result<()> {
    cmd::ensure_privilege().await?;
    info!("Installing cluster dependencies...");

    install_containerd(None).await?;
    install_runc(None).await?;
    install_cni_plugins(None).await?;
    install_crictl(None).await?;
    install_kubernetes_components(kubernetes_version).await?;
    install_helm(None).await?;
    install_cilium_cli(cilium_version).await?;

    info!("All dependencies installed");
    Ok(())
}

/// Remove all Kubernetes cluster components and related tooling installed by
/// `install_all`. Binaries that are not present are silently skipped.
pub async fn uninstall_all() -> Result<()> {
    cmd::ensure_privilege().await?;
    info!("Uninstalling cluster dependencies...");

    // Uninstall Helm releases while the cluster is still running
    uninstall_helm_releases().await;

    // Gracefully tear down the cluster before removing binaries
    stop_cluster_workloads().await;

    uninstall_kubernetes_components().await?;
    uninstall_crictl().await?;
    uninstall_cilium_cli().await?;
    uninstall_helm().await?;
    uninstall_cni_plugins().await?;
    uninstall_containerd().await?;
    uninstall_runc().await?;
    uninstall_data_and_config().await;

    info!("All cluster dependencies removed");
    Ok(())
}

/// Uninstall Helm releases (OpenEBS, Cilium) while the API server is still
/// reachable, so resources are cleaned up properly.
async fn uninstall_helm_releases() {
    if !cmd::binary_exists("helm").await {
        return;
    }

    info!("Removing Helm releases...");
    cmd::run("helm", &["uninstall", "openebs-localpv", "-n", "openebs"])
        .await
        .ok();
    cmd::run("helm", &["uninstall", "cilium", "-n", "kube-system"])
        .await
        .ok();

    // Remove Gateway API CRDs
    if cmd::binary_exists("kubectl").await {
        let version = config::DEFAULT_GATEWAY_API_VERSION;
        let url = config::URL_GATEWAY_API_CRDS.replace("{version}", version);
        info!("Removing Gateway API CRDs...");
        cmd::run("kubectl", &["delete", "-f", &url, "--ignore-not-found"])
            .await
            .ok();
    }
}

/// Remove leftover data directories, kubeinit-managed configs, and user
/// kubeconfig after all binaries have been removed.
async fn uninstall_data_and_config() {
    info!("Cleaning up data directories and configs...");

    // OpenEBS data
    cmd::run_privileged("rm", &["-rf", "/var/openebs"]).await.ok();

    // kubeinit-managed kernel module and sysctl configs
    cmd::run_privileged("rm", &["-f", "/etc/modules-load.d/kubeinit.conf"]).await.ok();
    cmd::run_privileged("rm", &["-f", "/etc/sysctl.d/99-kubeinit.conf"]).await.ok();

    // User kubeconfig
    if let Some(user) = cmd::real_user() {
        let kube_dir = format!("{}/.kube", user.home);
        cmd::run_privileged("rm", &["-rf", &kube_dir]).await.ok();
    }
}

/// Gracefully stop all cluster workloads and control-plane containers before
/// removing binaries. Each step is best-effort — failures are logged but do
/// not abort the uninstall.
async fn stop_cluster_workloads() {
    // 1. If kubectl is available and a cluster is reachable, try to drain this node
    if cmd::binary_exists("kubectl").await {
        let hostname = cmd::run_output("hostname", &[])
            .await
            .unwrap_or_default();
        if !hostname.is_empty() {
            info!("Draining node {hostname}...");
            cmd::run(
                "kubectl",
                &[
                    "drain", &hostname,
                    "--ignore-daemonsets",
                    "--delete-emptydir-data",
                    "--force",
                    "--grace-period=30",
                    "--timeout=60s",
                ],
            )
            .await
            .ok();
        }
    }

    // 2. Run kubeadm reset to tear down the control plane
    if cmd::binary_exists("kubeadm").await {
        info!("Running kubeadm reset...");
        cmd::run_privileged("kubeadm", &["reset", "--force"]).await.ok();
    }

    // 3. Stop kubelet service so it doesn't restart containers
    cmd::run_privileged("systemctl", &["stop", "kubelet"]).await.ok();

    // 4. Stop all remaining containers via crictl, then wait for exit
    if cmd::binary_exists("crictl").await {
        info!("Stopping all containers via crictl...");

        // List running container IDs
        let container_ids = cmd::run_privileged_output("crictl", &["ps", "-q"])
            .await
            .unwrap_or_default();

        if !container_ids.is_empty() {
            // Stop each container (30s grace period)
            for id in container_ids.lines() {
                let id = id.trim();
                if !id.is_empty() {
                    cmd::run_privileged("crictl", &["stop", "--timeout", "30", id])
                        .await
                        .ok();
                }
            }

            // Wait for containers to actually exit — poll up to 60s
            info!("Waiting for containers to exit...");
            for _ in 0..12 {
                let remaining = cmd::run_privileged_output("crictl", &["ps", "-q"])
                    .await
                    .unwrap_or_default();
                if remaining.trim().is_empty() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }

            // Force-remove any that are still around
            let stragglers = cmd::run_privileged_output("crictl", &["ps", "-q"])
                .await
                .unwrap_or_default();
            for id in stragglers.lines() {
                let id = id.trim();
                if !id.is_empty() {
                    cmd::run_privileged("crictl", &["rm", "--force", id]).await.ok();
                }
            }
        }

        // Remove all pods (sandboxes)
        info!("Removing pod sandboxes...");
        let pod_ids = cmd::run_privileged_output("crictl", &["pods", "-q"])
            .await
            .unwrap_or_default();
        for id in pod_ids.lines() {
            let id = id.trim();
            if !id.is_empty() {
                cmd::run_privileged("crictl", &["stopp", id]).await.ok();
                cmd::run_privileged("crictl", &["rmp", "--force", id]).await.ok();
            }
        }

        info!("All containers and pods stopped");
    }
}

// ── containerd ───────────────────────────────────────────────────────────────

async fn install_containerd(version: Option<&str>) -> Result<()> {
    if cmd::binary_exists("containerd").await {
        info!("containerd already installed, skipping");
        return Ok(());
    }

    let version = version.unwrap_or(config::DEFAULT_CONTAINERD_VERSION);
    let arch = go_arch()?;
    let url = expand_url(config::URL_CONTAINERD, version, arch);
    let service_url = expand_url(config::URL_CONTAINERD_SERVICE, version, arch);
    let service_dest = format!("{}/containerd.service", config::PATH_SYSTEMD_UNIT_DIR);
    let conf_toml = format!("{}/config.toml", config::PATH_CONTAINERD_CONF_DIR);

    info!("Installing containerd {version}...");
    cmd::run_privileged("bash", &[
        "-c",
        &format!("curl -fsSL '{url}' | tar -xz -C {}", config::PATH_BIN_DIR.trim_end_matches("/bin")),
    ]).await?;

    // Install the systemd unit and enable the service
    cmd::run_privileged("bash", &[
        "-c",
        &format!("curl -fsSL '{service_url}' -o '{service_dest}'"),
    ]).await?;

    // Write default config with SystemdCgroup enabled
    cmd::run_privileged("mkdir", &["-p", config::PATH_CONTAINERD_CONF_DIR]).await?;
    cmd::run_privileged("bash", &[
        "-c",
        &format!("containerd config default > '{conf_toml}'"),
    ]).await?;
    cmd::run_privileged("sed", &[
        "-i",
        "s/SystemdCgroup = false/SystemdCgroup = true/",
        &conf_toml,
    ]).await?;

    cmd::run_privileged("systemctl", &["daemon-reload"]).await?;
    cmd::run_privileged("systemctl", &["enable", "--now", "containerd"]).await?;

    info!("containerd {version} installed");
    Ok(())
}

// ── runc ─────────────────────────────────────────────────────────────────────

async fn install_runc(version: Option<&str>) -> Result<()> {
    if cmd::binary_exists("runc").await {
        info!("runc already installed, skipping");
        return Ok(());
    }

    let version = version.unwrap_or(config::DEFAULT_RUNC_VERSION);
    let arch = go_arch()?;
    let url = expand_url(config::URL_RUNC, version, arch);
    let dest = format!("{}/runc", config::PATH_SBIN_DIR);

    info!("Installing runc {version}...");
    cmd::run_privileged("bash", &[
        "-c",
        &format!("curl -fsSL '{url}' -o '{dest}' && chmod 755 '{dest}'"),
    ]).await?;

    info!("runc {version} installed");
    Ok(())
}

// ── CNI plugins ──────────────────────────────────────────────────────────────

async fn install_cni_plugins(version: Option<&str>) -> Result<()> {
    let has_plugins = cmd::run_privileged_output("ls", &[config::PATH_CNI_BIN_DIR])
        .await
        .map(|o| !o.is_empty())
        .unwrap_or(false);

    if has_plugins {
        info!("CNI plugins already installed, skipping");
        return Ok(());
    }

    let version = version.unwrap_or(config::DEFAULT_CNI_PLUGINS_VERSION);
    let arch = go_arch()?;
    let url = expand_url(config::URL_CNI_PLUGINS, version, arch);

    info!("Installing CNI plugins {version}...");
    cmd::run_privileged("mkdir", &["-p", config::PATH_CNI_BIN_DIR]).await?;
    cmd::run_privileged("bash", &[
        "-c",
        &format!("curl -fsSL '{url}' | tar -xz -C {}", config::PATH_CNI_BIN_DIR),
    ]).await?;

    info!("CNI plugins {version} installed");
    Ok(())
}

// ── crictl ───────────────────────────────────────────────────────────────────

async fn install_crictl(version: Option<&str>) -> Result<()> {
    if cmd::binary_exists("crictl").await {
        info!("crictl already installed, skipping");
        return Ok(());
    }

    let version = version.unwrap_or(config::DEFAULT_CRICTL_VERSION);
    let arch = go_arch()?;
    let url = expand_url(config::URL_CRICTL, version, arch);

    info!("Installing crictl {version}...");
    cmd::run_privileged("bash", &[
        "-c",
        &format!("curl -fsSL '{url}' | tar -xz -C {}", config::PATH_BIN_DIR),
    ]).await?;

    // Point crictl at containerd by default
    cmd::run_privileged("bash", &[
        "-c",
        &format!(
            "cat > '{}' <<'EOF'\nruntime-endpoint: {}\nimage-endpoint: {}\ntimeout: 10\nEOF",
            config::PATH_CRICTL_CONFIG,
            config::CONTAINERD_SOCKET,
            config::CONTAINERD_SOCKET,
        ),
    ]).await?;

    info!("crictl {version} installed");
    Ok(())
}

// ── kubeadm / kubelet / kubectl ──────────────────────────────────────────────

async fn install_kubernetes_components(version: Option<&str>) -> Result<()> {
    let version = version.unwrap_or(config::DEFAULT_KUBERNETES_VERSION);
    let version_trimmed = version.trim_start_matches('v');
    let arch = go_arch()?;
    let base_url = expand_url(config::URL_KUBERNETES, version_trimmed, arch);

    for component in ["kubeadm", "kubelet", "kubectl"] {
        if cmd::binary_exists(component).await {
            info!("{component} already installed, skipping");
            continue;
        }

        let url = format!("{base_url}/{component}");
        let dest = format!("{}/{component}", config::PATH_BIN_DIR);

        info!("Installing {component} {version_trimmed}...");
        cmd::run_privileged("bash", &[
            "-c",
            &format!("curl -fsSL '{url}' -o '{dest}' && chmod 755 '{dest}'"),
        ]).await?;
    }

    install_kubelet_service(version_trimmed).await?;

    info!("Kubernetes components {version_trimmed} installed");
    Ok(())
}

async fn install_kubelet_service(version: &str) -> Result<()> {
    let minor = version
        .split('.')
        .take(2)
        .collect::<Vec<_>>()
        .join(".");

    let unit_dir = config::PATH_SYSTEMD_UNIT_DIR;
    let bin_dir = config::PATH_BIN_DIR;
    let k8s_conf = config::PATH_KUBERNETES_CONF_DIR;
    let kubelet_data = config::PATH_KUBELET_DATA_DIR;

    let script = format!(
        r#"mkdir -p {unit_dir}/kubelet.service.d
cat > {unit_dir}/kubelet.service <<'UNIT'
[Unit]
Description=kubelet: The Kubernetes Node Agent
Documentation=https://kubernetes.io/docs/
Wants=network-online.target
After=network-online.target

[Service]
ExecStart={bin_dir}/kubelet
Restart=always
StartLimitInterval=0
RestartSec=10

[Install]
WantedBy=multi-user.target
UNIT

cat > {unit_dir}/kubelet.service.d/10-kubeadm.conf <<'DROP'
[Service]
Environment="KUBELET_KUBECONFIG_ARGS=--bootstrap-kubeconfig={k8s_conf}/bootstrap-kubelet.conf --kubeconfig={k8s_conf}/kubelet.conf"
Environment="KUBELET_CONFIG_ARGS=--config={kubelet_data}/config.yaml"
EnvironmentFile=-{kubelet_data}/kubeadm-flags.env
ExecStart=
ExecStart={bin_dir}/kubelet $KUBELET_KUBECONFIG_ARGS $KUBELET_CONFIG_ARGS $KUBELET_KUBEADM_ARGS $KUBELET_EXTRA_ARGS
DROP"#
    );

    cmd::run_privileged("bash", &["-c", &script]).await?;
    cmd::run_privileged("systemctl", &["daemon-reload"]).await?;
    cmd::run_privileged("systemctl", &["enable", "kubelet"]).await?;

    info!("kubelet systemd service configured (v{minor}.x)");
    Ok(())
}

// ── Helm ─────────────────────────────────────────────────────────────────────

async fn install_helm(version: Option<&str>) -> Result<()> {
    if cmd::binary_exists("helm").await {
        info!("helm already installed, skipping");
        return Ok(());
    }

    let version = version.unwrap_or(config::DEFAULT_HELM_VERSION);
    let arch = go_arch()?;
    let url = expand_url(config::URL_HELM, version, arch);

    info!("Installing helm {version}...");
    cmd::run_privileged("bash", &[
        "-c",
        &format!(
            "curl -fsSL '{url}' | tar -xz --strip-components=1 -C {} linux-{arch}/helm",
            config::PATH_BIN_DIR
        ),
    ]).await?;

    info!("helm {version} installed");
    Ok(())
}

// ── Cilium CLI ───────────────────────────────────────────────────────────────

async fn install_cilium_cli(version: Option<&str>) -> Result<()> {
    if cmd::binary_exists("cilium").await {
        info!("cilium CLI already installed, skipping");
        return Ok(());
    }

    let version = version.unwrap_or(config::DEFAULT_CILIUM_CLI_VERSION);
    let arch = go_arch()?;
    let url = expand_url(config::URL_CILIUM_CLI, version, arch);

    info!("Installing cilium CLI {version}...");
    cmd::run_privileged("bash", &[
        "-c",
        &format!("curl -fsSL '{url}' | tar -xz -C {}", config::PATH_BIN_DIR),
    ]).await?;

    info!("cilium CLI {version} installed");
    Ok(())
}

// ── Uninstall helpers ────────────────────────────────────────────────────────

async fn uninstall_kubernetes_components() -> Result<()> {
    // kubelet already stopped by stop_cluster_workloads()
    cmd::run_privileged("systemctl", &["disable", "kubelet"]).await.ok();

    for component in ["kubeadm", "kubelet", "kubectl"] {
        if cmd::binary_exists(component).await {
            info!("Removing {component}...");
            cmd::run_privileged("rm", &["-f", &format!("{}/{component}", config::PATH_BIN_DIR)]).await?;
        }
    }

    let unit_dir = config::PATH_SYSTEMD_UNIT_DIR;
    cmd::run_privileged("rm", &["-f", &format!("{unit_dir}/kubelet.service")]).await.ok();
    cmd::run_privileged("rm", &["-rf", &format!("{unit_dir}/kubelet.service.d")]).await.ok();

    cmd::run_privileged("rm", &["-rf", config::PATH_KUBERNETES_CONF_DIR]).await.ok();
    cmd::run_privileged("rm", &["-rf", config::PATH_KUBELET_DATA_DIR]).await.ok();
    cmd::run_privileged("rm", &["-rf", config::PATH_ETCD_DATA_DIR]).await.ok();

    cmd::run_privileged("systemctl", &["daemon-reload"]).await.ok();

    info!("Kubernetes components removed");
    Ok(())
}

async fn uninstall_crictl() -> Result<()> {
    if !cmd::binary_exists("crictl").await {
        return Ok(());
    }
    info!("Removing crictl...");
    cmd::run_privileged("rm", &["-f", &format!("{}/crictl", config::PATH_BIN_DIR)]).await?;
    cmd::run_privileged("rm", &["-f", config::PATH_CRICTL_CONFIG]).await.ok();
    info!("crictl removed");
    Ok(())
}

async fn uninstall_cilium_cli() -> Result<()> {
    if !cmd::binary_exists("cilium").await {
        return Ok(());
    }
    info!("Removing cilium CLI...");
    cmd::run_privileged("rm", &["-f", &format!("{}/cilium", config::PATH_BIN_DIR)]).await?;
    info!("cilium CLI removed");
    Ok(())
}

async fn uninstall_helm() -> Result<()> {
    if !cmd::binary_exists("helm").await {
        return Ok(());
    }
    info!("Removing helm...");
    cmd::run_privileged("rm", &["-f", &format!("{}/helm", config::PATH_BIN_DIR)]).await?;
    info!("helm removed");
    Ok(())
}

async fn uninstall_cni_plugins() -> Result<()> {
    info!("Removing CNI plugins...");
    cmd::run_privileged("rm", &["-rf", config::PATH_CNI_BIN_DIR]).await.ok();
    cmd::run_privileged("rm", &["-rf", config::PATH_CNI_CONF_DIR]).await.ok();
    info!("CNI plugins removed");
    Ok(())
}

async fn uninstall_containerd() -> Result<()> {
    if !cmd::binary_exists("containerd").await {
        return Ok(());
    }
    info!("Stopping and removing containerd...");

    cmd::run_privileged("systemctl", &["stop", "containerd"]).await.ok();
    cmd::run_privileged("systemctl", &["disable", "containerd"]).await.ok();

    for bin in [
        "containerd",
        "containerd-shim",
        "containerd-shim-runc-v1",
        "containerd-shim-runc-v2",
        "containerd-stress",
        "ctr",
    ] {
        cmd::run_privileged("rm", &["-f", &format!("{}/{bin}", config::PATH_BIN_DIR)]).await.ok();
    }

    let unit_dir = config::PATH_SYSTEMD_UNIT_DIR;
    cmd::run_privileged("rm", &["-f", &format!("{unit_dir}/containerd.service")]).await.ok();
    cmd::run_privileged("rm", &["-rf", config::PATH_CONTAINERD_CONF_DIR]).await.ok();
    cmd::run_privileged("rm", &["-rf", config::PATH_CONTAINERD_DATA_DIR]).await.ok();
    cmd::run_privileged("rm", &["-rf", config::PATH_CONTAINERD_RUN_DIR]).await.ok();

    cmd::run_privileged("systemctl", &["daemon-reload"]).await.ok();

    info!("containerd removed");
    Ok(())
}

async fn uninstall_runc() -> Result<()> {
    if !cmd::binary_exists("runc").await {
        return Ok(());
    }
    info!("Removing runc...");
    cmd::run_privileged("rm", &["-f", &format!("{}/runc", config::PATH_SBIN_DIR)]).await?;
    info!("runc removed");
    Ok(())
}
